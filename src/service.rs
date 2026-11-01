// The background service. It keeps the cursors on screen between commands, remembers each
// window's refs, and runs agents side by side: one queue per window, one for the real mouse.
use crate::{
    act::{self, NEEDS_REAL},
    apps,
    lines::{self, Seen},
    overlay::{Overlay, Rect},
    pen::Pen,
    pipe::Pipe,
    real,
    shot::{self, Frame, Frames},
    uia::{self, Node, Uia},
    win::{self, FocusGuard, Hwnd},
};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::UI::WindowsAndMessaging::{SW_MAXIMIZE, SW_MINIMIZE, SW_RESTORE, SW_SHOWNOACTIVATE};

const IDLE_EXIT: Duration = Duration::from_secs(30 * 60);
const SETTLE_FIRST: Duration = Duration::from_millis(50);
const SETTLE_STEP: Duration = Duration::from_millis(60);
const SETTLE_LEAST: Duration = Duration::from_millis(150);
const SETTLE_MOST: Duration = Duration::from_millis(800);
const BETWEEN_STEPS: Duration = Duration::from_millis(40);
const NAME_WAIT: Duration = Duration::from_millis(600);
const FIELD_SETTLE: Duration = Duration::from_millis(60);
const SHARED_PIECE: usize = 10;
const SHARED_OVERLAP: usize = 4;
const SHARED_GAP: Duration = Duration::from_millis(3);
const TYPE_CHECK: Duration = Duration::from_millis(350);
const FOCUS_WATCH: Duration = Duration::from_millis(10);
const FOCUS_LINGER: Duration = Duration::from_millis(500);
/// Chromium fills in its page some time after the first request, in bursts.
const CHROMIUM_STEP: Duration = Duration::from_millis(250);
const CHROMIUM_READS: usize = 16;
const CHROMIUM_STEADY: usize = 3;
const CHROMIUM_NAME_WAIT: Duration = Duration::from_millis(3000);
const WAIT_DEFAULT_MS: u64 = 10_000;
const WAIT_MOST_MS: u64 = 60_000;
const WAIT_STEP: Duration = Duration::from_millis(200);
const MAX_WINDOWS: usize = 40;
const MAX_GET_CHARS: usize = 20_000;
const STOPPED: &str = "stopped by the person with Ctrl+Alt+Esc: ask them before doing more (deskcrew resume clears it)";
const NO_WINDOW: &str = "no window yet: run deskcrew windows, then deskcrew see <name>";

pub const HELP: &str = r#"deskcrew drives Windows apps in the background. Refs stay valid, so read once.
windows [filter]                      list windows
see [window] [-f] [-a] [--find word] [--from N]
                                      controls; later reads show only changes (-f for all, -a off-screen too)
get e3                                a control's full text
shot [window|e3] [-f] [--screen] [--changed] [--edge N]
                                      PNG path; "unchanged" if it matches the last one
click e3 [e4 ...] | "Name" | x,y [--right] [--double]
set e3 "value"     type [e3] "text"     key enter     select e3 "Item"
scroll [e3] up|down [n]     show e3     wait "text" [--gone] [--ms N]
drag a b     draw e3 "x,y x,y; x,y x,y" [--smooth]
do "click e1; set e2 hi; key enter"   many steps, one reply
launch app [args] [--front]     focus     close     place x,y,w,h|min|max|restore
clip     clip set "text"     status     resume     quit
Every action replies with what changed. x,y are pixels in the last shot. Ctrl+Alt+Esc stops every agent.
Flags: -w window, --agent name, --fast (no glide), --force (allow risky keys),
--real (brings the window forward for a moment and uses the keyboard or a pen; the person's cursor stays put),
--mouse (the real mouse, which moves the person's cursor: only if nothing else works)."#;

/// A press that needs the window in front, by the quietest way this spot of the app accepts.
fn press_at(root: Hwnd, x: i32, y: i32, flags: &Flags) -> Reply {
    if flags.mouse {
        real::click(x, y, flags.right, flags.double);
        return Ok("mouse".into());
    }
    if !act::draws_at(root, x, y) {
        return act::post_click(root, x, y, flags.right, flags.double);
    }
    let pen = Pen::new().ok_or(act::NO_PEN)?;
    if pen.tap(x, y, flags.right, flags.double) { Ok("pen".into()) } else { Err(act::NO_PEN.into()) }
}

/// Run a classic menu entry without opening the menu, or list what is under it.
fn run_menu_entry(root: Hwnd, entry: &win::MenuEntry) -> Reply {
    if entry.greyed {
        return Err(format!("'{}' is disabled", entry.label));
    }
    if entry.submenu != 0 {
        let labels: Vec<String> = win::menu_entries(entry.submenu).into_iter().map(|e| e.label).collect();
        return Ok(format!("menu: {}", labels.join(", ")));
    }
    act::post_command(root, entry.command)
}

type Reply = Result<String, String>;
type Bounds = (i32, i32, i32, i32);

#[derive(Default)]
struct Refs {
    by_runtime: HashMap<Vec<i32>, u32>,
    by_identity: HashMap<String, Vec<u32>>,
    next: u32,
}

impl Refs {
    /// The same control keeps its number across reads, even if the app rebuilt it.
    fn assign(&mut self, nodes: Vec<Node>) -> BTreeMap<u32, Node> {
        let mut out: BTreeMap<u32, Node> = BTreeMap::new();
        for node in nodes {
            let known = self.by_runtime.get(&node.runtime).copied().filter(|id| !node.runtime.is_empty() && !out.contains_key(id));
            let same = self.by_identity.entry(node.identity.clone()).or_default();
            let id = known.or_else(|| same.iter().copied().find(|id| !out.contains_key(id))).unwrap_or_else(|| {
                self.next += 1;
                self.next
            });
            if !same.contains(&id) {
                same.push(id);
            }
            self.by_runtime.insert(node.runtime.clone(), id);
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
    pids: Vec<u32>,
    known: Option<HashSet<Hwnd>>,
    surfaces: Vec<Hwnd>,
    read_once: bool,
}

struct Tracked {
    id: u32,
    hwnd: Hwnd,
    view: Mutex<View>,
    pointer: Mutex<()>,
}

struct LastShot {
    hash: u64,
    frame: Frame,
    frames: Frames,
}

#[derive(Default)]
struct AgentState {
    current: Option<Hwnd>,
    seen: HashMap<Hwnd, Seen>,
    shots: HashMap<Hwnd, LastShot>,
}

// Pixels are only ever touched while the agent map is locked.
unsafe impl Send for AgentState {}

#[derive(Default)]
struct Flags {
    real: bool,
    mouse: bool,
    fast: bool,
    force: bool,
    full: bool,
    all: bool,
    right: bool,
    double: bool,
    gone: bool,
    changed: bool,
    screen: bool,
    front: bool,
    smooth: bool,
    window: Option<String>,
    find: Option<String>,
    from: usize,
    ms: Option<u64>,
    edge: Option<u32>,
}

fn parse(words: &[String]) -> (Flags, Vec<String>) {
    let mut flags = Flags::default();
    let mut rest = Vec::new();
    let mut at = 0;
    let mut literal = false;
    while at < words.len() {
        let word = words[at].as_str();
        let next = words.get(at + 1).cloned();
        let mut took_next = false;
        if literal {
            rest.push(word.to_string());
        } else {
            match word {
                "--" => literal = true,
                "--real" => flags.real = true,
                "--mouse" => flags.mouse = true,
                "--fast" => flags.fast = true,
                "--force" => flags.force = true,
                "-f" | "--full" => flags.full = true,
                "-a" | "--all" => flags.all = true,
                "--right" => flags.right = true,
                "--double" => flags.double = true,
                "--gone" => flags.gone = true,
                "--changed" => flags.changed = true,
                "--screen" => flags.screen = true,
                "--front" => flags.front = true,
                "--smooth" => flags.smooth = true,
                "-w" | "--window" => {
                    flags.window = next;
                    took_next = true;
                }
                "--find" => {
                    flags.find = next;
                    took_next = true;
                }
                "--from" => {
                    flags.from = next.and_then(|n| n.parse().ok()).unwrap_or(0);
                    took_next = true;
                }
                "--ms" => {
                    flags.ms = next.and_then(|n| n.parse().ok());
                    took_next = true;
                }
                "--edge" => {
                    flags.edge = next.and_then(|n| n.parse().ok());
                    took_next = true;
                }
                _ => rest.push(word.to_string()),
            }
        }
        at += if took_next { 2 } else { 1 };
    }
    (flags, rest)
}

enum Target {
    Control(u32),
    Point(i32, i32),
    Menu(win::MenuEntry),
}

fn is_ref(word: &str) -> Option<u32> {
    word.strip_prefix('e').and_then(|n| n.parse().ok())
}

fn is_point(word: &str) -> Option<(i32, i32)> {
    let (x, y) = word.split_once(',')?;
    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
}

pub struct Service {
    uia: Uia,
    overlay: Overlay,
    windows: Mutex<(u32, HashMap<Hwnd, Arc<Tracked>>)>,
    agents: Mutex<HashMap<String, AgentState>>,
    real_input: Mutex<()>,
    home: Mutex<Home>,
    keeper: OnceLock<thread::Thread>,
    stop: Arc<AtomicBool>,
    last_request: Mutex<Instant>,
    active: AtomicUsize,
}

#[derive(Default)]
struct Home {
    agents: usize,
    guard: Option<FocusGuard>,
    pids: Vec<u32>,
    leaving: Option<Instant>,
    on_loan: bool,
}

impl Service {
    fn track(&self, hwnd: Hwnd) -> Arc<Tracked> {
        let mut windows = self.windows.lock().unwrap();
        if let Some(found) = windows.1.get(&hwnd) {
            return found.clone();
        }
        windows.0 += 1;
        let tracked = Arc::new(Tracked { id: windows.0, hwnd, view: Mutex::new(View::default()), pointer: Mutex::new(()) });
        windows.1.insert(hwnd, tracked.clone());
        tracked
    }

    fn by_id(&self, id: u32) -> Option<Arc<Tracked>> {
        self.windows.lock().unwrap().1.values().find(|t| t.id == id).cloned()
    }

    fn set_current(&self, who: &str, hwnd: Hwnd) {
        self.agents.lock().unwrap().entry(who.to_string()).or_default().current = Some(hwnd);
    }

    fn pick(&self, who: &str, spec: Option<&str>) -> Result<Arc<Tracked>, String> {
        let Some(spec) = spec else {
            let current = self.agents.lock().unwrap().get(who).and_then(|a| a.current).ok_or(NO_WINDOW)?;
            if !win::exists(current) {
                return Err("that window closed: run deskcrew windows".into());
            }
            return Ok(self.track(current));
        };
        if let Some(id) = spec.strip_prefix('w').and_then(|n| n.parse::<u32>().ok()) {
            let found = self.by_id(id).filter(|t| win::exists(t.hwnd)).ok_or(format!("no window {spec}: run deskcrew windows"))?;
            self.set_current(who, found.hwnd);
            return Ok(found);
        }
        let wanted = spec.to_lowercase();
        let all = win::list();
        let exact: Vec<_> = all.iter().filter(|w| w.title.to_lowercase() == wanted || w.app.to_lowercase() == wanted).collect();
        let matches = if exact.is_empty() {
            all.iter().filter(|w| w.title.to_lowercase().contains(&wanted) || w.app.to_lowercase().contains(&wanted)).collect()
        } else {
            exact
        };
        match matches.as_slice() {
            [] => Err(format!("no window matches '{spec}'")),
            [only] => {
                let tracked = self.track(only.hwnd);
                self.set_current(who, only.hwnd);
                Ok(tracked)
            }
            many => {
                let mut out = vec![format!("'{spec}' matches {} windows:", many.len())];
                out.extend(many.iter().take(8).map(|w| format!("w{} \"{}\" {}", self.track(w.hwnd).id, lines::clip(&w.title, 50), w.app)));
                Err(out.join("\n"))
            }
        }
    }

    /// Read the window and any popups or dialogs that belong to it.
    fn read(&self, tracked: &Tracked, view: &mut View, all: bool) -> Result<Vec<String>, String> {
        if !win::exists(tracked.hwnd) {
            return Err("window closed".into());
        }
        if view.pids.is_empty() {
            let mut pids = vec![win::pid(tracked.hwnd), win::content_pid(tracked.hwnd)];
            pids.dedup();
            view.pids = pids;
        }
        let mut notes = Vec::new();
        let open = win::of_processes(&view.pids);
        let first = view.known.is_none();
        let known = view.known.get_or_insert_with(HashSet::new);
        for hwnd in open.iter().copied() {
            if known.insert(hwnd) && !first && hwnd != tracked.hwnd {
                let mut family = view.surfaces.clone();
                family.push(tracked.hwnd);
                if win::belongs(hwnd, &family) {
                    notes.push(format!("+ window \"{}\"", lines::clip(&win::title(hwnd), 50)));
                    view.surfaces.push(hwnd);
                }
            }
        }
        known.retain(|hwnd| open.contains(hwnd));
        view.surfaces.retain(|hwnd| open.contains(hwnd));

        let mut nodes = Vec::new();
        let mut hidden = 0;
        self.uia.read(tracked.hwnd, all, &mut nodes, &mut hidden)?;
        for hwnd in view.surfaces.clone() {
            let before = nodes.len();
            if self.uia.read(hwnd, all, &mut nodes, &mut hidden).is_err() {
                nodes.truncate(before);
            }
        }
        // Windows lists a dialog under its owner too, so the same control can arrive twice.
        let mut met = HashSet::new();
        nodes.retain(|node| node.runtime.is_empty() || met.insert(node.runtime.clone()));
        view.nodes = view.refs.assign(nodes);
        view.lines = view.nodes.iter().map(|(id, node)| (*id, lines::line(&node.parts))).collect();
        view.hidden = hidden;
        Ok(notes)
    }

    /// Chromium builds its tree on first request, so the first read comes back nearly empty.
    fn first_read(&self, tracked: &Tracked, view: &mut View, all: bool) -> Result<(), String> {
        self.read(tracked, view, all)?;
        if !view.read_once && win::class(tracked.hwnd).starts_with("Chrome_") {
            let mut steady = 0;
            for _ in 0..CHROMIUM_READS {
                let count = view.lines.len();
                thread::sleep(CHROMIUM_STEP);
                self.read(tracked, view, all)?;
                steady = if view.lines.len() == count { steady + 1 } else { 0 };
                if steady == CHROMIUM_STEADY {
                    break;
                }
            }
        }
        view.read_once = true;
        Ok(())
    }

    /// Read until the window stops changing, so a reply shows the result and not a moment in it.
    fn settle(&self, tracked: &Tracked, view: &mut View) -> Result<Vec<String>, String> {
        let started = Instant::now();
        thread::sleep(SETTLE_FIRST);
        let mut notes = self.read(tracked, view, false)?;
        let mut last = view.lines.clone();
        while started.elapsed() < SETTLE_MOST {
            thread::sleep(SETTLE_STEP);
            notes.extend(self.read(tracked, view, false)?);
            let same = view.lines == last;
            last = view.lines.clone();
            if same && started.elapsed() >= SETTLE_LEAST {
                break;
            }
        }
        Ok(notes)
    }

    /// Remember the person's window once, however many agents are working. An agent arriving
    /// while another app has just grabbed focus must not mistake that app for the person's.
    fn focus_in(&self, pids: &[u32]) {
        let mut home = self.home.lock().unwrap();
        if home.guard.is_none() {
            home.guard = Some(FocusGuard::take());
            if let Some(keeper) = self.keeper.get() {
                keeper.unpark();
            }
        }
        home.agents += 1;
        home.leaving = None;
        home.pids.extend_from_slice(pids);
    }

    /// Done with this action. The keeper hands focus back, so no agent queues behind a restore.
    fn focus_out(&self) {
        let mut home = self.home.lock().unwrap();
        home.agents -= 1;
        if home.agents == 0 {
            home.leaving = Some(Instant::now());
        }
    }

    /// Watch for apps taking focus while agents work, and hand it straight back. It sleeps
    /// through idle time and is woken when an agent starts.
    fn keep_focus(&self) {
        loop {
            let held = {
                let mut home = self.home.lock().unwrap();
                let held = home.guard.clone().filter(|_| !home.on_loan).map(|guard| (guard, home.pids.clone()));
                if home.agents == 0 && home.leaving.is_some_and(|at| at.elapsed() > FOCUS_LINGER) {
                    home.guard = None;
                    home.pids.clear();
                    home.leaving = None;
                }
                held
            };
            match held {
                Some((guard, pids)) => {
                    guard.restore(&pids);
                    thread::sleep(FOCUS_WATCH);
                }
                None if self.home.lock().unwrap().on_loan => thread::sleep(FOCUS_WATCH),
                None => thread::park(),
            }
        }
    }

    fn seen(&self, who: &str, hwnd: Hwnd) -> Option<Seen> {
        self.agents.lock().unwrap().get(who).and_then(|a| a.seen.get(&hwnd).cloned())
    }

    fn remember(&self, who: &str, hwnd: Hwnd, lines: Seen) {
        self.agents.lock().unwrap().entry(who.to_string()).or_default().seen.insert(hwnd, lines);
    }

    fn cursor_on(&self, who: &str, flags: &Flags, rect: Bounds) {
        let (x, y) = act::centre(rect);
        if flags.fast {
            self.overlay.jump(who, x, y);
        } else {
            self.overlay.move_to(who, x, y);
        }
        if rect.2 > 0 && rect.3 > 0 {
            self.overlay.press(who, Rect { x: rect.0, y: rect.1, w: rect.2, h: rect.3 });
        }
    }

    fn resolve(&self, word: &str, who: &str, tracked: &Tracked, view: &mut View) -> Result<Target, String> {
        if let Some(id) = is_ref(word) {
            if !view.nodes.contains_key(&id) {
                self.read(tracked, view, false)?;
            }
            return if view.nodes.contains_key(&id) { Ok(Target::Control(id)) } else { Err(format!("e{id} is gone: see")) };
        }
        if let Some((x, y)) = is_point(word) {
            let frames = self.agents.lock().unwrap().get(who).and_then(|a| a.shots.get(&tracked.hwnd).map(|s| s.frames));
            let frames = frames.ok_or("x,y are pixels in a shot: take one first")?;
            let (x, y) = frames.to_screen(x, y);
            return Ok(Target::Point(x, y));
        }
        let wanted = word.to_lowercase();
        let started = Instant::now();
        loop {
            self.read(tracked, view, false)?;
            let named = |exact: bool| -> Vec<u32> {
                view.nodes
                    .iter()
                    .filter(|(_, node)| {
                        let name = node.parts.name.to_lowercase();
                        if exact { name == wanted } else { name.contains(&wanted) }
                    })
                    .map(|(id, _)| *id)
                    .collect()
            };
            let mut hits = named(true);
            if hits.is_empty() {
                hits = named(false);
            }
            match hits.as_slice() {
                [only] => return Ok(Target::Control(*only)),
                [] => {
                    if let Some(entry) = win::find_menu(win::menu_bar(tracked.hwnd), word) {
                        return Ok(Target::Menu(entry));
                    }
                    let patience = if win::class(tracked.hwnd).starts_with("Chrome_") { CHROMIUM_NAME_WAIT } else { NAME_WAIT };
                    if started.elapsed() >= patience {
                        return Err(format!("no control named '{word}': see"));
                    }
                    thread::sleep(SETTLE_STEP);
                }
                many => {
                    // A label and the control it names share a name: the one that acts is meant.
                    let acting: Vec<u32> = many.iter().copied().filter(|id| !view.nodes[id].parts.role.is_empty()).collect();
                    if let [only] = acting.as_slice() {
                        return Ok(Target::Control(*only));
                    }
                    let mut out = vec![format!("'{word}' matches {} controls:", many.len())];
                    out.extend(many.iter().take(8).map(|id| format!("e{id} {}", view.lines[id])));
                    return Err(out.join("\n"));
                }
            }
        }
    }

    /// A control's place on screen now, reading again if it was rebuilt since the last read.
    fn locate(&self, id: u32, tracked: &Tracked, view: &mut View) -> Result<(Node, Bounds), String> {
        let node = view.nodes[&id].clone();
        if let Ok(rect) = node.element.rect() {
            return Ok((node, rect));
        }
        self.read(tracked, view, false)?;
        let node = view.nodes.get(&id).cloned().ok_or(format!("e{id} is gone: see"))?;
        let rect = node.element.rect()?;
        Ok((node, rect))
    }

    /// Bring the window forward for work that needs it, and give focus back after. Every point has
    /// to land on this window: a press anywhere else would hit whatever the person has open there.
    /// The person's cursor is only borrowed for --mouse.
    /// True if something else is on top of this point of the window.
    fn covered(&self, tracked: &Tracked, view: &View, x: i32, y: i32) -> bool {
        let mut family = view.surfaces.clone();
        family.push(tracked.hwnd);
        !win::owns_point(&family, x, y)
    }

    fn with_front<T>(&self, tracked: &Tracked, view: &View, points: &[(i32, i32)], mouse: bool, work: impl FnOnce() -> T) -> Result<T, String> {
        let _turn = self.real_input.lock().unwrap();
        // The focus keeper stands aside while focus is on loan, or it would take it straight back.
        self.home.lock().unwrap().on_loan = true;
        let result = (|| {
            let _borrowed = real::borrow(tracked.hwnd, &view.pids, mouse)?;
            let mut family = view.surfaces.clone();
            family.push(tracked.hwnd);
            if let Some((x, y)) = points.iter().find(|(x, y)| !win::owns_point(&family, *x, *y)) {
                return Err(format!("refused: {x},{y} on screen is not on this window, so a real press there would hit something else"));
            }
            Ok(work())
        })();
        self.home.lock().unwrap().on_loan = false;
        result
    }

    fn click(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, word: &str) -> Reply {
        let plain = !flags.right && !flags.double;
        let (node, (x, y)) = match self.resolve(word, who, tracked, view)? {
            Target::Menu(entry) => return run_menu_entry(tracked.hwnd, &entry),
            Target::Point(x, y) => {
                self.cursor_on(who, flags, (x, y, 0, 0));
                (None, (x, y))
            }
            Target::Control(id) => {
                let (node, rect) = self.locate(id, tracked, view)?;
                self.cursor_on(who, flags, rect);
                (Some(node), act::centre(rect))
            }
        };
        if flags.real || flags.mouse {
            return self.with_front(tracked, view, &[(x, y)], flags.mouse, || press_at(tracked.hwnd, x, y, flags))?;
        }
        if let Some(node) = &node {
            if node.parts.states.contains(&"disabled") {
                return Err("that control is disabled".into());
            }
            if plain && let Some(result) = act::post_button(node, tracked.hwnd) {
                return result;
            }
            if plain && node.parts.role == "menuitem" && let Some(entry) = win::find_menu(win::menu_bar(tracked.hwnd), &node.parts.name) {
                return run_menu_entry(tracked.hwnd, &entry);
            }
        }
        // A browser's accessibility click pulls its window forward, so a message is better while
        // the window shows. A covered browser drops messages, so that is when accessibility is needed.
        let chromium_shows = win::is_chromium(tracked.hwnd) && !self.covered(tracked, view, x, y);
        let node = node.filter(|node| !act::is_classic_row(node, tracked.hwnd) && !chromium_shows);
        if let (Some(node), true) = (&node, plain) {
            let classic = !act::draws_at(tracked.hwnd, x, y);
            let (pressed, tried_mouse) = if classic { self.overlay.shielded(|| act::press(node)) } else { (act::press(node), false) };
            if tried_mouse {
                return Err(act::NEEDS_MOUSE.into());
            }
            match pressed {
                Some(Ok(route)) => return Ok(route),
                Some(Err(error)) if error.contains("administrator") => return Err(error),
                _ => {}
            }
        }
        act::post_click(tracked.hwnd, x, y, flags.right, flags.double)
    }

    fn set(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, word: &str, value: &str) -> Reply {
        let Target::Control(id) = self.resolve(word, who, tracked, view)? else { return Err("set needs a control".into()) };
        let (node, rect) = self.locate(id, tracked, view)?;
        self.cursor_on(who, flags, rect);
        if flags.real || flags.mouse {
            let (x, y) = act::centre(rect);
            let all = real::chord("ctrl+a")?;
            return self.with_front(tracked, view, &[(x, y)], flags.mouse, || {
                press_at(tracked.hwnd, x, y, flags)?;
                real::press_chord(&all);
                real::type_text(value);
                Ok("keyboard".to_string())
            })?;
        }
        act::set(&node, tracked.hwnd, value)
    }

    fn type_text(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, words: &[String]) -> Reply {
        let (target, text) = match words {
            [text] => (None, text),
            [word, text] => (Some(word), text),
            _ => return Err("type [control] \"text\"".into()),
        };
        let mut place = None;
        let mut hwnd = None;
        let mut field = None;
        if let Some(word) = target {
            let Target::Control(id) = self.resolve(word, who, tracked, view)? else { return Err("type needs a control".into()) };
            let (node, rect) = self.locate(id, tracked, view)?;
            self.cursor_on(who, flags, rect);
            place = Some(act::centre(rect));
            hwnd = act::native_window(&node, tracked.hwnd);
            field = Some(node);
        }
        if flags.real || flags.mouse {
            let points: Vec<(i32, i32)> = place.into_iter().collect();
            return self.with_front(tracked, view, &points, flags.mouse, || {
                if let Some((x, y)) = place {
                    press_at(tracked.hwnd, x, y, flags)?;
                }
                real::type_text(text);
                Ok("keyboard".to_string())
            })?;
        }
        if let (Some(_), Some(node)) = (place.filter(|(x, y)| win::is_chromium(tracked.hwnd) && self.covered(tracked, view, *x, *y)), &field) {
            let mut value = node.element.value_text();
            value.push_str(text);
            return act::set(node, tracked.hwnd, &value);
        }
        if let (Some((x, y)), true) = (place, win::is_chromium(tracked.hwnd)) {
            // The page has to know which field has focus, and a click inside it says so.
            act::post_click(tracked.hwnd, x, y, false, false)?;
            thread::sleep(FIELD_SETTLE);
            // Every window of a browser shares a thread, so "the focused window" could be another
            // agent's. This field's own window keeps the text here.
            let before = field.as_ref().filter(|node| node.can.value).map(|node| node.element.value_text());
            let route = act::post_text(win::child_at(tracked.hwnd, x, y).0, text, &self.stop)?;
            // Chromium can drop characters while its window is not active, and says nothing. A
            // field that can be read is read back, and set directly if the text did not land.
            if let (Some(before), Some(node)) = (before, &field) {
                let wanted = format!("{before}{text}");
                let until = Instant::now() + TYPE_CHECK;
                while node.element.value_text() != wanted && Instant::now() < until {
                    thread::sleep(SETTLE_STEP);
                }
                if node.element.value_text() != wanted {
                    return act::set(node, tracked.hwnd, &wanted);
                }
            }
            return Ok(route);
        }
        match (target, hwnd) {
            (Some(_), None) => Err(NEEDS_REAL.into()),
            (Some(_), Some(hwnd)) => act::post_text(hwnd, text, &self.stop),
            (None, _) if win::is_chromium(tracked.hwnd) => act::post_text(tracked.hwnd, text, &self.stop),
            (None, _) => act::post_text(win::keyboard_target(tracked.hwnd), text, &self.stop),
        }
    }

    fn key(&self, flags: &Flags, tracked: &Tracked, view: &View, spec: &str) -> Reply {
        if real::is_risky(spec) && !flags.force {
            return Err(format!("{spec} is refused: it can end the session or open the shell (--force to allow)"));
        }
        let chord = real::chord(spec)?;
        if flags.real || flags.mouse {
            self.with_front(tracked, view, &[], false, || real::press_chord(&chord))?;
            return Ok("keyboard".into());
        }
        act::post_key(tracked.hwnd, &chord, spec)
    }

    fn select(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, word: &str, item: &str) -> Reply {
        let Target::Control(id) = self.resolve(word, who, tracked, view)? else { return Err("select needs a control".into()) };
        let (node, rect) = self.locate(id, tracked, view)?;
        self.cursor_on(who, flags, rect);
        let (result, tried_mouse) = self.overlay.shielded(|| act::select_item(&node, item, &self.uia, tracked.hwnd));
        if tried_mouse { Err(act::NEEDS_MOUSE.into()) } else { result }
    }

    fn scroll(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, words: &[String]) -> Reply {
        let direction = |word: &String| word == "up" || word == "down";
        let at = words.iter().position(direction).ok_or("scroll [control] up|down [n]")?;
        let down = words[at] == "down";
        let times: i32 = words.get(at + 1).and_then(|n| n.parse().ok()).unwrap_or(1);
        let id = match words[..at].first() {
            Some(word) => match self.resolve(word, who, tracked, view)? {
                Target::Control(id) => Some(id),
                _ => return Err("scroll needs a control".into()),
            },
            None => view.nodes.iter().find(|(_, node)| node.parts.states.contains(&"scroll")).map(|(id, _)| *id),
        };
        let Some(id) = id else { return Err("nothing here scrolls: name a control".into()) };
        let (node, rect) = self.locate(id, tracked, view)?;
        self.cursor_on(who, flags, rect);
        let (x, y) = act::centre(rect);
        let notches = if down { -times } else { times };
        if flags.mouse {
            self.with_front(tracked, view, &[(x, y)], true, || real::wheel(x, y, notches))?;
            return Ok("mouse".into());
        }
        if flags.real {
            let page = real::chord(if down { "pagedown" } else { "pageup" })?;
            self.with_front(tracked, view, &[(x, y)], false, || {
                for _ in 0..times.max(1) {
                    real::press_chord(&page);
                }
            })?;
            return Ok("keyboard".into());
        }
        if node.can.scroll {
            for _ in 0..times.max(1) {
                node.element.scroll(down, true)?;
            }
            return Ok("scroll".into());
        }
        act::post_wheel(tracked.hwnd, x, y, notches)
    }

    fn point_of(&self, word: &str, who: &str, tracked: &Tracked, view: &mut View) -> Result<(i32, i32), String> {
        match self.resolve(word, who, tracked, view)? {
            Target::Point(x, y) => Ok((x, y)),
            Target::Control(id) => Ok(act::centre(self.locate(id, tracked, view)?.1)),
            Target::Menu(_) => Err("that is a menu entry, not a place".into()),
        }
    }

    /// Strokes by the quietest way the app accepts: messages to a classic window, the pen for one
    /// that draws itself, and the real mouse only when asked for.
    fn walk(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &View, strokes: Vec<Vec<(f32, f32)>>) -> Reply {
        let points: Vec<(i32, i32)> = strokes.iter().flatten().map(|p| (p.0.round() as i32, p.1.round() as i32)).collect();
        let (x, y) = *points.first().ok_or("nothing to draw")?;
        let jump = |x: i32, y: i32| self.overlay.jump(who, x, y);
        let (route, finished) = if flags.mouse {
            let done = self.with_front(tracked, view, &points, true, || strokes.iter().all(|path| real::stroke(path, &self.stop, jump)))?;
            ("mouse", done)
        } else if act::draws_at(tracked.hwnd, x, y) {
            let pen = Pen::new().ok_or(act::NO_PEN)?;
            let done = self.with_front(tracked, view, &points, false, || {
                strokes.iter().try_fold(true, |done, path| Ok::<bool, String>(done && pen.stroke(path, &self.stop, jump)?))
            })??;
            ("pen", done)
        } else {
            if let Some((x, y)) = points.iter().find(|(x, y)| !win::inside(tracked.hwnd, *x, *y)) {
                return Err(format!("refused: {x},{y} on screen is not on this window"));
            }
            if !flags.real && win::is_chromium(tracked.hwnd) && points.iter().any(|(x, y)| self.covered(tracked, view, *x, *y)) {
                return Err("this browser window is covered and would drop the strokes: uncover it or add --real".into());
            }
            let send = || strokes.iter().try_fold(true, |done, path| Ok::<bool, String>(done && act::post_stroke(tracked.hwnd, path, &self.stop, jump)?));
            let done = if flags.real { self.with_front(tracked, view, &points, false, send)?? } else { send()? };
            ("message", done)
        };
        if finished { Ok(route.into()) } else { Err(STOPPED.into()) }
    }

    fn drag(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, from: &str, to: &str) -> Reply {
        let start = self.point_of(from, who, tracked, view)?;
        let end = self.point_of(to, who, tracked, view)?;
        self.overlay.move_to(who, start.0, start.1);
        self.walk(who, flags, tracked, view, vec![real::trace(&[start, end], false)])
    }

    fn draw(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, words: &[String]) -> Reply {
        let strokes = self.plan_draw(who, flags, tracked, view, words)?;
        let first = strokes[0][0];
        self.overlay.move_to(who, first.0 as i32, first.1 as i32);
        self.walk(who, flags, tracked, view, strokes)
    }

    /// Several agents drawing in one browser or Electron window. An app has one pointer, so strokes
    /// are cut into pieces short enough that turns between agents look like drawing at once.
    fn draw_together(&self, who: &str, flags: &Flags, words: &[String]) -> Reply {
        if self.stop.load(Ordering::Relaxed) {
            return Err(STOPPED.into());
        }
        let tracked = self.pick(who, flags.window.as_deref())?;
        let (strokes, pids) = {
            let mut guard = tracked.view.lock().unwrap();
            let view = &mut *guard;
            if !view.read_once {
                self.first_read(&tracked, view, false)?;
            }
            let strokes = self.plan_draw(who, flags, &tracked, view, words)?;
            if strokes.iter().flatten().any(|p| !win::inside(tracked.hwnd, p.0 as i32, p.1 as i32)) {
                return Err("refused: that stroke leaves the window".into());
            }
            if strokes.iter().flatten().any(|p| self.covered(&tracked, view, p.0 as i32, p.1 as i32)) {
                return Err("this browser window is covered and would drop the strokes: uncover it or add --real".into());
            }
            (strokes, view.pids.clone())
        };
        let first = strokes[0][0];
        self.overlay.move_to(who, first.0 as i32, first.1 as i32);
        self.focus_in(&pids);
        let mut result = Ok(true);
        'strokes: for stroke in &strokes {
            let mut start = 0;
            while start + 1 < stroke.len() {
                let end = (start + SHARED_PIECE).min(stroke.len() - 1);
                let done = {
                    let _turn = tracked.pointer.lock().unwrap();
                    act::post_stroke(tracked.hwnd, &stroke[start..=end], &self.stop, |x, y| self.overlay.jump(who, x, y))
                };
                match done {
                    Ok(true) => {}
                    other => {
                        result = other;
                        break 'strokes;
                    }
                }
                if end + 1 == stroke.len() {
                    break;
                }
                // Brushes lose the first few pixels of a press, so each piece starts back over the last.
                start = end - SHARED_OVERLAP;
                // Leave the pointer free long enough for the next agent to get a turn.
                thread::sleep(SHARED_GAP);
            }
        }
        self.focus_out();
        match result? {
            true => Ok("ok message".into()),
            false => Err(STOPPED.into()),
        }
    }

    fn plan_draw(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, words: &[String]) -> Result<Vec<Vec<(f32, f32)>>, String> {
        let (word, script) = match words {
            [script] => (None, script),
            [word, script] => (Some(word), script),
            _ => return Err("draw [control] \"x,y x,y; x,y x,y\"".into()),
        };
        let origin = match word {
            Some(word) => match self.resolve(word, who, tracked, view)? {
                Target::Control(id) => Some(self.locate(id, tracked, view)?.1),
                _ => return Err("draw needs a control".into()),
            },
            None => None,
        };
        let frames = self.agents.lock().unwrap().get(who).and_then(|a| a.shots.get(&tracked.hwnd).map(|s| s.frames));
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
            if points.is_empty() {
                continue;
            }
            strokes.push(real::trace(&points, flags.smooth));
        }
        if strokes.iter().map(Vec::len).sum::<usize>() == 0 {
            return Err("nothing to draw".into());
        }
        Ok(strokes)
    }

    fn wait(&self, flags: &Flags, tracked: &Tracked, view: &mut View, text: &str) -> Reply {
        if let Ok(ms) = text.parse::<u64>() {
            thread::sleep(Duration::from_millis(ms.min(WAIT_MOST_MS)));
            return Ok("waited".into());
        }
        let wanted = text.to_lowercase();
        let until = Instant::now() + Duration::from_millis(flags.ms.unwrap_or(WAIT_DEFAULT_MS).min(WAIT_MOST_MS));
        loop {
            self.read(tracked, view, false)?;
            let present = view.lines.values().any(|line| line.to_lowercase().contains(&wanted));
            if present != flags.gone {
                return Ok("found".into());
            }
            if Instant::now() >= until {
                return Err(format!("timed out waiting for '{text}'"));
            }
            if self.stop.load(Ordering::Relaxed) {
                return Err(STOPPED.into());
            }
            thread::sleep(WAIT_STEP);
        }
    }

    fn step(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, words: &[String]) -> Reply {
        let (own, words) = parse(words);
        let flags = Flags {
            real: flags.real || own.real,
            mouse: flags.mouse || own.mouse,
            fast: flags.fast || own.fast,
            force: flags.force || own.force,
            right: flags.right || own.right,
            double: flags.double || own.double,
            gone: flags.gone || own.gone,
            smooth: flags.smooth || own.smooth,
            ms: own.ms.or(flags.ms),
            ..Flags::default()
        };
        let verb = words.first().map_or("", String::as_str);
        let rest = words.get(1..).unwrap_or(&[]);
        match (verb, rest) {
            ("click", [word]) => self.click(who, &flags, tracked, view, word),
            ("set", [word, value]) => self.set(who, &flags, tracked, view, word, value),
            ("type", _) => self.type_text(who, &flags, tracked, view, rest),
            ("key", [spec]) => self.key(&flags, tracked, view, spec),
            ("select", [word, item]) => self.select(who, &flags, tracked, view, word, item),
            ("scroll", _) => self.scroll(who, &flags, tracked, view, rest),
            ("show", [word]) => match self.resolve(word, who, tracked, view)? {
                Target::Control(id) if view.nodes[&id].can.scroll_item => view.nodes[&id].element.bring_into_view().map(|_| "show".to_string()),
                Target::Control(_) => Err("that control cannot scroll into view".into()),
                _ => Err("show needs a control".into()),
            },
            ("drag", [from, to]) => self.drag(who, &flags, tracked, view, from, to),
            ("draw", _) => self.draw(who, &flags, tracked, view, rest),
            ("wait", [text]) => self.wait(&flags, tracked, view, text),
            _ => Err(format!("can't do '{}': see deskcrew help", words.join(" "))),
        }
    }

    /// Run steps against one window, then report the result as a single set of changes.
    fn act(&self, who: &str, flags: &Flags, steps: &[Vec<String>]) -> Reply {
        if self.stop.load(Ordering::Relaxed) {
            return Err(STOPPED.into());
        }
        let tracked = self.pick(who, flags.window.as_deref())?;
        let mut guard = tracked.view.lock().unwrap();
        let view = &mut *guard;
        if !view.read_once {
            self.first_read(&tracked, view, false)?;
        }
        let before = self.seen(who, tracked.hwnd).unwrap_or_else(|| view.lines.clone());
        self.focus_in(&view.pids);
        let mut routes = Vec::new();
        let mut failure = None;
        for step in steps {
            if self.stop.load(Ordering::Relaxed) {
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
        let waited_only = routes.iter().all(|r| r == "found" || r == "waited") && !routes.is_empty();
        let settled = if waited_only { Ok(Vec::new()) } else { self.settle(&tracked, view) };
        self.focus_out();

        let mut out = Vec::new();
        out.push(if routes.len() > 1 { format!("ok {} steps", routes.len()) } else { format!("ok {}", routes.first().map_or("", String::as_str)) });
        match settled {
            Ok(notes) => {
                let (changes, complete) = lines::change_report(&before, &view.lines);
                let nothing = changes.is_empty() && notes.is_empty();
                out.extend(notes);
                out.extend(changes);
                if complete {
                    self.remember(who, tracked.hwnd, view.lines.clone());
                }
                if nothing && routes.iter().any(|r| r == "message") {
                    out.push("nothing changed: try --real if it should have".into());
                } else if nothing && routes.iter().any(|r| r == "pen" || r == "mouse" || r == "keyboard") {
                    out.push("nothing changed: check with shot".into());
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
        let listed = win::list();
        self.windows.lock().unwrap().1.retain(|hwnd, _| win::exists(*hwnd));
        let shown: Vec<String> = listed
            .iter()
            .filter(|w| wanted.as_ref().is_none_or(|f| w.title.to_lowercase().contains(f) || w.app.to_lowercase().contains(f)))
            .take(MAX_WINDOWS)
            .map(|w| {
                let mark = if current == Some(w.hwnd) { "*" } else { "" };
                format!("w{}{mark} \"{}\" {}", self.track(w.hwnd).id, lines::clip(&w.title, 50), w.app)
            })
            .collect();
        Ok(if shown.is_empty() { "no windows".into() } else { shown.join("\n") })
    }

    fn see(&self, who: &str, flags: &Flags, words: &[String]) -> Reply {
        let spec = words.first().map(String::as_str).or(flags.window.as_deref());
        let tracked = self.pick(who, spec)?;
        let mut guard = tracked.view.lock().unwrap();
        let view = &mut *guard;
        self.first_read(&tracked, view, flags.all)?;
        let filtered = flags.find.is_some() || flags.from > 0;
        let seen = self.seen(who, tracked.hwnd).filter(|_| !flags.full && !filtered && !flags.all);
        if let Some(seen) = seen {
            let changes = lines::changes(&seen, &view.lines);
            if changes.len() * 2 <= view.lines.len().max(1) {
                self.remember(who, tracked.hwnd, view.lines.clone());
                return Ok(if changes.is_empty() { "no change".into() } else { changes.join("\n") });
            }
        }
        if !filtered {
            self.remember(who, tracked.hwnd, view.lines.clone());
        }
        let hidden = if view.hidden > 0 { format!(", {} off screen", view.hidden) } else { String::new() };
        let mut out = vec![format!("w{} \"{}\" {} controls{hidden}", tracked.id, lines::clip(&win::title(tracked.hwnd), 50), view.lines.len())];
        out.extend(lines::page(&view.lines, flags.find.as_deref(), flags.from));
        Ok(out.join("\n"))
    }

    fn get(&self, who: &str, flags: &Flags, words: &[String]) -> Reply {
        let word = words.first().ok_or("get needs a control")?;
        let tracked = self.pick(who, flags.window.as_deref())?;
        let mut view = tracked.view.lock().unwrap();
        if !view.read_once {
            self.first_read(&tracked, &mut view, false)?;
        }
        let Target::Control(id) = self.resolve(word, who, &tracked, &mut view)? else { return Err("get needs a control".into()) };
        let text = view.nodes[&id].element.full_text();
        Ok(text.chars().take(MAX_GET_CHARS).collect())
    }

    fn capture_screen(&self, x: i32, y: i32, w: i32, h: i32) -> Result<Frame, String> {
        self.overlay.aside(true);
        let frame = shot::screen(x, y, w, h);
        self.overlay.aside(false);
        frame
    }

    fn shot(&self, who: &str, flags: &Flags, words: &[String]) -> Reply {
        let edge = flags.edge.unwrap_or(shot::DEFAULT_EDGE);
        if flags.screen {
            let (width, height) = crate::overlay::screen_size();
            let frame = self.capture_screen(0, 0, width, height)?;
            let (small, scale) = frame.shrink(edge);
            let path = shot::path_for(who, 0);
            small.save(&path)?;
            return Ok(format!("{} {}x{} scale {scale:.2}", path.display(), small.w, small.h));
        }
        let word = words.first();
        let control = word.and_then(|w| is_ref(w));
        let tracked = self.pick(who, if control.is_some() { flags.window.as_deref() } else { word.map(String::as_str).or(flags.window.as_deref()) })?;
        let (left, top, ..) = win::rect(tracked.hwnd);
        let mut note = "";
        let (mut frame, mut units) = shot::window(tracked.hwnd)?;
        if frame.is_blank() {
            let (x, y, w, h) = win::rect(tracked.hwnd);
            let copy = self.capture_screen(x, y, w, h)?;
            if !copy.is_blank() {
                frame = copy;
                units = 1.0;
                note = " (copied from the screen: anything covering the window shows)";
            }
        }
        let mut origin = (left, top);
        if let Some(id) = control {
            let mut view = tracked.view.lock().unwrap();
            if !view.nodes.contains_key(&id) {
                self.read(&tracked, &mut view, false)?;
            }
            let node = view.nodes.get(&id).ok_or(format!("e{id} is gone: see"))?;
            let rect = node.element.rect()?;
            let inside = |value: i32| (value as f32 / units) as u32;
            frame = frame.crop(inside((rect.0 - left).max(0)), inside((rect.1 - top).max(0)), inside(rect.2).max(1), inside(rect.3).max(1));
            origin = (rect.0.max(left), rect.1.max(top));
        }

        let hash = frame.hash();
        let mut agents = self.agents.lock().unwrap();
        let state = agents.entry(who.to_string()).or_default();
        let last = state.shots.get(&tracked.hwnd);
        if control.is_none() && !flags.full && last.is_some_and(|l| l.hash == hash) {
            return Ok("unchanged".into());
        }
        let mut crop = None;
        if flags.changed && control.is_none() {
            crop = last.and_then(|l| frame.changed_from(&l.frame));
        }
        let picture = match crop {
            Some((x, y, w, h)) => {
                origin = (origin.0 + (x as f32 * units) as i32, origin.1 + (y as f32 * units) as i32);
                frame.crop(x, y, w, h)
            }
            None => frame.crop(0, 0, frame.w, frame.h),
        };
        let (small, scale) = picture.shrink(edge);
        let path = shot::path_for(who, tracked.id);
        small.save(&path)?;
        let frames = Frames { x: origin.0, y: origin.1, scale: scale * units };
        if control.is_none() {
            state.shots.insert(tracked.hwnd, LastShot { hash, frame, frames });
        } else if let Some(last) = state.shots.get_mut(&tracked.hwnd) {
            last.frames = frames;
        } else {
            state.shots.insert(tracked.hwnd, LastShot { hash: 0, frame: Frame { w: 0, h: 0, pixels: Vec::new() }, frames });
        }
        let part = if crop.is_some() { " changed part" } else { "" };
        Ok(format!("{} {}x{} scale {scale:.2}{part}{note}", path.display(), small.w, small.h))
    }

    fn launch(&self, who: &str, flags: &Flags, words: &[String]) -> Reply {
        if self.stop.load(Ordering::Relaxed) {
            return Err(STOPPED.into());
        }
        let app = words.first().ok_or("launch needs an app")?;
        let args = words[1..].join(" ");
        let opened = apps::launch(app, &args, flags.front)?;
        let Some(hwnd) = opened else { return Ok(format!("started {app}; no new window yet")) };
        let tracked = self.track(hwnd);
        self.set_current(who, hwnd);
        Ok(format!("opened w{} \"{}\"", tracked.id, lines::clip(&win::title(hwnd), 50)))
    }

    fn place(&self, who: &str, flags: &Flags, words: &[String]) -> Reply {
        let tracked = self.pick(who, flags.window.as_deref())?;
        let word = words.first().ok_or("place x,y,w,h|min|max|restore")?;
        match word.as_str() {
            "min" => win::show(tracked.hwnd, SW_MINIMIZE),
            "max" => win::show(tracked.hwnd, SW_MAXIMIZE),
            "restore" => win::show(tracked.hwnd, SW_RESTORE),
            "show" => win::show(tracked.hwnd, SW_SHOWNOACTIVATE),
            box_text => {
                let parts: Vec<i32> = box_text.split(',').filter_map(|n| n.trim().parse().ok()).collect();
                let [x, y, w, h] = parts[..] else { return Err("place x,y,w,h|min|max|restore".into()) };
                win::place(tracked.hwnd, x, y, w, h);
            }
        }
        Ok("ok".into())
    }

    fn status(&self) -> Reply {
        let agents = self.agents.lock().unwrap();
        let mut names: Vec<&String> = agents.keys().collect();
        names.sort();
        let names: Vec<&str> = names.iter().map(|n| n.as_str()).collect();
        let state = if self.stop.load(Ordering::Relaxed) { "stopped by the person" } else { "running" };
        Ok(format!("deskcrew {} {state}; agents: {}", env!("CARGO_PKG_VERSION"), if names.is_empty() { "none".into() } else { names.join(", ") }))
    }

    fn handle(&self, who: &str, words: &[String]) -> Reply {
        let (flags, rest) = parse(words);
        let verb = rest.first().map_or("", String::as_str);
        let args = rest.get(1..).unwrap_or(&[]);
        match verb {
            "windows" => self.windows_list(who, args.first()),
            "see" => self.see(who, &flags, args),
            "get" => self.get(who, &flags, args),
            "shot" => self.shot(who, &flags, args),
            "launch" => self.launch(who, &flags, args),
            "focus" => {
                let tracked = self.pick(who, args.first().map(String::as_str).or(flags.window.as_deref()))?;
                if win::bring_forward(tracked.hwnd) { Ok("ok".into()) } else { Err("Windows would not bring that window forward".into()) }
            }
            "close" => {
                let tracked = self.pick(who, args.first().map(String::as_str).or(flags.window.as_deref()))?;
                act::close_window(tracked.hwnd).map(|_| "ok".to_string())
            }
            "place" => self.place(who, &flags, args),
            "clip" => match args {
                [] => apps::clipboard_text(),
                [set, text] if set == "set" => apps::set_clipboard(text).map(|_| "ok".to_string()),
                _ => Err("clip, or clip set \"text\"".into()),
            },
            "status" => self.status(),
            "resume" => {
                self.stop.store(false, Ordering::Relaxed);
                Ok("ok".into())
            }
            "do" => {
                let script = args.first().ok_or("do \"click e1; key enter\"")?;
                self.act(who, &flags, &lines::split_steps(script))
            }
            "click" if !args.is_empty() => {
                let steps: Vec<Vec<String>> = args.iter().map(|word| vec!["click".to_string(), word.clone()]).collect();
                self.act(who, &flags, &steps)
            }
            "draw" if !flags.real && !flags.mouse && self.pick(who, flags.window.as_deref()).is_ok_and(|t| win::is_chromium(t.hwnd)) => self.draw_together(who, &flags, args),
            "set" | "type" | "key" | "select" | "scroll" | "show" | "drag" | "draw" | "wait" | "click" => self.act(who, &flags, std::slice::from_ref(&rest)),
            "" | "help" => Ok(HELP.to_string()),
            _ => Err(format!("unknown command '{verb}': deskcrew help")),
        }
    }

    fn serve(&self, pipe: Pipe) {
        uia::enter_com();
        if let Ok(message) = pipe.receive() {
            self.active.fetch_add(1, Ordering::SeqCst);
            *self.last_request.lock().unwrap() = Instant::now();
            let text = String::from_utf8_lossy(&message);
            let mut words = text.split('\0').map(str::to_string);
            let who = words.next().unwrap_or_default();
            let words: Vec<String> = words.collect();
            let quitting = words.first().is_some_and(|w| w == "quit");
            let reply = if quitting { Ok("ok".to_string()) } else { self.handle(&who, &words) };
            let (code, body) = match reply {
                Ok(body) => (0, body),
                Err(body) => (1, body),
            };
            let mut framed = vec![code];
            framed.extend_from_slice(body.as_bytes());
            let _ = pipe.send(&framed);
            pipe.finish();
            *self.last_request.lock().unwrap() = Instant::now();
            self.active.fetch_sub(1, Ordering::SeqCst);
            if quitting {
                std::process::exit(0);
            }
        }
    }
}

pub fn run() {
    crate::overlay::make_aware();
    uia::enter_com();
    let Ok(mut listening) = Pipe::listen(true) else { return };
    let Ok(uia) = Uia::new() else { return };
    let stop = Arc::new(AtomicBool::new(false));
    let service = Arc::new(Service {
        uia,
        overlay: Overlay::start(stop.clone()),
        windows: Mutex::new((0, HashMap::new())),
        agents: Mutex::new(HashMap::new()),
        real_input: Mutex::new(()),
        home: Mutex::new(Home::default()),
        keeper: OnceLock::new(),
        stop,
        last_request: Mutex::new(Instant::now()),
        active: AtomicUsize::new(0),
    });

    let keeper = service.clone();
    let _ = service.keeper.set(thread::spawn(move || keeper.keep_focus()).thread().clone());

    let watcher = service.clone();
    thread::spawn(move || {
        loop {
            thread::sleep(Duration::from_secs(30));
            if watcher.active.load(Ordering::SeqCst) == 0 && watcher.last_request.lock().unwrap().elapsed() > IDLE_EXIT {
                std::process::exit(0);
            }
        }
    });

    loop {
        if !listening.accept() {
            continue;
        }
        // The next instance exists before this one is handed off, so the name never lapses and
        // a caller can never conclude the service is down and start a second one.
        let Ok(next) = Pipe::listen(false) else { continue };
        let current = std::mem::replace(&mut listening, next);
        let worker = service.clone();
        thread::spawn(move || worker.serve(current));
    }
}
