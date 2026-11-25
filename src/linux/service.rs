// The background service on Linux (X11/Xvfb and Wayland AT-SPI). It remembers each window's
// refs and runs agents side by side. Controls are read and pressed through AT-SPI, which needs no
// focus and leaves the pointer alone — and also works on Wayland. On X11, keys go through XTest
// to the window given the keyboard focus without raising it, and points/drawing use the display's
// pointer, bringing the window forward. Agents take turns with both.
use super::{
    atspi::{self, FrameWindow, Node, Obj},
    overlay::Overlay,
    portal,
    x::{self, Bounds, Button, By, Window},
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
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

pub const HELP: &str = "\
deskcrew drives Linux apps on X11 (a desktop or Xvfb) and Wayland (AT-SPI controls). Refs stay valid, so read once.
windows [filter]                      list windows
see [window] [-f] [-a] [--find word] [--from N]
                                      controls; later reads show only changes (-f for all, -a off-screen too)
get e3                                a control's full text
table e3 [--from N]                   rows of a table or list
shot [window|e3] [-f] [--marks] [--rulers] [--screen [-a]] [--region x,y,w,h] [--changed] [--edge N]
                                      PNG path; \"unchanged\" if it matches the last one
click e3 [e4 ...] | \"Name\" | x,y [--right] [--double] [--triple] [--middle] [--back] [--forward] [--with ctrl+shift] [--hold ms]
hover e3 | x,y     check e3     uncheck e3     toggle e3     expand e3     collapse e3     clear e3
is e3 on|off|open|closed|selected|enabled|disabled
down shift     up (lets go; 30 s limit)
set e3 \"value\"     type [e3] \"text\" [--paste]     key enter [--hold ms]     select e3 \"Item\"
scroll [e3] up|down|left|right [n]     show e3     wait \"text\"|ms [--gone] [--ms N]
drag a b     draw e3 \"x,y x,y; x,y x,y\" [--smooth] [--fill] [--close]
pick-file \"/path/to/file\"            fill and accept an open file dialog
notifications [close]                 list recent desktop notifications
do \"click e1; set e2 hi; key enter\" [--dry] [--risk low|medium|high]   many steps, one reply
Any action: --expect \"text\" fails unless the text shows afterwards.
launch app [args] [--front]     focus     close     place x,y,w,h|min|max|restore|left|right|monitor N
apps     screens     cursor     quiet [on|off]     clip     clip set \"text\"     clip clear     status     resume     quit     setup     mcp
Every action replies with what changed, in the picture too. x,y are pixels in the last shot, or text see read. Ctrl+Alt+Esc stops every agent.
Controls go through accessibility (AT-SPI) and background X11 events so the person's focus and pointer stay untouched.
Flags: -w window, --agent name, --fast, --real (bring forward / real XTest input), --mouse (use pointer device), --front (launch in front), --force (allow risky keys).
On Wayland, AT-SPI commands (windows, see, get, set, check, select, pressing controls, scroll bars, tables, notifications) work; screenshots, global keys/pointer and window placement still need X11 (or desktop portals).";

/// What a picture of a window the agent has not read yet says: reading is far cheaper.
const READ_FIRST: &str = "\nnot read yet: see gives this window's controls and its text with x,y to click, with no picture. Shots are for looking at drawings, not for reading text or checking what was typed.";
const IDLE_EXIT: Duration = Duration::from_secs(30 * 60);
const HUNG_EXIT: Duration = Duration::from_secs(60 * 60);
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
const TABLE_CHARS: usize = 6000;
const MAX_TABLE_ROWS: usize = 500;
const MAX_TABLE_COLUMNS: usize = 20;
const MAX_CELL_CHARS: usize = 60;
const DIALOG_CLOSE: Duration = Duration::from_secs(3);
const LAUNCH_WAIT: Duration = Duration::from_secs(10);
const LAUNCH_STEP: Duration = Duration::from_millis(150);
const HOLD_MOST: Duration = Duration::from_secs(30);
const FRONT_SETTLE: Duration = Duration::from_millis(120);
/// How long a window takes to notice it has the keyboard, and to finish with the last key.
const KEYS_SETTLE: Duration = Duration::from_millis(40);
const PASTE_SETTLE: Duration = Duration::from_millis(150);
const MENU_OPEN: Duration = Duration::from_millis(250);
const MENU_DEPTH: usize = 4;
const LEAST_CHANGE: u32 = 4;
const STOP_WATCH: Duration = Duration::from_millis(50);
const STOPPED: &str = "stopped by the person with Ctrl+Alt+Esc: ask them before doing more (deskcrew resume clears it)";
const NO_WINDOW: &str = "no window yet: run deskcrew windows, then deskcrew see <name>";
const MARKED: [&str; 11] = ["button", "edit", "menuitem", "checkbox", "radio", "combobox", "tab", "item", "link", "slider", "spinner"];
// Roles that hold other controls or drawable surfaces, so blind_spot can still inspect/OCR them.
const CONTAINERS: [&str; 4] = ["", "image", "canvas", "document"];

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
        // Controls the app kept claim their numbers first, so a new control with the same role
        // and name never takes the number of one further down the window.
        let mut taken = std::collections::HashSet::new();
        let claimed: Vec<Option<u32>> = nodes.iter().map(|node| self.by_key.get(&node.key).copied().filter(|id| taken.insert(*id))).collect();
        let mut out: BTreeMap<u32, Node> = BTreeMap::new();
        for (node, known) in nodes.into_iter().zip(claimed) {
            let same = self.by_identity.entry(node.identity.clone()).or_default();
            let id = known.or_else(|| same.iter().copied().find(|id| taken.insert(*id))).unwrap_or_else(|| {
                self.next += 1;
                taken.insert(self.next);
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
    quiet: Vec<Bounds>,
    element: Option<Obj>,
    read_once: bool,
    blind: Option<(u64, Vec<String>)>,
    ocr_hits: Vec<(String, Bounds)>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum WindowKey {
    X11(u32),
    AtSpi(Obj),
}

struct WindowInfo {
    key: WindowKey,
    x11: Option<u32>,
    element: Option<Obj>,
    pid: u32,
    app: String,
    title: String,
    rect: Bounds,
}

impl WindowInfo {
    fn from_x(w: Window) -> Self {
        Self { key: WindowKey::X11(w.id), x11: Some(w.id), element: None, pid: w.pid, app: w.app, title: w.title, rect: w.rect }
    }

    fn from_atspi(w: FrameWindow) -> Self {
        Self { key: WindowKey::AtSpi(w.element.clone()), x11: None, element: Some(w.element), pid: w.pid, app: w.app, title: w.title, rect: w.rect }
    }
}

struct Tracked {
    id: u32,
    key: WindowKey,
    x11: Option<u32>,
    view: Mutex<View>,
}

struct LastShot {
    hash: u64,
    frame: Frame,
    frames: Frames,
}

#[derive(Default)]
struct AgentState {
    current: Option<WindowKey>,
    seen: HashMap<WindowKey, Seen>,
    shots: HashMap<WindowKey, LastShot>,
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
    /// From --button; an error when the word was not left, right or middle.
    button: Option<Result<x::Button, ()>>,
    flag_error: Option<String>,
    fill: bool,
    close: bool,
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
    fast: bool,
    real: bool,
    front: bool,
    mouse: bool,
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
        self.button = self.button.or(outer.button);
        self.fill |= outer.fill;
        self.close |= outer.close;
        self.force |= outer.force;
        self.paste |= outer.paste;
        self.smooth |= outer.smooth;
        self.gone |= outer.gone;
        self.with = self.with.or_else(|| outer.with.clone());
        self.ms = self.ms.or(outer.ms);
        self.hold = self.hold.or(outer.hold);
        self.fast |= outer.fast;
        self.real |= outer.real;
        self.front |= outer.front;
        self.mouse |= outer.mouse;
        self
    }

    fn mouse_button(&self) -> Result<x::Button, String> {
        Ok(if let Some(button) = self.button {
            button.map_err(|_| "--button takes left, right or middle".to_string())?
        } else if self.middle {
            x::Button::Middle
        } else if self.right {
            x::Button::Right
        } else if self.back {
            x::Button::Back
        } else if self.forward {
            x::Button::Forward
        } else {
            x::Button::Left
        })
    }

    fn modifiers(&self) -> Result<Vec<u32>, String> {
        match &self.with {
            Some(spec) => match x::chord(spec)? {
                x::Chord { modifiers, key: None } => Ok(modifiers),
                _ => Err("--with takes shift, ctrl or alt, joined with +".into()),
            },
            None => Ok(Vec::new()),
        }
    }
}

#[derive(Default)]
struct Step {
    flags: Flags,
    words: Vec<String>,
}

fn take_text(dst: &mut Option<String>, next: Option<String>, flag: &str, err: &mut Option<String>) {
    match next {
        Some(value) => *dst = Some(value),
        None => {
            err.get_or_insert_with(|| format!("{flag} needs a value"));
        }
    }
}

fn take_num<T: std::str::FromStr>(next: Option<String>, flag: &str, hint: &str, err: &mut Option<String>) -> Option<T> {
    let parsed = next.and_then(|n| n.parse::<T>().ok());
    if parsed.is_none() {
        err.get_or_insert_with(|| format!("{flag} needs {hint}"));
    }
    parsed
}

fn parse(words: &[String]) -> (Flags, Vec<String>) {
    let mut flags = Flags::default();
    let mut rest = Vec::new();
    let mut at = 0;
    while at < words.len() {
        let word = &words[at];
        if word == "--" {
            rest.extend(words[at + 1..].iter().cloned());
            break;
        }
        let next = words.get(at + 1).cloned();
        let mut took = true;
        match word.as_str() {
            "-w" | "--window" => take_text(&mut flags.window, next, word, &mut flags.flag_error),
            "--find" => take_text(&mut flags.find, next, word, &mut flags.flag_error),
            "--region" => take_text(&mut flags.region, next, word, &mut flags.flag_error),
            "--expect" => take_text(&mut flags.expect, next, word, &mut flags.flag_error),
            "--with" => take_text(&mut flags.with, next, word, &mut flags.flag_error),
            "--button" => {
                match next.as_deref().and_then(x::Button::named) {
                    Some(button @ (x::Button::Left | x::Button::Right | x::Button::Middle)) => flags.button = Some(Ok(button)),
                    _ => {
                        flags.button = Some(Err(()));
                        flags.flag_error.get_or_insert_with(|| "--button takes left, right or middle".into());
                    }
                }
            }
            "--risk" => match next.as_deref().map(str::to_ascii_lowercase).as_deref() {
                Some("low" | "medium" | "high") => flags.risk = next.map(|v| v.to_ascii_lowercase()),
                _ => {
                    flags.flag_error.get_or_insert_with(|| "--risk takes low, medium or high".into());
                }
            },
            "--from" => flags.from = take_num::<u64>(next, word, "a number", &mut flags.flag_error).unwrap_or(0).min(usize::MAX as u64) as usize,
            "--edge" => flags.edge = take_num::<u64>(next, word, "pixels", &mut flags.flag_error).map(|n| n.min(u32::MAX as u64) as u32),
            "--ms" | "--timeout" => flags.ms = take_num::<u64>(next, word, "milliseconds", &mut flags.flag_error),
            "--hold" => flags.hold = take_num::<u64>(next, word, "milliseconds", &mut flags.flag_error),
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
            "--fill" => flags.fill = true,
            "--close" => flags.close = true,
            "--force" => flags.force = true,
            "--paste" => flags.paste = true,
            "--smooth" => flags.smooth = true,
            "--gone" => flags.gone = true,
            "--dry" => flags.dry = true,
            "--marks" => flags.marks = true,
            "--rulers" => flags.rulers = true,
            "--changed" => flags.changed = true,
            "--screen" => flags.screen = true,
            "--fast" => flags.fast = true,
            "--real" => flags.real = true,
            "--front" => flags.front = true,
            "--mouse" => flags.mouse = true,
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
    (rect.0.saturating_add(rect.2 / 2), rect.1.saturating_add(rect.3 / 2))
}

enum Target {
    Control(u32),
    Point(i32, i32),
}

/// Modifier keys and mouse buttons one agent holds down.
struct Held {
    keys: Vec<u32>,
    buttons: Vec<(u32, Button, i32, i32)>,
    since: Instant,
}

struct Service {
    held: Mutex<HashMap<String, Held>>,
    real_input: Mutex<()>,
    windows: Mutex<(u32, HashMap<WindowKey, Arc<Tracked>>)>,
    agents: Mutex<HashMap<String, AgentState>>,
    last_used: Mutex<Instant>,
    active: AtomicUsize,
    quiet: AtomicBool,
    quit: AtomicBool,
    unlinked: AtomicBool,
    wake_lock: Mutex<()>,
    wake: Condvar,
    overlay: Overlay,
}

fn env_quiet() -> bool {
    std::env::var("DESKCREW_QUIET")
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "on" | "yes"))
        .unwrap_or(false)
}

fn quiet_blocked(verb: &str, flags: &Flags, steps: Option<&[Step]>) -> Option<&'static str> {
    if verb == "focus" {
        return Some("refused: quiet mode is on (focus is disabled; run 'deskcrew quiet off' to allow)");
    }
    if verb == "down" {
        return Some("refused: quiet mode is on (down borrows real input; run 'deskcrew quiet off' to allow)");
    }
    if flags.real || steps.is_some_and(|s| s.iter().any(|st| st.flags.real)) {
        return Some("refused: quiet mode is on (--real is disabled; run 'deskcrew quiet off' to allow)");
    }
    if flags.mouse || steps.is_some_and(|s| s.iter().any(|st| st.flags.mouse)) {
        return Some("refused: quiet mode is on (--mouse is disabled; run 'deskcrew quiet off' to allow)");
    }
    if verb == "launch" && flags.front {
        return Some("refused: quiet mode is on (launch --front is disabled; run 'deskcrew quiet off' to allow)");
    }
    None
}

struct ActiveGuard<'a> {
    active: &'a AtomicUsize,
    last_used: &'a Mutex<Instant>,
    wake: &'a Condvar,
}

impl<'a> ActiveGuard<'a> {
    fn enter(active: &'a AtomicUsize, last_used: &'a Mutex<Instant>, wake: &'a Condvar) -> Self {
        active.fetch_add(1, Ordering::SeqCst);
        *last_used.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
        Self { active, last_used, wake }
    }
}

impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        *self.last_used.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
        if self.active.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.wake.notify_all();
        }
    }
}

impl Service {
    /// Glide (or jump when `--fast`) this agent's drawn cursor to a spot, keep it above the
    /// agent's window, and light up the control there when it is pressed.
    fn cursor_on(&self, who: &str, flags: &Flags, tracked: &Tracked, rect: Bounds, press: bool) {
        let Some(xid) = tracked.x11 else { return };
        let (px, py) = centre(rect);
        self.overlay.over(who, xid);
        if flags.fast {
            self.overlay.jump(who, px, py);
        } else {
            self.overlay.move_to(who, px, py);
        }
        if press && rect.2 > 0 && rect.3 > 0 {
            self.overlay.press(who, rect);
        }
    }

    /// Ensure a short background action never leaves the target window focused over the person's
    /// window. Skips restoration if the person moved the pointer during the action so intentional
    /// window switches are never undone.
    fn keep_behind<T>(&self, tracked: &Tracked, flags: &Flags, work: impl FnOnce() -> T) -> T {
        let before = (!flags.real && !flags.mouse).then(x::front).flatten();
        let ptr_before = before.and_then(|_| x::pointer());
        let started = Instant::now();
        let result = work();
        if let (Some(xid), Some(prev)) = (tracked.x11, before)
            && prev != xid
            && x::front() == Some(xid)
            && started.elapsed() < Duration::from_millis(800)
            && x::pointer() == ptr_before
        {
            x::lower_behind(xid, prev);
            let _ = x::activate(prev);
        }
        result
    }

    fn track(&self, window: &WindowInfo) -> Arc<Tracked> {
        let mut windows = self.windows.lock().unwrap();
        let (next, known) = &mut *windows;
        known
            .entry(window.key.clone())
            .or_insert_with(|| {
                *next += 1;
                let view = View { element: window.element.clone(), ..View::default() };
                Arc::new(Tracked { id: *next, key: window.key.clone(), x11: window.x11, view: Mutex::new(view) })
            })
            .clone()
    }

    fn by_id(&self, id: u32) -> Option<Arc<Tracked>> {
        self.windows.lock().unwrap().1.values().find(|t| t.id == id).cloned()
    }

    fn set_current(&self, who: &str, window: WindowKey) {
        self.agents.lock().unwrap().entry(who.to_string()).or_default().current = Some(window);
    }

    fn list_windows(&self) -> Result<Vec<WindowInfo>, String> {
        if !x::has_display() {
            return Ok(atspi::windows()?.into_iter().map(WindowInfo::from_atspi).collect());
        }
        x::ready()?;
        let mut out: Vec<WindowInfo> = x::list().into_iter().map(WindowInfo::from_x).collect();
        if !x::is_xwayland() {
            return Ok(out);
        }
        // On Wayland with XWayland, native Wayland windows only appear on AT-SPI.
        if let Ok(frames) = atspi::windows() {
            let mut matched = vec![false; frames.len()];
            for xw in &mut out {
                let mut candidates: Vec<usize> = frames
                    .iter()
                    .enumerate()
                    .filter(|(i, f)| !matched[*i] && ((xw.pid > 0 && f.pid == xw.pid) || xw.pid == 0))
                    .map(|(i, _)| i)
                    .collect();
                if candidates.is_empty() {
                    continue;
                }
                let pick = candidates
                    .iter()
                    .copied()
                    .find(|&i| !xw.title.is_empty() && frames[i].title == xw.title)
                    .or_else(|| {
                        candidates.iter().copied().find(|&i| {
                            let r = frames[i].rect;
                            r.2 > 0 && r.0.abs_diff(xw.rect.0) < 60 && r.1.abs_diff(xw.rect.1) < 60 && r.2.abs_diff(xw.rect.2) < 60
                        })
                    })
                    .or_else(|| (xw.pid > 0 && candidates.len() == 1).then(|| candidates.remove(0)));
                if let Some(idx) = pick {
                    matched[idx] = true;
                    xw.element = Some(frames[idx].element.clone());
                }
            }
            for (i, fw) in frames.into_iter().enumerate() {
                if !matched[i] {
                    out.push(WindowInfo::from_atspi(fw));
                }
            }
        }
        Ok(out)
    }

    fn pick(&self, who: &str, spec: Option<&str>) -> Result<Arc<Tracked>, String> {
        let Some(spec) = spec else {
            let current = self.agents.lock().unwrap().get(who).and_then(|a| a.current.clone()).ok_or(NO_WINDOW)?;
            let tracked = self.windows.lock().unwrap().1.get(&current).cloned().ok_or(NO_WINDOW)?;
            self.window_of(&tracked)?;
            return Ok(tracked);
        };
        if let Some(id) = spec.strip_prefix('w').and_then(|n| n.parse::<u32>().ok()) {
            let tracked = self.by_id(id).ok_or(format!("no window w{id}: run deskcrew windows"))?;
            self.window_of(&tracked)?;
            self.set_current(who, tracked.key.clone());
            return Ok(tracked);
        }
        let wanted = spec.to_lowercase();
        let listed = self.list_windows()?;
        let exact: Vec<&WindowInfo> = listed.iter().filter(|w| w.title.to_lowercase() == wanted || w.app.to_lowercase() == wanted).collect();
        let mut matches: Vec<&WindowInfo> = if exact.is_empty() {
            listed.iter().filter(|w| w.title.to_lowercase().contains(&wanted) || w.app.to_lowercase().contains(&wanted)).collect()
        } else {
            exact
        };
        if matches.len() > 1 {
            let current = self.agents.lock().unwrap().get(who).and_then(|a| a.current.clone());
            if let Some(mine) = matches.iter().find(|w| current.as_ref() == Some(&w.key)) {
                matches = vec![*mine];
            } else if matches.iter().filter(|w| !w.title.is_empty()).count() == 1 {
                matches.retain(|w| !w.title.is_empty());
            }
        }
        match matches.as_slice() {
            [] => Err(format!("no window matches '{spec}'")),
            [one] => {
                let tracked = self.track(one);
                self.set_current(who, one.key.clone());
                Ok(tracked)
            }
            many => {
                let mut out = vec![format!("'{spec}' matches {} windows:", many.len())];
                out.extend(many.iter().take(8).map(|w| format!("w{} \"{}\" {}", self.track(w).id, lines::clip(&w.title, 50), w.app)));
                Err(out.join("\n"))
            }
        }
    }

    fn window_of(&self, tracked: &Tracked) -> Result<WindowInfo, String> {
        match &tracked.key {
            WindowKey::X11(id) => x::find(*id).map(WindowInfo::from_x).ok_or_else(|| "that window closed: run deskcrew windows".to_string()),
            WindowKey::AtSpi(obj) => atspi::frame_info(obj).map(WindowInfo::from_atspi).ok_or_else(|| "that window closed: run deskcrew windows".to_string()),
        }
    }

    fn read(&self, tracked: &Tracked, view: &mut View, all: bool) -> Result<(), String> {
        let window = self.window_of(tracked)?;
        view.ocr_hits.clear();
        let bus_result = atspi::bus();
        if let Err(err) = bus_result {
            if tracked.x11.is_some() {
                view.element = None;
                view.nodes.clear();
                view.lines.clear();
                view.hidden = 0;
                view.quiet.clear();
                view.read_once = true;
                return Ok(());
            }
            return Err(err);
        }
        if let WindowKey::AtSpi(obj) = &tracked.key {
            if view.element.is_none() {
                view.element = Some(obj.clone());
            }
        } else {
            // A window's accessible object can change when the app rebuilds it: find it again then.
            let stale = view.element.as_ref().is_none_or(|e| atspi::role_name(e).is_empty());
            if stale {
                view.element = atspi::frame_for(window.pid, &window.title, window.rect);
            }
        }
        let Some(element) = view.element.as_ref() else {
            // X11 windows without an AT-SPI tree (SDL, Wine, Tk, FLTK, OpenGL/canvas apps, or
            // Qt/Chrome started without accessibility flags) still work via OCR + x,y + XSendEvent.
            if tracked.x11.is_some() {
                view.nodes.clear();
                view.lines.clear();
                view.hidden = 0;
                view.quiet.clear();
                view.read_once = true;
                return Ok(());
            }
            return Err(
                "that window does not answer accessibility requests: start the app with accessibility on (GTK and Qt apps are, once the AT-SPI bus runs; for Qt set QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1, for Chrome --force-renderer-accessibility)".into(),
            );
        };
        let mut surfaces = vec![element.clone()];
        surfaces.extend(atspi::popup_frames(window.pid, element));
        let (nodes, hidden, quiet) = atspi::read_surfaces(&surfaces, window.rect, all);
        view.nodes = view.refs.assign(nodes);
        view.lines = view.nodes.iter().map(|(id, node)| (*id, lines::line(&node.parts))).collect();
        view.hidden = hidden;
        view.quiet = quiet;
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

    fn seen(&self, who: &str, window: &WindowKey) -> Option<Seen> {
        self.agents.lock().unwrap().get(who).and_then(|a| a.seen.get(window).cloned())
    }

    fn remember(&self, who: &str, window: WindowKey, lines: Seen) {
        self.agents.lock().unwrap().entry(who.to_string()).or_default().seen.insert(window, lines);
    }

    fn resolve(&self, word: &str, who: &str, tracked: &Tracked, view: &mut View) -> Result<Target, String> {
        self.resolve_for(word, who, tracked, view, &[])
    }

    /// Resolve a target, preferring controls whose roles appear in `preferred` when a name matches
    /// more than one control (e.g. a label + edit box both named "Search").
    fn resolve_for(&self, word: &str, who: &str, tracked: &Tracked, view: &mut View, preferred: &[&str]) -> Result<Target, String> {
        if let Some((px, py)) = is_point(word) {
            let frames = self.agents.lock().unwrap().get(who).and_then(|a| a.shots.get(&tracked.key).map(|s| s.frames));
            let frames = frames.ok_or("x,y are pixels in a shot: take one first")?;
            let (px, py) = frames.to_screen(px, py);
            let (left, top, w, h) = self.window_of(tracked)?.rect;
            if w > 0 && h > 0 && (px < left || py < top || px >= left + w || py >= top + h) {
                return Err("refused: that point is not on the window".into());
            }
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
                    if !preferred.is_empty() {
                        let fitted: Vec<u32> = many.iter().copied().filter(|id| preferred.contains(&view.nodes[id].parts.role.as_str())).collect();
                        if let [one] = fitted.as_slice() {
                            return Ok(Target::Control(*one));
                        }
                    }
                    let acting: Vec<u32> = many.iter().copied().filter(|id| MARKED.contains(&view.nodes[id].parts.role.as_str())).collect();
                    if let [one] = acting.as_slice() {
                        return Ok(Target::Control(*one));
                    }
                    // If one matched control encloses all other matched controls, pick the innermost acting one.
                    let innermost: Vec<u32> = acting
                        .iter()
                        .copied()
                        .filter(|id| !acting.iter().any(|other| other != id && atspi::encloses(view.nodes[id].rect, view.nodes[other].rect)))
                        .collect();
                    if let [one] = innermost.as_slice() {
                        return Ok(Target::Control(*one));
                    }
                    let shown: Vec<String> = many.iter().take(6).map(|id| format!("e{id} {}", view.lines[id])).collect();
                    return Err(format!("'{word}' matches {} controls:\n{}", many.len(), shown.join("\n")));
                }
            }
        }
        // On X11 apps with no AT-SPI match (SDL, Wine, Tk, FLTK, OpenGL/canvas), resolve visible
        // labels via OCR so `click "Play"` / `hover "Settings"` work by name in the background.
        if let Some(target) = self.ocr_resolve(word, tracked, view)? {
            return Ok(target);
        }
        if view.element.is_none() && tracked.x11.is_some() {
            return Err(format!(
                "no control or OCR text named '{word}': this window exposes no AT-SPI controls (use x,y from see/shot, install tesseract for OCR name lookup, or start with accessibility enabled)"
            ));
        }
        Err(format!("no control named '{word}': see"))
    }

    /// Look up `word` among OCR text lines captured from an X11 window (populating `view.ocr_hits`
    /// on demand, or refreshing once on a miss) and return its screen centre as `Target::Point`.
    fn ocr_resolve(&self, word: &str, tracked: &Tracked, view: &mut View) -> Result<Option<Target>, String> {
        let Some(xid) = tracked.x11 else { return Ok(None) };
        let window = self.window_of(tracked)?;
        let wanted = word.to_lowercase();
        let mut refreshed = false;
        loop {
            if view.ocr_hits.is_empty() && !refreshed {
                refreshed = true;
                if let Ok((frame, _)) = x::window_picture(xid)
                    && let Ok(lines) = look::read_text(&frame)
                {
                    view.ocr_hits = lines
                        .into_iter()
                        .map(|l| (l.text, (window.rect.0 + l.rect.0, window.rect.1 + l.rect.1, l.rect.2, l.rect.3)))
                        .collect();
                }
            }
            let exact: Vec<&(String, Bounds)> = view.ocr_hits.iter().filter(|(t, _)| t.trim().to_lowercase() == wanted).collect();
            let hits = if exact.is_empty() {
                view.ocr_hits.iter().filter(|(t, _)| t.to_lowercase().contains(&wanted)).collect()
            } else {
                exact
            };
            match hits.as_slice() {
                [] if !refreshed => {
                    view.ocr_hits.clear();
                    continue;
                }
                [] => return Ok(None),
                [(_, rect)] => return Ok(Some(Target::Point(centre(*rect).0, centre(*rect).1))),
                many => {
                    let shown: Vec<String> = many
                        .iter()
                        .take(6)
                        .map(|(t, r)| {
                            let (cx, cy) = centre(*r);
                            format!("\"{}\" {},{}", lines::clip(t, 40), cx - window.rect.0, cy - window.rect.1)
                        })
                        .collect();
                    return Err(format!("'{word}' matches {} OCR labels:\n{}", many.len(), shown.join("\n")));
                }
            }
        }
    }

    fn point_of(&self, word: &str, who: &str, tracked: &Tracked, view: &mut View) -> Result<(i32, i32), String> {
        match self.resolve(word, who, tracked, view)? {
            Target::Point(px, py) => Ok((px, py)),
            Target::Control(id) => Ok(centre(atspi::rect(&view.nodes[&id].element).unwrap_or(view.nodes[&id].rect))),
        }
    }

    /// A control's box now, and its object, for acting on it.
    fn control(&self, word: &str, who: &str, tracked: &Tracked, view: &mut View) -> Result<(u32, Bounds, Obj), String> {
        self.control_for(word, who, tracked, view, &[])
    }

    fn control_for(&self, word: &str, who: &str, tracked: &Tracked, view: &mut View, preferred: &[&str]) -> Result<(u32, Bounds, Obj), String> {
        match self.resolve_for(word, who, tracked, view, preferred)? {
            Target::Control(id) => {
                let node = &view.nodes[&id];
                Ok((id, atspi::rect(&node.element).unwrap_or(node.rect), node.element.clone()))
            }
            Target::Point(..) => Err("that needs a control".into()),
        }
    }

    /// Give the window the keyboard for `work` without raising it, then give it back: the window in
    /// front stays in front, and apps that ignore keys while in the back, as Chrome does, still take
    /// them. Agents take turns here: the display has one keyboard.
    fn keyboard<T>(&self, tracked: &Tracked, work: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        let Some(xid) = tracked.x11 else {
            return Err("sending keys on Wayland needs X11 (or the RemoteDesktop portal)".into());
        };
        let _turn = self.real_input.lock().unwrap();
        let before = x::focus_keys(xid)?;
        thread::sleep(KEYS_SETTLE);
        let result = work();
        thread::sleep(KEYS_SETTLE);
        if before != xid {
            x::restore_keys(before);
        }
        result
    }

    /// Bring the window forward for `work` with the X pointer or keyboard, then give the front
    /// and the pointer back. Agents take turns here: the display has one of each.
    fn front<T>(&self, tracked: &Tracked, work: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        let Some(xid) = tracked.x11 else {
            return Err("raising windows and global pointer input on Wayland need X11 (or the RemoteDesktop portal)".into());
        };
        let _turn = self.real_input.lock().unwrap();
        let before = x::front();
        let pointer = x::pointer();
        if before != Some(xid) {
            x::activate(xid)?;
            thread::sleep(FRONT_SETTLE);
        }
        let result = work();
        thread::sleep(FRONT_SETTLE);
        if let Some(window) = before
            && window != xid
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

    fn all_held_keys(&self) -> Vec<u32> {
        let mut keys = Vec::new();
        for h in self.held.lock().unwrap().values() {
            for &k in &h.keys {
                if !keys.contains(&k) {
                    keys.push(k);
                }
            }
        }
        keys
    }

    /// Let go of held keys and mouse buttons: everyone's, or only those held too long or since Ctrl+Alt+Esc.
    fn release(&self, all: bool) -> Vec<String> {
        let stopped = STOP.load(Ordering::Relaxed);
        let mut held = self.held.lock().unwrap();
        let mut gone = Vec::new();
        held.retain(|who, keys| {
            let keep = !all && !stopped && keys.since.elapsed() < HOLD_MOST;
            if !keep {
                for &(xid, btn, px, py) in keys.buttons.iter().rev() {
                    let _ = x::send_button_at(xid, px, py, btn, false, &keys.keys);
                }
                if !keys.keys.is_empty() {
                    let _ = x::modifiers(&keys.keys, false);
                }
                gone.push(who.clone());
            }
            keep
        });
        gone
    }

    fn click(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, word: &str) -> Reply {
        let button = flags.mouse_button()?;
        let count = if flags.triple { 3 } else if flags.double { 2 } else { 1 };
        let held = self.held_keys(who);
        // Only the --with keys go down and up for this click: keys held with down stay down.
        let own: Vec<u32> = match &flags.with {
            Some(spec) => x::chord(spec)?.modifiers.into_iter().filter(|m| !held.contains(m)).collect(),
            None => Vec::new(),
        };
        let with: Vec<u32> = own.iter().chain(&held).copied().collect();
        let (rect, element) = match self.resolve(word, who, tracked, view)? {
            Target::Point(px, py) => ((px - 6, py - 6, 12, 12), None),
            Target::Control(id) => {
                let node = &view.nodes[&id];
                (atspi::rect(&node.element).unwrap_or(node.rect), Some(node.element.clone()))
            }
        };
        self.cursor_on(who, flags, tracked, rect, element.is_some());
        let plain = with.is_empty() && count == 1 && flags.hold.is_none() && button == Button::Left && !flags.real && !flags.mouse;
        // Only grab focus on an editable field if Wayland-only or the window is already focused;
        // on X11, GrabFocus on a background window can raise it or steal the person's focus.
        if let Some(element) = &element
            && plain
            && atspi::editable(element)
            && (tracked.x11.is_none() || x::front() == tracked.x11)
            && atspi::focus(element).is_ok()
        {
            return Ok("focus".into());
        }
        // An action the app performs itself works behind other windows and takes no focus.
        let mut atspi_error = None;
        if let Some(element) = &element
            && plain
        {
            match self.keep_behind(tracked, flags, || atspi::press(element)) {
                Ok(action) => return Ok(action),
                Err(error) => atspi_error = Some(error),
            }
        }
        // A click at x,y lands on whatever is drawn there: a button, check box, tab, link or list
        // item there is pressed by its own action, which needs neither the pointer nor the front.
        if element.is_none() && plain {
            let (px, py) = centre(rect);
            self.read(tracked, view, false)?;
            let inside = |&(left, top, width, height): &Bounds| width > 0 && height > 0 && px >= left && px < left + width && py >= top && py < top + height;
            let under = view
                .nodes
                .values()
                .filter(|node| inside(&node.rect) && matches!(node.parts.role.as_str(), "button" | "checkbox" | "radio" | "menuitem" | "tab" | "item" | "link"))
                .min_by_key(|node| node.rect.2 as i64 * node.rect.3 as i64);
            if let Some(node) = under
                && let Ok(action) = self.keep_behind(tracked, flags, || atspi::press(&node.element))
            {
                return Ok(format!("{action} on '{}' under {word}", node.parts.name));
            }
        }
        let (px, py) = centre(rect);
        let hold = Duration::from_millis(flags.hold.unwrap_or(0).min(HOLD_MOST.as_millis() as u64));
        let Some(xid) = tracked.x11 else {
            if self.quiet.load(Ordering::Relaxed) {
                return Err("refused: quiet mode is on (Wayland portal input uses the shared desktop seat; run 'deskcrew quiet off' to allow)".into());
            }
            return match portal::click(px, py, button, count, hold, &with) {
                Ok(()) => Ok("portal".into()),
                Err(portal_err) => Err(match atspi_error {
                    Some(reason) => format!("{reason}; Wayland portal fallback failed ({portal_err})"),
                    None => format!("Wayland portal click failed ({portal_err})"),
                }),
            };
        };
        // When uncovered on X11, prefer deskcrew's dedicated XInput 2 agent pointer (`By::Agent`),
        // which works on GTK/Qt/Chromium/Firefox without raising the window or moving the person's
        // cursor. Fall back to background XSendEvent when covered or in quiet mode.
        if !flags.real && !flags.mouse {
            if !self.quiet.load(Ordering::Relaxed) && self.pointer_for(&with) == By::Agent {
                let _turn = self.real_input.lock().unwrap();
                if x::shows_at(xid, px, py) {
                    x::click(By::Agent, px, py, button, count, hold)?;
                    return Ok("own pointer".into());
                }
            }
            self.keep_behind(tracked, flags, || x::send_click(xid, px, py, button, count, hold, &with))?;
            return Ok("event".into());
        }
        let by = if flags.mouse { By::Person } else { self.pointer_for(&with) };
        self.with_pointer(by, tracked, (px, py), || {
            x::modifiers(&own, true)?;
            let clicked = x::click(by, px, py, button, count, hold);
            x::modifiers(&own, false)?;
            clicked
        })
    }

    /// The agents' own pointer where there is one and no keys are held with it.
    fn pointer_for(&self, held: &[u32]) -> By {
        if held.is_empty() && x::own_pointer() { By::Agent } else { By::Person }
    }

    /// Run pointer work: with the agents' own pointer where the window shows at the point, with
    /// nothing brought forward and the person's pointer untouched; otherwise with the window
    /// brought forward for the moment. Agents take turns on a pointer.
    fn with_pointer(&self, by: By, tracked: &Tracked, (px, py): (i32, i32), work: impl FnOnce() -> Result<(), String>) -> Reply {
        let Some(xid) = tracked.x11 else {
            return Err("pointer input on Wayland needs X11 (or the RemoteDesktop portal)".into());
        };
        if by == By::Agent {
            let _turn = self.real_input.lock().unwrap();
            if x::shows_at(xid, px, py) {
                work()?;
                return Ok("own pointer".into());
            }
        }
        self.front(tracked, || {
            if !x::shows_at(xid, px, py) {
                return Err(format!("refused: {px},{py} on screen is not on this window, so a click there would hit something else"));
            }
            work()
        })?;
        Ok(if by == By::Agent { "own pointer, window brought forward" } else { "pointer" }.into())
    }

    fn hover(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, word: &str) -> Reply {
        let (px, py) = self.point_of(word, who, tracked, view)?;
        self.cursor_on(who, flags, tracked, (px, py, 0, 0), false);
        let Some(xid) = tracked.x11 else {
            if self.quiet.load(Ordering::Relaxed) {
                return Err("refused: quiet mode is on (Wayland portal hover moves the real pointer; run 'deskcrew quiet off' to allow)".into());
            }
            portal::hover(px, py)?;
            return Ok("portal".into());
        };
        if !flags.real && !flags.mouse {
            if !self.quiet.load(Ordering::Relaxed) && self.pointer_for(&[]) == By::Agent {
                let _turn = self.real_input.lock().unwrap();
                if x::shows_at(xid, px, py) {
                    x::move_by(By::Agent, px, py)?;
                    return Ok("own pointer".into());
                }
            }
            self.keep_behind(tracked, flags, || x::send_hover(xid, px, py))?;
            return Ok("event".into());
        }
        let by = if flags.mouse { By::Person } else { self.pointer_for(&[]) };
        self.with_pointer(by, tracked, (px, py), || x::move_by(by, px, py))
    }

    fn set(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, word: &str, value: &str) -> Reply {
        let (id, rect, element) = self.control_for(word, who, tracked, view, &["edit", "document", "slider", "spinner", "combobox"])?;
        self.cursor_on(who, flags, tracked, rect, true);
        self.overlay.typing(who, value.chars().count());
        if atspi::editable(&element)
            && self.keep_behind(tracked, flags, || atspi::set_text(&element, value)).is_ok()
        {
            return Ok("text".into());
        }
        if atspi::has_value(&element)
            && let Ok(number) = value.trim().parse::<f64>()
        {
            self.keep_behind(tracked, flags, || atspi::set_value(&element, number))?;
            return Ok("value".into());
        }
        let editable_role = matches!(view.nodes[&id].parts.role.as_str(), "edit" | "document" | "combobox" | "spinner");
        if editable_role
            && let Some(xid) = tracked.x11
            && !flags.real
        {
            let spot = centre(rect);
            let stop = || STOP.load(Ordering::Relaxed);
            self.keep_behind(tracked, flags, || -> Result<(), String> {
                x::send_click(xid, spot.0, spot.1, Button::Left, 1, Duration::ZERO, &[])?;
                thread::sleep(KEYS_SETTLE);
                x::send_key_at(xid, Some(spot), &x::chord("ctrl+a")?, Duration::ZERO)?;
                x::send_key_at(xid, Some(spot), &x::chord("backspace")?, Duration::ZERO)?;
                if !value.is_empty() {
                    let typed = x::send_text_at(xid, Some(spot), value, &stop)?;
                    if typed < value.chars().count() {
                        return Err(format!("{STOPPED} (typed {typed} of {} characters)", value.chars().count()));
                    }
                }
                Ok(())
            })?;
            return Ok("event".into());
        }
        Err("that control's text can't be set: try type".into())
    }

    /// Leave a checkbox or switch alone if it is already that way, or press it once.
    fn ensure(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, word: &str, wanted: Option<&'static str>) -> Reply {
        let (id, rect, element) = self.control_for(word, who, tracked, view, &["checkbox", "radio", "button"])?;
        self.cursor_on(who, flags, tracked, rect, true);
        if let Some(wanted) = wanted {
            let role = &view.nodes[&id].parts.role;
            if atspi::toggle_state(&element, role)? == Some(wanted) {
                return Ok(format!("already {wanted}"));
            }
        }
        self.keep_behind(tracked, flags, || atspi::perform(&element, &["toggle", "click", "press", "activate", "*"]))
    }

    fn expand(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, word: &str, open: bool) -> Reply {
        let (id, rect, element) = self.control_for(word, who, tracked, view, &["combobox", "button", "item"])?;
        self.cursor_on(who, flags, tracked, rect, true);
        let wanted = if open { "open" } else { "closed" };
        let role = &view.nodes[&id].parts.role;
        if atspi::expand_state(&element, role)? == Some(wanted) {
            return Ok(format!("already {wanted}"));
        }
        self.keep_behind(tracked, flags, || {
            atspi::perform(&element, &["expand or contract", if open { "expand" } else { "collapse" }, "press", "click", "activate"])
        })
        .map_err(|_| format!("that control cannot be {}", if open { "expanded" } else { "collapsed" }))
    }

    fn show(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, word: &str) -> Reply {
        let (_, rect, element) = self.control(word, who, tracked, view)?;
        self.cursor_on(who, flags, tracked, rect, false);
        self.keep_behind(tracked, flags, || atspi::show(&element))?;
        Ok("scroll".into())
    }

    /// Pick an entry from a combo box or a list.
    fn select(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, word: &str, item: &str) -> Reply {
        let (id, rect, element) = self.control_for(word, who, tracked, view, &["combobox", "list", "tree", "edit"])?;
        self.cursor_on(who, flags, tracked, rect, true);
        if view.nodes[&id].parts.role == "edit" && atspi::editable(&element) {
            self.keep_behind(tracked, flags, || atspi::set_text(&element, item))?;
            return Ok("text".into());
        }
        let combo = view.nodes[&id].parts.role == "combobox";
        if combo && self.keep_behind(tracked, flags, || atspi::choose(&element, item).is_ok()) {
            return Ok("select".into());
        }
        if !combo
            && let Some(entry) = atspi::find_named(&element, item, MENU_DEPTH)
            && self.keep_behind(tracked, flags, || atspi::select_child(&element, &entry).is_ok() || atspi::press(&entry).is_ok())
        {
            return Ok("select".into());
        }
        // A combo box may build its list only once it opens.
        if combo {
            self.keep_behind(tracked, flags, || atspi::press(&element)).map_err(|_| "that dropdown did not answer".to_string())?;
            thread::sleep(MENU_OPEN);
            if let Some(entry) = atspi::find_named(&element, item, MENU_DEPTH)
                && self.keep_behind(tracked, flags, || atspi::press(&entry).is_ok())
            {
                thread::sleep(MENU_OPEN);
                let chosen = atspi::selected_name(&element)
                    .or_else(|| atspi::text_of(&element).filter(|t| !t.is_empty()))
                    .unwrap_or_else(|| atspi::name(&element));
                if chosen.eq_ignore_ascii_case(item) {
                    return Ok("select".into());
                }
            }
            if let Some(xid) = tracked.x11 {
                if flags.real {
                    let _ = self.keyboard(tracked, || x::press(&x::chord("esc")?, Duration::ZERO));
                } else {
                    let _ = x::chord("esc").and_then(|esc| x::send_key(xid, &esc, Duration::ZERO));
                }
            } else {
                let _ = atspi::press(&element);
            }
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
                let (_, rect, element) = self.control_for(word, who, tracked, view, &["edit", "document", "combobox"])?;
                self.cursor_on(who, flags, tracked, rect, true);
                Some((rect, element))
            }
            None => None,
        };
        self.overlay.typing(who, text.chars().count());
        // The agent's cursor turns into a caret where the text goes in.
        let caret_at = |field: &Obj| {
            if tracked.x11.is_some()
                && let Some((cx, cy, h)) = atspi::caret(field)
            {
                self.overlay.caret(who, cx, cy, h);
            }
        };
        if let Some((_, field)) = &field {
            caret_at(field);
        }
        let focused_el = field
            .as_ref()
            .map(|(_, el)| el.clone())
            .or_else(|| view.nodes.values().find(|n| n.parts.states.contains(&"focused")).map(|n| n.element.clone()))
            .or_else(|| view.nodes.values().find(|n| matches!(n.parts.role.as_str(), "edit" | "document") && atspi::focused(&n.element)).map(|n| n.element.clone()));
        if !flags.real
            && let Some((_, field)) = &field
            && atspi::editable(field)
            && self.keep_behind(tracked, flags, || atspi::append_text(field, text).is_ok())
        {
            caret_at(field);
            return Ok("text".into());
        }
        let Some(xid) = tracked.x11 else {
            // Pure Wayland: insert directly through AT-SPI EditableText when a focused editable
            // control exists, avoiding the global Wayland portal seat altogether.
            if !flags.real
                && let Some(ref el) = focused_el
                && atspi::editable(el)
                && atspi::insert_at_caret(el, text).is_ok()
            {
                return Ok("text".into());
            }
            if let Some((_, field)) = &field {
                let _ = atspi::focus(field);
            }
            if self.quiet.load(Ordering::Relaxed) {
                return Err("refused: quiet mode is on (Wayland portal typing uses the focused window; run 'deskcrew quiet off' to allow)".into());
            }
            let stop = || STOP.load(Ordering::Relaxed);
            let typed = portal::type_text(text, &stop)?;
            if typed < text.chars().count() {
                return Err(format!("{STOPPED} (typed {typed} of {} characters)", text.chars().count()));
            }
            return Ok("portal".into());
        };
        let spot = field.as_ref().map(|(rect, _)| centre(*rect));
        // In quiet mode (where even XSetInputFocus without raising is avoided), use XSendEvent +
        // AT-SPI EditableText fallback. Otherwise use `keyboard()` (XSetInputFocus without raising,
        // which reaches Chromium, Electron, GTK, Qt, and Firefox reliably in the background).
        if !flags.real && self.quiet.load(Ordering::Relaxed) {
            if let Some((px, py)) = spot {
                let _ = self.keep_behind(tracked, flags, || x::send_click(xid, px, py, Button::Left, 1, Duration::ZERO, &[]));
                thread::sleep(KEYS_SETTLE);
            }
            if flags.paste {
                let before_text = focused_el.as_ref().and_then(atspi::text_of);
                let _turn = self.real_input.lock().unwrap();
                let mut saved = x::SavedClipboard::take();
                if let Err(error) = saved.lend(text) {
                    let _ = saved.put_back();
                    return Err(error);
                }
                thread::sleep(PASTE_SETTLE);
                let sent = self.keep_behind(tracked, flags, || x::chord("ctrl+v").and_then(|paste| x::send_key_at(xid, spot, &paste, Duration::ZERO)));
                thread::sleep(PASTE_SETTLE);
                saved.put_back()?;
                sent?;
                // If an app (e.g. Chromium/Electron/GTK4) ignored XSendEvent Ctrl+V in the
                // background, insert directly at the caret via AT-SPI EditableText.
                if let (Some(el), Some(before)) = (&focused_el, &before_text)
                    && atspi::text_of(el).as_deref() == Some(before.as_str())
                    && atspi::editable(el)
                    && self.keep_behind(tracked, flags, || atspi::insert_at_caret(el, text).is_ok())
                {
                    caret_at(el);
                    return Ok("text".into());
                }
                return Ok("paste".into());
            }
            let stop = || STOP.load(Ordering::Relaxed);
            let typed = self.keep_behind(tracked, flags, || x::send_text_at(xid, spot, text, &stop))?;
            if let Some((_, field)) = &field {
                caret_at(field);
            }
            if typed < text.chars().count() {
                return Err(format!("{STOPPED} (typed {typed} of {} characters)", text.chars().count()));
            }
            return Ok("event".into());
        }
        // Standard X11 path: `keyboard()` sets X input focus briefly without raising the window.
        let person = x::front();
        if let Some((_, field)) = &field
            && !atspi::focused(field)
        {
            let _ = atspi::focus(field);
        }
        if flags.paste {
            self.keyboard(tracked, || {
                let mut saved = x::SavedClipboard::take();
                if let Err(error) = saved.lend(text) {
                    let _ = saved.put_back();
                    return Err(error);
                }
                thread::sleep(PASTE_SETTLE);
                let sent = x::chord("ctrl+v").and_then(|paste| x::press(&paste, Duration::ZERO));
                thread::sleep(PASTE_SETTLE);
                saved.put_back()?;
                sent
            })?;
            return Ok("paste".into());
        }
        let stop = || STOP.load(Ordering::Relaxed);
        let typed = self.keyboard(tracked, || x::type_text(text, &stop));
        if let Some(person) = person
            && person != xid
            && x::front() != Some(person)
        {
            let _ = x::activate(person);
        }
        let typed = typed?;
        if let Some((_, field)) = &field {
            caret_at(field);
        }
        if typed < text.chars().count() {
            return Err(format!("{STOPPED} (typed {typed} of {} characters)", text.chars().count()));
        }
        Ok("keys".into())
    }

    fn focused_atspi(view: &View) -> Option<Obj> {
        view.nodes
            .values()
            .find(|n| n.parts.states.contains(&"focused"))
            .or_else(|| view.nodes.values().find(|n| matches!(n.parts.role.as_str(), "edit" | "document") && atspi::focused(&n.element)))
            .map(|n| n.element.clone())
    }

    fn key(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &View, spec: &str) -> Reply {
        const XK_CONTROL_L: u32 = 0xffe3;
        const XK_CONTROL_R: u32 = 0xffe4;
        let chord = x::chord(spec)?;
        let hold = Duration::from_millis(flags.hold.unwrap_or(0).min(HOLD_MOST.as_millis() as u64));
        let ctrl_letter = |c: &x::Chord| -> Option<char> {
            (c.modifiers.len() == 1 && matches!(c.modifiers[0], XK_CONTROL_L | XK_CONTROL_R))
                .then(|| c.key.and_then(|k| u8::try_from(k).ok()).map(|b| (b as char).to_ascii_lowercase()))
                .flatten()
        };
        let Some(xid) = tracked.x11 else {
            if x::is_risky(&chord) && !flags.force {
                return Err(format!("'{spec}' can quit or close things: ask the person, then add --force"));
            }
            // On pure Wayland, handle clipboard/selection shortcuts through AT-SPI silently
            // before touching the global Wayland portal seat.
            if !flags.real && hold.is_zero()
                && let Some(letter) = ctrl_letter(&chord)
                && let Some(focused) = Self::focused_atspi(view)
            {
                match letter {
                    'a' if atspi::select_all(&focused) => return Ok("select all".into()),
                    'c' => {
                        if let Some(sel) = atspi::selected_text(&focused)
                            && x::set_clipboard(Some(&sel)).is_ok()
                        {
                            return Ok("text selection".into());
                        }
                    }
                    'x' => {
                        if atspi::editable(&focused)
                            && let Some(sel) = atspi::selected_text(&focused)
                            && x::set_clipboard(Some(&sel)).is_ok()
                            && atspi::delete_selection(&focused).is_some()
                        {
                            return Ok("cut selection".into());
                        }
                    }
                    'v' => {
                        if let Ok(Some(clip)) = x::clipboard()
                            && atspi::insert_at_caret(&focused, &clip).is_ok()
                        {
                            return Ok("text paste".into());
                        }
                    }
                    _ => {}
                }
            }
            if self.quiet.load(Ordering::Relaxed) {
                return Err("refused: quiet mode is on (Wayland portal keys go to the focused window; run 'deskcrew quiet off' to allow)".into());
            }
            portal::key(&chord, hold)?;
            return Ok("portal".into());
        };
        if !flags.real && self.quiet.load(Ordering::Relaxed) {
            let held = self.held_keys(who);
            let combined = x::Chord { modifiers: chord.modifiers.iter().chain(&held).copied().collect(), key: chord.key };
            if x::is_risky(&combined) && !flags.force {
                return Err(format!("'{spec}' can quit or close things: ask the person, then add --force"));
            }
            let letter = hold.is_zero().then(|| ctrl_letter(&combined)).flatten();
            let clipboard_shortcut = matches!(letter, Some('c' | 'x' | 'v' | 'a'));
            let focused = clipboard_shortcut.then(|| Self::focused_atspi(view)).flatten();
            let clip_before = matches!(letter, Some('c' | 'x')).then(|| x::clipboard().ok().flatten());
            let val_before = if letter == Some('v') {
                focused.as_ref().and_then(atspi::text_of)
            } else {
                None
            };
            self.keep_behind(tracked, flags, || x::send_key(xid, &combined, hold))?;
            // If an X11 app (e.g. Chromium, Electron, GTK4) ignored synthetic XSendEvent for
            // Copy/Cut/Paste/Select-All in the background, complete it via AT-SPI without ever
            // raising the window or stealing focus.
            if let (Some(letter), Some(el)) = (letter, focused.as_ref()) {
                match letter {
                    'c' => {
                        thread::sleep(PASTE_SETTLE);
                        if clip_before.as_ref() == Some(&x::clipboard().ok().flatten())
                            && let Some(sel) = atspi::selected_text(el)
                            && x::set_clipboard(Some(&sel)).is_ok()
                        {
                            return Ok("text selection".into());
                        }
                    }
                    'x' => {
                        thread::sleep(PASTE_SETTLE);
                        if clip_before.as_ref() == Some(&x::clipboard().ok().flatten())
                            && atspi::editable(el)
                            && let Some(sel) = atspi::selected_text(el)
                            && x::set_clipboard(Some(&sel)).is_ok()
                            && atspi::delete_selection(el).is_some()
                        {
                            return Ok("cut selection".into());
                        }
                    }
                    'v' => {
                        thread::sleep(PASTE_SETTLE);
                        if let Some(before) = &val_before
                            && atspi::text_of(el).as_deref() == Some(before.as_str())
                            && let Ok(Some(clip)) = x::clipboard()
                            && self.keep_behind(tracked, flags, || atspi::insert_at_caret(el, &clip).is_ok())
                        {
                            return Ok("text paste".into());
                        }
                    }
                    'a' => {
                        thread::sleep(KEYS_SETTLE);
                        if atspi::selected_text(el).is_none() && self.keep_behind(tracked, flags, || atspi::select_all(el)) {
                            return Ok("select all".into());
                        }
                    }
                    _ => {}
                }
            }
            return Ok("event".into());
        }
        self.keyboard(tracked, || {
            let mut chord = x::Chord { modifiers: chord.modifiers.clone(), key: chord.key };
            let held = self.all_held_keys();
            // Keys held with down by any agent are physically pressed too: down ctrl, then key q, is ctrl+q.
            let pressed = x::Chord { modifiers: chord.modifiers.iter().chain(&held).copied().collect(), key: chord.key };
            if x::is_risky(&pressed) && !flags.force {
                return Err(format!("'{spec}' can quit or close things: ask the person, then add --force"));
            }
            chord.modifiers.retain(|m| !held.contains(m));
            x::press(&chord, hold)
        })?;
        Ok("keys".into())
    }

    /// Hold modifier keys or a mouse button for this agent's later clicks and keys, until `up`.
    fn down(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, words: &[String]) -> Reply {
        let (spec, at_word) = match words {
            [spec] => (spec.as_str(), None),
            [spec, spot] => (spec.as_str(), Some(spot.as_str())),
            _ => return Err("down shift|ctrl|alt|super|left|right|middle [target]".into()),
        };
        if let Some(btn) = Button::named(spec) {
            let Some(xid) = tracked.x11 else {
                return Err("holding a mouse button on Wayland needs X11".into());
            };
            let (px, py) = match at_word {
                Some(word) => self.point_of(word, who, tracked, view)?,
                None => centre(self.window_of(tracked)?.rect),
            };
            let held_mods = self.held_keys(who);
            self.keep_behind(tracked, flags, || x::send_button_at(xid, px, py, btn, true, &held_mods))?;
            let mut held = self.held.lock().unwrap();
            let entry = held.entry(who.to_string()).or_insert(Held { keys: Vec::new(), buttons: Vec::new(), since: Instant::now() });
            entry.buttons.push((xid, btn, px, py));
            entry.since = Instant::now();
            return Ok("held".into());
        }
        if at_word.is_some() {
            return Err("down shift (or ctrl, alt, super, left, right, middle)".into());
        }
        if !x::has_display() {
            return Err("holding modifier keys (down) on Wayland needs X11".into());
        }
        let chord = x::chord(spec)?;
        if chord.key.is_some() {
            return Err("down holds modifier keys or mouse buttons: shift, ctrl, alt, super, left, right or middle".into());
        }
        let _turn = self.real_input.lock().unwrap();
        {
            let held = self.held.lock().unwrap();
            if let Some((other, _)) = held.iter().find(|(holder, h)| holder.as_str() != who && !h.keys.is_empty()) {
                return Err(format!("{other} is already holding keys: wait for it to run up"));
            }
        }
        x::modifiers(&chord.modifiers, true)?;
        let mut held = self.held.lock().unwrap();
        let entry = held.entry(who.to_string()).or_insert(Held { keys: Vec::new(), buttons: Vec::new(), since: Instant::now() });
        entry.keys.extend(chord.modifiers);
        entry.since = Instant::now();
        Ok("held".into())
    }

    fn scroll(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, words: &[String]) -> Reply {
        let direction = |word: &String| matches!(word.as_str(), "up" | "down" | "left" | "right");
        let at = words.iter().position(direction).ok_or("scroll [control] up|down|left|right [n]")?;
        let times = motion::parse_scroll_count(words.get(at + 1).map(String::as_str), 3)?;
        let (px, py) = match words[..at].first() {
            Some(word) => self.point_of(word, who, tracked, view)?,
            None => centre(self.window_of(tracked)?.rect),
        };
        self.cursor_on(who, flags, tracked, (px, py, 0, 0), false);
        let sideways = words[at] == "left" || words[at] == "right";
        // A scroll bar moves through accessibility, with no pointer and the window left where it is.
        let control = match words[..at].first() {
            Some(word) => match self.resolve(word, who, tracked, view)? {
                Target::Control(id) => view.nodes.get(&id).map(|node| node.element.clone()),
                Target::Point(..) => None,
            },
            None => view.element.clone(),
        };
        let onward = if words[at] == "down" || words[at] == "right" { times } else { -times };
        if !flags.real
            && !flags.mouse
            && let Some(control) = control
            && self.keep_behind(tracked, flags, || atspi::scroll(&control, !sideways, onward).is_ok())
        {
            return Ok("scroll bar".into());
        }
        // Positive is up, or right when sideways, as x::wheel takes it.
        let notches = if words[at] == "up" || words[at] == "right" { times } else { -times };
        let Some(xid) = tracked.x11 else {
            if self.quiet.load(Ordering::Relaxed) {
                return Err("refused: quiet mode is on (Wayland portal scroll moves the real pointer; run 'deskcrew quiet off' to allow)".into());
            }
            portal::wheel(px, py, notches, sideways)?;
            return Ok("portal".into());
        };
        if !flags.real && !flags.mouse {
            if !self.quiet.load(Ordering::Relaxed) && self.pointer_for(&[]) == By::Agent {
                let _turn = self.real_input.lock().unwrap();
                if x::shows_at(xid, px, py) {
                    x::wheel(By::Agent, px, py, notches, sideways)?;
                    return Ok("own pointer".into());
                }
            }
            self.keep_behind(tracked, flags, || x::send_wheel(xid, px, py, notches, sideways))?;
            return Ok("event".into());
        }
        let by = if flags.mouse { By::Person } else { self.pointer_for(&[]) };
        self.with_pointer(by, tracked, (px, py), || x::wheel(by, px, py, notches, sideways))
    }

    fn strokes(&self, who: &str, flags: &Flags, tracked: &Tracked, strokes: &[Vec<(f32, f32)>], button: Button, modifiers: &[u32]) -> Reply {
        let stop = || STOP.load(Ordering::Relaxed);
        let Some(xid) = tracked.x11 else {
            if self.quiet.load(Ordering::Relaxed) {
                return Err("refused: quiet mode is on (Wayland portal drawing moves the real pointer; run 'deskcrew quiet off' to allow)".into());
            }
            for stroke in strokes {
                if !portal::stroke(stroke, button, modifiers, &stop)? {
                    return Err(STOPPED.into());
                }
            }
            return Ok("portal".into());
        };
        let window = self.window_of(tracked)?.rect;
        if !strokes.iter().flatten().all(|p| motion::point_in_rect(*p, window)) {
            return Err("refused: that stroke leaves the window".into());
        }
        if let Some(&(px, py)) = strokes.first().and_then(|s| s.first()) {
            self.cursor_on(who, flags, tracked, (px as i32, py as i32, 0, 0), false);
        }
        // When uncovered on X11, prefer deskcrew's dedicated XI2 agent pointer (`By::Agent`),
        // which works in Krita/GIMP/GTK/Qt/Chromium without raising the window or touching the
        // person's cursor. Fall back to background XSendEvent when covered or in quiet mode.
        if !flags.real && !flags.mouse {
            if !self.quiet.load(Ordering::Relaxed) && self.pointer_for(modifiers) == By::Agent {
                let _turn = self.real_input.lock().unwrap();
                if strokes.iter().flatten().all(|p| x::shows_at(xid, p.0 as i32, p.1 as i32)) {
                    for stroke in strokes {
                        if !x::stroke(By::Agent, stroke, button, &stop, &mut |at, px, py| {
                            if at.is_multiple_of(3) {
                                self.overlay.jump(who, px, py);
                            }
                        })? {
                            return Err(STOPPED.into());
                        }
                    }
                    return Ok("own pointer".into());
                }
            }
            let mut held = self.held_keys(who);
            held.extend_from_slice(modifiers);
            let finished = self.keep_behind(tracked, flags, || -> Result<bool, String> {
                for stroke in strokes {
                    let ok = x::send_stroke(xid, stroke, button, &held, &stop, &mut |at, px, py| {
                        if at.is_multiple_of(3) {
                            self.overlay.jump(who, px, py);
                        }
                    })?;
                    if !ok {
                        return Ok(false);
                    }
                }
                Ok(true)
            })?;
            return if finished { Ok("event".into()) } else { Err(STOPPED.into()) };
        }
        // Explicit --real / --mouse path.
        let by = if flags.mouse { By::Person } else { self.pointer_for(modifiers) };
        let all_visible = || strokes.iter().flatten().all(|p| x::shows_at(xid, p.0 as i32, p.1 as i32));
        let finished = std::cell::Cell::new(true);
        let draw = || {
            x::modifiers(modifiers, true)?;
            let res = (|| {
                for stroke in strokes {
                    if !x::stroke(by, stroke, button, &stop, &mut |at, px, py| {
                        if at.is_multiple_of(3) {
                            self.overlay.jump(who, px, py);
                        }
                    })? {
                        finished.set(false);
                        return Ok(());
                    }
                }
                Ok(())
            })();
            let _ = x::modifiers(modifiers, false);
            res
        };
        let mut route = None;
        if by == By::Agent {
            let _turn = self.real_input.lock().unwrap();
            if all_visible() {
                draw()?;
                route = Some("own pointer".to_string());
            }
        }
        let route = match route {
            Some(route) => route,
            None => {
                self.front(tracked, || {
                    if !all_visible() {
                        return Err("refused: another window covers part of that stroke".into());
                    }
                    draw()
                })?;
                if by == By::Agent { "own pointer, window brought forward" } else { "pointer" }.to_string()
            }
        };
        if finished.get() { Ok(route) } else { Err(STOPPED.into()) }
    }

    fn drag(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, from: &str, to: &str) -> Reply {
        let a = self.point_of(from, who, tracked, view)?;
        let b = self.point_of(to, who, tracked, view)?;
        let button = flags.mouse_button()?;
        let mut modifiers = flags.modifiers()?;
        // Keys this agent already holds stay down after the stroke.
        modifiers.retain(|m| !self.held_keys(who).contains(m));
        self.strokes(who, flags, tracked, &[motion::trace(&[a, b], flags.smooth)], button, &modifiers)
    }

    fn plan_draw(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, words: &[String]) -> Result<Vec<Vec<(f32, f32)>>, String> {
        let (word, script) = match words {
            [] => return Err("draw [control] \"x,y x,y; rect x,y w,h; circle x,y r\"".into()),
            // A first word that is neither a point nor a shape names the control to draw on.
            [first, rest @ ..] if !rest.is_empty() && is_point(first).is_none() && !motion::is_shape(first) => (Some(first.clone()), rest.join(" ")),
            _ => (None, words.join(" ")),
        };
        let origin = match word {
            Some(ref word) => Some(self.control(word, who, tracked, view)?.1),
            None => None,
        };
        let frames = self.agents.lock().unwrap().get(who).and_then(|a| a.shots.get(&tracked.key).map(|s| s.frames));
        let (offset, scale) = match (origin, frames) {
            (Some(rect), _) => ((rect.0 as f32, rect.1 as f32), 1.0_f32),
            (None, Some(frames)) => ((frames.x as f32, frames.y as f32), frames.scale),
            (None, None) => return Err("draw on a control, or take a shot first".into()),
        };

        let mut strokes = Vec::new();
        for stroke in script.split(';').filter(|s| !s.trim().is_empty()) {
            let stroke = stroke.trim();
            if let Some(shape) = motion::parse_shape(stroke)? {
                let transformed = shape.with_flags(flags.fill, flags.close).transform(offset, scale);
                strokes.extend(motion::generate_shape(&transformed));
            } else {
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
        }
        if strokes.is_empty() || strokes.iter().map(Vec::len).sum::<usize>() == 0 {
            return Err("nothing to draw".into());
        }
        Ok(strokes)
    }

    /// Strokes of x,y points in the last shot, or from a control's top left.
    fn draw(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, words: &[String]) -> Reply {
        let strokes = self.plan_draw(who, flags, tracked, view, words)?;
        let button = flags.mouse_button()?;
        let mut modifiers = flags.modifiers()?;
        // Keys this agent already holds stay down after the stroke.
        modifiers.retain(|m| !self.held_keys(who).contains(m));
        self.strokes(who, flags, tracked, &strokes, button, &modifiers)
    }

    fn wait(&self, flags: &Flags, tracked: &Tracked, view: &mut View, text: &str, allow_ms: bool) -> Reply {
        // Numeric `wait 500` sleeps for that many milliseconds, matching Windows and macOS.
        if allow_ms
            && !flags.gone
            && let Ok(ms) = text.parse::<u64>()
        {
            let delay = Duration::from_millis(ms.min(WAIT_MOST_MS));
            let started = Instant::now();
            while started.elapsed() < delay {
                if STOP.load(Ordering::Relaxed) {
                    return Err(STOPPED.into());
                }
                thread::sleep(Duration::from_millis(50).min(delay.saturating_sub(started.elapsed())));
            }
            return Ok("waited".into());
        }
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

    /// Fill a GTK or Qt file dialog's file name with this path and accept it with Enter, which
    /// works in any language. GTK's Open dialog shows its location field on Ctrl+L. The dialog can
    /// be this window, or one its app opened over it.
    fn pick_file(&self, flags: &Flags, tracked: &Tracked, view: &mut View, path: &str) -> Reply {
        let pid = self.window_of(tracked)?.pid;
        if let Some(done) = self.fill_file_dialog(flags, tracked, view, path) {
            return done;
        }
        if pid > 0 {
            for window in self.list_windows()?.into_iter().filter(|w| w.pid == pid && w.key != tracked.key) {
                let dialog = Tracked { id: 0, key: window.key, x11: window.x11, view: Mutex::new(View { element: window.element, ..View::default() }) };
                let mut dialog_view = dialog.view.lock().unwrap();
                if let Some(done) = self.fill_file_dialog(flags, &dialog, &mut dialog_view, path) {
                    return done;
                }
            }
        }
        Err("no file dialog in this window or one its app opened: open it first, then pick-file".into())
    }

    /// None if this window has no field for a file name, even after Ctrl+L.
    fn fill_file_dialog(&self, flags: &Flags, tracked: &Tracked, view: &mut View, path: &str) -> Option<Reply> {
        let field = |view: &View| {
            let fields: Vec<&Obj> = view.nodes.values().filter(|n| n.parts.role == "edit").map(|n| &n.element).filter(|e| atspi::editable(e)).collect();
            fields.iter().find(|e| atspi::focused(e)).or(fields.first()).map(|e| (*e).clone())
        };
        self.read(tracked, view, false).ok()?;
        // Only a dialog, never a main window that happens to have a text field: GTK names its
        // file dialogs as such, and Qt's are dialogs.
        let role = view.element.as_ref().map(atspi::role_name).unwrap_or_default();
        if !matches!(role.as_str(), "file chooser" | "dialog") {
            return None;
        }
        // GTK's dialog keeps hidden fields, its search box among them: Ctrl+L shows its location
        // field and puts the keyboard there. Qt's has its file name field focused already.
        if role == "file chooser" || field(view).is_none() {
            if let Some(xid) = tracked.x11 {
                if flags.real {
                    self.keyboard(tracked, || x::press(&x::chord("ctrl+l")?, Duration::ZERO)).ok()?;
                } else {
                    self.keep_behind(tracked, flags, || x::chord("ctrl+l").and_then(|c| x::send_key(xid, &c, Duration::ZERO))).ok()?;
                }
                thread::sleep(MENU_OPEN);
                self.read(tracked, view, false).ok()?;
            } else if field(view).is_none() {
                return Some(Err("opening the path field in a GTK file dialog on Wayland needs X11 (for Ctrl+L); open the location field first".into()));
            }
        }
        let element = field(view)?;
        Some((|| {
            self.keep_behind(tracked, flags, || atspi::set_text(&element, path))?;
            // The dialog's default button, Open or Save in any language, accepts it as Enter would,
            // and needs no keyboard focus, which a dialog that is not the active window lacks.
            thread::sleep(MENU_OPEN);
            self.read(tracked, view, false)?;
            let default = view.nodes.values().find(|n| n.parts.role == "button" && !n.parts.states.contains(&"disabled") && atspi::is_default(&n.element));
            if let Some(button) = default
                && self.keep_behind(tracked, flags, || atspi::press(&button.element).is_ok())
            {
                // Accepted through AT-SPI.
            } else if let Some(xid) = tracked.x11 {
                if flags.real {
                    if !atspi::focused(&element) {
                        let _ = atspi::focus(&element);
                    }
                    self.keyboard(tracked, || x::press(&x::chord("enter")?, Duration::ZERO))?;
                } else {
                    self.keep_behind(tracked, flags, || x::chord("enter").and_then(|c| x::send_key(xid, &c, Duration::ZERO)))?;
                }
            } else {
                return Err("set the path, but that dialog has no AT-SPI default button and pressing Enter on Wayland needs X11".into());
            }
            // The dialog closes a moment after: wait, so the reply says so.
            let started = Instant::now();
            while started.elapsed() < DIALOG_CLOSE {
                thread::sleep(WAIT_STEP);
                if self.window_of(tracked).is_err() {
                    return Ok("dialog closed".into());
                }
            }
            Ok("dialog still open: see what it asks".into())
        })())
    }

    fn step(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, step: &Step) -> Reply {
        if let Some(error) = &step.flags.flag_error {
            return Err(error.clone());
        }
        if step.flags.window.is_some() {
            return Err("a step cannot pick another window: a do runs in one, so run another command for that window".into());
        }
        if matches!(step.flags.risk.as_deref(), Some("medium" | "high")) && !flags.force && !step.flags.force {
            return Err("declared risky: ask the person, then add --force".into());
        }
        let merged = step.flags.clone().merged(flags);
        let words = &step.words;
        let (command, args) = words.split_first().ok_or("empty step")?;
        let route = match (command.as_str(), args) {
            ("click", [word]) => self.click(who, &merged, tracked, view, word),
            ("hover", [word]) => self.hover(who, &merged, tracked, view, word),
            ("set", [word, value]) => self.set(who, &merged, tracked, view, word, value),
            ("clear", [word]) => self.set(who, &merged, tracked, view, word, ""),
            ("check", [word]) => self.ensure(who, &merged, tracked, view, word, Some("on")),
            ("uncheck", [word]) => self.ensure(who, &merged, tracked, view, word, Some("off")),
            ("toggle", [word]) => self.ensure(who, &merged, tracked, view, word, None),
            ("expand", [word]) => self.expand(who, &merged, tracked, view, word, true),
            ("collapse", [word]) => self.expand(who, &merged, tracked, view, word, false),
            ("show", [word]) => self.show(who, &merged, tracked, view, word),
            ("select", [word, item]) => self.select(who, &merged, tracked, view, word, item),
            ("type", rest) => self.type_text(who, &merged, tracked, view, rest),
            ("key", [spec]) => self.key(who, &merged, tracked, view, spec),
            ("down", rest) => self.down(who, &merged, tracked, view, rest),
            ("scroll", rest) => self.scroll(who, &merged, tracked, view, rest),
            ("drag", [from, to]) => self.drag(who, &merged, tracked, view, from, to),
            ("draw", rest) => self.draw(who, &merged, tracked, view, rest),
            ("wait", [text]) => self.wait(&merged, tracked, view, text, true),
            ("expect", [text]) => self.wait(&merged, tracked, view, text, false),
            ("pick-file", [path]) => self.pick_file(&merged, tracked, view, path),
            _ => Err(format!("can't do '{}': see deskcrew help", words.join(" "))),
        }?;
        if let Some(text) = &step.flags.expect {
            let waiting = Flags { ms: merged.ms, ..Flags::default() };
            self.wait(&waiting, tracked, view, text, false)?;
        }
        Ok(route)
    }

    /// Where the window's picture changed since `before`, ignoring a caret.
    fn picture_change(&self, tracked: &Tracked, before: &Frame) -> Option<String> {
        let (after, _) = x::window_picture(tracked.x11?).ok()?;
        let (cx, cy, w, h) = after.changed_from(before)?;
        if w < LEAST_CHANGE || h < LEAST_CHANGE {
            return None;
        }
        Some(format!("controls unchanged; the picture changed at {cx},{cy} {w}x{h}"))
    }

    /// Run steps against one window, then report the result as a single set of changes.
    fn act(&self, who: &str, flags: &Flags, steps: &[Step]) -> Reply {
        if STOP.load(Ordering::Relaxed) {
            return Err(STOPPED.into());
        }
        let tracked = self.pick(who, flags.window.as_deref())?;
        let mut guard = tracked.view.lock().unwrap();
        let view = &mut *guard;
        if !view.read_once {
            self.read(&tracked, view, false)?;
        }
        let before = self.seen(who, &tracked.key).unwrap_or_else(|| view.lines.clone());
        let picture_before = tracked.x11.and_then(|id| x::window_picture(id).ok()).map(|(f, _)| f).filter(|f| !f.is_blank());
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
                    self.remember(who, tracked.key.clone(), view.lines.clone());
                }
                let picture = if nothing && !waited_only { picture_before.as_ref().map(|b| self.picture_change(&tracked, b)) } else { None };
                match picture {
                    Some(Some(note)) => out.push(note),
                    Some(None) => out.push("nothing changed, on screen either".into()),
                    None if nothing && !waited_only => out.push(if tracked.x11.is_some() {
                        "nothing changed: check with shot".into()
                    } else {
                        "nothing changed in controls".into()
                    }),
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
        let current = self.agents.lock().unwrap().get(who).and_then(|a| a.current.clone());
        let known_before: std::collections::HashSet<WindowKey> = self.windows.lock().unwrap().1.keys().cloned().collect();
        let listed = self.list_windows()?;
        self.windows.lock().unwrap().1.retain(|key, _| !known_before.contains(key) || listed.iter().any(|w| &w.key == key));
        let shown: Vec<String> = listed
            .iter()
            .filter(|w| wanted.as_ref().is_none_or(|f| w.title.to_lowercase().contains(f) || w.app.to_lowercase().contains(f)))
            .take(MAX_WINDOWS)
            .map(|w| {
                let mark = if current.as_ref() == Some(&w.key) { "*" } else { "" };
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
        let seen = self.seen(who, &tracked.key).filter(|_| !flags.full && !filtered && !flags.all);
        if let Some(seen) = seen {
            let changes = lines::changes(&seen, &view.lines);
            if changes.len() * 2 <= view.lines.len().max(1) {
                self.remember(who, tracked.key.clone(), view.lines.clone());
                return Ok(if changes.is_empty() { "no change".into() } else { changes.join("\n") });
            }
        }
        if !filtered {
            self.remember(who, tracked.key.clone(), view.lines.clone());
        }
        let window = self.window_of(&tracked)?;
        let hidden = if view.hidden > 0 { format!(", {} off screen", view.hidden) } else { String::new() };
        let mut out = vec![format!("w{} \"{}\" {} controls{hidden}", tracked.id, lines::clip(&window.title, 50), view.lines.len())];
        out.extend(lines::page(&view.lines, flags.find.as_deref(), flags.from));
        if !filtered {
            let blind = self.blind_spot(who, &tracked, &window, view);
            if blind.is_empty() && view.element.is_none() && tracked.x11.is_some() {
                out.push("(no AT-SPI controls: use x,y from shot, or start with accessibility enabled)".into());
            } else {
                out.extend(blind);
            }
        }
        Ok(out.join("\n"))
    }

    /// Point out the largest part of the window with no controls in it, reading any text drawn
    /// there via OCR (tesseract) so the agent gets clickable coordinates and named OCR lookup
    /// without needing a shot. Uses the window's composited backing pixmap so covered windows work.
    fn blind_spot(&self, who: &str, tracked: &Tracked, window: &WindowInfo, view: &mut View) -> Vec<String> {
        let Some(xid) = tracked.x11 else {
            return Vec::new();
        };
        let rect = window.rect;
        let mut rects: Vec<_> = view
            .nodes
            .values()
            .map(|n| (n.rect, CONTAINERS.contains(&n.parts.role.as_str()) && n.parts.name.is_empty()))
            .collect();
        rects.extend(view.quiet.iter().map(|&r| (r, false)));
        let Some((bx, by, w, h)) = look::empty_area(rect, &rects) else { return Vec::new() };
        let part = x::window_picture(xid)
            .ok()
            .map(|(full, _)| {
                let lx = (bx - rect.0).max(0) as u32;
                let ly = (by - rect.1).max(0) as u32;
                full.crop(lx, ly, w.max(1) as u32, h.max(1) as u32)
            })
            .or_else(|| x::capture((bx, by, w, h)).ok());
        let Some(part) = part else { return Vec::new() };
        if !part.busy() {
            return Vec::new();
        }
        let hash = part.hash();
        if let Some((seen, said)) = &view.blind
            && *seen == hash
        {
            return said.clone();
        }
        let mut said = Vec::new();
        let text = look::read_text(&part).unwrap_or_default();
        view.ocr_hits = text
            .iter()
            .map(|line| (line.text.clone(), (bx + line.rect.0, by + line.rect.1, line.rect.2, line.rect.3)))
            .collect();
        if text.is_empty() {
            said.push(format!("no controls in {},{} {w}x{h}, only drawing: shot --region {bx},{by},{w},{h}", bx - rect.0, by - rect.1));
        } else {
            said.push(format!("read from {},{} {w}x{h} (no controls there; click name or x,y works):", bx - rect.0, by - rect.1));
            for line in text {
                let cx = (bx - rect.0) + line.rect.0 + line.rect.2 / 2;
                let cy = (by - rect.1) + line.rect.1 + line.rect.3 / 2;
                said.push(format!("  \"{}\" {cx},{cy}", lines::clip(&line.text, 80)));
            }
        }
        // x,y in the window read as screen pixels from its corner until the next shot.
        let mut agents = self.agents.lock().unwrap();
        let state = agents.entry(who.to_string()).or_default();
        state.shots.entry(tracked.key.clone()).or_insert(LastShot { hash: 0, frame: Frame { w: 0, h: 0, pixels: Vec::new() }, frames: Frames { x: rect.0, y: rect.1, scale: 1.0 } });
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

    /// A table's rows, as many as fit in one reply, with its column names first. `--from N`
    /// starts at row N.
    fn table(&self, who: &str, flags: &Flags, words: &[String]) -> Reply {
        let word = words.first().ok_or("table needs a control")?;
        let tracked = self.pick(who, flags.window.as_deref())?;
        let mut view = tracked.view.lock().unwrap();
        if !view.read_once {
            self.read(&tracked, &mut view, false)?;
        }
        let Target::Control(id) = self.resolve(word, who, &tracked, &mut view)? else { return Err("table needs a control".into()) };
        let skip = flags.from.saturating_sub(1);
        let table = atspi::table(&view.nodes[&id].element, skip, MAX_TABLE_ROWS, MAX_TABLE_COLUMNS)?;
        let clip = |cells: &[String]| cells.iter().map(|cell| lines::clip(cell, MAX_CELL_CHARS)).collect::<Vec<_>>().join("	");
        let mut out = vec![format!("{} rows, {} columns", table.total_rows, table.total_columns)];
        if !table.headers.is_empty() {
            out.push(clip(&table.headers));
        }
        let mut size = out.iter().map(String::len).sum::<usize>();
        let mut shown = 0;
        for row in &table.rows {
            let line = clip(row);
            if size + line.len() > TABLE_CHARS && shown > 0 {
                break;
            }
            size += line.len() + 1;
            out.push(line);
            shown += 1;
        }
        let last = skip + shown;
        if last < table.total_rows {
            out.push(format!("rows {}-{last} of {} shown: table {word} --from {} reads on", skip + 1, table.total_rows, last + 1));
        }
        Ok(out.join("
"))
    }

    fn save_screen(&self, who: &str, rect: Bounds, edge: u32) -> Reply {
        let frame = x::capture(rect)?;
        let (small, scale) = frame.shrink(edge);
        let path = shot::path_for(who, 0);
        small.save(&path)?;
        // x,y in this picture are what the agent's next click x,y means, in its current window.
        let mut agents = self.agents.lock().unwrap();
        let state = agents.entry(who.to_string()).or_default();
        if let Some(window) = state.current.clone() {
            let frames = Frames { x: rect.0, y: rect.1, scale };
            match state.shots.get_mut(&window) {
                Some(last) => last.frames = frames,
                None => {
                    state.shots.insert(window, LastShot { hash: 0, frame: Frame { w: 0, h: 0, pixels: Vec::new() }, frames });
                }
            }
        }
        Ok(format!("{} {}x{} scale {:.2}", path.display(), small.w, small.h, scale))
    }

    fn shot(&self, who: &str, flags: &Flags, words: &[String]) -> Reply {
        let edge = flags.edge.unwrap_or(shot::DEFAULT_EDGE);
        let pure_wayland = !x::has_display() && x::has_wayland();
        if let Some(region) = &flags.region {
            let Some((rx, ry, w, h)) = lines::parse_rect(region) else {
                return Err("--region x,y,w,h (width and height >= 1)".into());
            };
            if pure_wayland {
                let (full, origin, units) = portal::screenshot(Some((rx.saturating_add(w / 2), ry.saturating_add(h / 2))))?;
                let px_x = (((rx - origin.0) as f32 / units).round() as i32).max(0) as u32;
                let px_y = (((ry - origin.1) as f32 / units).round() as i32).max(0) as u32;
                let px_w = ((w as f32 / units).round() as u32).max(1);
                let px_h = ((h as f32 / units).round() as u32).max(1);
                let frame = full.crop(px_x, px_y, px_w, px_h);
                let hash = frame.hash();
                let (small, shrink) = frame.shrink(edge);
                let scale = shrink * units;
                let (sw, sh) = (small.w, small.h);
                let path = shot::path_for(who, 0);
                small.save(&path)?;
                let mut agents = self.agents.lock().unwrap();
                let state = agents.entry(who.to_string()).or_default();
                if let Some(window) = state.current.clone() {
                    let frames = Frames { x: rx, y: ry, scale };
                    state.shots.insert(window, LastShot { hash, frame: small, frames });
                }
                return Ok(format!("{} {}x{} scale {scale:.2}", path.display(), sw, sh));
            }
            x::ready()?;
            return self.save_screen(who, (rx, ry, w, h), edge);
        }
        if flags.screen {
            if pure_wayland {
                let (frame, origin, units) = portal::screenshot(None)?;
                let hash = frame.hash();
                let (small, shrink) = frame.shrink(edge);
                let scale = shrink * units;
                let (sw, sh) = (small.w, small.h);
                let path = shot::path_for(who, 0);
                small.save(&path)?;
                let mut agents = self.agents.lock().unwrap();
                let state = agents.entry(who.to_string()).or_default();
                if let Some(window) = state.current.clone() {
                    let frames = Frames { x: origin.0, y: origin.1, scale };
                    state.shots.insert(window, LastShot { hash, frame: small, frames });
                }
                return Ok(format!("{} {}x{} scale {scale:.2}", path.display(), sw, sh));
            }
            x::ready()?;
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
        if !pure_wayland {
            x::ready()?;
        }
        let word = words.first();
        let control = word.and_then(|w| is_ref(w));
        let tracked = self.pick(who, if control.is_some() { flags.window.as_deref() } else { word.map(String::as_str).or(flags.window.as_deref()) })?;
        let window = self.window_of(&tracked)?;
        let (left, top, ww, wh) = window.rect;
        let (mut frame, mut origin, units) = match tracked.x11 {
            Some(xid) => {
                let (f, u) = x::window_picture(xid)?;
                (f, (left, top), u)
            }
            None => {
                let hint = (ww > 0 && wh > 0).then_some((left.saturating_add(ww / 2), top.saturating_add(wh / 2)));
                let (full, screen_origin, units) = portal::screenshot(hint)?;
                if ww > 0 && wh > 0 {
                    let px_x = (((left - screen_origin.0) as f32 / units).round() as i32).max(0) as u32;
                    let px_y = (((top - screen_origin.1) as f32 / units).round() as i32).max(0) as u32;
                    let px_w = ((ww as f32 / units).round() as u32).max(1);
                    let px_h = ((wh as f32 / units).round() as u32).max(1);
                    let cropped = full.crop(px_x, px_y, px_w, px_h);
                    (cropped, (left.max(screen_origin.0), top.max(screen_origin.1)), units)
                } else {
                    (full, screen_origin, units)
                }
            }
        };
        if let Some(id) = control {
            let mut view = tracked.view.lock().unwrap();
            if !view.nodes.contains_key(&id) {
                self.read(&tracked, &mut view, false)?;
            }
            let node = view.nodes.get(&id).ok_or(format!("e{id} is gone: see"))?;
            let rect = atspi::rect(&node.element).unwrap_or(node.rect);
            if rect.2 <= 0 || rect.3 <= 0 {
                return Err(format!("e{id} has no visible size"));
            }
            let rx0 = rect.0.max(origin.0);
            let ry0 = rect.1.max(origin.1);
            let vw = rect.0.saturating_add(rect.2).saturating_sub(rx0);
            let vh = rect.1.saturating_add(rect.3).saturating_sub(ry0);
            if vw <= 0 || vh <= 0 {
                return Err(format!("e{id} is outside the window picture"));
            }
            let cx = (((rx0 - origin.0) as f32 / units).round() as i32).max(0) as u32;
            let cy = (((ry0 - origin.1) as f32 / units).round() as i32).max(0) as u32;
            let cw = ((vw as f32 / units).round() as u32).max(1);
            let ch = ((vh as f32 / units).round() as u32).max(1);
            frame = frame.crop(cx, cy, cw, ch);
            if frame.w == 0 || frame.h == 0 {
                return Err(format!("e{id} is outside the window picture"));
            }
            origin = (rx0, ry0);
        }
        let hash = frame.hash();
        // The view before the agents: every other command takes the two locks in that order.
        let marked: Vec<(u32, (i32, i32, i32, i32))> = if flags.marks {
            let view = tracked.view.lock().unwrap();
            view.nodes.iter().filter(|(_, node)| MARKED.contains(&node.parts.role.as_str())).map(|(id, node)| (*id, node.rect)).collect()
        } else {
            Vec::new()
        };
        let mut agents = self.agents.lock().unwrap();
        let state = agents.entry(who.to_string()).or_default();
        let last = state.shots.get(&tracked.key);
        if control.is_none() && !flags.full && !flags.marks && !flags.rulers && last.is_some_and(|l| l.hash == hash) {
            return Ok("unchanged".into());
        }
        let crop = if flags.changed && control.is_none() { last.and_then(|l| frame.changed_from(&l.frame)) } else { None };
        let picture = match crop {
            Some((cx, cy, w, h)) => {
                origin = (origin.0 + (cx as f32 * units).round() as i32, origin.1 + (cy as f32 * units).round() as i32);
                frame.crop(cx, cy, w, h)
            }
            None => frame.crop(0, 0, frame.w, frame.h),
        };
        let (mut small, shrink) = picture.shrink(edge);
        let scale = shrink * units;
        if flags.marks && scale.is_finite() && scale > 0.0 {
            let to_image = |value: i32, from: i32| ((value - from) as f32 / scale) as i32;
            for (id, (nx, ny, w, h)) in marked {
                small.mark(to_image(nx, origin.0), to_image(ny, origin.1), (w as f32 / scale) as i32, (h as f32 / scale) as i32, id);
            }
        }
        if flags.rulers {
            small.rulers();
        }
        let path = shot::path_for(who, tracked.id);
        small.save(&path)?;
        let frames = Frames { x: origin.0, y: origin.1, scale };
        if control.is_none() {
            state.shots.insert(tracked.key.clone(), LastShot { hash, frame, frames });
        } else if let Some(last) = state.shots.get_mut(&tracked.key) {
            last.frames = frames;
        } else {
            state.shots.insert(tracked.key.clone(), LastShot { hash: 0, frame: Frame { w: 0, h: 0, pixels: Vec::new() }, frames });
        }
        // An agent taking pictures of a window it has never read is pointed to the cheaper way.
        let unread = if control.is_none() && !flags.marks && !state.seen.contains_key(&tracked.key) { READ_FIRST } else { "" };
        let part = if crop.is_some() { " changed part" } else { "" };
        Ok(format!("{} {}x{} scale {scale:.2}{part}", path.display(), small.w, small.h) + unread)
    }

    fn launch(&self, who: &str, flags: &Flags, words: &[String]) -> Reply {
        if STOP.load(Ordering::Relaxed) {
            return Err(STOPPED.into());
        }
        let app = words.first().ok_or("launch needs an app")?;
        let before_front = (!flags.front).then(x::front).flatten();
        let before: Vec<WindowKey> = self.list_windows()?.into_iter().map(|w| w.key).collect();
        let pid = x::spawn(app, &words[1..])?;
        let xwayland = x::is_xwayland();
        let started = Instant::now();
        let mut found = None;
        let mut atspi_seen_at = None;
        while started.elapsed() < LAUNCH_WAIT {
            thread::sleep(LAUNCH_STEP);
            let now = self.list_windows()?;
            let picked = now
                .iter()
                .find(|w| w.pid == pid)
                .map(|own| WindowInfo {
                    key: own.key.clone(),
                    x11: own.x11,
                    element: own.element.clone(),
                    pid: own.pid,
                    app: own.app.clone(),
                    title: own.title.clone(),
                    rect: own.rect,
                })
                .or_else(|| now.into_iter().find(|w| !before.contains(&w.key)));
            if let Some(window) = picked {
                let wait_x11 = xwayland && window.x11.is_none();
                found = Some(window);
                if wait_x11 && atspi_seen_at.get_or_insert_with(Instant::now).elapsed() < Duration::from_millis(600) {
                    continue;
                }
                break;
            }
        }
        let Some(window) = found else { return Ok(format!("started {app}; no new window yet")) };
        // Keep background launches behind the person's active window unless `--front` was passed.
        if !flags.front
            && let (Some(new_xid), Some(prev)) = (window.x11, before_front)
            && prev != new_xid
        {
            x::lower_behind(new_xid, prev);
            if x::front() == Some(new_xid) {
                let _ = x::activate(prev);
            }
        }
        let tracked = self.track(&window);
        self.set_current(who, window.key);
        Ok(format!("w{} \"{}\" {}", tracked.id, lines::clip(&window.title, 50), window.app))
    }

    fn status(&self) -> Reply {
        let mut agents: Vec<String> = self.agents.lock().unwrap().keys().cloned().collect();
        agents.sort();
        let stopped = if STOP.load(Ordering::Relaxed) { "; stopped by Ctrl+Alt+Esc" } else { "" };
        let display = if !x::has_display() && x::has_wayland() {
            format!("; wayland {} (AT-SPI + desktop portals; no X11)", std::env::var("WAYLAND_DISPLAY").unwrap_or_default())
        } else if x::is_xwayland() {
            format!(
                "; wayland {} + xwayland {}",
                std::env::var("WAYLAND_DISPLAY").unwrap_or_default(),
                std::env::var("DISPLAY").unwrap_or_default()
            )
        } else {
            match x::ready() {
                Ok(()) => format!("; display {}", std::env::var("DISPLAY").unwrap_or_default()),
                Err(e) => format!("; {e}"),
            }
        };
        let access = match atspi::bus() {
            Ok(_) => "",
            Err(_) => "; no accessibility bus",
        };
        let quiet = if self.quiet.load(Ordering::Relaxed) { "; quiet: on" } else { "" };
        Ok(format!("deskcrew {} running; agents: {}{stopped}{quiet}{display}{access}", env!("CARGO_PKG_VERSION"), agents.join(", ")))
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

    fn dry_run(&self, tracked: &Tracked, view: &View, steps: &[Step]) -> String {
        let name = |word: &str| is_ref(word).and_then(|id| view.nodes.get(&id)).map(|node| lines::line(&node.parts));
        steps
            .iter()
            .enumerate()
            .map(|(at, step)| {
                let named: Vec<String> = step.words.iter().map(|word| name(word).map_or(word.clone(), |line| format!("{word} ({line})"))).collect();
                format!("{}. {}", at + 1, named.join(" "))
            })
            .chain(Some(format!("nothing done in w{}", tracked.id)))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn handle(&self, who: &str, words: &[String]) -> Reply {
        let (flags, rest) = parse(words);
        if let Some(error) = &flags.flag_error {
            return Err(error.clone());
        }
        let (command, args) = rest.split_first().ok_or(HELP)?;
        let with_expect = |mut steps: Vec<Step>| {
            if let Some(text) = &flags.expect {
                steps.push(Step { flags: Flags::default(), words: vec!["expect".to_string(), text.clone()] });
            }
            steps
        };
        let window_spec = || args.first().map(String::as_str).or(flags.window.as_deref());
        // Commands that change windows or the clipboard stop with every action on Ctrl+Alt+Esc.
        let changes = matches!(command.as_str(), "focus" | "close" | "place")
            || (command == "clip" && !args.is_empty())
            || (matches!(command.as_str(), "quiet" | "background-only") && args.first().map(String::as_str) == Some("off"));
        if changes && STOP.load(Ordering::Relaxed) {
            return Err(STOPPED.into());
        }
        if self.quiet.load(Ordering::Relaxed) && command != "do"
            && let Some(msg) = quiet_blocked(command, &flags, None)
        {
            return Err(msg.into());
        }
        if !(command == "do" && flags.dry) && matches!(flags.risk.as_deref(), Some("medium" | "high")) && !flags.force {
            return Err("declared risky: ask the person, then add --force".into());
        }
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
                let Some(xid) = tracked.x11 else {
                    return Err("focusing another app's window on Wayland needs X11 (Wayland does not let one app raise or focus another)".into());
                };
                x::activate(xid)?;
                Ok("ok".into())
            }
            "close" => {
                let tracked = self.pick(who, window_spec())?;
                let Some(xid) = tracked.x11 else {
                    return Err("closing another app's window on Wayland needs X11".into());
                };
                x::close(xid).map(|_| "ok".to_string())
            }
            "place" => {
                let tracked = self.pick(who, flags.window.as_deref())?;
                let Some(xid) = tracked.x11 else {
                    return Err("moving or resizing another app's window on Wayland needs X11 (Wayland keeps window placement with the compositor)".into());
                };
                if args.is_empty() {
                    return Err("place x,y,w,h|min|max|restore|left|right|monitor N".into());
                }
                let how = args.join(" ");
                x::place(xid, &how).map(|_| "ok".to_string())
            }
            "clip" => match args {
                [] => Ok(x::clipboard()?.unwrap_or_else(|| "(no text on the clipboard)".into())),
                [set, text] if set == "set" => x::set_clipboard(Some(text)).map(|_| "ok".to_string()),
                [clear] if clear == "clear" => x::set_clipboard(None).map(|_| "ok".to_string()),
                _ => Err("clip, clip set \"text\", or clip clear".into()),
            },
            "screens" => {
                if !x::has_display() && x::has_wayland() {
                    return Err("listing monitors on Wayland needs X11 (Wayland keeps display layout private from apps)".into());
                }
                x::ready()?;
                Ok(x::screens()
                    .iter()
                    .enumerate()
                    .map(|(at, ((sx, sy, w, h), main))| format!("{at}: {sx},{sy} {w}x{h}{}", if *main { " primary" } else { "" }))
                    .collect::<Vec<_>>()
                    .join("\n"))
            }
            "apps" => {
                let mut names: Vec<String> = self.list_windows()?.into_iter().map(|w| w.app).collect();
                names.sort_unstable();
                names.dedup();
                Ok(names.join("\n"))
            }
            "up" => {
                let gone = self.release(true);
                Ok(if gone.is_empty() { "nothing was held".into() } else { format!("let go of held input by {}", gone.join(", ")) })
            }
            "cursor" => Ok(if x::has_display() && x::own_pointer() {
                "agents use background events and their own XInput 2 pointer; the person's cursor is untouched".into()
            } else {
                "agents use background accessibility and events; the person's cursor is untouched".into()
            }),
            "quiet" | "background-only" => match args.first().map(String::as_str) {
                None | Some("status") => Ok(if self.quiet.load(Ordering::Relaxed) {
                    "quiet: on (focus, --real, --mouse, --front, and down are refused)".into()
                } else {
                    "quiet: off".into()
                }),
                Some("on") => {
                    self.quiet.store(true, Ordering::Relaxed);
                    Ok("quiet: on (focus, --real, --mouse, --front, and down are refused)".into())
                }
                Some("off") => {
                    self.quiet.store(false, Ordering::Relaxed);
                    Ok("quiet: off".into())
                }
                _ => Err("quiet [on|off|status]".into()),
            },
            "table" => self.table(who, &flags, args),
            "notifications" => match args.first().map(String::as_str) {
                // Notifications are not a window here, so there is nothing to close.
                Some("close") => Ok("ok".into()),
                _ => super::notifications::list(),
            },
            "resume" => {
                STOP.store(false, Ordering::Relaxed);
                Ok("ok".into())
            }
            "quit" => {
                self.release(true);
                if !self.unlinked.swap(true, Ordering::AcqRel) {
                    pipe::remove();
                }
                self.quit.store(true, Ordering::Release);
                self.wake.notify_all();
                Ok("bye".into())
            }
            "do" => {
                let script = args.join(" ");
                let steps: Vec<Step> = lines::split_steps(&script)
                    .into_iter()
                    .map(|raw| {
                        let (flags, words) = parse(&raw);
                        Step { flags, words }
                    })
                    .collect();
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
                for step in &steps {
                    if let Some(error) = &step.flags.flag_error {
                        return Err(error.clone());
                    }
                    if step.flags.window.is_some() {
                        return Err("a step cannot pick another window: a do runs in one, so run another command for that window".into());
                    }
                    if matches!(step.flags.risk.as_deref(), Some("medium" | "high")) && !flags.force && !step.flags.force {
                        return Err("declared risky: ask the person, then add --force".into());
                    }
                }
                if self.quiet.load(Ordering::Relaxed) {
                    if let Some(msg) = quiet_blocked("do", &flags, Some(&steps)) {
                        return Err(msg.into());
                    }
                    for step in &steps {
                        let step_verb = step.words.first().map_or("", String::as_str);
                        if let Some(msg) = quiet_blocked(step_verb, &step.flags, None) {
                            return Err(msg.into());
                        }
                    }
                }
                self.act(who, &flags, &with_expect(steps))
            }
            "click" if args.len() > 1 => {
                let steps = args.iter().map(|word| Step { flags: Flags::default(), words: vec!["click".to_string(), word.clone()] }).collect();
                self.act(who, &flags, &with_expect(steps))
            }
            "click" | "hover" | "set" | "clear" | "check" | "uncheck" | "toggle" | "expand" | "collapse" | "show" | "select" | "type" | "key"
            | "down" | "scroll" | "drag" | "draw" | "wait" | "pick-file" => {
                let mut words = vec![command.clone()];
                words.extend(args.iter().cloned());
                self.act(who, &flags, &with_expect(vec![Step { flags: Flags::default(), words }]))
            }
            other => Err(format!("unknown command '{other}': deskcrew help")),
        }
    }

    fn serve(&self, pipe: Pipe, _active: ActiveGuard<'_>) {
        let Ok(message) = pipe.receive() else { return };
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
        active: AtomicUsize::new(0),
        quiet: AtomicBool::new(env_quiet()),
        quit: AtomicBool::new(false),
        unlinked: AtomicBool::new(false),
        wake_lock: Mutex::new(()),
        wake: Condvar::new(),
        overlay: Overlay::start(),
    });
    super::notifications::watch();
    let server = service.clone();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let worker = server.clone();
            thread::spawn(move || {
                let active = ActiveGuard::enter(&worker.active, &worker.last_used, &worker.wake);
                worker.serve(Pipe::from_stream(stream), active);
            });
        }
    });
    // Ctrl+Alt+Esc, read from the key state: nothing is grabbed, so the person's keys are untouched.
    if x::has_display() {
        thread::spawn(|| {
            loop {
                if x::stop_keys_down() {
                    STOP.store(true, Ordering::Relaxed);
                }
                thread::sleep(STOP_WATCH);
            }
        });
    }
    loop {
        let guard = service.wake_lock.lock().unwrap();
        let (guard, _) = service.wake.wait_timeout(guard, Duration::from_millis(250)).unwrap();
        drop(guard);
        service.release(false);
        let active = service.active.load(Ordering::SeqCst);
        let idle = service.last_used.lock().unwrap_or_else(|e| e.into_inner()).elapsed();
        if service.quit.load(Ordering::Acquire) || (active == 0 && idle > IDLE_EXIT) || idle > HUNG_EXIT {
            break;
        }
    }
    if !service.unlinked.swap(true, Ordering::AcqRel) {
        pipe::remove();
        // Give the accept thread a moment to take any connection already queued on the socket.
        thread::sleep(Duration::from_millis(20));
    }
    let drain_start = Instant::now();
    while service.active.load(Ordering::SeqCst) > 0 && drain_start.elapsed() < Duration::from_secs(2) {
        let guard = service.wake_lock.lock().unwrap();
        let _ = service.wake.wait_timeout(guard, Duration::from_millis(25)).unwrap();
    }
    service.release(true);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_mode_blocks_focus_and_real_input() {
        let plain = Flags::default();
        assert!(quiet_blocked("focus", &plain, None).is_some());
        assert!(quiet_blocked("down", &plain, None).is_some());
        assert!(quiet_blocked("click", &Flags { real: true, ..Flags::default() }, None).is_some());
        assert!(quiet_blocked("click", &Flags { mouse: true, ..Flags::default() }, None).is_some());
        assert!(quiet_blocked("launch", &Flags { front: true, ..Flags::default() }, None).is_some());
        assert!(quiet_blocked("click", &plain, None).is_none());
        assert!(quiet_blocked("type", &Flags { paste: true, ..Flags::default() }, None).is_none());
        let step_real = vec![Step { flags: Flags { real: true, ..Flags::default() }, words: vec!["click".into(), "e1".into()] }];
        assert!(quiet_blocked("do", &plain, Some(&step_real)).is_some());
    }
}
