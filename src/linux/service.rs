// The background service on Linux (X11). It remembers each window's refs and runs agents side by
// side. Controls are read and pressed through AT-SPI, which needs no focus and leaves the pointer
// alone; points, keys and drawing go through XTest, which uses the display's one pointer and
// keyboard, so for those the window is brought forward and agents take turns.
use super::{
    atspi::{self, Node, Obj},
    overlay::Overlay,
    x::{self, Bounds, Button, Window},
};
use crate::{
    lines::{self, Seen},
    look, motion,
    pipe::{self, Pipe},
    shot::{self, Frame, Frames},
};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

pub const HELP: &str = "\
deskcrew drives Linux apps on X11 (a desktop or Xvfb). Refs stay valid, so read once.
windows [filter]                      list windows
see [window] [-f] [-a] [--find word] [--from N]
                                      controls; later reads show only changes (-f for all, -a off-screen too)
get e3                                a control's full text
shot [window|e3] [-f] [--marks] [--rulers] [--screen [-a]] [--region x,y,w,h] [--changed] [--edge N]
                                      PNG path; \"unchanged\" if it matches the last one
click e3 [e4 ...] | \"Name\" | x,y [--right] [--double] [--triple] [--middle] [--back] [--forward] [--with ctrl+shift] [--hold ms]
hover e3 | x,y     check e3     uncheck e3     toggle e3     expand e3     collapse e3     clear e3
is e3 on|off|open|closed|selected|enabled|disabled
down shift     up (lets go; 30 s limit)
set e3 \"value\"     type [e3] \"text\" [--paste]     key enter [--hold ms]     select e3 \"Item\"
scroll [e3] up|down|left|right [n]     show e3     wait \"text\" [--gone] [--ms N]
drag a b     draw e3 \"x,y x,y; x,y x,y\" [--smooth]
do \"click e1; set e2 hi; key enter\" [--dry] [--risk low|medium|high]   many steps, one reply
Any action: --expect \"text\" fails unless the text shows afterwards.
launch app [args]     focus     close     place x,y,w,h|min|max|restore
apps     screens     clip     clip set \"text\"     clip clear     status     resume     quit     setup (installs for your coding agents)     mcp (the same as MCP tools, over stdio)
Every action replies with what changed, in the picture too. x,y are pixels in the last shot, or text see read. Ctrl+Alt+Esc stops every agent.
Controls go through accessibility (AT-SPI) in the background. Points, keys and drawing use the X pointer and
keyboard, bringing the window forward for a moment. Flags: -w window, --agent name, --fast, --force (allow risky keys).
Wayland is not supported: it does not let one app read or drive another.";

const IDLE_EXIT: Duration = Duration::from_secs(30 * 60);
const SETTLE_FIRST: Duration = Duration::from_millis(60);
const SETTLE_STEP: Duration = Duration::from_millis(80);
const SETTLE_LEAST: Duration = Duration::from_millis(200);
const SETTLE_MOST: Duration = Duration::from_millis(900);
const BETWEEN_STEPS: Duration = Duration::from_millis(40);
const WAIT_DEFAULT_MS: u64 = 10_000;
const WAIT_MOST_MS: u64 = 60_000;
const WAIT_STEP: Duration = Duration::from_millis(200);
const MAX_WINDOWS: usize = 40;
const MAX_GET_CHARS: usize = 20_000;
const HOLD_MOST: Duration = Duration::from_secs(30);
const FRONT_SETTLE: Duration = Duration::from_millis(120);
const PASTE_SETTLE: Duration = Duration::from_millis(150);
const MENU_OPEN: Duration = Duration::from_millis(250);
const MENU_DEPTH: usize = 4;
const LEAST_CHANGE: u32 = 4;
const STOP_WATCH: Duration = Duration::from_millis(50);
const STOPPED: &str = "stopped by the person with Ctrl+Alt+Esc: ask them before doing more (deskcrew resume clears it)";
const NO_WINDOW: &str = "no window yet: run deskcrew windows, then deskcrew see <name>";
const MARKED: [&str; 11] = ["button", "edit", "menuitem", "checkbox", "radio", "combobox", "tab", "item", "link", "slider", "spinner"];
// Roles that hold other controls, and so may be a canvas rather than something covering its space.
const CONTAINERS: [&str; 2] = ["", "image"];

static STOP: AtomicBool = AtomicBool::new(false);

type Reply = Result<String, String>;

#[derive(Default)]
struct Refs {
    by_key: HashMap<usize, u32>,
    by_identity: HashMap<String, Vec<u32>>,
    next: u32,
}

impl Refs {
    /// The same control keeps its number across reads, even if the app rebuilt it.
    fn assign(&mut self, nodes: Vec<Node>) -> BTreeMap<u32, Node> {
        let mut out: BTreeMap<u32, Node> = BTreeMap::new();
        for node in nodes {
            let known = self.by_key.get(&node.key).copied().filter(|id| !out.contains_key(id));
            let same = self.by_identity.entry(node.identity.clone()).or_default();
            let id = known.or_else(|| same.iter().copied().find(|id| !out.contains_key(id))).unwrap_or_else(|| {
                self.next += 1;
                self.next
            });
            if !same.contains(&id) {
                same.push(id);
            }
            self.by_key.insert(node.key, id);
            out.insert(id, node);
        }
        out
    }
}

#[derive(Default)]
struct View {
    refs: Refs,
    nodes: BTreeMap<u32, Node>,
    lines: Seen,
    hidden: usize,
    element: Option<Obj>,
    read_once: bool,
    blind: Option<(u64, Vec<String>)>,
}

struct Tracked {
    id: u32,
    window: u32,
    view: Mutex<View>,
}

struct LastShot {
    hash: u64,
    frame: Frame,
    frames: Frames,
}

#[derive(Default)]
struct AgentState {
    current: Option<u32>,
    seen: HashMap<u32, Seen>,
    shots: HashMap<u32, LastShot>,
}

#[derive(Clone, Default)]
struct Flags {
    window: Option<String>,
    full: bool,
    all: bool,
    find: Option<String>,
    from: usize,
    right: bool,
    double: bool,
    triple: bool,
    middle: bool,
    back: bool,
    forward: bool,
    with: Option<String>,
    force: bool,
    paste: bool,
    smooth: bool,
    gone: bool,
    dry: bool,
    risk: Option<String>,
    marks: bool,
    rulers: bool,
    changed: bool,
    screen: bool,
    region: Option<String>,
    edge: Option<u32>,
    ms: Option<u64>,
    hold: Option<u64>,
    expect: Option<String>,
}

impl Flags {
    /// A step's own flags, with the command's switched on too.
    fn merged(mut self, outer: &Flags) -> Flags {
        self.right |= outer.right;
        self.double |= outer.double;
        self.triple |= outer.triple;
        self.middle |= outer.middle;
        self.back |= outer.back;
        self.forward |= outer.forward;
        self.force |= outer.force;
        self.paste |= outer.paste;
        self.smooth |= outer.smooth;
        self.gone |= outer.gone;
        self.with = self.with.or_else(|| outer.with.clone());
        self.ms = self.ms.or(outer.ms);
        self.hold = self.hold.or(outer.hold);
        self
    }
}

fn parse(words: &[String]) -> (Flags, Vec<String>) {
    let mut flags = Flags::default();
    let mut rest = Vec::new();
    let mut at = 0;
    while at < words.len() {
        let word = &words[at];
        let next = words.get(at + 1).cloned();
        let number = |text: &Option<String>| text.as_ref().and_then(|n| n.parse::<u64>().ok());
        let mut took = true;
        match word.as_str() {
            "-w" | "--window" => flags.window = next.clone(),
            "--find" => flags.find = next.clone(),
            "--region" => flags.region = next.clone(),
            "--expect" => flags.expect = next.clone(),
            "--with" => flags.with = next.clone(),
            "--risk" => flags.risk = next.clone(),
            "--from" => flags.from = number(&next).unwrap_or(0) as usize,
            "--edge" => flags.edge = number(&next).map(|n| n as u32),
            "--ms" | "--timeout" => flags.ms = number(&next),
            "--hold" => flags.hold = number(&next),
            _ => took = false,
        }
        if took {
            at += 2;
            continue;
        }
        match word.as_str() {
            "-f" | "--full" => flags.full = true,
            "-a" | "--all" => flags.all = true,
            "--right" => flags.right = true,
            "--double" => flags.double = true,
            "--triple" => flags.triple = true,
            "--middle" => flags.middle = true,
            "--back" => flags.back = true,
            "--forward" => flags.forward = true,
            "--force" => flags.force = true,
            "--paste" => flags.paste = true,
            "--smooth" => flags.smooth = true,
            "--gone" => flags.gone = true,
            "--dry" => flags.dry = true,
            "--marks" => flags.marks = true,
            "--rulers" => flags.rulers = true,
            "--changed" => flags.changed = true,
            "--screen" => flags.screen = true,
            // Accepted for scripts written for other systems; Linux has one way for each.
            "--real" | "--fast" | "--front" | "--mouse" => {}
            _ => rest.push(word.clone()),
        }
        at += 1;
    }
    (flags, rest)
}

fn is_ref(word: &str) -> Option<u32> {
    word.strip_prefix('e').and_then(|n| n.parse().ok())
}

fn is_point(word: &str) -> Option<(i32, i32)> {
    let (x, y) = word.split_once(',')?;
    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
}

fn centre(rect: Bounds) -> (i32, i32) {
    (rect.0 + rect.2 / 2, rect.1 + rect.3 / 2)
}

enum Target {
    Control(u32),
    Point(i32, i32),
}

/// Modifier keys one agent holds down.
struct Held {
    keys: Vec<u32>,
    since: Instant,
}

struct Service {
    held: Mutex<HashMap<String, Held>>,
    real_input: Mutex<()>,
    windows: Mutex<(u32, HashMap<u32, Arc<Tracked>>)>,
    agents: Mutex<HashMap<String, AgentState>>,
    last_used: Mutex<Instant>,
    quit: AtomicBool,
    overlay: Overlay,
}

impl Service {
    /// Glide this agent's drawn cursor to a spot, keep it above the agent's window, and light up
    /// the control there when it is pressed.
    fn cursor_on(&self, who: &str, tracked: &Tracked, rect: Bounds, press: bool) {
        let (px, py) = centre(rect);
        self.overlay.move_to(who, px, py);
        self.overlay.over(who, tracked.window);
        if press && rect.2 > 0 && rect.3 > 0 {
            self.overlay.press(who, rect);
        }
    }

    fn track(&self, window: &Window) -> Arc<Tracked> {
        let mut windows = self.windows.lock().unwrap();
        let (next, known) = &mut *windows;
        known
            .entry(window.id)
            .or_insert_with(|| {
                *next += 1;
                Arc::new(Tracked { id: *next, window: window.id, view: Mutex::new(View::default()) })
            })
            .clone()
    }

    fn by_id(&self, id: u32) -> Option<Arc<Tracked>> {
        self.windows.lock().unwrap().1.values().find(|t| t.id == id).cloned()
    }

    fn set_current(&self, who: &str, window: u32) {
        self.agents.lock().unwrap().entry(who.to_string()).or_default().current = Some(window);
    }

    fn pick(&self, who: &str, spec: Option<&str>) -> Result<Arc<Tracked>, String> {
        let Some(spec) = spec else {
            let current = self.agents.lock().unwrap().get(who).and_then(|a| a.current).ok_or(NO_WINDOW)?;
            let tracked = self.windows.lock().unwrap().1.get(&current).cloned().ok_or(NO_WINDOW)?;
            if x::find(tracked.window).is_none() {
                return Err("that window closed: run deskcrew windows".into());
            }
            return Ok(tracked);
        };
        if let Some(id) = spec.strip_prefix('w').and_then(|n| n.parse::<u32>().ok()) {
            let tracked = self.by_id(id).ok_or(format!("no window w{id}: run deskcrew windows"))?;
            if x::find(tracked.window).is_none() {
                return Err("that window closed: run deskcrew windows".into());
            }
            self.set_current(who, tracked.window);
            return Ok(tracked);
        }
        let wanted = spec.to_lowercase();
        let listed = x::list();
        let exact: Vec<&Window> = listed.iter().filter(|w| w.title.to_lowercase() == wanted || w.app.to_lowercase() == wanted).collect();
        let mut matches: Vec<&Window> = if exact.is_empty() {
            listed.iter().filter(|w| w.title.to_lowercase().contains(&wanted) || w.app.to_lowercase().contains(&wanted)).collect()
        } else {
            exact
        };
        if matches.len() > 1 {
            let current = self.agents.lock().unwrap().get(who).and_then(|a| a.current);
            if let Some(mine) = matches.iter().find(|w| Some(w.id) == current) {
                matches = vec![*mine];
            } else if matches.iter().filter(|w| !w.title.is_empty()).count() == 1 {
                matches.retain(|w| !w.title.is_empty());
            }
        }
        match matches.as_slice() {
            [] => Err(format!("no window matches '{spec}'")),
            [one] => {
                let tracked = self.track(one);
                self.set_current(who, one.id);
                Ok(tracked)
            }
            many => {
                let mut out = vec![format!("'{spec}' matches {} windows:", many.len())];
                out.extend(many.iter().take(8).map(|w| format!("w{} \"{}\" {}", self.track(w).id, lines::clip(&w.title, 50), w.app)));
                Err(out.join("\n"))
            }
        }
    }

    fn window_of(&self, tracked: &Tracked) -> Result<Window, String> {
        x::find(tracked.window).ok_or_else(|| "that window closed: run deskcrew windows".to_string())
    }

    fn read(&self, tracked: &Tracked, view: &mut View, all: bool) -> Result<(), String> {
        let window = self.window_of(tracked)?;
        atspi::bus()?;
        // A window's accessible object can change when the app rebuilds it: find it again then.
        let stale = view.element.as_ref().is_none_or(|e| atspi::role_name(e).is_empty());
        if stale {
            view.element = atspi::frame_for(window.pid, &window.title, window.rect);
        }
        let element = view.element.as_ref().ok_or(
            "that window does not answer accessibility requests: start the app with accessibility on (GTK and Qt apps are, once the AT-SPI bus runs; for Qt set QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1, for Chrome --force-renderer-accessibility)",
        )?;
        let (nodes, hidden) = atspi::read(element, window.rect, all);
        view.nodes = view.refs.assign(nodes);
        view.lines = view.nodes.iter().map(|(id, node)| (*id, lines::line(&node.parts))).collect();
        view.hidden = hidden;
        view.read_once = true;
        Ok(())
    }

    /// Read until the window stops changing, so a reply shows the result and not a moment in it.
    fn settle(&self, tracked: &Tracked, view: &mut View) -> Result<(), String> {
        let started = Instant::now();
        thread::sleep(SETTLE_FIRST);
        self.read(tracked, view, false)?;
        let mut last = view.lines.clone();
        while started.elapsed() < SETTLE_MOST {
            thread::sleep(SETTLE_STEP);
            self.read(tracked, view, false)?;
            let same = view.lines == last;
            last = view.lines.clone();
            if same && started.elapsed() >= SETTLE_LEAST {
                break;
            }
        }
        Ok(())
    }

    fn seen(&self, who: &str, window: u32) -> Option<Seen> {
        self.agents.lock().unwrap().get(who).and_then(|a| a.seen.get(&window).cloned())
    }

    fn remember(&self, who: &str, window: u32, lines: Seen) {
        self.agents.lock().unwrap().entry(who.to_string()).or_default().seen.insert(window, lines);
    }

    fn resolve(&self, word: &str, who: &str, tracked: &Tracked, view: &mut View) -> Result<Target, String> {
        if let Some((px, py)) = is_point(word) {
            let frames = self.agents.lock().unwrap().get(who).and_then(|a| a.shots.get(&tracked.window).map(|s| s.frames));
            let frames = frames.ok_or("x,y are pixels in a shot: take one first")?;
            let (px, py) = frames.to_screen(px, py);
            return Ok(Target::Point(px, py));
        }
        if let Some(id) = is_ref(word) {
            if !view.nodes.contains_key(&id) {
                self.read(tracked, view, false)?;
            }
            return if view.nodes.contains_key(&id) { Ok(Target::Control(id)) } else { Err(format!("e{id} is gone: see")) };
        }
        let wanted = word.to_lowercase();
        for attempt in 0..2 {
            if attempt == 1 || !view.read_once {
                self.read(tracked, view, false)?;
            }
            let exact: Vec<u32> = view.nodes.iter().filter(|(_, n)| n.parts.name.to_lowercase() == wanted).map(|(id, _)| *id).collect();
            let found = if exact.is_empty() {
                view.nodes.iter().filter(|(_, n)| n.parts.name.to_lowercase().contains(&wanted)).map(|(id, _)| *id).collect()
            } else {
                exact
            };
            match found.as_slice() {
                [] => continue,
                [one] => return Ok(Target::Control(*one)),
                many => {
                    let acting: Vec<u32> = many.iter().copied().filter(|id| MARKED.contains(&view.nodes[id].parts.role.as_str())).collect();
                    if let [one] = acting.as_slice() {
                        return Ok(Target::Control(*one));
                    }
                    let shown: Vec<String> = many.iter().take(6).map(|id| format!("e{id} {}", view.lines[id])).collect();
                    return Err(format!("'{word}' matches {} controls:\n{}", many.len(), shown.join("\n")));
                }
            }
        }
        Err(format!("no control named '{word}': see"))
    }

    fn point_of(&self, word: &str, who: &str, tracked: &Tracked, view: &mut View) -> Result<(i32, i32), String> {
        match self.resolve(word, who, tracked, view)? {
            Target::Point(px, py) => Ok((px, py)),
            Target::Control(id) => Ok(centre(atspi::rect(&view.nodes[&id].element).unwrap_or(view.nodes[&id].rect))),
        }
    }

    /// A control's box now, and its object, for acting on it.
    fn control(&self, word: &str, who: &str, tracked: &Tracked, view: &mut View) -> Result<(u32, Bounds, Obj), String> {
        match self.resolve(word, who, tracked, view)? {
            Target::Control(id) => {
                let node = &view.nodes[&id];
                Ok((id, atspi::rect(&node.element).unwrap_or(node.rect), node.element.clone()))
            }
            Target::Point(..) => Err("that needs a control".into()),
        }
    }

    /// Bring the window forward for `work` with the X pointer or keyboard, then give the front
    /// and the pointer back. Agents take turns here: the display has one of each.
    fn front<T>(&self, tracked: &Tracked, work: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        let _turn = self.real_input.lock().unwrap();
        let before = x::front();
        let pointer = x::pointer();
        if before != Some(tracked.window) {
            x::activate(tracked.window)?;
            thread::sleep(FRONT_SETTLE);
        }
        let result = work();
        thread::sleep(FRONT_SETTLE);
        if let Some(window) = before
            && window != tracked.window
        {
            let _ = x::activate(window);
        }
        if let Some((px, py)) = pointer {
            let _ = x::move_to(px, py);
        }
        result
    }

    fn held_keys(&self, who: &str) -> Vec<u32> {
        self.held.lock().unwrap().get(who).map(|h| h.keys.clone()).unwrap_or_default()
    }

    /// Let go of held keys: everyone's, or only those held too long or since Ctrl+Alt+Esc.
    fn release(&self, all: bool) -> Vec<String> {
        let stopped = STOP.load(Ordering::Relaxed);
        let mut held = self.held.lock().unwrap();
        let mut gone = Vec::new();
        held.retain(|who, keys| {
            let keep = !all && !stopped && keys.since.elapsed() < HOLD_MOST;
            if !keep {
                let _ = x::modifiers(&keys.keys, false);
                gone.push(who.clone());
            }
            keep
        });
        gone
    }

    fn click(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, word: &str) -> Reply {
        let button = match () {
            _ if flags.right => Button::Right,
            _ if flags.middle => Button::Middle,
            _ if flags.back => Button::Back,
            _ if flags.forward => Button::Forward,
            _ => Button::Left,
        };
        let count = if flags.triple { 3 } else if flags.double { 2 } else { 1 };
        let mut with = match &flags.with {
            Some(spec) => x::chord(spec)?.modifiers,
            None => Vec::new(),
        };
        with.extend(self.held_keys(who));
        let (rect, element) = match self.resolve(word, who, tracked, view)? {
            Target::Point(px, py) => ((px - 6, py - 6, 12, 12), None),
            Target::Control(id) => {
                let node = &view.nodes[&id];
                (atspi::rect(&node.element).unwrap_or(node.rect), Some(node.element.clone()))
            }
        };
        self.cursor_on(who, tracked, rect, element.is_some());
        let plain = with.is_empty() && count == 1 && flags.hold.is_none() && button == Button::Left;
        // Clicking a text field puts the caret there: focus does that without the pointer.
        if let Some(element) = &element
            && plain
            && atspi::editable(element)
            && atspi::focus(element).is_ok()
        {
            return Ok("focus".into());
        }
        // An action the app performs itself works behind other windows and takes no focus.
        if let Some(element) = &element
            && with.is_empty()
            && count == 1
            && flags.hold.is_none()
            && button == Button::Left
            && let Ok(action) = atspi::press(element)
        {
            return Ok(action);
        }
        let (px, py) = centre(rect);
        let hold = Duration::from_millis(flags.hold.unwrap_or(0).min(HOLD_MOST.as_millis() as u64));
        self.front(tracked, || {
            x::modifiers(&with, true)?;
            let clicked = x::click(px, py, button, count, hold);
            x::modifiers(&with, false)?;
            clicked
        })?;
        Ok("pointer".into())
    }

    fn hover(&self, who: &str, tracked: &Tracked, view: &mut View, word: &str) -> Reply {
        let (px, py) = self.point_of(word, who, tracked, view)?;
        self.cursor_on(who, tracked, (px, py, 0, 0), false);
        let _turn = self.real_input.lock().unwrap();
        x::move_to(px, py)?;
        Ok("pointer".into())
    }

    fn set(&self, who: &str, tracked: &Tracked, view: &mut View, word: &str, value: &str) -> Reply {
        let (_, rect, element) = self.control(word, who, tracked, view)?;
        self.cursor_on(who, tracked, rect, true);
        self.overlay.typing(who, value.chars().count());
        if atspi::editable(&element) {
            atspi::set_text(&element, value)?;
            return Ok("text".into());
        }
        if atspi::has_value(&element)
            && let Ok(number) = value.trim().parse::<f64>()
        {
            atspi::set_value(&element, number)?;
            return Ok("value".into());
        }
        Err("that control's text can't be set: try type".into())
    }

    /// Leave a checkbox or switch alone if it is already that way, or press it once.
    fn ensure(&self, who: &str, tracked: &Tracked, view: &mut View, word: &str, wanted: Option<&'static str>) -> Reply {
        let (id, rect, element) = self.control(word, who, tracked, view)?;
        self.cursor_on(who, tracked, rect, true);
        if let Some(wanted) = wanted
            && view.nodes[&id].parts.states.contains(&wanted)
        {
            return Ok(format!("already {wanted}"));
        }
        atspi::perform(&element, &["toggle", "click", "press", "activate", "*"])
    }

    fn expand(&self, who: &str, tracked: &Tracked, view: &mut View, word: &str, open: bool) -> Reply {
        let (id, rect, element) = self.control(word, who, tracked, view)?;
        self.cursor_on(who, tracked, rect, true);
        let wanted = if open { "open" } else { "closed" };
        if view.nodes[&id].parts.states.contains(&wanted) {
            return Ok(format!("already {wanted}"));
        }
        atspi::perform(&element, &["expand or contract", if open { "expand" } else { "collapse" }, "press", "click", "activate"])
            .map_err(|_| format!("that control cannot be {}", if open { "expanded" } else { "collapsed" }))
    }

    fn show(&self, who: &str, tracked: &Tracked, view: &mut View, word: &str) -> Reply {
        let (_, rect, element) = self.control(word, who, tracked, view)?;
        self.cursor_on(who, tracked, rect, false);
        atspi::show(&element)?;
        Ok("scroll".into())
    }

    /// Pick an entry from a combo box or a list.
    fn select(&self, who: &str, tracked: &Tracked, view: &mut View, word: &str, item: &str) -> Reply {
        let (id, rect, element) = self.control(word, who, tracked, view)?;
        self.cursor_on(who, tracked, rect, true);
        if view.nodes[&id].parts.role == "edit" && atspi::editable(&element) {
            atspi::set_text(&element, item)?;
            return Ok("text".into());
        }
        let combo = view.nodes[&id].parts.role == "combobox";
        if combo && atspi::choose(&element, item).is_ok() {
            return Ok("select".into());
        }
        if !combo
            && let Some(entry) = atspi::find_named(&element, item, MENU_DEPTH)
            && (atspi::select_child(&element, &entry).is_ok() || atspi::press(&entry).is_ok())
        {
            return Ok("select".into());
        }
        // A combo box may build its list only once it opens.
        if combo {
            atspi::press(&element).map_err(|_| "that dropdown did not answer".to_string())?;
            thread::sleep(MENU_OPEN);
            if let Some(entry) = atspi::find_named(&element, item, MENU_DEPTH)
                && atspi::press(&entry).is_ok()
            {
                thread::sleep(MENU_OPEN);
                if atspi::selected_name(&element).is_some_and(|n| n.eq_ignore_ascii_case(item)) {
                    return Ok("select".into());
                }
            }
            let _ = self.front(tracked, || x::press(&x::chord("esc")?, Duration::ZERO));
        }
        Err(format!("no entry named '{item}'"))
    }

    fn type_text(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, words: &[String]) -> Reply {
        let (target, text) = match words {
            [text] => (None, text.as_str()),
            [target, text] => (Some(target.as_str()), text.as_str()),
            _ => return Err("type [control] \"text\"".into()),
        };
        // Into a named field: insert through accessibility, in the background. Without one, keys
        // go to whatever has focus, so they replace a selection as typing would.
        let field = match target {
            Some(word) => {
                let (_, rect, element) = self.control(word, who, tracked, view)?;
                self.cursor_on(who, tracked, rect, true);
                Some(element)
            }
            None => None,
        };
        self.overlay.typing(who, text.chars().count());
        if !flags.paste
            && let Some(field) = &field
            && atspi::editable(field)
            && atspi::insert_text(field, text).is_ok()
        {
            return Ok("text".into());
        }
        if let Some(field) = &field {
            let _ = atspi::focus(field);
        }
        if flags.paste {
            let saved = x::clipboard().ok().flatten();
            x::set_clipboard(Some(text))?;
            thread::sleep(PASTE_SETTLE);
            let sent = self.front(tracked, || x::press(&x::chord("ctrl+v")?, Duration::ZERO));
            thread::sleep(PASTE_SETTLE);
            let _ = x::set_clipboard(saved.as_deref());
            sent?;
            return Ok("paste".into());
        }
        let stop = || STOP.load(Ordering::Relaxed);
        let typed = self.front(tracked, || x::type_text(text, &stop))?;
        if typed < text.chars().count() {
            return Err(format!("{STOPPED} (typed {typed} of {} characters)", text.chars().count()));
        }
        Ok("keys".into())
    }

    fn key(&self, who: &str, flags: &Flags, tracked: &Tracked, spec: &str) -> Reply {
        let mut chord = x::chord(spec)?;
        if x::is_risky(&chord) && !flags.force {
            return Err(format!("'{spec}' can quit or close things: ask the person, then add --force"));
        }
        chord.modifiers.retain(|m| !self.held_keys(who).contains(m));
        let hold = Duration::from_millis(flags.hold.unwrap_or(0).min(HOLD_MOST.as_millis() as u64));
        self.front(tracked, || x::press(&chord, hold))?;
        Ok("keys".into())
    }

    /// Hold modifier keys for this agent's later clicks and keys, until `up`.
    fn down(&self, who: &str, words: &[String]) -> Reply {
        let [spec] = words else { return Err("down shift (or ctrl, alt, super)".into()) };
        if matches!(spec.as_str(), "left" | "right" | "middle") {
            return Err("holding a mouse button is not on Linux yet".into());
        }
        let chord = x::chord(spec)?;
        if chord.key.is_some() {
            return Err("down holds modifier keys: shift, ctrl, alt or super".into());
        }
        let _turn = self.real_input.lock().unwrap();
        x::modifiers(&chord.modifiers, true)?;
        let mut held = self.held.lock().unwrap();
        let entry = held.entry(who.to_string()).or_insert(Held { keys: Vec::new(), since: Instant::now() });
        entry.keys.extend(chord.modifiers);
        entry.since = Instant::now();
        Ok("held".into())
    }

    fn scroll(&self, who: &str, tracked: &Tracked, view: &mut View, words: &[String]) -> Reply {
        let direction = |word: &String| matches!(word.as_str(), "up" | "down" | "left" | "right");
        let at = words.iter().position(direction).ok_or("scroll [control] up|down|left|right [n]")?;
        let times: i32 = words.get(at + 1).and_then(|n| n.parse().ok()).unwrap_or(3);
        let (px, py) = match words[..at].first() {
            Some(word) => self.point_of(word, who, tracked, view)?,
            None => centre(self.window_of(tracked)?.rect),
        };
        self.cursor_on(who, tracked, (px, py, 0, 0), false);
        let sideways = words[at] == "left" || words[at] == "right";
        let notches = if words[at] == "up" || words[at] == "right" { times } else { -times };
        let notches = if sideways { -notches } else { notches };
        self.front(tracked, || x::wheel(px, py, notches, sideways))?;
        Ok("pointer".into())
    }

    fn strokes(&self, who: &str, tracked: &Tracked, strokes: &[Vec<(f32, f32)>]) -> Reply {
        let window = self.window_of(tracked)?.rect;
        let inside = |p: &(f32, f32)| {
            let (px, py) = (p.0 as i32, p.1 as i32);
            px >= window.0 && py >= window.1 && px < window.0 + window.2 && py < window.1 + window.3
        };
        if !strokes.iter().flatten().all(inside) {
            return Err("refused: that stroke leaves the window".into());
        }
        let stop = || STOP.load(Ordering::Relaxed);
        if let Some(&(px, py)) = strokes.first().and_then(|s| s.first()) {
            self.cursor_on(who, tracked, (px as i32, py as i32, 0, 0), false);
        }
        // The drawn cursor rides along with the pen.
        let finished = self.front(tracked, || {
            for stroke in strokes {
                if !x::stroke(stroke, &stop, &mut |_, px, py| self.overlay.jump(who, px, py))? {
                    return Ok(false);
                }
            }
            Ok(true)
        })?;
        if finished { Ok("pointer".into()) } else { Err(STOPPED.into()) }
    }

    fn drag(&self, who: &str, tracked: &Tracked, view: &mut View, from: &str, to: &str) -> Reply {
        let a = self.point_of(from, who, tracked, view)?;
        let b = self.point_of(to, who, tracked, view)?;
        self.strokes(who, tracked, &[motion::trace(&[a, b], false)])
    }

    /// Strokes of x,y points in the last shot, or from a control's top left.
    fn draw(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, words: &[String]) -> Reply {
        let (word, script) = match words {
            [script] => (None, script),
            [word, script] => (Some(word), script),
            _ => return Err("draw [control] \"x,y x,y; x,y x,y\"".into()),
        };
        let origin = match word {
            Some(word) => Some(self.control(word, who, tracked, view)?.1),
            None => None,
        };
        let frames = self.agents.lock().unwrap().get(who).and_then(|a| a.shots.get(&tracked.window).map(|s| s.frames));
        let mut strokes = Vec::new();
        for stroke in script.split(';').filter(|s| !s.trim().is_empty()) {
            let mut points = Vec::new();
            for word in stroke.split_whitespace() {
                let (px, py) = is_point(word).ok_or(format!("'{word}' is not x,y"))?;
                points.push(match (origin, frames) {
                    (Some(rect), _) => (rect.0 + px, rect.1 + py),
                    (None, Some(frames)) => frames.to_screen(px, py),
                    (None, None) => return Err("draw on a control, or take a shot first".into()),
                });
            }
            if !points.is_empty() {
                strokes.push(motion::trace(&points, flags.smooth));
            }
        }
        if strokes.is_empty() {
            return Err("nothing to draw".into());
        }
        self.strokes(who, tracked, &strokes)
    }

    fn wait(&self, flags: &Flags, tracked: &Tracked, view: &mut View, text: &str) -> Reply {
        let limit = Duration::from_millis(flags.ms.unwrap_or(WAIT_DEFAULT_MS).min(WAIT_MOST_MS));
        let wanted = text.to_lowercase();
        let started = Instant::now();
        loop {
            self.read(tracked, view, false)?;
            let shows = view.lines.values().any(|line| line.to_lowercase().contains(&wanted));
            if shows != flags.gone {
                return Ok(if started.elapsed() < WAIT_STEP { "found".into() } else { "waited".into() });
            }
            if started.elapsed() >= limit {
                return Err(format!("timed out waiting for '{text}'"));
            }
            if STOP.load(Ordering::Relaxed) {
                return Err(STOPPED.into());
            }
            thread::sleep(WAIT_STEP);
        }
    }

    fn step(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, words: &[String]) -> Reply {
        let (own, words) = parse(words);
        let flags = own.merged(flags);
        let (command, args) = words.split_first().ok_or("empty step")?;
        match (command.as_str(), args) {
            ("click", [word]) => self.click(who, &flags, tracked, view, word),
            ("hover", [word]) => self.hover(who, tracked, view, word),
            ("set", [word, value]) => self.set(who, tracked, view, word, value),
            ("clear", [word]) => self.set(who, tracked, view, word, ""),
            ("check", [word]) => self.ensure(who, tracked, view, word, Some("on")),
            ("uncheck", [word]) => self.ensure(who, tracked, view, word, Some("off")),
            ("toggle", [word]) => self.ensure(who, tracked, view, word, None),
            ("expand", [word]) => self.expand(who, tracked, view, word, true),
            ("collapse", [word]) => self.expand(who, tracked, view, word, false),
            ("show", [word]) => self.show(who, tracked, view, word),
            ("select", [word, item]) => self.select(who, tracked, view, word, item),
            ("type", rest) => self.type_text(who, &flags, tracked, view, rest),
            ("key", [spec]) => self.key(who, &flags, tracked, spec),
            ("down", rest) => self.down(who, rest),
            ("scroll", rest) => self.scroll(who, tracked, view, rest),
            ("drag", [from, to]) => self.drag(who, tracked, view, from, to),
            ("draw", rest) => self.draw(who, &flags, tracked, view, rest),
            ("wait", [text]) => self.wait(&flags, tracked, view, text),
            _ => Err(format!("can't do '{}': see deskcrew help", words.join(" "))),
        }
    }

    /// Where the window's picture changed since `before`, ignoring a caret.
    fn picture_change(&self, tracked: &Tracked, before: &Frame) -> Option<String> {
        let (after, _) = x::window_picture(tracked.window).ok()?;
        let (cx, cy, w, h) = after.changed_from(before)?;
        if w < LEAST_CHANGE || h < LEAST_CHANGE {
            return None;
        }
        Some(format!("controls unchanged; the picture changed at {cx},{cy} {w}x{h}"))
    }

    /// Run steps against one window, then report the result as a single set of changes.
    fn act(&self, who: &str, flags: &Flags, steps: &[Vec<String>]) -> Reply {
        if STOP.load(Ordering::Relaxed) {
            return Err(STOPPED.into());
        }
        let tracked = self.pick(who, flags.window.as_deref())?;
        let mut guard = tracked.view.lock().unwrap();
        let view = &mut *guard;
        if !view.read_once {
            self.read(&tracked, view, false)?;
        }
        let before = self.seen(who, tracked.window).unwrap_or_else(|| view.lines.clone());
        let picture_before = x::window_picture(tracked.window).ok().map(|(f, _)| f).filter(|f| !f.is_blank());
        let mut routes = Vec::new();
        let mut failure = None;
        for step in steps {
            if STOP.load(Ordering::Relaxed) {
                failure = Some(STOPPED.to_string());
                break;
            }
            match self.step(who, flags, &tracked, view, step) {
                Ok(route) => routes.push(route),
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
            thread::sleep(BETWEEN_STEPS);
        }
        let waited_only = !routes.is_empty() && routes.iter().all(|r| r == "found" || r == "waited");
        let settled = if waited_only { Ok(()) } else { self.settle(&tracked, view) };
        let mut out = vec![if routes.len() > 1 { format!("ok {} steps", routes.len()) } else { format!("ok {}", routes.first().map_or("", String::as_str)) }];
        match settled {
            Ok(()) => {
                let (changes, complete) = lines::change_report(&before, &view.lines);
                let nothing = changes.is_empty();
                out.extend(changes);
                if complete {
                    self.remember(who, tracked.window, view.lines.clone());
                }
                let picture = if nothing && !waited_only { picture_before.as_ref().map(|b| self.picture_change(&tracked, b)) } else { None };
                match picture {
                    Some(Some(note)) => out.push(note),
                    Some(None) => out.push("nothing changed, on screen either".into()),
                    None if nothing && !waited_only => out.push("nothing changed: check with shot".into()),
                    None => {}
                }
            }
            Err(error) => out.push(error),
        }
        match failure {
            Some(error) if routes.is_empty() => Err(error),
            Some(error) => {
                out.push(format!("failed at step {}: {error}", routes.len() + 1));
                Err(out.join("\n"))
            }
            None => Ok(out.join("\n")),
        }
    }

    fn windows_list(&self, who: &str, filter: Option<&String>) -> Reply {
        x::ready()?;
        let wanted = filter.map(|f| f.to_lowercase());
        let current = self.agents.lock().unwrap().get(who).and_then(|a| a.current);
        let listed = x::list();
        self.windows.lock().unwrap().1.retain(|id, _| listed.iter().any(|w| w.id == *id));
        let shown: Vec<String> = listed
            .iter()
            .filter(|w| wanted.as_ref().is_none_or(|f| w.title.to_lowercase().contains(f) || w.app.to_lowercase().contains(f)))
            .take(MAX_WINDOWS)
            .map(|w| {
                let mark = if current == Some(w.id) { "*" } else { "" };
                format!("w{}{mark} \"{}\" {}", self.track(w).id, lines::clip(&w.title, 50), w.app)
            })
            .collect();
        Ok(if shown.is_empty() { "no windows".into() } else { shown.join("\n") })
    }

    fn see(&self, who: &str, flags: &Flags, words: &[String]) -> Reply {
        let spec = words.first().map(String::as_str).or(flags.window.as_deref());
        let tracked = self.pick(who, spec)?;
        let mut guard = tracked.view.lock().unwrap();
        let view = &mut *guard;
        self.read(&tracked, view, flags.all)?;
        let filtered = flags.find.is_some() || flags.from > 0;
        let seen = self.seen(who, tracked.window).filter(|_| !flags.full && !filtered && !flags.all);
        if let Some(seen) = seen {
            let changes = lines::changes(&seen, &view.lines);
            if changes.len() * 2 <= view.lines.len().max(1) {
                self.remember(who, tracked.window, view.lines.clone());
                return Ok(if changes.is_empty() { "no change".into() } else { changes.join("\n") });
            }
        }
        if !filtered {
            self.remember(who, tracked.window, view.lines.clone());
        }
        let window = self.window_of(&tracked)?;
        let hidden = if view.hidden > 0 { format!(", {} off screen", view.hidden) } else { String::new() };
        let mut out = vec![format!("w{} \"{}\" {} controls{hidden}", tracked.id, lines::clip(&window.title, 50), view.lines.len())];
        out.extend(lines::page(&view.lines, flags.find.as_deref(), flags.from));
        if !filtered {
            out.extend(self.blind_spot(who, &tracked, &window, view));
        }
        Ok(out.join("\n"))
    }

    /// Point out the largest part of the window with no controls in it.
    fn blind_spot(&self, who: &str, tracked: &Tracked, window: &Window, view: &mut View) -> Vec<String> {
        let rect = window.rect;
        let rects: Vec<_> = view.nodes.values().map(|n| (n.rect, CONTAINERS.contains(&n.parts.role.as_str()) && n.parts.name.is_empty())).collect();
        let Some((bx, by, w, h)) = look::empty_area(rect, &rects) else { return Vec::new() };
        let Ok(part) = x::capture((bx, by, w, h)) else { return Vec::new() };
        if !part.busy() {
            return Vec::new();
        }
        let hash = part.hash();
        if let Some((seen, said)) = &view.blind
            && *seen == hash
        {
            return said.clone();
        }
        let said = vec![format!("no controls in {},{} {w}x{h}, only drawing: shot --region {bx},{by},{w},{h}", bx - rect.0, by - rect.1)];
        // x,y in the window read as screen pixels from its corner until the next shot.
        let mut agents = self.agents.lock().unwrap();
        let state = agents.entry(who.to_string()).or_default();
        state.shots.entry(tracked.window).or_insert(LastShot { hash: 0, frame: Frame { w: 0, h: 0, pixels: Vec::new() }, frames: Frames { x: rect.0, y: rect.1, scale: 1.0 } });
        view.blind = Some((hash, said.clone()));
        said
    }

    fn get(&self, who: &str, flags: &Flags, words: &[String]) -> Reply {
        let word = words.first().ok_or("get needs a control")?;
        let tracked = self.pick(who, flags.window.as_deref())?;
        let mut view = tracked.view.lock().unwrap();
        if !view.read_once {
            self.read(&tracked, &mut view, false)?;
        }
        let Target::Control(id) = self.resolve(word, who, &tracked, &mut view)? else { return Err("get needs a control".into()) };
        if view.nodes[&id].parts.states.contains(&"password") {
            return Err("that is a password field: its text stays hidden".into());
        }
        Ok(atspi::full_text(&view.nodes[&id].element).chars().take(MAX_GET_CHARS).collect())
    }

    fn save_screen(&self, who: &str, rect: Bounds, edge: u32) -> Reply {
        let frame = x::capture(rect)?;
        let (small, scale) = frame.shrink(edge);
        let path = shot::path_for(who, 0);
        small.save(&path)?;
        Ok(format!("{} {}x{} scale {:.2}", path.display(), small.w, small.h, scale))
    }

    fn shot(&self, who: &str, flags: &Flags, words: &[String]) -> Reply {
        x::ready()?;
        let edge = flags.edge.unwrap_or(shot::DEFAULT_EDGE);
        if let Some(region) = &flags.region {
            let parts: Vec<i32> = region.split(',').filter_map(|n| n.trim().parse().ok()).collect();
            let [rx, ry, w, h] = parts[..] else { return Err("--region x,y,w,h".into()) };
            if w < 1 || h < 1 {
                return Err("--region needs a width and height".into());
            }
            return self.save_screen(who, (rx, ry, w, h), edge);
        }
        if flags.screen {
            let screens = x::screens();
            let main = screens.iter().find(|(_, main)| *main).map(|(r, _)| *r).unwrap_or((0, 0, 1, 1));
            let rect = if flags.all {
                let left = screens.iter().map(|(r, _)| r.0).min().unwrap_or(0);
                let top = screens.iter().map(|(r, _)| r.1).min().unwrap_or(0);
                let right = screens.iter().map(|(r, _)| r.0 + r.2).max().unwrap_or(1);
                let bottom = screens.iter().map(|(r, _)| r.1 + r.3).max().unwrap_or(1);
                (left, top, right - left, bottom - top)
            } else {
                main
            };
            return self.save_screen(who, rect, edge);
        }
        let word = words.first();
        let control = word.and_then(|w| is_ref(w));
        let tracked = self.pick(who, if control.is_some() { flags.window.as_deref() } else { word.map(String::as_str).or(flags.window.as_deref()) })?;
        let window = self.window_of(&tracked)?;
        let (left, top, _, _) = window.rect;
        let (mut frame, _) = x::window_picture(tracked.window)?;
        let mut origin = (left, top);
        if let Some(id) = control {
            let mut view = tracked.view.lock().unwrap();
            if !view.nodes.contains_key(&id) {
                self.read(&tracked, &mut view, false)?;
            }
            let node = view.nodes.get(&id).ok_or(format!("e{id} is gone: see"))?;
            let rect = atspi::rect(&node.element).unwrap_or(node.rect);
            frame = frame.crop((rect.0 - left).max(0) as u32, (rect.1 - top).max(0) as u32, rect.2.max(1) as u32, rect.3.max(1) as u32);
            origin = (rect.0.max(left), rect.1.max(top));
        }
        let hash = frame.hash();
        let mut agents = self.agents.lock().unwrap();
        let state = agents.entry(who.to_string()).or_default();
        let last = state.shots.get(&tracked.window);
        if control.is_none() && !flags.full && !flags.marks && !flags.rulers && last.is_some_and(|l| l.hash == hash) {
            return Ok("unchanged".into());
        }
        let crop = if flags.changed && control.is_none() { last.and_then(|l| frame.changed_from(&l.frame)) } else { None };
        let picture = match crop {
            Some((cx, cy, w, h)) => {
                origin = (origin.0 + cx as i32, origin.1 + cy as i32);
                frame.crop(cx, cy, w, h)
            }
            None => frame.crop(0, 0, frame.w, frame.h),
        };
        let (mut small, scale) = picture.shrink(edge);
        if flags.marks {
            let to_image = |value: i32, from: i32| ((value - from) as f32 / scale) as i32;
            let view = tracked.view.lock().unwrap();
            for (id, node) in view.nodes.iter().filter(|(_, node)| MARKED.contains(&node.parts.role.as_str())) {
                let (nx, ny, w, h) = node.rect;
                small.mark(to_image(nx, origin.0), to_image(ny, origin.1), (w as f32 / scale) as i32, (h as f32 / scale) as i32, *id);
            }
        }
        if flags.rulers {
            small.rulers();
        }
        let path = shot::path_for(who, tracked.id);
        small.save(&path)?;
        let frames = Frames { x: origin.0, y: origin.1, scale };
        if control.is_none() {
            state.shots.insert(tracked.window, LastShot { hash, frame, frames });
        } else if let Some(last) = state.shots.get_mut(&tracked.window) {
            last.frames = frames;
        } else {
            state.shots.insert(tracked.window, LastShot { hash: 0, frame: Frame { w: 0, h: 0, pixels: Vec::new() }, frames });
        }
        let part = if crop.is_some() { " changed part" } else { "" };
        Ok(format!("{} {}x{} scale {scale:.2}{part}", path.display(), small.w, small.h))
    }

    fn launch(&self, who: &str, words: &[String]) -> Reply {
        if STOP.load(Ordering::Relaxed) {
            return Err(STOPPED.into());
        }
        x::ready()?;
        let app = words.first().ok_or("launch needs an app")?;
        let Some(window) = x::launch(app, &words[1..])? else { return Ok(format!("started {app}; no new window yet")) };
        let tracked = self.track(&window);
        self.set_current(who, window.id);
        Ok(format!("w{} \"{}\" {}", tracked.id, lines::clip(&window.title, 50), window.app))
    }

    fn status(&self) -> Reply {
        let mut agents: Vec<String> = self.agents.lock().unwrap().keys().cloned().collect();
        agents.sort();
        let stopped = if STOP.load(Ordering::Relaxed) { "; stopped by Ctrl+Alt+Esc" } else { "" };
        let display = match x::ready() {
            Ok(()) => format!("; display {}", std::env::var("DISPLAY").unwrap_or_default()),
            Err(e) => format!("; {e}"),
        };
        let access = match atspi::bus() {
            Ok(_) => "",
            Err(_) => "; no accessibility bus",
        };
        Ok(format!("deskcrew {} running; agents: {}{stopped}{display}{access}", env!("CARGO_PKG_VERSION"), agents.join(", ")))
    }

    fn is(&self, who: &str, flags: &Flags, words: &[String]) -> Reply {
        let [word, state] = words else { return Err("is e3 on|off|open|closed|selected|enabled|disabled".into()) };
        let tracked = self.pick(who, flags.window.as_deref())?;
        let mut view = tracked.view.lock().unwrap();
        self.read(&tracked, &mut view, false)?;
        let Target::Control(id) = self.resolve(word, who, &tracked, &mut view)? else { return Err("is needs a control".into()) };
        let states = &view.nodes.get(&id).ok_or(format!("e{id} is gone: see"))?.parts.states;
        let yes = match state.as_str() {
            "enabled" => !states.contains(&"disabled"),
            other => states.contains(&other),
        };
        Ok(if yes { "yes" } else { "no" }.into())
    }

    fn dry_run(&self, tracked: &Tracked, view: &View, steps: &[Vec<String>]) -> String {
        let name = |word: &str| is_ref(word).and_then(|id| view.nodes.get(&id)).map(|node| lines::line(&node.parts));
        steps
            .iter()
            .enumerate()
            .map(|(at, step)| {
                let (_, words) = parse(step);
                let named: Vec<String> = words.iter().map(|word| name(word).map_or(word.clone(), |line| format!("{word} ({line})"))).collect();
                format!("{}. {}", at + 1, named.join(" "))
            })
            .chain(Some(format!("nothing done in w{}", tracked.id)))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn handle(&self, who: &str, words: &[String]) -> Reply {
        let (flags, rest) = parse(words);
        let (command, args) = rest.split_first().ok_or(HELP)?;
        let with_expect = |mut steps: Vec<Vec<String>>| {
            if let Some(text) = &flags.expect {
                steps.push(vec!["wait".to_string(), text.clone()]);
            }
            steps
        };
        let window_spec = || args.first().map(String::as_str).or(flags.window.as_deref());
        match command.as_str() {
            "help" => Ok(HELP.into()),
            "windows" => self.windows_list(who, args.first()),
            "see" => self.see(who, &flags, args),
            "get" => self.get(who, &flags, args),
            "is" => self.is(who, &flags, args),
            "shot" => self.shot(who, &flags, args),
            "launch" => self.launch(who, args),
            "status" => self.status(),
            "focus" => {
                let tracked = self.pick(who, window_spec())?;
                x::activate(tracked.window)?;
                Ok("ok".into())
            }
            "close" => {
                let tracked = self.pick(who, window_spec())?;
                x::close(tracked.window).map(|_| "ok".to_string())
            }
            "place" => {
                let tracked = self.pick(who, flags.window.as_deref())?;
                let how = args.first().ok_or("place x,y,w,h|min|max|restore")?;
                x::place(tracked.window, how).map(|_| "ok".to_string())
            }
            "clip" => match args {
                [] => Ok(x::clipboard()?.unwrap_or_else(|| "(no text on the clipboard)".into())),
                [set, text] if set == "set" => x::set_clipboard(Some(text)).map(|_| "ok".to_string()),
                [clear] if clear == "clear" => x::set_clipboard(None).map(|_| "ok".to_string()),
                _ => Err("clip, clip set \"text\", or clip clear".into()),
            },
            "screens" => {
                x::ready()?;
                Ok(x::screens()
                    .iter()
                    .enumerate()
                    .map(|(at, ((sx, sy, w, h), main))| format!("{at}: {sx},{sy} {w}x{h}{}", if *main { " primary" } else { "" }))
                    .collect::<Vec<_>>()
                    .join("\n"))
            }
            "apps" => {
                x::ready()?;
                let mut names: Vec<String> = x::list().into_iter().map(|w| w.app).collect();
                names.sort_unstable();
                names.dedup();
                Ok(names.join("\n"))
            }
            "up" => {
                let gone = self.release(true);
                Ok(if gone.is_empty() { "nothing was held".into() } else { format!("let go of keys held by {}", gone.join(", ")) })
            }
            "cursor" => Err("the Linux version never borrows anyone's cursor: on X11 it uses the display's pointer for points and drawing only".into()),
            "notifications" | "table" | "pick-file" => Err(format!("{command} is not on the Linux version yet")),
            "resume" => {
                STOP.store(false, Ordering::Relaxed);
                Ok("ok".into())
            }
            "quit" => {
                self.release(true);
                self.quit.store(true, Ordering::Relaxed);
                Ok("bye".into())
            }
            "do" => {
                let script = args.join(" ");
                let steps = lines::split_steps(&script);
                if flags.dry {
                    let tracked = self.pick(who, flags.window.as_deref())?;
                    let mut view = tracked.view.lock().unwrap();
                    if !view.read_once {
                        self.read(&tracked, &mut view, false)?;
                    }
                    return Ok(self.dry_run(&tracked, &view, &steps));
                }
                if matches!(flags.risk.as_deref(), Some("medium" | "high")) && !flags.force {
                    return Err("declared risky: ask the person, then add --force".into());
                }
                self.act(who, &flags, &with_expect(steps))
            }
            "click" if args.len() > 1 => {
                let steps = args.iter().map(|word| vec!["click".to_string(), word.clone()]).collect();
                self.act(who, &flags, &with_expect(steps))
            }
            "click" | "hover" | "set" | "clear" | "check" | "uncheck" | "toggle" | "expand" | "collapse" | "show" | "select" | "type" | "key"
            | "down" | "scroll" | "drag" | "draw" | "wait" => {
                let mut step = vec![command.clone()];
                step.extend(args.iter().cloned());
                self.act(who, &flags, &with_expect(vec![step]))
            }
            other => Err(format!("unknown command '{other}': deskcrew help")),
        }
    }

    fn serve(&self, pipe: Pipe) {
        let Ok(message) = pipe.receive() else { return };
        *self.last_used.lock().unwrap() = Instant::now();
        let text = String::from_utf8_lossy(&message).into_owned();
        let mut words: Vec<String> = text.split('\0').map(str::to_string).collect();
        let who = if words.is_empty() { "your agent".to_string() } else { words.remove(0) };
        let reply = self.handle(&who, &words);
        let (code, body) = match reply {
            Ok(body) => (0u8, body),
            Err(body) => (1u8, body),
        };
        let mut bytes = vec![code];
        bytes.extend_from_slice(body.as_bytes());
        let _ = pipe.send(&bytes);
        pipe.finish();
    }
}

pub fn run() {
    let Ok(listener) = Pipe::bind() else { return };
    let service = Arc::new(Service {
        held: Mutex::new(HashMap::new()),
        real_input: Mutex::new(()),
        windows: Mutex::new((0, HashMap::new())),
        agents: Mutex::new(HashMap::new()),
        last_used: Mutex::new(Instant::now()),
        quit: AtomicBool::new(false),
        overlay: Overlay::start(),
    });
    let server = service.clone();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let worker = server.clone();
            thread::spawn(move || worker.serve(Pipe::from_stream(stream)));
        }
    });
    // Ctrl+Alt+Esc, read from the key state: nothing is grabbed, so the person's keys are untouched.
    thread::spawn(|| {
        loop {
            if x::stop_keys_down() {
                STOP.store(true, Ordering::Relaxed);
            }
            thread::sleep(STOP_WATCH);
        }
    });
    loop {
        thread::sleep(Duration::from_millis(250));
        service.release(false);
        if service.quit.load(Ordering::Relaxed) || service.last_used.lock().unwrap().elapsed() > IDLE_EXIT {
            break;
        }
    }
    service.release(true);
    thread::sleep(Duration::from_millis(100));
    pipe::remove();
}
