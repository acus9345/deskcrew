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
    look,
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
deskcrew - drive Mac apps in the background. Each agent gets its own cursor.
windows [filter]                 list windows as w1, w2...
see [window] [-f] [--all] [--find text] [--from N]
                                 list controls as e1, e2... later calls show only changes
get e3                           the whole text of a control
click e3|name|x,y ... [--right] [--double]
set e3 \"text\"                    replace a field's text
type [e3] \"text\"                 type into a field, or wherever the app's focus is
key cmd+s [--force]              press keys in the app, without bringing it forward
scroll e3|x,y up|down [N]
drag e3|x,y e4|x,y
wait \"text\" [--timeout ms]       wait until text shows
shot [window|e3] [-f] [--marks] [--rulers] [--changed] [--region x,y,w,h] [--edge N]
launch App [--front] [args...]   open an app, behind the front one unless --front
do \"click e1; set e2 hi; key enter\"
status | resume | quit
Pick a window with -w name|w2. Any action: --expect \"text\" fails unless the text shows afterwards.
Every action replies with what changed, in the picture too. x,y are points in the last shot, or text see read.
Ctrl+Option+Esc stops every agent. macOS asks once for Accessibility, and for Screen Recording before pictures.";

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
const DRAG_STEPS: usize = 24;
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

#[derive(Default)]
struct Flags {
    window: Option<String>,
    full: bool,
    all: bool,
    find: Option<String>,
    from: usize,
    right: bool,
    double: bool,
    force: bool,
    front: bool,
    marks: bool,
    rulers: bool,
    changed: bool,
    region: Option<String>,
    edge: Option<u32>,
    timeout: Option<u64>,
    expect: Option<String>,
}

fn parse(words: &[String]) -> (Flags, Vec<String>) {
    let mut flags = Flags::default();
    let mut rest = Vec::new();
    let mut at = 0;
    while at < words.len() {
        let word = &words[at];
        let next = words.get(at + 1).cloned();
        let mut takes = |value: &mut Option<String>| {
            *value = next.clone();
            at += 1;
        };
        match word.as_str() {
            "-w" | "--window" => takes(&mut flags.window),
            "--find" => takes(&mut flags.find),
            "--region" => takes(&mut flags.region),
            "--expect" => takes(&mut flags.expect),
            "--from" => {
                flags.from = next.and_then(|n| n.parse().ok()).unwrap_or(0);
                at += 1;
            }
            "--edge" => {
                flags.edge = next.and_then(|n| n.parse().ok());
                at += 1;
            }
            "--timeout" => {
                flags.timeout = next.and_then(|n| n.parse().ok());
                at += 1;
            }
            "-f" | "--full" => flags.full = true,
            "-a" | "--all" => flags.all = true,
            "--right" => flags.right = true,
            "--double" => flags.double = true,
            "--force" => flags.force = true,
            "--front" => flags.front = true,
            "--marks" => flags.marks = true,
            "--rulers" => flags.rulers = true,
            "--changed" => flags.changed = true,
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

struct Service {
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

    fn cursor_on(&self, who: &str, tracked: &Tracked, x: i32, y: i32) {
        self.overlay.over(who, tracked.window);
        self.overlay.move_to(who, x, y);
        self.overlay.over(who, tracked.window);
    }

    fn click(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, word: &str) -> Reply {
        let button = if flags.right { Button::Right } else { Button::Left };
        let count = if flags.double { 2 } else { 1 };
        match self.resolve(word, who, tracked, view)? {
            Target::Point(x, y) => {
                self.cursor_on(who, tracked, x, y);
                self.overlay.press(who, Rect { x: x - 6, y: y - 6, w: 12, h: 12 });
                input::click(tracked.pid, tracked.window, x as f64, y as f64, button, count)?;
                Ok("event".into())
            }
            Target::Control(id) => {
                let node = &view.nodes[&id];
                let rect = ax::rect(&node.element).unwrap_or(node.rect);
                let (x, y) = centre(rect);
                self.cursor_on(who, tracked, x, y);
                self.overlay.press(who, Rect { x: rect.0, y: rect.1, w: rect.2, h: rect.3 });
                // An action the app performs itself works behind other windows and takes no focus.
                let action = match (flags.right, flags.double) {
                    (true, _) => Some("AXShowMenu"),
                    (false, false) => Some("AXPress"),
                    _ => None,
                };
                if let Some(action) = action
                    && ax::perform(&node.element, action).is_ok()
                {
                    return Ok(if flags.right { "menu".into() } else { "press".into() });
                }
                input::click(tracked.pid, tracked.window, x as f64, y as f64, button, count)?;
                Ok("event".into())
            }
        }
    }

    fn set(&self, who: &str, tracked: &Tracked, view: &mut View, word: &str, value: &str) -> Reply {
        let Target::Control(id) = self.resolve(word, who, tracked, view)? else { return Err("set needs a control".into()) };
        let node = &view.nodes[&id];
        let rect = ax::rect(&node.element).unwrap_or(node.rect);
        let (x, y) = centre(rect);
        self.cursor_on(who, tracked, x, y);
        if !ax::settable(&node.element, "AXValue") {
            return Err("that control's text can't be set: try type".into());
        }
        ax::set_text(&node.element, value)?;
        self.overlay.press(who, Rect { x: rect.0, y: rect.1, w: rect.2, h: rect.3 });
        Ok("value".into())
    }

    fn type_text(&self, who: &str, tracked: &Tracked, view: &mut View, words: &[String]) -> Reply {
        let (target, text) = match words {
            [text] => (None, text.as_str()),
            [target, text] => (Some(target.as_str()), text.as_str()),
            _ => return Err("type needs text, and can take a control first".into()),
        };
        let mut field = None;
        if let Some(word) = target {
            let Target::Control(id) = self.resolve(word, who, tracked, view)? else { return Err("type needs a control, not x,y".into()) };
            let node = &view.nodes[&id];
            let rect = ax::rect(&node.element).unwrap_or(node.rect);
            let (x, y) = centre(rect);
            self.cursor_on(who, tracked, x, y);
            let _ = ax::focus(&node.element);
            field = Some(node.element.clone());
        }
        let before = field.as_ref().and_then(|f| ax::text(f, "AXValue"));
        let typed = input::type_text(tracked.pid, text, &|| STOP.load(Ordering::Relaxed))?;
        if typed < text.chars().count() {
            return Err(format!("{STOPPED} (typed {typed} of {} characters)", text.chars().count()));
        }
        // Some apps only take keys while in front. A field that ignored them gets the text set instead.
        if let Some(field) = field {
            thread::sleep(SETTLE_FIRST);
            let after = ax::text(&field, "AXValue");
            if after == before && ax::settable(&field, "AXValue") {
                ax::set_text(&field, &(before.unwrap_or_default() + text))?;
                return Ok("value".into());
            }
        }
        Ok("keys".into())
    }

    fn key(&self, flags: &Flags, tracked: &Tracked, spec: &str) -> Reply {
        let chord = input::chord(spec)?;
        if input::is_risky(&chord) && !flags.force {
            return Err(format!("'{spec}' can quit or close things: ask the person, then add --force"));
        }
        input::press(tracked.pid, &chord)?;
        Ok("keys".into())
    }

    fn scroll(&self, who: &str, tracked: &Tracked, view: &mut View, words: &[String]) -> Reply {
        let (target, direction, amount) = match words {
            [direction] => (None, direction.as_str(), 3),
            [target, direction] => (Some(target.as_str()), direction.as_str(), 3),
            [target, direction, amount] => (Some(target.as_str()), direction.as_str(), amount.parse().unwrap_or(3)),
            _ => return Err("scroll e3 up|down [N]".into()),
        };
        let (x, y) = match target {
            Some(word) => self.point_of(word, who, tracked, view)?,
            None => centre(self.window_of(tracked)?.rect),
        };
        let lines = match direction {
            "up" => amount,
            "down" => -amount,
            _ => return Err("scroll up or down".into()),
        };
        self.cursor_on(who, tracked, x, y);
        input::scroll(tracked.pid, x as f64, y as f64, lines)?;
        Ok("event".into())
    }

    fn drag(&self, who: &str, tracked: &Tracked, view: &mut View, from: &str, to: &str) -> Reply {
        let a = self.point_of(from, who, tracked, view)?;
        let b = self.point_of(to, who, tracked, view)?;
        self.cursor_on(who, tracked, a.0, a.1);
        let points: Vec<(f64, f64)> = (0..=DRAG_STEPS)
            .map(|i| {
                let t = i as f64 / DRAG_STEPS as f64;
                (a.0 as f64 + (b.0 - a.0) as f64 * t, a.1 as f64 + (b.1 - a.1) as f64 * t)
            })
            .collect();
        input::stroke(tracked.pid, tracked.window, &points)?;
        self.overlay.move_to(who, b.0, b.1);
        Ok("event".into())
    }

    fn wait(&self, flags: &Flags, tracked: &Tracked, view: &mut View, text: &str) -> Reply {
        let limit = Duration::from_millis(flags.timeout.unwrap_or(WAIT_DEFAULT_MS).min(WAIT_MOST_MS));
        let wanted = text.to_lowercase();
        let started = Instant::now();
        loop {
            self.read(tracked, view, false)?;
            if view.lines.values().any(|line| line.to_lowercase().contains(&wanted)) {
                return Ok(if started.elapsed() < WAIT_STEP { "found".into() } else { "waited".into() });
            }
            if started.elapsed() >= limit {
                return Err(format!("'{text}' did not show within {} ms", limit.as_millis()));
            }
            if STOP.load(Ordering::Relaxed) {
                return Err(STOPPED.into());
            }
            thread::sleep(WAIT_STEP);
        }
    }

    fn step(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, words: &[String]) -> Reply {
        let (own, words) = parse(words);
        let merged = Flags { right: flags.right || own.right, double: flags.double || own.double, force: flags.force || own.force, timeout: own.timeout.or(flags.timeout), ..Flags::default() };
        let (command, args) = words.split_first().ok_or("empty step")?;
        match (command.as_str(), args) {
            ("click", [word]) => self.click(who, &merged, tracked, view, word),
            ("set", [word, value]) => self.set(who, tracked, view, word, value),
            ("type", rest) => self.type_text(who, tracked, view, rest),
            ("key", [spec]) => self.key(&merged, tracked, spec),
            ("scroll", rest) => self.scroll(who, tracked, view, rest),
            ("drag", [from, to]) => self.drag(who, tracked, view, from, to),
            ("wait", [text]) => self.wait(&merged, tracked, view, text),
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
                    None if nothing && routes.iter().any(|r| r == "event" || r == "keys") => out.push("nothing changed: check with shot".into()),
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

    fn handle(&self, who: &str, words: &[String]) -> Reply {
        let (flags, rest) = parse(words);
        let (command, args) = rest.split_first().ok_or(HELP)?;
        let with_expect = |mut steps: Vec<Vec<String>>| {
            if let Some(text) = &flags.expect {
                steps.push(vec!["wait".to_string(), text.clone()]);
            }
            steps
        };
        match command.as_str() {
            "help" => Ok(HELP.into()),
            "windows" => self.windows_list(who, args.first()),
            "see" => self.see(who, &flags, args),
            "get" => self.get(who, &flags, args),
            "shot" => self.shot(who, &flags, args),
            "launch" => self.launch(who, &flags, args),
            "status" => self.status(),
            "resume" => {
                STOP.store(false, Ordering::Relaxed);
                Ok("resumed".into())
            }
            "quit" => {
                self.quit.store(true, Ordering::Relaxed);
                Ok("bye".into())
            }
            "do" => {
                let script = args.join(" ");
                self.act(who, &flags, &with_expect(lines::split_steps(&script)))
            }
            "click" if args.len() > 1 => {
                let steps = args.iter().map(|word| vec!["click".to_string(), word.clone()]).collect();
                self.act(who, &flags, &with_expect(steps))
            }
            "click" | "set" | "type" | "key" | "scroll" | "drag" | "wait" => {
                let mut step = vec![command.clone()];
                step.extend(args.iter().cloned());
                self.act(who, &flags, &with_expect(vec![step]))
            }
            other => Err(format!("unknown command '{other}': see deskcrew help")),
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
    overlay::run(inbox, &move || keeper.quit.load(Ordering::Relaxed) || keeper.last_used.lock().unwrap().elapsed() > IDLE_EXIT);
    // Give the last reply a moment to leave before the process goes.
    thread::sleep(Duration::from_millis(100));
    pipe::remove();
}
