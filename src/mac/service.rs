// The background service on a Mac. It keeps the cursors on screen between commands, remembers each
// window's refs, and runs agents side by side: one queue per window.
use super::{
    apps::{self, Window},
    ax::{self, Bounds, Node},
    capture,
    ffi::*,
    input::{self, Button},
};
use crate::{
    lines::{self, Seen},
    look, motion,
    overlay::{self, Overlay, Rect},
    pipe::{self, Pipe},
    shot::{self, Frame, Frames},
};
use std::{
    collections::{BTreeMap, HashMap},
    ffi::c_void,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

pub const HELP: &str = "\
deskcrew drives Mac apps in the background. Refs stay valid, so read once.
windows [filter]                      list windows
see [window] [-f] [-a] [--find word] [--from N]
                                      controls; later reads show only changes (-f for all, -a off-screen too)
get e3                                a control's full text
shot [window|e3] [-f] [--marks] [--rulers] [--screen [-a]] [--region x,y,w,h] [--changed] [--edge N]
                                      PNG path; \"unchanged\" if it matches the last one
click e3 [e4 ...] | \"Name\" | x,y [--right] [--double] [--triple] [--middle] [--back] [--forward] [--with ctrl+shift]
hover e3 | x,y     check e3     uncheck e3     toggle e3     expand e3     collapse e3     clear e3
is e3 on|off|open|closed|selected|enabled|disabled
down shift     up (lets go; 30 s limit)
set e3 \"value\"     type [e3] \"text\" [--paste]     key enter [--hold ms]     select e3 \"Item\"
scroll [e3] up|down [n]     show e3     wait \"text\" [--gone] [--ms N]
drag a b     draw e3 \"x,y x,y; x,y x,y\" [--smooth]
do \"click e1; set e2 hi; key enter\" [--dry] [--risk low|medium|high]   many steps, one reply
Any action: --expect \"text\" fails unless the text shows afterwards.
launch app [args] [--front]     focus     close     place x,y,w,h|min|max|restore
apps     screens     clip     clip set \"text\"     clip clear     status     resume     quit     setup (installs for your coding agents)     mcp (the same as MCP tools, over stdio)
Every action replies with what changed, in the picture too. x,y are points in the last shot, or text see read. Ctrl+Option+Esc stops every agent.
Flags: -w window, --agent name, --fast (no glide), --force (allow risky keys),
--real (brings the app forward for a moment and uses the system keyboard: a last resort).
macOS asks once for Accessibility, and for Screen Recording before the first picture.";

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
const FRONT_SETTLE: Duration = Duration::from_millis(80);
const PASTE_SETTLE: Duration = Duration::from_millis(150);
const MENU_OPEN: Duration = Duration::from_millis(200);
const MENU_DEPTH: usize = 3;
const LIST_DEPTH: usize = 4;
const LEAST_CHANGE: u32 = 4;
const STOPPED: &str = "stopped by the person with Ctrl+Option+Esc: ask them before doing more (deskcrew resume clears it)";
const NO_WINDOW: &str = "no window yet: run deskcrew windows, then deskcrew see <name>";
const NO_ACCESS: &str = "deskcrew needs Accessibility permission: System Settings > Privacy & Security > Accessibility, allow the app running your agent (Terminal, iTerm, Cursor...), then try again";
const MARKED: [&str; 11] = ["button", "edit", "menuitem", "checkbox", "radio", "combobox", "tab", "item", "link", "slider", "spinner"];
// Roles that hold other controls, and so may be a canvas rather than something covering its space.
const CONTAINERS: [&str; 2] = ["", "image"];

static STOP: AtomicBool = AtomicBool::new(false);
static ASKED: AtomicBool = AtomicBool::new(false);

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
    element: Option<Owned>,
    read_once: bool,
    blind: Option<(u64, Vec<String>)>,
}

struct Tracked {
    id: u32,
    window: u32,
    pid: i32,
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
    front: bool,
    fast: bool,
    real: bool,
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
        self.fast |= outer.fast;
        self.real |= outer.real;
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
            "--front" => flags.front = true,
            "--fast" => flags.fast = true,
            "--real" => flags.real = true,
            "--paste" => flags.paste = true,
            "--smooth" => flags.smooth = true,
            "--gone" => flags.gone = true,
            "--dry" => flags.dry = true,
            "--marks" => flags.marks = true,
            "--rulers" => flags.rulers = true,
            "--changed" => flags.changed = true,
            "--screen" => flags.screen = true,
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

/// Modifier keys one agent holds down in one app.
struct Held {
    pid: i32,
    flags: u64,
    since: Instant,
}

struct Service {
    held: Mutex<HashMap<String, Held>>,
    real_input: Mutex<()>,
    windows: Mutex<(u32, HashMap<u32, Arc<Tracked>>)>,
    agents: Mutex<HashMap<String, AgentState>>,
    overlay: Overlay,
    last_used: Mutex<Instant>,
    quit: AtomicBool,
}

impl Service {
    fn track(&self, window: &Window) -> Arc<Tracked> {
        let mut windows = self.windows.lock().unwrap();
        let (next, known) = &mut *windows;
        known
            .entry(window.id)
            .or_insert_with(|| {
                *next += 1;
                Arc::new(Tracked { id: *next, window: window.id, pid: window.pid, view: Mutex::new(View::default()) })
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
            if apps::find(tracked.window).is_none() {
                return Err("that window closed: run deskcrew windows".into());
            }
            return Ok(tracked);
        };
        if let Some(id) = spec.strip_prefix('w').and_then(|n| n.parse::<u32>().ok()) {
            let tracked = self.by_id(id).ok_or(format!("no window w{id}: run deskcrew windows"))?;
            if apps::find(tracked.window).is_none() {
                return Err("that window closed: run deskcrew windows".into());
            }
            self.set_current(who, tracked.window);
            return Ok(tracked);
        }
        let wanted = spec.to_lowercase();
        let listed = apps::list();
        let matches: Vec<&Window> = listed.iter().filter(|w| w.title.to_lowercase().contains(&wanted) || w.app.to_lowercase().contains(&wanted)).collect();
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
        apps::find(tracked.window).ok_or_else(|| "that window closed: run deskcrew windows".to_string())
    }

    fn read(&self, tracked: &Tracked, view: &mut View, all: bool) -> Result<(), String> {
        if !ax::trusted(false) {
            // macOS's own prompt, once: asking on every read would bury the person in dialogs.
            if !ASKED.swap(true, Ordering::Relaxed) {
                ax::trusted(true);
            }
            return Err(NO_ACCESS.into());
        }
        let window = self.window_of(tracked)?;
        if view.element.is_none() {
            view.element = apps::element(&window);
        }
        let element = view.element.as_ref().ok_or("that window does not answer accessibility requests")?;
        let (nodes, hidden) = ax::read(element, window.rect, all);
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
        if let Some((x, y)) = is_point(word) {
            let frames = self.agents.lock().unwrap().get(who).and_then(|a| a.shots.get(&tracked.window).map(|s| s.frames));
            let frames = frames.ok_or("x,y are points in a shot: take one first")?;
            let (x, y) = frames.to_screen(x, y);
            return Ok(Target::Point(x, y));
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
            Target::Point(x, y) => Ok((x, y)),
            Target::Control(id) => Ok(centre(ax::rect(&view.nodes[&id].element).unwrap_or(view.nodes[&id].rect))),
        }
    }

    fn cursor_on(&self, who: &str, flags: &Flags, tracked: &Tracked, x: i32, y: i32) {
        self.overlay.over(who, tracked.window);
        if flags.fast {
            self.overlay.jump(who, x, y);
        } else {
            self.overlay.move_to(who, x, y);
        }
        self.overlay.over(who, tracked.window);
    }

    /// A control's box now, and its element, for acting on it.
    fn control(&self, word: &str, who: &str, tracked: &Tracked, view: &mut View) -> Result<(u32, Bounds, Owned), String> {
        match self.resolve(word, who, tracked, view)? {
            Target::Control(id) => {
                let node = &view.nodes[&id];
                Ok((id, ax::rect(&node.element).unwrap_or(node.rect), node.element.clone()))
            }
            Target::Point(..) => Err("that needs a control".into()),
        }
    }

    fn pressed(&self, who: &str, rect: Bounds) {
        self.overlay.press(who, Rect { x: rect.0, y: rect.1, w: rect.2, h: rect.3 });
    }

    /// Bring the app forward for `work`, then give the front back to whatever had it. Agents take
    /// turns here, because only one app can be in front.
    fn front<T>(&self, tracked: &Tracked, work: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        let _turn = self.real_input.lock().unwrap();
        let before = apps::front_pid();
        if before != Some(tracked.pid) && !apps::activate(tracked.pid) {
            return Err("macOS would not bring that app forward".into());
        }
        if let Some(window) = apps::find(tracked.window).and_then(|w| apps::element(&w)) {
            let _ = ax::perform(&window, "AXRaise");
        }
        thread::sleep(FRONT_SETTLE);
        let result = work();
        thread::sleep(FRONT_SETTLE);
        if let Some(pid) = before
            && pid != tracked.pid
        {
            apps::activate(pid);
        }
        result
    }

    /// Modifier keys this agent holds down in this app, let go of after 30 seconds.
    fn held_flags(&self, who: &str, pid: i32) -> u64 {
        self.held.lock().unwrap().get(who).filter(|held| held.pid == pid).map_or(0, |held| held.flags)
    }

    /// Let go of held keys: everyone's, or only those held too long or since Ctrl+Option+Esc.
    fn release(&self, all: bool) -> Vec<String> {
        let stopped = STOP.load(Ordering::Relaxed);
        let mut held = self.held.lock().unwrap();
        let mut gone = Vec::new();
        held.retain(|who, keys| {
            let keep = !all && !stopped && keys.since.elapsed() < HOLD_MOST;
            if !keep {
                let _ = input::modifiers_changed(keys.pid, keys.flags, false);
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
        let with = match &flags.with {
            Some(spec) => input::modifiers(spec)?,
            None => 0,
        } | self.held_flags(who, tracked.pid);
        let (rect, element) = match self.resolve(word, who, tracked, view)? {
            Target::Point(x, y) => ((x - 6, y - 6, 12, 12), None),
            Target::Control(id) => {
                let node = &view.nodes[&id];
                (ax::rect(&node.element).unwrap_or(node.rect), Some(node.element.clone()))
            }
        };
        let (x, y) = centre(rect);
        self.cursor_on(who, flags, tracked, x, y);
        self.pressed(who, rect);
        // An action the app performs itself works behind other windows and takes no focus.
        if let Some(element) = &element
            && with == 0
            && count == 1
            && !flags.real
        {
            let action = match button {
                Button::Left => Some("AXPress"),
                Button::Right => Some("AXShowMenu"),
                _ => None,
            };
            if let Some(action) = action
                && ax::perform(element, action).is_ok()
            {
                return Ok(if button == Button::Right { "menu".into() } else { "press".into() });
            }
        }
        let send = || input::click(tracked.pid, tracked.window, x as f64, y as f64, button, count, with);
        if flags.real {
            self.front(tracked, send)?;
            Ok("real".into())
        } else {
            send()?;
            Ok("event".into())
        }
    }

    fn hover(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, word: &str) -> Reply {
        let (x, y) = self.point_of(word, who, tracked, view)?;
        self.cursor_on(who, flags, tracked, x, y);
        input::hover(tracked.pid, tracked.window, x as f64, y as f64)?;
        Ok("hover".into())
    }

    fn set(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, word: &str, value: &str) -> Reply {
        let (_, rect, element) = self.control(word, who, tracked, view)?;
        let (x, y) = centre(rect);
        self.cursor_on(who, flags, tracked, x, y);
        if !ax::settable(&element, "AXValue") {
            return Err("that control's text can't be set: try type".into());
        }
        ax::set_text(&element, value)?;
        self.pressed(who, rect);
        Ok("value".into())
    }

    /// Leave a checkbox or switch alone if it is already that way, or press it once.
    fn ensure(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, word: &str, wanted: Option<&'static str>) -> Reply {
        let (id, rect, element) = self.control(word, who, tracked, view)?;
        if let Some(wanted) = wanted
            && view.nodes[&id].parts.states.contains(&wanted)
        {
            return Ok(format!("already {wanted}"));
        }
        let (x, y) = centre(rect);
        self.cursor_on(who, flags, tracked, x, y);
        self.pressed(who, rect);
        ax::perform(&element, "AXPress")?;
        Ok("toggle".into())
    }

    fn expand(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, word: &str, open: bool) -> Reply {
        let (id, rect, element) = self.control(word, who, tracked, view)?;
        let wanted = if open { "open" } else { "closed" };
        if view.nodes[&id].parts.states.contains(&wanted) {
            return Ok(format!("already {wanted}"));
        }
        let (x, y) = centre(rect);
        self.cursor_on(who, flags, tracked, x, y);
        self.pressed(who, rect);
        if ax::settable(&element, "AXExpanded") {
            ax::set_bool(&element, "AXExpanded", open)?;
        } else if open && view.nodes[&id].parts.role == "combobox" {
            ax::perform(&element, "AXShowMenu").or_else(|_| ax::perform(&element, "AXPress"))?;
        } else {
            ax::perform(&element, "AXPress").map_err(|_| format!("that control cannot be {}", if open { "expanded" } else { "collapsed" }))?;
        }
        Ok("expand".into())
    }

    fn show(&self, who: &str, tracked: &Tracked, view: &mut View, word: &str) -> Reply {
        let (_, _, element) = self.control(word, who, tracked, view)?;
        ax::perform(&element, "AXScrollToVisible").map_err(|_| "that control cannot scroll into view".to_string())?;
        Ok("scroll".into())
    }

    /// Pick an entry: from a pop-up button's menu, by typing into a combo box, or in a list.
    fn select(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, word: &str, item: &str) -> Reply {
        let (id, rect, element) = self.control(word, who, tracked, view)?;
        let (x, y) = centre(rect);
        self.cursor_on(who, flags, tracked, x, y);
        self.pressed(who, rect);
        let ax_role = view.nodes[&id].ax_role.clone();
        if ax_role == "AXComboBox" && ax::settable(&element, "AXValue") {
            ax::set_text(&element, item)?;
            return Ok("value".into());
        }
        if ax_role == "AXPopUpButton" || ax_role == "AXMenuButton" {
            ax::perform(&element, "AXPress").map_err(|_| "that dropdown did not answer".to_string())?;
            thread::sleep(MENU_OPEN);
            if let Some(entry) = ax::find_named(&element, item, MENU_DEPTH) {
                ax::perform(&entry, "AXPress")?;
                return Ok("select".into());
            }
            let _ = input::press(input::To::App(tracked.pid), &input::chord("esc")?, 0, Duration::ZERO);
            return Err(format!("no entry named '{item}'"));
        }
        let entry = ax::find_named(&element, item, LIST_DEPTH).ok_or(format!("no entry named '{item}'"))?;
        if ax::settable(&entry, "AXSelected") {
            ax::set_bool(&entry, "AXSelected", true)?;
        } else {
            ax::perform(&entry, "AXPress")?;
        }
        Ok("select".into())
    }

    fn type_text(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, words: &[String]) -> Reply {
        let (target, text) = match words {
            [text] => (None, text.as_str()),
            [target, text] => (Some(target.as_str()), text.as_str()),
            _ => return Err("type [control] \"text\"".into()),
        };
        let mut field = None;
        if let Some(word) = target {
            let (_, rect, element) = self.control(word, who, tracked, view)?;
            let (x, y) = centre(rect);
            self.cursor_on(who, flags, tracked, x, y);
            let _ = ax::focus(&element);
            field = Some(element);
        }
        if flags.paste {
            let saved = apps::clipboard();
            apps::set_clipboard(Some(text))?;
            let paste = input::chord("cmd+v")?;
            let sent = if flags.real {
                self.front(tracked, || input::press(input::To::Front, &paste, 0, Duration::ZERO))
            } else {
                input::press(input::To::App(tracked.pid), &paste, 0, Duration::ZERO)
            };
            thread::sleep(PASTE_SETTLE);
            let _ = apps::set_clipboard(saved.as_deref());
            sent?;
            return Ok("paste".into());
        }
        let before = field.as_ref().and_then(|f| ax::text(f, "AXValue"));
        let stop = || STOP.load(Ordering::Relaxed);
        let typed = if flags.real {
            self.front(tracked, || input::type_text(input::To::Front, text, &stop))?
        } else {
            input::type_text(input::To::App(tracked.pid), text, &stop)?
        };
        if typed < text.chars().count() {
            return Err(format!("{STOPPED} (typed {typed} of {} characters)", text.chars().count()));
        }
        // Some apps only take keys while in front. A field that ignored them gets the text set instead.
        if let Some(field) = field
            && !flags.real
        {
            thread::sleep(SETTLE_FIRST);
            let after = ax::text(&field, "AXValue");
            if after == before && ax::settable(&field, "AXValue") {
                ax::set_text(&field, &(before.unwrap_or_default() + text))?;
                return Ok("value".into());
            }
        }
        Ok(if flags.real { "keyboard".into() } else { "keys".into() })
    }

    fn key(&self, who: &str, flags: &Flags, tracked: &Tracked, spec: &str) -> Reply {
        let chord = input::chord(spec)?;
        if input::is_risky(&chord) && !flags.force {
            return Err(format!("'{spec}' can quit or close things: ask the person, then add --force"));
        }
        let hold = Duration::from_millis(flags.hold.unwrap_or(0).min(HOLD_MOST.as_millis() as u64));
        let held = self.held_flags(who, tracked.pid);
        if flags.real {
            self.front(tracked, || input::press(input::To::Front, &chord, held, hold))?;
            Ok("keyboard".into())
        } else {
            input::press(input::To::App(tracked.pid), &chord, held, hold)?;
            Ok("keys".into())
        }
    }

    /// Hold modifier keys for this agent's later clicks and keys in this app, until `up`.
    fn down(&self, who: &str, tracked: &Tracked, words: &[String]) -> Reply {
        let [spec] = words else { return Err("down shift (or ctrl, alt, cmd)".into()) };
        if matches!(spec.as_str(), "left" | "right" | "middle") {
            return Err("holding a mouse button needs the real mouse, which the Mac version does not borrow".into());
        }
        let chord = input::chord(spec)?;
        if chord.key.is_some() {
            return Err("on a Mac, down holds modifier keys: shift, ctrl, alt or cmd".into());
        }
        input::modifiers_changed(tracked.pid, chord.flags, true)?;
        let mut held = self.held.lock().unwrap();
        let flags = held.get(who).filter(|h| h.pid == tracked.pid).map_or(0, |h| h.flags) | chord.flags;
        held.insert(who.to_string(), Held { pid: tracked.pid, flags, since: Instant::now() });
        Ok("held".into())
    }

    fn scroll(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, words: &[String]) -> Reply {
        let (target, direction, amount) = match words {
            [direction] => (None, direction.as_str(), 3),
            [target, direction] if direction == "up" || direction == "down" => (Some(target.as_str()), direction.as_str(), 3),
            [direction, amount] => (None, direction.as_str(), amount.parse().unwrap_or(3)),
            [target, direction, amount] => (Some(target.as_str()), direction.as_str(), amount.parse().unwrap_or(3)),
            _ => return Err("scroll [control] up|down [n]".into()),
        };
        let (x, y) = match target {
            Some(word) => self.point_of(word, who, tracked, view)?,
            None => centre(self.window_of(tracked)?.rect),
        };
        let lines = match direction {
            "up" => amount,
            "down" => -amount,
            _ => return Err("scroll [control] up|down [n]".into()),
        };
        self.cursor_on(who, flags, tracked, x, y);
        input::scroll(tracked.pid, x as f64, y as f64, lines)?;
        Ok("event".into())
    }

    /// Press, follow each stroke and let go, with the drawn cursor following along.
    fn strokes(&self, who: &str, flags: &Flags, tracked: &Tracked, strokes: &[Vec<(f32, f32)>]) -> Reply {
        let window = self.window_of(tracked)?.rect;
        let inside = |p: &(f32, f32)| {
            let (x, y) = (p.0 as i32, p.1 as i32);
            x >= window.0 && y >= window.1 && x < window.0 + window.2 && y < window.1 + window.3
        };
        if !strokes.iter().flatten().all(inside) {
            return Err("refused: that stroke leaves the window".into());
        }
        let first = strokes.iter().flatten().next().ok_or("nothing to draw")?;
        self.cursor_on(who, flags, tracked, first.0 as i32, first.1 as i32);
        let stop = || STOP.load(Ordering::Relaxed);
        for stroke in strokes {
            let points: Vec<(f64, f64)> = stroke.iter().map(|p| (p.0 as f64, p.1 as f64)).collect();
            let mut moved = |at: usize, x: f64, y: f64| {
                if at.is_multiple_of(3) {
                    self.overlay.jump(who, x as i32, y as i32);
                }
            };
            if !input::stroke(tracked.pid, tracked.window, &points, &stop, &mut moved)? {
                return Err(STOPPED.into());
            }
        }
        Ok("event".into())
    }

    fn drag(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, from: &str, to: &str) -> Reply {
        let a = self.point_of(from, who, tracked, view)?;
        let b = self.point_of(to, who, tracked, view)?;
        let path = motion::trace(&[a, b], false);
        self.strokes(who, flags, tracked, &[path])
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
                let (x, y) = is_point(word).ok_or(format!("'{word}' is not x,y"))?;
                points.push(match (origin, frames) {
                    (Some(rect), _) => (rect.0 + x, rect.1 + y),
                    (None, Some(frames)) => frames.to_screen(x, y),
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
        self.strokes(who, flags, tracked, &strokes)
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
            ("hover", [word]) => self.hover(who, &flags, tracked, view, word),
            ("set", [word, value]) => self.set(who, &flags, tracked, view, word, value),
            ("clear", [word]) => self.set(who, &flags, tracked, view, word, ""),
            ("check", [word]) => self.ensure(who, &flags, tracked, view, word, Some("on")),
            ("uncheck", [word]) => self.ensure(who, &flags, tracked, view, word, Some("off")),
            ("toggle", [word]) => self.ensure(who, &flags, tracked, view, word, None),
            ("expand", [word]) => self.expand(who, &flags, tracked, view, word, true),
            ("collapse", [word]) => self.expand(who, &flags, tracked, view, word, false),
            ("show", [word]) => self.show(who, tracked, view, word),
            ("select", [word, item]) => self.select(who, &flags, tracked, view, word, item),
            ("type", rest) => self.type_text(who, &flags, tracked, view, rest),
            ("key", [spec]) => self.key(who, &flags, tracked, spec),
            ("down", rest) => self.down(who, tracked, rest),
            ("scroll", rest) => self.scroll(who, &flags, tracked, view, rest),
            ("drag", [from, to]) => self.drag(who, &flags, tracked, view, from, to),
            ("draw", rest) => self.draw(who, &flags, tracked, view, rest),
            ("wait", [text]) => self.wait(&flags, tracked, view, text),
            _ => Err(format!("can't do '{}': see deskcrew help", words.join(" "))),
        }
    }

    /// Where the window's picture changed since `before`, in window points, ignoring a caret.
    fn picture_change(&self, tracked: &Tracked, width: i32, (before, units): &(Frame, f32)) -> Option<String> {
        let (after, _) = capture::window(tracked.window, width).ok()?;
        let (x, y, w, h) = after.changed_from(before)?;
        let at = |value: u32| (value as f32 * units).round() as i32;
        if at(w) < LEAST_CHANGE as i32 || at(h) < LEAST_CHANGE as i32 {
            return None;
        }
        Some(format!("controls unchanged; the picture changed at {},{} {}x{}", at(x), at(y), at(w), at(h)))
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
        let width = self.window_of(&tracked)?.rect.2;
        let picture_before = if unsafe { CGPreflightScreenCaptureAccess() } { capture::window(tracked.window, width).ok().filter(|(f, _)| !f.is_blank()) } else { None };
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
                let picture = if nothing && !waited_only { picture_before.as_ref().map(|b| self.picture_change(&tracked, width, b)) } else { None };
                match picture {
                    Some(Some(note)) => out.push(note),
                    Some(None) => out.push("nothing changed, on screen either".into()),
                    None if nothing && routes.iter().any(|r| r == "event" || r == "keys" || r == "real" || r == "keyboard") => out.push("nothing changed: check with shot".into()),
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
        let wanted = filter.map(|f| f.to_lowercase());
        let current = self.agents.lock().unwrap().get(who).and_then(|a| a.current);
        let listed = apps::list();
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

    /// Point out the largest part of the window with no controls in it, and read any text there.
    fn blind_spot(&self, who: &str, tracked: &Tracked, window: &Window, view: &mut View) -> Vec<String> {
        if !unsafe { CGPreflightScreenCaptureAccess() } {
            return Vec::new();
        }
        let rect = window.rect;
        let rects: Vec<_> = view.nodes.values().map(|n| (n.rect, CONTAINERS.contains(&n.parts.role.as_str()) && n.parts.name.is_empty())).collect();
        let Some((x, y, w, h)) = look::empty_area(rect, &rects) else { return Vec::new() };
        let Ok((frame, units)) = capture::window(tracked.window, rect.2) else { return Vec::new() };
        let inside = |value: i32| (value as f32 / units) as u32;
        let part = frame.crop(inside(x - rect.0), inside(y - rect.1), inside(w).max(1), inside(h).max(1));
        if !part.busy() {
            return Vec::new();
        }
        let hash = part.hash();
        if let Some((seen, said)) = &view.blind
            && *seen == hash
        {
            return said.clone();
        }
        let (left, top) = (x - rect.0, y - rect.1);
        let picture = format!("shot --region {x},{y},{w},{h}");
        let texts = look::read_text(&part).unwrap_or_default();
        let said = if texts.is_empty() {
            vec![format!("no controls in {left},{top} {w}x{h}, only drawing: {picture}")]
        } else {
            let mut said = vec![format!("no controls in {left},{top} {w}x{h}; text read there (x,y for click):")];
            for text in &texts {
                let (tx, ty, tw, th) = text.rect;
                let middle = |at: i32, size: i32, offset: i32| offset + ((at as f32 + size as f32 / 2.0) * units).round() as i32;
                said.push(format!("\"{}\" {},{}", lines::clip(&text.text, 60), middle(tx, tw, left), middle(ty, th, top)));
            }
            said.push(format!("for drawings: {picture}"));
            // These x,y are window points: clicks read them that way until the next shot.
            let mut agents = self.agents.lock().unwrap();
            let state = agents.entry(who.to_string()).or_default();
            let frames = Frames { x: rect.0, y: rect.1, scale: 1.0 };
            match state.shots.get_mut(&tracked.window) {
                Some(last) => {
                    last.frames = frames;
                    last.hash = 0;
                }
                None => {
                    state.shots.insert(tracked.window, LastShot { hash: 0, frame: Frame { w: 0, h: 0, pixels: Vec::new() }, frames });
                }
            }
            said
        };
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
        Ok(ax::full_text(&view.nodes[&id].element).chars().take(MAX_GET_CHARS).collect())
    }

    fn shot(&self, who: &str, flags: &Flags, words: &[String]) -> Reply {
        let edge = flags.edge.unwrap_or(shot::DEFAULT_EDGE);
        if let Some(region) = &flags.region {
            let parts: Vec<i32> = region.split(',').filter_map(|n| n.trim().parse().ok()).collect();
            let [x, y, w, h] = parts[..] else { return Err("--region x,y,w,h".into()) };
            if w < 1 || h < 1 {
                return Err("--region needs a width and height".into());
            }
            self.overlay.aside(true);
            let taken = capture::screen(x, y, w, h);
            self.overlay.aside(false);
            let (frame, units) = taken?;
            let (small, scale) = frame.shrink(edge);
            let path = shot::path_for(who, 0);
            small.save(&path)?;
            return Ok(format!("{} {}x{} scale {:.2}", path.display(), small.w, small.h, scale * units));
        }
        if flags.screen {
            let screens = apps::screens();
            let main = screens.iter().find(|(_, main)| *main).map(|(r, _)| *r).unwrap_or((0, 0, 1, 1));
            let (x, y, w, h) = if flags.all {
                let left = screens.iter().map(|(r, _)| r.0).min().unwrap_or(0);
                let top = screens.iter().map(|(r, _)| r.1).min().unwrap_or(0);
                let right = screens.iter().map(|(r, _)| r.0 + r.2).max().unwrap_or(1);
                let bottom = screens.iter().map(|(r, _)| r.1 + r.3).max().unwrap_or(1);
                (left, top, right - left, bottom - top)
            } else {
                main
            };
            self.overlay.aside(true);
            let taken = capture::screen(x, y, w, h);
            self.overlay.aside(false);
            let (frame, units) = taken?;
            let (small, scale) = frame.shrink(edge);
            let path = shot::path_for(who, 0);
            small.save(&path)?;
            return Ok(format!("{} {}x{} scale {:.2}", path.display(), small.w, small.h, scale * units));
        }
        let word = words.first();
        let control = word.and_then(|w| is_ref(w));
        let tracked = self.pick(who, if control.is_some() { flags.window.as_deref() } else { word.map(String::as_str).or(flags.window.as_deref()) })?;
        let window = self.window_of(&tracked)?;
        let (left, top, width, _) = window.rect;
        let (mut frame, units) = capture::window(tracked.window, width)?;
        let mut origin = (left, top);
        if let Some(id) = control {
            let mut view = tracked.view.lock().unwrap();
            if !view.nodes.contains_key(&id) {
                self.read(&tracked, &mut view, false)?;
            }
            let node = view.nodes.get(&id).ok_or(format!("e{id} is gone: see"))?;
            let rect = ax::rect(&node.element).unwrap_or(node.rect);
            let inside = |value: i32| (value as f32 / units) as u32;
            frame = frame.crop(inside((rect.0 - left).max(0)), inside((rect.1 - top).max(0)), inside(rect.2).max(1), inside(rect.3).max(1));
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
            Some((x, y, w, h)) => {
                origin = (origin.0 + (x as f32 * units) as i32, origin.1 + (y as f32 * units) as i32);
                frame.crop(x, y, w, h)
            }
            None => frame.crop(0, 0, frame.w, frame.h),
        };
        let (mut small, scale) = picture.shrink(edge);
        let per_pixel = scale * units;
        if flags.marks {
            let to_image = |value: i32, from: i32| ((value - from) as f32 / per_pixel) as i32;
            let view = tracked.view.lock().unwrap();
            for (id, node) in view.nodes.iter().filter(|(_, node)| MARKED.contains(&node.parts.role.as_str())) {
                let (x, y, w, h) = node.rect;
                small.mark(to_image(x, origin.0), to_image(y, origin.1), (w as f32 / per_pixel) as i32, (h as f32 / per_pixel) as i32, *id);
            }
        }
        if flags.rulers {
            small.rulers();
        }
        let path = shot::path_for(who, tracked.id);
        small.save(&path)?;
        let frames = Frames { x: origin.0, y: origin.1, scale: per_pixel };
        if control.is_none() {
            state.shots.insert(tracked.window, LastShot { hash, frame, frames });
        } else if let Some(last) = state.shots.get_mut(&tracked.window) {
            last.frames = frames;
        } else {
            state.shots.insert(tracked.window, LastShot { hash: 0, frame: Frame { w: 0, h: 0, pixels: Vec::new() }, frames });
        }
        let part = if crop.is_some() { " changed part" } else { "" };
        Ok(format!("{} {}x{} scale {per_pixel:.2}{part}", path.display(), small.w, small.h))
    }

    fn launch(&self, who: &str, flags: &Flags, words: &[String]) -> Reply {
        if STOP.load(Ordering::Relaxed) {
            return Err(STOPPED.into());
        }
        let app = words.first().ok_or("launch needs an app")?;
        let Some(window) = apps::launch(app, &words[1..], flags.front)? else { return Ok(format!("started {app}; no new window yet")) };
        let tracked = self.track(&window);
        self.set_current(who, window.id);
        Ok(format!("w{} \"{}\" {}", tracked.id, lines::clip(&window.title, 50), window.app))
    }

    fn status(&self) -> Reply {
        let agents: Vec<String> = self.agents.lock().unwrap().keys().cloned().collect();
        let mut agents = agents;
        agents.sort();
        let stopped = if STOP.load(Ordering::Relaxed) { "; stopped by Ctrl+Option+Esc" } else { "" };
        let access = if ax::trusted(false) { "" } else { "; needs Accessibility permission" };
        Ok(format!("deskcrew {} running; agents: {}{stopped}{access}", env!("CARGO_PKG_VERSION"), agents.join(", ")))
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

    fn place(&self, who: &str, flags: &Flags, words: &[String]) -> Reply {
        let tracked = self.pick(who, flags.window.as_deref())?;
        let window = self.window_of(&tracked)?;
        let element = apps::element(&window).ok_or("that window does not answer accessibility requests")?;
        let word = words.first().ok_or("place x,y,w,h|min|max|restore")?;
        match word.as_str() {
            "min" => ax::set_bool(&element, "AXMinimized", true)?,
            "restore" | "show" => {
                let _ = ax::set_bool(&element, "AXFullScreen", false);
                ax::set_bool(&element, "AXMinimized", false)?;
            }
            "max" => {
                let (x, y, w, h) = apps::work_area();
                ax::set_position(&element, x, y)?;
                ax::set_size(&element, w, h)?;
            }
            box_text => {
                let parts: Vec<i32> = box_text.split(',').filter_map(|n| n.trim().parse().ok()).collect();
                let [x, y, w, h] = parts[..] else { return Err("place x,y,w,h|min|max|restore".into()) };
                ax::set_position(&element, x, y)?;
                ax::set_size(&element, w, h)?;
            }
        }
        Ok("ok".into())
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
            "launch" => self.launch(who, &flags, args),
            "status" => self.status(),
            "focus" => {
                let tracked = self.pick(who, window_spec())?;
                if !apps::activate(tracked.pid) {
                    return Err("macOS would not bring that app forward".into());
                }
                if let Some(element) = apps::find(tracked.window).and_then(|w| apps::element(&w)) {
                    let _ = ax::perform(&element, "AXRaise");
                }
                Ok("ok".into())
            }
            "close" => {
                let tracked = self.pick(who, window_spec())?;
                let window = self.window_of(&tracked)?;
                let element = apps::element(&window).ok_or("that window does not answer accessibility requests")?;
                let button = ax::attribute(&element, "AXCloseButton").ok_or("that window has no close button")?;
                ax::perform(&button, "AXPress").map(|_| "ok".to_string())
            }
            "place" => self.place(who, &flags, args),
            "clip" => match args {
                [] => Ok(apps::clipboard().unwrap_or_else(|| "(no text on the clipboard)".into())),
                [set, text] if set == "set" => apps::set_clipboard(Some(text)).map(|_| "ok".to_string()),
                [clear] if clear == "clear" => apps::set_clipboard(None).map(|_| "ok".to_string()),
                _ => Err("clip, clip set \"text\", or clip clear".into()),
            },
            "screens" => Ok(apps::screens()
                .iter()
                .enumerate()
                .map(|(at, ((x, y, w, h), main))| format!("{at}: {x},{y} {w}x{h}{}", if *main { " primary" } else { "" }))
                .collect::<Vec<_>>()
                .join("\n")),
            "apps" => {
                let mut names: Vec<String> = apps::list().into_iter().map(|w| w.app).collect();
                names.sort_unstable();
                names.dedup();
                Ok(names.join("\n"))
            }
            "up" => {
                let gone = self.release(true);
                Ok(if gone.is_empty() { "nothing was held".into() } else { format!("let go of keys held by {}", gone.join(", ")) })
            }
            "cursor" => Err("the Mac version never borrows the cursor, so there is nothing to set".into()),
            "notifications" => Err("notifications are not on the Mac version yet".into()),
            "table" | "pick-file" => Err(format!("{command} is not on the Mac version yet")),
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

/// Ctrl+Option+Esc, seen by a listen-only tap: it can never block or change the person's keys.
extern "C" fn watch_keys(_proxy: *mut c_void, kind: u32, event: CGEventRef, _info: *mut c_void) -> CGEventRef {
    if kind == kCGEventKeyDown && !event.is_null() {
        let code = unsafe { CGEventGetIntegerValueField(event, kCGKeyboardEventKeycode) };
        let flags = unsafe { CGEventGetFlags(event) };
        let wanted = kCGEventFlagMaskControl | kCGEventFlagMaskAlternate;
        if code == 53 && flags & wanted == wanted {
            STOP.store(true, Ordering::Relaxed);
        }
    }
    event
}

fn watch_stop_keys() {
    unsafe {
        let tap = CGEventTapCreate(kCGSessionEventTap, kCGHeadInsertEventTap, kCGEventTapOptionListenOnly, 1 << kCGEventKeyDown, watch_keys, std::ptr::null_mut());
        if tap.is_null() {
            return;
        }
        let source = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
        if !source.is_null() {
            CFRunLoopAddSource(CFRunLoopGetMain(), source, kCFRunLoopCommonModes);
        }
    }
}

pub fn run() {
    let Ok(listener) = Pipe::bind() else { return };
    let (overlay, inbox) = Overlay::new();
    let service = Arc::new(Service {
        held: Mutex::new(HashMap::new()),
        real_input: Mutex::new(()),
        windows: Mutex::new((0, HashMap::new())),
        agents: Mutex::new(HashMap::new()),
        overlay,
        last_used: Mutex::new(Instant::now()),
        quit: AtomicBool::new(false),
    });
    let server = service.clone();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let worker = server.clone();
            thread::spawn(move || worker.serve(Pipe::from_stream(stream)));
        }
    });
    watch_stop_keys();
    let keeper = service.clone();
    overlay::run(inbox, &move || {
        // Held keys go after 30 seconds, and at once on Ctrl+Option+Esc.
        keeper.release(false);
        keeper.quit.load(Ordering::Relaxed) || keeper.last_used.lock().unwrap().elapsed() > IDLE_EXIT
    });
    service.release(true);
    // Give the last reply a moment to leave before the process goes.
    thread::sleep(Duration::from_millis(100));
    pipe::remove();
}
