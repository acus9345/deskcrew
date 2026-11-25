// The background service. It keeps the cursors on screen between commands, remembers each
// window's refs, and runs agents side by side: one queue per window, one for the real mouse.
use crate::borrow::{CallerLookup, Permission};
use crate::real::{Button, Loan, Modifiers, MouseButton};
use crate::{
    act::{self, NEEDS_REAL},
    apps,
    lines::{self, Seen},
    look, motion,
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

/// What a picture of a window the agent has not read yet says: reading is far cheaper.
const READ_FIRST: &str = "\nnot read yet: see gives this window's controls and its text with x,y to click, with no picture. Shots are for looking at drawings, not for reading text or checking what was typed.";
const IDLE_EXIT: Duration = Duration::from_secs(30 * 60);
/// If a request hangs forever so `active` never returns to 0, still exit instead of holding the stop hotkey all day.
const HUNG_EXIT: Duration = Duration::from_secs(60 * 60);
const SETTLE_FIRST: Duration = Duration::from_millis(50);
const SETTLE_STEP: Duration = Duration::from_millis(60);
const SETTLE_LEAST: Duration = Duration::from_millis(150);
const SETTLE_MOST: Duration = Duration::from_millis(800);
const BETWEEN_STEPS: Duration = Duration::from_millis(40);
const NAME_WAIT: Duration = Duration::from_millis(600);
const HOLD_MOST_MS: u64 = 10_000;
const MODIFIER_SETTLE: Duration = Duration::from_millis(80);
const PASTE_SETTLE: Duration = Duration::from_millis(120);
const NOTIFICATIONS_OPEN: Duration = Duration::from_secs(2);
const NOTIFICATIONS_SETTLE: Duration = Duration::from_millis(500);
const FIELD_SETTLE: Duration = Duration::from_millis(60);
const SHARED_PIECE: usize = 10;
const SHARED_OVERLAP: usize = 4;
const SHARED_GAP: Duration = Duration::from_millis(3);
const TYPE_CHECK: Duration = Duration::from_millis(350);
const FOCUS_WATCH: Duration = Duration::from_millis(2);
/// How long the keeper stays on watch after the last action: apps can reach for the front a
/// good while after they were pressed.
const FOCUS_LINGER: Duration = Duration::from_secs(3);
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
/// How much of a table one reply carries: a few thousand characters, however many rows that is.
const TABLE_CHARS: usize = 6000;
const DIALOG_CLOSE: Duration = Duration::from_secs(3);
const MAX_TABLE_ROWS: usize = 500;
const MAX_TABLE_COLUMNS: usize = 20;
const MAX_CELL_CHARS: usize = 60;
const STOPPED: &str = "stopped by the person with Ctrl+Alt+Esc: ask them before doing more (deskcrew resume clears it)";
const MINIMIZED_POINT: &str = "refused: this window is minimized, and Windows would not let deskcrew show it off screen without it coming forward just now, so x,y are nowhere on screen. Use a control by name, or ask the person to restore it";
const COVERED: &str ="refused: this window is minimized, and browser and Electron apps take no clicks or keys until they are shown again. Windows would not let deskcrew show it without it coming forward just now. Ask the person to restore it, or add --real";
/// Commands that act on a window, as opposed to reading it.
const ACTIONS: [&str; 18] = ["click", "hover", "set", "type", "key", "check", "uncheck", "toggle", "expand", "collapse", "clear", "select", "scroll", "show", "drag", "draw", "down", "pick-file"];
/// What the pen route cannot do without touching the person's keyboard or mouse.
const PEN_LIMITS: &str = "refused: this app takes drawing from deskcrew's pen, which has no middle button and cannot hold Shift, Ctrl or Alt without pressing your real keys. Drop --with or --button middle, or use --mouse";
const DRAW_USAGE: &str = "draw [control] \"x,y x,y; rect x,y w,h; circle x,y r\"";
/// Said when nothing is found and deskcrew runs on another desktop than the one on screen.
fn out_of_sight() -> String {
    match crate::pipe::desktop() {
        name if name.is_empty() || name.eq_ignore_ascii_case("Default") => String::new(),
        name => format!(
            "\ndeskcrew runs on the Windows desktop '{name}', not the one on screen, so it cannot see the windows there. Agent apps such as Antigravity run commands in a sandbox desktop like this: use deskcrew through the app's MCP tools instead. deskcrew setup adds them to Antigravity; restart it after."
        ),
    }
}

const NO_WINDOW: &str = "no window yet: run deskcrew windows, then deskcrew see <name>";

pub const HELP: &str = r#"deskcrew drives Windows apps in the background. Fastest: see a window once, then do every step in one call.
windows [filter]                      list windows
see [window] [-f] [-a] [--find word] [--from N]
                                      controls; later reads show only changes (-f for all, -a off-screen too)
get e3                                a control's full text
shot [window|e3] [-f] [--marks] [--rulers] [--screen [-a]] [--region x,y,w,h] [--changed] [--edge N]
                                      PNG path; "unchanged" if it matches the last one
click e3 [e4 ...] | "Name" | x,y [--right] [--double] [--triple] [--middle] [--back] [--forward] [--with ctrl+shift] [--hold ms]
hover e3 | x,y     check e3     uncheck e3     toggle e3     expand e3     collapse e3     clear e3
is e3 on|off|open|closed|selected|enabled|disabled
down shift --real | down left x,y --mouse     up (lets go; 30 s limit)
set e3 "value"     type [e3] "text" [--paste]     key enter [--hold ms --real]     select e3 "Item"
scroll [e3] up|down|left|right [n]     show e3     wait "text" [--gone] [--ms N]
table e5 [--from N]                   a table or grid, as many rows as fit     pick-file "path"   fill an Open or Save dialog
drag a b     draw [e3] "x,y x,y; x,y x,y" [--smooth]     both take [--button right|middle] [--with shift|ctrl|alt]
  shapes in draw: line a b | rect x,y w,h | circle x,y r | ellipse x,y rx,ry | arc x,y r from to | bezier a b c d | poly a b c [--close]; --fill fills rect and circle
do "set Name Ada; select Country Japan; check 'Gift wrap'; click Submit; click Yes" [--dry] [--risk low|medium|high]
                                      many steps, one reply; names work as refs, and it goes on into dialogs it opens
Any action: --expect "text" fails unless the text shows afterwards.
launch app [args] [--front]     focus     close     place x,y,w,h|min|max|restore|left|right|monitor N
apps     screens     notifications [close]     clip     clip set "text"     clip clear     status     resume     quit     setup (installs for your coding agents)     mcp (the same as MCP tools, over stdio)
Every action replies with what changed, new dialogs and their text too: no need to see again or sleep. x,y are pixels in the last shot, or text see read. Ctrl+Alt+Esc stops every agent.
Flags: -w window, --agent name, --fast (no glide), --force (allow risky keys),
--real (brings the window forward for a moment and uses the keyboard or a pen: a last resort),
--mouse (asks the person to borrow their cursor, if they allow asking: only if nothing else works).
cursor [off|ask|session|always]   who may borrow the cursor; off by default, and the person confirms anything else.
quiet [on|off|status]             hard background-only lock: refuses focus, --real, --mouse, --front, and down."#;

/// Add text to the end of a control through the value it reports, for a control that takes no
/// window messages. None if it has no value that can be set.
fn append_value(node: &Node, root: Hwnd, text: &str) -> Option<Reply> {
    if !node.can.value || node.element.value_read_only() {
        return None;
    }
    let wanted = format!("{}{text}", node.element.value_text());
    Some(act::set(node, root, &wanted).and_then(|route| {
        if node.element.value_text() == wanted { Ok(route) } else { Err("the text did not land: try --real".into()) }
    }))
}

/// Replace all the text of the browser field that has the page's focus with Ctrl+A and a lent
/// clipboard's Ctrl+V, as window messages, and read it back. Chromium can drop typed characters
/// while its window is in the back; accessibility would bring the window forward instead. None if
/// the text did not land.
fn replace_in_page(root: Hwnd, node: &Node, wanted: &str) -> Option<Reply> {
    let mut saved = apps::SavedClipboard::take().ok()?;
    saved.lend(wanted).ok()?;
    let _wake = act::wake_page(root);
    let _ = real::chord("ctrl+a").ok().and_then(|all| act::post_key(root, &all, "ctrl+a").ok());
    let sent = real::chord("ctrl+v").ok().and_then(|paste| act::post_key(root, &paste, "ctrl+v").ok());
    let until = Instant::now() + TYPE_CHECK;
    let matches = || node.element.field_text().as_deref() == Some(wanted);
    while sent.is_some() && !matches() && Instant::now() < until {
        thread::sleep(SETTLE_STEP);
    }
    let landed = sent.is_some() && matches();
    if let Err(error) = saved.put_back() {
        return Some(Err(error));
    }
    landed.then(|| Ok("paste message".to_string()))
}

/// The smallest control under a screen point that can be pressed: a button, check box, tab, list
/// item or the like. None where the spot holds only a drawing surface.
fn control_at(view: &View, x: i32, y: i32) -> Option<&Node> {
    let inside = |&(left, top, width, height): &Bounds| width > 0 && height > 0 && x >= left && x < left + width && y >= top && y < top + height;
    view.nodes
        .values()
        .filter(|node| inside(&node.rect) && (node.can.invoke || node.can.toggle || node.can.select || node.can.expand))
        .filter(|node| !node.parts.states.contains(&"disabled"))
        .min_by_key(|node| node.rect.2 as i64 * node.rect.3 as i64)
}

/// A press that needs the window in front, by the quietest way this spot of the app accepts.
fn press_at(root: Hwnd, x: i32, y: i32, flags: &Flags) -> Reply {
    if flags.mouse {
        real::click(x, y, flags.right, flags.double);
        return Ok("mouse".into());
    }
    if !act::needs_pen_at(root, x, y) {
        return act::post_click(root, x, y, flags.right, flags.double);
    }
    let pen = Pen::new().ok_or(act::NO_PEN)?;
    if pen.tap(x, y, flags.right, flags.double) { Ok("pen".into()) } else { Err(act::NO_PEN.into()) }
}

/// Read `DESKCREW_QUIET` at startup (`1`, `true`, `on`, `yes`).
fn env_quiet() -> bool {
    std::env::var("DESKCREW_QUIET")
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "on" | "yes"))
        .unwrap_or(false)
}

/// In quiet / background-only mode, refuse any command or flag that steals focus or borrows the
/// person's real mouse/keyboard.
fn quiet_blocked(verb: &str, args: &[String], flags: &Flags, steps: Option<&[Step]>) -> Option<&'static str> {
    if verb == "focus" {
        return Some("refused: quiet mode is on (focus is disabled; run 'deskcrew quiet off' to allow)");
    }
    if verb == "down" {
        return Some("refused: quiet mode is on (down borrows real input; run 'deskcrew quiet off' to allow)");
    }
    if verb == "notifications" && args.first().map(String::as_str) != Some("close") {
        return Some("refused: quiet mode is on (notifications opens Win+N in front; run 'deskcrew quiet off' to allow)");
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
        // Controls the app kept claim their numbers first, so a new control with the same role
        // and name never takes the number of one further down the window.
        let mut taken = std::collections::HashSet::new();
        let claimed: Vec<Option<u32>> = nodes
            .iter()
            .map(|node| self.by_runtime.get(&node.runtime).copied().filter(|id| !node.runtime.is_empty() && taken.insert(*id)))
            .collect();
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
    // The last look at the part with no controls: its picture's fingerprint and what it said.
    blind: Option<(u64, Vec<String>)>,
    // Where the captions left out of the list sit: they are not a part with no controls.
    quiet: Vec<Bounds>,
}

struct Tracked {
    id: u32,
    hwnd: Hwnd,
    identity: (u32, u64),
    content_identity: Mutex<Option<(u32, u64)>>,
    view: Mutex<View>,
    pointer: Mutex<()>,
    // Agents drawing in this window right now. Strokes are cut into turns only when there are two.
    drawers: AtomicUsize,
}

impl Tracked {
    fn same_window(&self) -> bool {
        if win::identity(self.hwnd) != self.identity {
            return false;
        }
        let now_content = win::content_identity(self.hwnd);
        let mut stored = self.content_identity.lock().unwrap();
        match (*stored, now_content) {
            (Some(old), Some(now)) => old == now,
            (None, Some(now)) => {
                *stored = Some(now);
                true
            }
            _ => true,
        }
    }
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

#[derive(Clone, Default)]
struct Flags {
    real: bool,
    mouse: bool,
    fast: bool,
    force: bool,
    full: bool,
    all: bool,
    right: bool,
    double: bool,
    triple: bool,
    middle: bool,
    back: bool,
    forward: bool,
    paste: bool,
    dry: bool,
    marks: bool,
    rulers: bool,
    gone: bool,
    changed: bool,
    screen: bool,
    front: bool,
    smooth: bool,
    fill: bool,
    close: bool,
    window: Option<String>,
    find: Option<String>,
    from: usize,
    ms: Option<u64>,
    edge: Option<u32>,
    hold: Option<u64>,
    region: Option<String>,
    with: Option<String>,
    button: Option<MouseButton>,
    flag_error: Option<String>,
    expect: Option<String>,
    risk: Option<String>,
}

impl Flags {
    fn button(&self) -> Button {
        match (self.middle, self.back, self.forward, self.right) {
            (true, ..) => Button::Middle,
            (_, true, ..) => Button::Back,
            (_, _, true, _) => Button::Forward,
            (.., true) => Button::Right,
            _ => Button::Left,
        }
    }

    fn mouse_button(&self) -> MouseButton {
        if let Some(button) = self.button {
            button
        } else if self.middle {
            MouseButton::Middle
        } else if self.right {
            MouseButton::Right
        } else {
            MouseButton::Left
        }
    }

    fn modifiers(&self) -> Result<Modifiers, String> {
        self.with.as_deref().map(Modifiers::parse).unwrap_or(Ok(Modifiers::NONE))
    }
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
                "--triple" => flags.triple = true,
                "--middle" => flags.middle = true,
                "--back" => flags.back = true,
                "--forward" => flags.forward = true,
                "--paste" => flags.paste = true,
                "--dry" => flags.dry = true,
                "--marks" => flags.marks = true,
                "--rulers" => flags.rulers = true,
                "--with" => {
                    take_text(&mut flags.with, next, word, &mut flags.flag_error);
                    took_next = true;
                }
                "--button" => {
                    match next.as_deref().and_then(MouseButton::named) {
                        Some(button @ (MouseButton::Left | MouseButton::Right | MouseButton::Middle)) => flags.button = Some(button),
                        _ => {
                            flags.flag_error.get_or_insert_with(|| "--button takes left, right or middle".into());
                        }
                    }
                    took_next = true;
                }
                "--expect" => {
                    take_text(&mut flags.expect, next, word, &mut flags.flag_error);
                    took_next = true;
                }
                "--risk" => {
                    match next.as_deref().map(str::to_ascii_lowercase).as_deref() {
                        Some("low" | "medium" | "high") => flags.risk = next.map(|v| v.to_ascii_lowercase()),
                        _ => {
                            flags.flag_error.get_or_insert_with(|| "--risk takes low, medium or high".into());
                        }
                    }
                    took_next = true;
                }
                "--gone" => flags.gone = true,
                "--changed" => flags.changed = true,
                "--screen" => flags.screen = true,
                "--front" => flags.front = true,
                "--smooth" => flags.smooth = true,
                "--fill" => flags.fill = true,
                "--close" => flags.close = true,
                "-w" | "--window" => {
                    take_text(&mut flags.window, next, word, &mut flags.flag_error);
                    took_next = true;
                }
                "--find" => {
                    take_text(&mut flags.find, next, word, &mut flags.flag_error);
                    took_next = true;
                }
                "--from" => {
                    flags.from = take_num(next, word, "a number", &mut flags.flag_error).unwrap_or(0);
                    took_next = true;
                }
                "--ms" => {
                    flags.ms = take_num(next, word, "milliseconds", &mut flags.flag_error);
                    took_next = true;
                }
                "--hold" => {
                    flags.hold = take_num(next, word, "milliseconds", &mut flags.flag_error);
                    took_next = true;
                }
                "--region" => {
                    take_text(&mut flags.region, next, word, &mut flags.flag_error);
                    took_next = true;
                }
                "--edge" => {
                    flags.edge = take_num(next, word, "pixels", &mut flags.flag_error);
                    took_next = true;
                }
                _ => rest.push(word.to_string()),
            }
        }
        at += if took_next { 2 } else { 1 };
    }
    (flags, rest)
}

/// The roles worth numbering on a picture: things an agent would act on.
// Roles that hold other controls, and so may be a canvas rather than something covering its space.
const CONTAINERS: [&str; 8] = ["list", "tree", "table", "menu", "tabs", "toolbar", "pane", "group"];
// A change thinner than this is a blinking caret, not a result.
const LEAST_CHANGE: u32 = 4;
/// How often a caret is looked for while typing into a control that is not a classic edit box.
const CARET_WATCH: Duration = Duration::from_millis(15);
/// The longest a caret waits for a typed letter to land in its box.
const CARET_SETTLE: Duration = Duration::from_millis(30);
const MARKED: [&str; 11] = ["button", "edit", "menuitem", "checkbox", "radio", "combobox", "tab", "item", "link", "slider", "spinner"];

#[derive(Default)]
struct Step {
    flags: Flags,
    words: Vec<String>,
}

/// The step that checks `--expect`: it always waits for the text, even text such as "42" that
/// `wait` would read as milliseconds.
fn expected(flags: &Flags) -> Option<Step> {
    flags.expect.as_ref().map(|text| Step { flags: Flags::default(), words: vec!["expect".to_string(), text.clone()] })
}

fn sleep_ms(text: &str, flags: &Flags) -> Option<u64> {
    (!flags.gone).then(|| text.parse::<u64>().ok()).flatten()
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
    borrow: Permission,
    home: Mutex<Home>,
    keeper: OnceLock<thread::Thread>,
    stop: Arc<AtomicBool>,
    quiet: AtomicBool,
    last_request: Mutex<Instant>,
    active: AtomicUsize,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum MouseHold {
    #[default]
    Off,
    Starting,
    Held,
}

#[derive(Default)]
struct Home {
    agents: usize,
    guard: Option<FocusGuard>,
    pids: Vec<u32>,
    leaving: Option<Instant>,
    on_loan: bool,
    /// State while a mouse button is being held down via `down left x,y --mouse`.
    holding_mouse: MouseHold,
    /// The notification panel focus is lent to: the loan ends when it closes, however it closes.
    panel: Option<Hwnd>,
    /// The person's window from the last time agents worked, kept after the keeper goes idle.
    person: Option<Hwnd>,
}

fn click_flags(flags: &Flags) -> bool {
    flags.right || flags.double || flags.triple || flags.middle || flags.back || flags.forward || flags.hold.is_some() || flags.with.is_some() || flags.button.is_some()
}

fn reject_step_flags(verb: &str, flags: &Flags, extra: Option<&Flags>) -> Result<(), String> {
    let any = |check: fn(&Flags) -> bool| check(flags) || extra.is_some_and(check);
    if verb == "select" && any(|f| f.real || f.mouse) {
        return Err("select does not take --real or --mouse: click the dropdown and item instead".into());
    }
    if matches!(verb, "check" | "uncheck" | "expand" | "collapse") && any(click_flags) {
        return Err("click flags (--right, --double, --hold, --with, etc.) belong on click".into());
    }
    Ok(())
}

fn node_disabled(node: &Node) -> bool {
    node.element.is_enabled().map(|enabled| !enabled).unwrap_or_else(|| node.parts.states.contains(&"disabled"))
}

fn store_control(root: Hwnd, node: &Node, surfaces: &[Hwnd], (x, y): (i32, i32)) -> bool {
    if !win::is_store(root) {
        return false;
    }
    if node.native != 0 {
        return win::draws_itself(node.native);
    }
    if win::draws_itself(root) {
        return true;
    }
    let child = win::child_at(root, x, y).0;
    if child != 0 && child != root {
        return win::draws_itself(child);
    }
    surfaces.iter().any(|&surface| win::inside(surface, x, y) && win::draws_itself(surface))
}

impl Service {
    fn track(&self, hwnd: Hwnd) -> Arc<Tracked> {
        win::note_worked_in(hwnd);
        let mut windows = self.windows.lock().unwrap();
        if let Some(found) = windows.1.get(&hwnd).filter(|found| found.same_window()) {
            return found.clone();
        }
        let identity = win::identity(hwnd);
        let content_identity = win::content_identity(hwnd);
        windows.0 += 1;
        let tracked = Arc::new(Tracked {
            id: windows.0,
            hwnd,
            identity,
            content_identity: Mutex::new(content_identity),
            view: Mutex::new(View::default()),
            pointer: Mutex::new(()),
            drawers: AtomicUsize::new(0),
        });
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
            let reused = self.windows.lock().unwrap().1.get(&current).is_some_and(|known| !known.same_window());
            if !win::exists(current) || reused {
                return Err("that window closed: run deskcrew windows".into());
            }
            return Ok(self.track(current));
        };
        if let Some(id) = spec.strip_prefix('w').and_then(|n| n.parse::<u32>().ok()) {
            let found = self.by_id(id).filter(|t| win::exists(t.hwnd) && t.same_window()).ok_or(format!("no window {spec}: run deskcrew windows"))?;
            self.set_current(who, found.hwnd);
            return Ok(found);
        }
        let wanted = spec.to_lowercase();
        let all = win::list();
        let exact: Vec<_> = all.iter().filter(|w| w.title.to_lowercase() == wanted || w.app.to_lowercase() == wanted).collect();
        let mut matches: Vec<_> = if exact.is_empty() {
            all.iter().filter(|w| w.title.to_lowercase().contains(&wanted) || w.app.to_lowercase().contains(&wanted)).collect()
        } else {
            exact
        };
        if matches.len() > 1 {
            // Several match: the one the agent is already working in.
            let current = self.agents.lock().unwrap().get(who).and_then(|a| a.current);
            if let Some(mine) = matches.iter().find(|w| Some(w.hwnd) == current) {
                matches = vec![*mine];
            }
        }
        match matches.as_slice() {
            [] => Err(format!("no window matches '{spec}'{}", out_of_sight())),
            [only] => {
                // A dialog already read as part of its window is worked from that window, so its
                // refs hold and the reply shows what the dialog changed there.
                if let Some(owner) = self.owner_of(only.hwnd) {
                    self.set_current(who, owner.hwnd);
                    return Ok(owner);
                }
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

    /// The window this one was read as a dialog or popup of, if any.
    fn owner_of(&self, hwnd: Hwnd) -> Option<Arc<Tracked>> {
        let windows: Vec<Arc<Tracked>> = self.windows.lock().unwrap().1.values().cloned().collect();
        windows.into_iter().find(|t| t.hwnd != hwnd && t.view.try_lock().is_ok_and(|view| view.surfaces.contains(&hwnd)))
    }

    /// Read the window and any popups or dialogs that belong to it.
    fn read(&self, tracked: &Tracked, view: &mut View, all: bool) -> Result<Vec<String>, String> {
        if !win::exists(tracked.hwnd) {
            return Err("window closed".into());
        }
        if view.pids.is_empty() {
            view.pids = win::app_pids(tracked.hwnd);
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
        let mut quiet = Vec::new();
        self.uia.read(tracked.hwnd, all, &mut nodes, &mut hidden, &mut quiet)?;
        for hwnd in view.surfaces.clone() {
            let before = nodes.len();
            if self.uia.read(hwnd, all, &mut nodes, &mut hidden, &mut quiet).is_err() {
                nodes.truncate(before);
            }
        }
        // Windows lists a dialog under its owner too, so the same control can arrive twice.
        let mut met = HashSet::new();
        nodes.retain(|node| node.runtime.is_empty() || met.insert(node.runtime.clone()));
        view.quiet = quiet;
        view.nodes = view.refs.assign(nodes);
        view.lines = view.nodes.iter().map(|(id, node)| (*id, lines::line(&node.parts))).collect();
        view.hidden = hidden;
        Ok(notes)
    }

    /// Chromium builds its tree on first request, so the first read comes back nearly empty.
    fn first_read(&self, tracked: &Tracked, view: &mut View, all: bool) -> Result<(), String> {
        self.read(tracked, view, all)?;
        if !view.read_once && win::is_chromium(tracked.hwnd) {
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
        home.agents += 1;
        home.leaving = None;
        home.pids.extend_from_slice(pids);
        win::note_worked(pids);
        if home.guard.is_none() {
            let mut guard = FocusGuard::take();
            // An app agents work in is never the person's window, even when it is in front: it
            // took focus at some point, and the person's last window gets it back.
            if guard.is_one_of(&home.pids) && let Some(person) = home.person.filter(|hwnd| win::exists(*hwnd)) {
                guard = FocusGuard::of(person);
                guard.restore(&home.pids);
            }
            if !guard.is_one_of(&home.pids) {
                home.person = Some(guard.front());
            }
            home.guard = Some(guard);
            if let Some(keeper) = self.keeper.get() {
                keeper.unpark();
            }
        }
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
                if home.on_loan && home.panel.is_some_and(|panel| win::foreground() != panel) {
                    home.on_loan = false;
                    home.panel = None;
                }
                if home.holding_mouse == MouseHold::Held && !real::holding_button() {
                    home.holding_mouse = MouseHold::Off;
                }
                let paused = home.on_loan || home.holding_mouse != MouseHold::Off;
                let held = home.guard.clone().filter(|_| !paused).map(|guard| (guard, home.pids.clone()));
                if home.agents == 0 && home.leaving.is_some_and(|at| at.elapsed() > FOCUS_LINGER) {
                    home.guard = None;
                    home.pids.clear();
                    home.leaving = None;
                }
                held
            };
            match held {
                Some((guard, pids)) => {
                    // A window of the app the person picks themself becomes theirs; anything the
                    // app pulls forward on its own goes back behind.
                    match guard.chosen(&pids) {
                        Some(theirs) => {
                            let mut home = self.home.lock().unwrap();
                            home.guard = Some(FocusGuard::of(theirs));
                            home.person = Some(theirs);
                        }
                        None => {
                            guard.restore(&pids);
                        }
                    }
                    thread::sleep(FOCUS_WATCH);
                }
                None if {
                    let home = self.home.lock().unwrap();
                    home.on_loan || home.holding_mouse != MouseHold::Off
                } =>
                {
                    thread::sleep(FOCUS_WATCH)
                }
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

    /// Where the agent's last shot of this window lies on screen now. Shots keep their place in the
    /// window, so x,y still fit after the window moves, or while a minimized one is shown off screen.
    fn shot_frames(&self, who: &str, tracked: &Tracked) -> Option<Frames> {
        let kept = self.agents.lock().unwrap().get(who).and_then(|a| a.shots.get(&tracked.hwnd).map(|s| s.frames))?;
        let (left, top, ..) = win::rect(tracked.hwnd);
        Some(Frames { x: kept.x + left, y: kept.y + top, ..kept })
    }

    fn resolve(&self, word: &str, who: &str, tracked: &Tracked, view: &mut View) -> Result<Target, String> {
        self.resolve_for(word, who, tracked, view, |_| true)
    }

    /// A control by ref, name or point. When a name fits several, the one that can take the action is meant.
    fn resolve_for(&self, word: &str, who: &str, tracked: &Tracked, view: &mut View, fits: impl Fn(&Node) -> bool) -> Result<Target, String> {
        if let Some(id) = is_ref(word) {
            if !view.nodes.contains_key(&id) {
                self.read(tracked, view, false)?;
            }
            return if view.nodes.contains_key(&id) { Ok(Target::Control(id)) } else { Err(format!("e{id} is gone: see")) };
        }
        if let Some((x, y)) = is_point(word) {
            let frames = self.shot_frames(who, tracked).ok_or("x,y are pixels in a shot: take one first")?;
            let (x, y) = frames.to_screen(x, y);
            if !win::inside(tracked.hwnd, x, y) {
                return Err(if win::minimized(tracked.hwnd) { MINIMIZED_POINT.into() } else { format!("refused: {word} is not on this window: x,y are pixels in its last shot") });
            }
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
            } else if !hits.iter().any(|id| fits(&view.nodes[id])) {
                // A label named exactly so, while a longer name belongs to a control that can take
                // the action, such as a menu entry "Gemini 4 Hydrogen Extra New": that one is meant.
                let able: Vec<u32> = named(false).into_iter().filter(|id| fits(&view.nodes[id])).collect();
                if !able.is_empty() {
                    hits = able;
                }
            }
            match hits.as_slice() {
                [only] => return Ok(Target::Control(*only)),
                [] => {
                    if let Some(entry) = win::find_menu(win::menu_bar(tracked.hwnd), word) {
                        return Ok(Target::Menu(entry));
                    }
                    let patience = if win::is_chromium(tracked.hwnd) { CHROMIUM_NAME_WAIT } else { NAME_WAIT };
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
                    let able: Vec<u32> = acting.iter().copied().filter(|id| fits(&view.nodes[id])).collect();
                    // A control and the shell around it: the inner one does the work.
                    let inner: Vec<u32> = able.iter().copied().filter(|id| !able.iter().any(|other| uia::encloses(view.nodes[id].rect, view.nodes[other].rect))).collect();
                    if let [only] = inner.as_slice() {
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
    fn with_front<T>(&self, tracked: &Tracked, view: &View, points: &[(i32, i32)], work: impl FnOnce() -> T) -> Result<T, String> {
        self.front(tracked, view, points, Loan::Keys, work)
    }

    /// Bring the window forward for this work. The cursor is only borrowed with `mouse`, which the
    /// person has to have allowed.
    fn front<T>(&self, tracked: &Tracked, view: &View, points: &[(i32, i32)], loan: Loan, work: impl FnOnce() -> T) -> Result<T, String> {
        self.front_inner(tracked, view, points, loan, true, work)
    }

    /// Run foreground-only work ONLY when the window is already in front — never brings it forward.
    fn front_if_already<T>(&self, tracked: &Tracked, view: &View, points: &[(i32, i32)], loan: Loan, work: impl FnOnce() -> T) -> Result<T, String> {
        self.front_inner(tracked, view, points, loan, false, work)
    }

    fn front_inner<T>(&self, tracked: &Tracked, view: &View, points: &[(i32, i32)], loan: Loan, allow_bring_forward: bool, work: impl FnOnce() -> T) -> Result<T, String> {
        if self.quiet.load(Ordering::Relaxed) {
            return Err("refused: quiet mode is on (real keyboard/pointer input is disabled; run 'deskcrew quiet off' to allow)".into());
        }
        // A point off the window, or off every screen, is refused before focus moves at all.
        let mut family = view.surfaces.clone();
        family.push(tracked.hwnd);
        if let Some((x, y)) = points.iter().find(|(x, y)| !win::on_screen(*x, *y) || !family.iter().any(|w| win::inside(*w, *x, *y))) {
            return Err(format!("refused: {x},{y} on screen is not on this window, so a real press there would hit something else"));
        }
        let _turn = self.real_input.lock().unwrap();
        // The focus keeper stands aside while focus is on loan, or it would take it straight back.
        {
            let mut home = self.home.lock().unwrap();
            home.on_loan = true;
            home.panel = None;
        }
        let result = (|| {
            let _borrowed = if allow_bring_forward {
                real::borrow(tracked.hwnd, &view.pids, loan)?
            } else {
                real::borrow_if_front(tracked.hwnd, &view.surfaces, &view.pids, loan)?
            };
            if let Some((x, y)) = points.iter().find(|(x, y)| !win::owns_point(&family, *x, *y)) {
                return Err(format!("refused: {x},{y} on screen is not on this window, so a real press there would hit something else"));
            }
            Ok(work())
        })();
        self.home.lock().unwrap().on_loan = false;
        result
    }

    fn click(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, word: &str) -> Reply {
        let extra = flags.triple || flags.middle || flags.back || flags.forward;
        let plain = !flags.right && !flags.double && !extra && flags.with.is_none();
        let (node, (x, y)) = match self.resolve(word, who, tracked, view)? {
            Target::Menu(entry) => return run_menu_entry(tracked.hwnd, &entry),
            Target::Point(x, y) => {
                self.cursor_on(who, flags, (x, y, 0, 0));
                (None, (x, y))
            }
            Target::Control(id) => {
                let (node, rect) = self.locate(id, tracked, view)?;
                if node_disabled(&node) {
                    return Err("that control is disabled".into());
                }
                self.cursor_on(who, flags, rect);
                (Some(node), act::centre(rect))
            }
        };
        if let Some(ms) = flags.hold {
            if flags.mouse || extra || flags.double || flags.with.is_some() {
                return Err("--hold is a long press of the left or right button: leave out the other click flags".into());
            }
            let hold = Duration::from_millis(ms.min(HOLD_MOST_MS));
            let press = || act::post_long_press(tracked.hwnd, x, y, flags.right, hold);
            return if flags.real { self.with_front(tracked, view, &[(x, y)], press)? } else { press() };
        }
        if flags.mouse {
            let times = if flags.triple { 3 } else if flags.double { 2 } else { 1 };
            let held = flags.with.as_deref().map(real::chord).transpose()?;
            if let (Some(chord), Some(spec)) = (&held, &flags.with) {
                refuse_risky(chord, spec, flags)?;
            }
            return self.front(tracked, view, &[(x, y)], Loan::Mouse, || {
                if let (Some(chord), Some(spec)) = (&held, &flags.with) {
                    refuse_risky(chord, spec, flags)?;
                }
                if let Some(chord) = &held {
                    real::set_keys(chord, true);
                }
                real::press_times(x, y, flags.button(), times);
                if let Some(chord) = &held {
                    real::set_keys(chord, false);
                }
                Ok("mouse".to_string())
            })?;
        }
        if let Some(with) = &flags.with {
            let chord = real::chord(with)?;
            refuse_risky(&chord, with, flags)?;
            let times = if flags.triple { 3 } else if flags.double { 2 } else { 1 };
            if !flags.real {
                return act::post_press(tracked.hwnd, x, y, flags.button(), times, chord.mouse_keys(), &chord.modifiers());
            }
            return self.with_front(tracked, view, &[(x, y)], || {
                refuse_risky(&chord, with, flags)?;
                real::set_keys(&chord, true);
                // Posted messages overtake real input, so the app has to see the modifier first.
                thread::sleep(MODIFIER_SETTLE);
                let result = act::post_press(tracked.hwnd, x, y, flags.button(), times, 0, &[]);
                thread::sleep(MODIFIER_SETTLE);
                real::set_keys(&chord, false);
                result
            })?;
        }
        if extra {
            let times = if flags.triple { 3 } else if flags.double { 2 } else { 1 };
            let post = || act::post_press(tracked.hwnd, x, y, flags.button(), times, 0, &[]);
            return if flags.real { self.with_front(tracked, view, &[(x, y)], post)? } else { post() };
        }
        if flags.real {
            return self.with_front(tracked, view, &[(x, y)], || press_at(tracked.hwnd, x, y, flags))?;
        }
        if let Some(node) = &node {
            if plain && let Some(result) = act::post_button(node, tracked.hwnd) {
                return result;
            }
            if plain && let Some(result) = act::flip_self_drawn(node) {
                return result;
            }
            if plain && node.parts.role == "menuitem" && let Some(entry) = win::find_menu(win::menu_bar(tracked.hwnd), &node.parts.name) {
                return run_menu_entry(tracked.hwnd, &entry);
            }
            // An entry of an open classic popup menu, such as a right-click menu, is picked with
            // its command, sent to the menu's owner: a click on it would need the real mouse.
            if plain && node.parts.role == "menuitem"
                && let Some(result) = win::popup_menus(&view.pids).into_iter().find_map(|popup| act::choose_in_popup(popup, &node.parts.name))
            {
                return result;
            }
        }
        // Accessibility makes a browser, and any app seen to do the same, pull its window over the
        // person's. A window message never does, covered or not.
        // Qt runs a dialog that a button or menu entry opens inside the accessibility call, and
        // answers no one until it is closed; a posted click returns at once. It goes to whichever
        // of the app's windows is on top there, such as a dialog or a menu over this one.
        let qt = win::is_qt(tracked.hwnd).then(|| win::own_window_at(tracked.hwnd, x, y));
        if let (Some(window), Some(node), true) = (qt, &node, plain)
            && matches!(node.parts.role.as_str(), "button" | "menuitem")
            && win::inside(window, x, y)
        {
            return act::post_click(window, x, y, flags.right, flags.double);
        }
        let store_control = node.as_ref().is_some_and(|n| store_control(tracked.hwnd, n, &view.surfaces, (x, y)));
        let by_message = win::is_chromium_at(tracked.hwnd, x, y)
            || (win::pulls_forward(&view.pids)
                && !win::draws_itself(tracked.hwnd)
                && !store_control
                && view.surfaces.is_empty()
                && node.as_ref().is_some_and(|node| node.parts.role != "menuitem"));
        let node = node.filter(|node| !act::is_classic_row(node, tracked.hwnd) && !by_message);
        if let (Some(node), true) = (&node, plain) {
            let classic = !act::needs_pen_at(tracked.hwnd, x, y) && !store_control;
            let (pressed, tried_mouse) = if classic {
                self.overlay.shielded(|| act::press_behind(node, tracked.hwnd))
            } else {
                (act::press_behind(node, tracked.hwnd), false)
            };
            if tried_mouse {
                return Err(act::NEEDS_MOUSE.into());
            }
            match pressed {
                Some(Ok(route)) => return Ok(route),
                Some(Err(error)) if error.contains("administrator") || store_control => return Err(error),
                _ => {}
            }
        }
        // A click on a WPF text box puts the caret there, for the keys that follow.
        if let (Some(node), true) = (&node, plain)
            && win::is_wpf(tracked.hwnd)
            && matches!(node.parts.role.as_str(), "edit" | "document" | "combobox")
        {
            return act::focus_wpf_field(tracked.hwnd, node);
        }
        // A spot in an app that draws itself takes no posted click, but the control drawn there
        // can still be pressed through its own action.
        if node.is_none() && plain && act::needs_pen_at(tracked.hwnd, x, y) {
            self.read(tracked, view, false)?;
            if let Some(found) = control_at(view, x, y)
                && let Some(pressed) = act::press_behind(found, tracked.hwnd)
            {
                return pressed.map(|route| format!("{route} on '{}' under {word}", found.parts.name));
            }
        }
        // WinUI 3 / Store / Paint coordinate clicks: synthetic pen input activates the target window
        // and borrows the cursor, so only inject without `--real` when the window (or one of its
        // surfaces) is already in front and uncovered, routed through `front_if_already(..., Loan::Pen)`
        // so `keep_focus`, `PEN_DRAIN`, and `quiet` mode stay completely synchronized.
        if act::needs_pen_at(tracked.hwnd, x, y) && !act::posts_with_capture(tracked.hwnd) && !win::minimized(tracked.hwnd) && win::inside(tracked.hwnd, x, y) {
            if self.quiet.load(Ordering::Relaxed) {
                return Err("refused: quiet mode is on (synthetic pen borrows the cursor; run 'deskcrew quiet off' to allow)".into());
            }
            let mut family = view.surfaces.clone();
            family.push(tracked.hwnd);
            if !win::front_is(tracked.hwnd, &view.surfaces) || !win::owns_point(&family, x, y) {
                return Err(act::NEEDS_REAL.into());
            }
            return self.front_if_already(tracked, view, &[(x, y)], Loan::Pen, || {
                let pen = Pen::new().ok_or(act::NO_PEN)?;
                if pen.tap(x, y, flags.right, flags.double) { Ok("pen".into()) } else { Err(act::NEEDS_REAL.into()) }
            })?;
        }
        act::post_click(qt.unwrap_or(tracked.hwnd), x, y, flags.right, flags.double)
    }

    /// Check, uncheck, expand or collapse, leaving a control that is already that way alone.
    fn ensure(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, word: &str, (wanted, opposite): (&str, &str)) -> Reply {
        if click_flags(flags) {
            return Err("click flags (--right, --double, --hold, --with, etc.) belong on click".into());
        }
        let Target::Control(id) = self.resolve_for(word, who, tracked, view, |n| {
            if matches!(wanted, "open" | "closed") { n.can.expand } else { n.can.toggle }
        })? else {
            return Err("that needs a control".into());
        };
        self.read(tracked, view, false)?;
        let states = view.nodes.get(&id).ok_or(format!("e{id} is gone: see"))?.parts.states.clone();
        if states.contains(&wanted) {
            return Ok(format!("already {wanted}"));
        }
        if !states.contains(&opposite) {
            return Err(format!("that control cannot be {wanted}"));
        }
        if flags.real || flags.mouse {
            return self.click(who, flags, tracked, view, &format!("e{id}"));
        }
        if matches!(wanted, "open" | "closed") {
            let (node, rect) = self.locate(id, tracked, view)?;
            if node_disabled(&node) {
                return Err("that control is disabled".into());
            }
            self.cursor_on(who, flags, rect);
            if wanted == "open"
                && let Some(opened) = self.open_submenu(tracked, view, id)
            {
                return opened;
            }
            let (x, y) = act::centre(rect);
            let classic = !act::needs_pen_at(tracked.hwnd, x, y) && !store_control(tracked.hwnd, &node, &view.surfaces, (x, y));
            let (expanded, tried_mouse) = if classic {
                self.overlay.shielded(|| act::expand_behind(&node, tracked.hwnd, wanted == "open"))
            } else {
                (act::expand_behind(&node, tracked.hwnd, wanted == "open"), false)
            };
            return if tried_mouse { Err(act::NEEDS_MOUSE.into()) } else { expanded };
        }
        self.click(who, flags, tracked, view, &format!("e{id}"))
    }

    /// Text put in through the clipboard, which is faster for a lot of it. The clipboard is put back.
    fn paste(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, words: &[String]) -> Reply {
        let (target, text) = match words {
            [text] => (None, text),
            [target, text] => (Some(target), text),
            _ => return Err("type [control] \"text\" --paste".into()),
        };
        if text.is_empty() {
            return Err("nothing to type: to empty a field, use clear".into());
        }
        // A classic text box pastes when asked to, with no keyboard and no focus.
        let mut field = None;
        let mut target_node: Option<(uia::Node, (i32, i32))> = None;
        let mut target_chromium = win::is_chromium(tracked.hwnd);
        if let Some(word) = target
            && let Target::Control(id) = self.resolve(word, who, tracked, view)?
        {
            let (node, rect) = self.locate(id, tracked, view)?;
            self.cursor_on(who, flags, rect);
            let (cx, cy) = act::centre(rect);
            target_chromium = win::is_chromium_at(tracked.hwnd, cx, cy);
            field = act::native_window(&node, tracked.hwnd).filter(|hwnd| !flags.real && !win::draws_itself(*hwnd) && !target_chromium);
            // A control the app draws itself gets the text added to the end of its own, with no
            // clipboard and no keyboard. A browser would come forward for that, so it does not.
            if field.is_none() && !flags.real && !target_chromium
                && let Some(done) = append_value(&node, tracked.hwnd, text)
            {
                return done;
            }
            target_node = Some((node, (cx, cy)));
        }
        let mut saved = apps::SavedClipboard::take()?;
        saved.lend(text)?;
        let route = (|| {
            // A browser takes Ctrl+V as window messages too, once a click has put its page's focus in
            // the field. The field is read back where it can be, and ValuePattern/keyboard is fallback.
            if !flags.real && target_chromium {
                if let Some(route) = self.paste_in_page(who, tracked, view, target) {
                    return Ok(route);
                }
                if let Some((ref node, _)) = target_node
                    && (!win::front_thread(tracked.hwnd) || win::foreground() == tracked.hwnd)
                    && let Some(done) = append_value(node, tracked.hwnd, text)
                {
                    return done;
                }
            }
            if let Some(hwnd) = field {
                let route = act::post_paste(hwnd)?;
                thread::sleep(PASTE_SETTLE);
                return Ok(route);
            }
            // Background Ctrl+V into non-classic apps (WPF, Qt, Firefox) without UIA SetFocus,
            // which can trigger focus steals and taskbar flashing on Qt/WPF/WinUI windows.
            // When a named target was requested, only post Ctrl+V if we actually focused it (WPF or
            // posted-mouse target) so we never paste into an unrelated focused field.
            if !flags.real && !target_chromium {
                let before = target_node.as_ref().and_then(|(n, _)| n.element.field_text());
                let focused_target = match target_node {
                    None => true,
                    Some((ref node, (cx, cy))) => {
                        if win::is_wpf(tracked.hwnd) {
                            act::focus_wpf_field(tracked.hwnd, node).is_ok()
                        } else if win::reads_posted_mouse(tracked.hwnd) {
                            let ok = act::post_click(tracked.hwnd, cx, cy, false, false).is_ok();
                            if ok {
                                thread::sleep(FIELD_SETTLE);
                            }
                            ok
                        } else {
                            false
                        }
                    }
                };
                if focused_target
                    && let Ok(chord) = real::chord("ctrl+v")
                {
                    let check_changed = |before: &Option<String>| -> bool {
                        let (Some(before), Some((node, _))) = (before, &target_node) else {
                            return target_node.is_none() || before.is_none();
                        };
                        let until = Instant::now() + TYPE_CHECK;
                        while node.element.field_text().as_deref() == Some(before.as_str()) && Instant::now() < until {
                            thread::sleep(SETTLE_STEP);
                        }
                        node.element.field_text().as_deref() != Some(before.as_str())
                    };
                    if let Ok(route) = act::post_key(tracked.hwnd, &chord, "ctrl+v") {
                        thread::sleep(PASTE_SETTLE);
                        if check_changed(&before) {
                            return Ok(route);
                        }
                    }
                }
            }
            // If the window is already in front (and quiet mode is off), paste directly into it
            // without moving focus, matching `type` and `key` on foreground windows.
            if !flags.real && !self.quiet.load(Ordering::Relaxed) && win::front_is(tracked.hwnd, &view.surfaces) {
                let points: Vec<(i32, i32)> = target_node.as_ref().map(|(_, p)| *p).into_iter().collect();
                let chord = real::chord("ctrl+v")?;
                return self.front_if_already(tracked, view, &points, Loan::Keys, || {
                    if let Some((_, (cx, cy))) = target_node {
                        press_at(tracked.hwnd, cx, cy, flags)?;
                    }
                    real::check_front(tracked.hwnd, &view.surfaces)?;
                    real::press_chord(&chord);
                    thread::sleep(PASTE_SETTLE);
                    Ok::<String, String>("paste".into())
                })?;
            }
            if !flags.real {
                return Err(act::NEEDS_REAL.into());
            }
            if let Some(word) = target {
                self.click(who, &Flags { real: true, ..Flags::default() }, tracked, view, word)?;
            }
            let chord = real::chord("ctrl+v")?;
            self.with_front(tracked, view, &[], || {
                real::check_front(tracked.hwnd, &view.surfaces)?;
                real::press_chord(&chord);
                thread::sleep(PASTE_SETTLE);
                Ok::<(), String>(())
            })??;
            Ok("paste".to_string())
        })();
        saved.put_back()?;
        route
    }

    /// Ctrl+End and Ctrl+V as window messages into a browser page, after a posted click in the
    /// field if one is named. None if the paste could not be sent or did not show in the field.
    fn paste_in_page(&self, who: &str, tracked: &Tracked, view: &mut View, target: Option<&String>) -> Option<String> {
        let _wake = act::wake_page(tracked.hwnd);
        let mut node = None;
        if let Some(word) = target {
            let Ok(Target::Control(id)) = self.resolve(word, who, tracked, view) else { return None };
            let (found, rect) = self.locate(id, tracked, view).ok()?;
            let (x, y) = act::centre(rect);
            act::post_click(tracked.hwnd, x, y, false, false).ok()?;
            thread::sleep(FIELD_SETTLE);
            node = Some(found);
        }
        let before = node.as_ref().and_then(|n| n.element.field_text());
        if node.is_some() {
            let _ = real::chord("ctrl+end").ok().and_then(|end| act::post_key(tracked.hwnd, &end, "ctrl+end").ok());
        }
        let route = act::post_key(tracked.hwnd, &real::chord("ctrl+v").ok()?, "ctrl+v").ok()?;
        thread::sleep(PASTE_SETTLE);
        if let (Some(before), Some(node)) = (before, &node) {
            let until = Instant::now() + TYPE_CHECK;
            while node.element.field_text().as_deref() == Some(before.as_str()) && Instant::now() < until {
                thread::sleep(SETTLE_STEP);
            }
            // A field that changed at all took the paste, even if it reformats or masks what it
            // shows: pasting again by keyboard would put the text in twice.
            if node.element.field_text().as_deref() == Some(before.as_str()) {
                return None;
            }
        }
        Some(route)
    }

    fn press_down(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, words: &[String]) -> Reply {
        let what = words.first().ok_or("down shift --real, or down left x,y --mouse")?;
        if let Some(button) = Button::named(what) {
            if !flags.mouse {
                return Err("holding a mouse button needs --mouse, which asks to borrow the person's cursor".into());
            }
            let word = words.get(1).ok_or("down left x,y --mouse")?;
            let (x, y) = self.point_of(word, who, tracked, view)?;
            // Handing focus back would end the press, so the window keeps it until up.
            if !win::inside(tracked.hwnd, x, y) {
                return Err("refused: that point is not on the window".into());
            }
            if !win::on_screen(x, y) {
                return Err("refused: that point is off screen".into());
            }
            self.cursor_on(who, flags, (x, y, 0, 0));
            // The real mouse is shared: agents take turns with it, and it presses only where this
            // window really is in front.
            let _turn = self.real_input.lock().unwrap();
            real::wait_for_quiet();
            self.home.lock().unwrap().holding_mouse = MouseHold::Starting;
            let result = (|| {
                if !win::bring_forward(tracked.hwnd) {
                    return Err("Windows would not bring that window forward".into());
                }
                let mut family = view.surfaces.clone();
                family.push(tracked.hwnd);
                if !win::owns_point(&family, x, y) {
                    return Err(format!("refused: {x},{y} on screen is not on this window, so a real press there would hit something else"));
                }
                real::hold_button(who, x, y, button)?;
                Ok("held: run up within 30 s".into())
            })();
            {
                let mut home = self.home.lock().unwrap();
                home.holding_mouse = if result.is_ok() || real::holding_button() { MouseHold::Held } else { MouseHold::Off };
            }
            return result;
        }
        if !flags.real {
            return Err("holding a key needs --real".into());
        }
        let chord = real::chord(what)?;
        refuse_risky(&chord, what, flags)?;
        self.with_front(tracked, view, &[], || {
            refuse_risky(&chord, what, flags)?;
            real::hold_keys(who, &chord)
        })??;
        Ok("held, and it reaches whatever has focus: run up within 30 s".into())
    }

    /// What a do script would touch, without touching it.
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

    /// Open the notification panel and list it: it is a window of its own, so its refs work as usual.
    fn notifications(&self, who: &str, words: &[String]) -> Reply {
        let chord = real::chord("win+n")?;
        if words.first().is_some_and(|word| word == "close") {
            let _turn = self.real_input.lock().unwrap();
            let open = {
                let home = self.home.lock().unwrap();
                home.panel.is_some() || home.on_loan
            };
            if open {
                real::press_chord(&chord);
            }
            let mut home = self.home.lock().unwrap();
            home.on_loan = false;
            home.panel = None;
            return Ok("ok".into());
        }
        // The panel closes the moment it loses focus, so the focus keeper stands aside until close.
        let front = {
            let _turn = self.real_input.lock().unwrap();
            {
                let mut home = self.home.lock().unwrap();
                home.on_loan = true;
                home.panel = None;
            }
            let before = win::foreground();
            real::press_chord(&chord);
            let until = Instant::now() + NOTIFICATIONS_OPEN;
            while win::foreground() == before && Instant::now() < until {
                thread::sleep(FIELD_SETTLE);
            }
            let front = win::foreground();
            if front == 0 || front == before || act::is_shell_window(front) {
                let mut home = self.home.lock().unwrap();
                home.on_loan = false;
                home.panel = None;
                return Err("the notification panel did not open".into());
            }
            let mut home = self.home.lock().unwrap();
            home.on_loan = true;
            home.panel = Some(front);
            front
        };
        thread::sleep(NOTIFICATIONS_SETTLE);
        let tracked = self.track(front);
        self.set_current(who, front);
        let mut view = tracked.view.lock().unwrap();
        self.first_read(&tracked, &mut view, false)?;
        Ok(view.nodes.iter().map(|(id, node)| format!("e{id} {}", lines::line(&node.parts))).collect::<Vec<_>>().join("\n"))
    }

    fn is(&self, who: &str, flags: &Flags, words: &[String]) -> Reply {
        let [word, state] = words else { return Err("is e3 on|off|open|closed|selected|enabled|disabled".into()) };
        let tracked = self.pick(who, flags.window.as_deref())?;
        // A covered browser window is drawn and takes input while an agent works in it.
        let _drawn = win::keep_drawn(tracked.hwnd, false);
        let mut view = tracked.view.lock().unwrap();
        self.first_read(&tracked, &mut view, false)?;
        let Target::Control(id) = self.resolve(word, who, &tracked, &mut view)? else { return Err("is needs a control".into()) };
        let states = &view.nodes.get(&id).ok_or(format!("e{id} is gone: see"))?.parts.states;
        let yes = match state.as_str() {
            "enabled" => !states.contains(&"disabled"),
            other => states.contains(&other),
        };
        Ok(if yes { "yes" } else { "no" }.into())
    }

    /// Open the submenu of an entry in a browser or Electron menu with the menu's own keys: Home,
    /// Down to the entry, then Right. These menus open a submenu on the real mouse hovering, which a
    /// posted move cannot hold, and accessibility would bring the window forward. None if this is
    /// not such an entry.
    fn open_submenu(&self, tracked: &Tracked, view: &mut View, id: u32) -> Option<Reply> {
        let node = view.nodes.get(&id)?;
        if !win::is_chromium(tracked.hwnd) || node.parts.role != "menuitem" || !node.parts.states.contains(&"closed") {
            return None;
        }
        // The entries of the same menu, in order: the ones after the nearest menu before this one.
        let menu = view.nodes.range(..id).rev().find(|(_, n)| n.parts.role == "menu").map(|(at, _)| *at)?;
        let entries: Vec<u32> = view
            .nodes
            .range(menu + 1..)
            .take_while(|(_, n)| n.parts.role != "menu")
            .filter(|(_, n)| n.parts.role == "menuitem" && !n.parts.states.contains(&"disabled"))
            .map(|(at, _)| *at)
            .collect();
        let downs = entries.iter().position(|at| *at == id)?;
        let _wake = act::wake_page(tracked.hwnd);
        let press = |spec: &str| real::chord(spec).and_then(|chord| act::post_key(tracked.hwnd, &chord, spec));
        let pressed = (|| {
            press("home")?;
            for _ in 0..downs {
                press("down")?;
            }
            press("right")
        })();
        if let Err(error) = pressed {
            return Some(Err(error));
        }
        let until = Instant::now() + Duration::from_millis(1500);
        loop {
            thread::sleep(SETTLE_STEP);
            if self.read(tracked, view, false).is_ok() && view.nodes.get(&id).is_some_and(|n| n.parts.states.contains(&"open")) {
                return Some(Ok("menu keys".into()));
            }
            if Instant::now() >= until {
                return Some(Err("the submenu did not open with the menu's keys: try click on the entry first, then key right".into()));
            }
        }
    }

    fn hover(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, word: &str) -> Reply {
        // A name that fits several things means the menu entry with a submenu, when one is open.
        let (x, y) = match self.resolve_for(word, who, tracked, view, |n| n.parts.role == "menuitem" && n.parts.states.contains(&"closed"))? {
            Target::Point(x, y) => (x, y),
            Target::Control(id) => {
                let rect = self.locate(id, tracked, view)?.1;
                if !flags.mouse
                    && let Some(opened) = self.open_submenu(tracked, view, id)
                {
                    self.cursor_on(who, flags, rect);
                    return opened;
                }
                act::centre(rect)
            }
            Target::Menu(_) => return Err("hover needs a control or x,y".into()),
        };
        self.cursor_on(who, flags, (x, y, 0, 0));
        if flags.mouse {
            self.front(tracked, view, &[(x, y)], Loan::Mouse, || real::move_to(x, y))?;
            return Ok("mouse".into());
        }
        act::post_hover(tracked.hwnd, x, y)
    }

    fn set(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, word: &str, value: &str) -> Reply {
        let Target::Control(id) = self.resolve_for(word, who, tracked, view, |n| n.can.value || n.can.range)? else { return Err("set needs a control".into()) };
        let (node, rect) = self.locate(id, tracked, view)?;
        self.cursor_on(who, flags, rect);
        self.overlay.typing(who, value.chars().count());
        if flags.real {
            let (x, y) = act::centre(rect);
            let all = real::chord("ctrl+a")?;
            let loan = if flags.mouse { Loan::Mouse } else { Loan::Keys };
            return self.front(tracked, view, &[(x, y)], loan, || {
                press_at(tracked.hwnd, x, y, flags)?;
                real::check_front(tracked.hwnd, &view.surfaces)?;
                real::press_chord(&all);
                if !real::type_text(value, tracked.hwnd, &view.surfaces, &self.stop)? {
                    return Err(STOPPED.to_string());
                }
                Ok("keyboard".to_string())
            })?;
        }
        // A browser raises its window when accessibility changes a field, so the field is filled
        // the way a person would, by window messages. Accessibility is only the last resort.
        let (cx, cy) = act::centre(rect);
        if win::is_chromium_at(tracked.hwnd, cx, cy)
            && (node.can.value || node.can.range || node.element.field_text().is_some())
            && !node.element.value_read_only()
        {
            if let Some(done) = self.fill_in_page(tracked, &node, rect, value) {
                return done;
            }
            if let Some(done) = replace_in_page(tracked.hwnd, &node, value) {
                return done;
            }
            if win::front_thread(tracked.hwnd) && win::foreground() != tracked.hwnd {
                return Err(NEEDS_REAL.into());
            }
        }
        act::set(&node, tracked.hwnd, value)
    }

    /// Select everything in a browser field with a triple click, type over it, and read it back.
    /// None if the text did not land.
    fn fill_in_page(&self, tracked: &Tracked, node: &Node, rect: Bounds, value: &str) -> Option<Reply> {
        let _wake = act::wake_page(tracked.hwnd);
        let (x, y) = act::centre(rect);
        act::post_press(tracked.hwnd, x, y, Button::Left, 3, 0, &[]).ok()?;
        thread::sleep(FIELD_SETTLE);
        let field = win::chromium_render_window(tracked.hwnd).unwrap_or_else(|| win::child_at(tracked.hwnd, x, y).0);
        if value.is_empty() {
            let bs = real::chord("backspace").ok()?;
            if let Err(error) = act::post_key(tracked.hwnd, &bs, "backspace") {
                return Some(Err(error));
            }
        } else if let Err(error) = act::post_text(field, value, &self.stop) {
            return Some(Err(error));
        }
        let until = Instant::now() + TYPE_CHECK;
        while node.element.field_text().as_deref() != Some(value) && Instant::now() < until {
            thread::sleep(SETTLE_STEP);
        }
        if node.element.field_text().as_deref() != Some(value) {
            return None;
        }
        // A number field tells the page only when it loses focus, so focus moves on with Tab.
        if node.can.range {
            let _ = act::post_key(tracked.hwnd, &real::chord("tab").ok()?, "tab");
        }
        Some(Ok("message".into()))
    }

    fn type_text(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, words: &[String]) -> Reply {
        let (target, text) = match words {
            [text] => (None, text),
            [word, text] => (Some(word), text),
            _ => return Err("type [control] \"text\"".into()),
        };
        if text.is_empty() {
            return Err("nothing to type: to empty a field, use clear".into());
        }
        let mut place = None;
        let mut hwnd = None;
        let mut field = None;
        if let Some(word) = target {
            let Target::Control(id) = self.resolve_for(word, who, tracked, view, |n| n.can.value)? else { return Err("type needs a control".into()) };
            let (node, rect) = self.locate(id, tracked, view)?;
            self.cursor_on(who, flags, rect);
            place = Some(act::centre(rect));
            hwnd = act::native_window(&node, tracked.hwnd);
            field = Some(node);
        }
        // A classic edit box follows its caret exactly as each letter goes in; anything else that
        // says where its caret is gets watched while the text goes in.
        let classic = hwnd.and_then(win::caret_in).is_some();
        let watched = (!classic).then(|| field.as_ref().map(|node| node.element.clone()));
        // An app that takes typing only with focus inside itself, in front already, has that focus
        // and the keyboard: typing reaches it with nothing moving, as --real would (unless quiet mode is on).
        let already_front = !flags.real && !self.quiet.load(Ordering::Relaxed) && target.is_none() && win::keys_need_focus(tracked.hwnd) && win::front_is(tracked.hwnd, &view.surfaces);
        self.watching_caret(who, tracked.hwnd, watched, || {
            if hwnd.and_then(win::caret_in).is_none() {
                self.overlay.typing(who, text.chars().count());
            }
            if flags.real || already_front {
                let points: Vec<(i32, i32)> = place.into_iter().collect();
                let to_end = real::chord("ctrl+end")?;
                let loan = if flags.mouse && place.is_some() { Loan::Mouse } else { Loan::Keys };
                let send = || {
                    if let Some((x, y)) = place {
                        press_at(tracked.hwnd, x, y, flags)?;
                        // A click lands mid-text; typing adds to the end, as it does without --real.
                        real::press_chord(&to_end);
                    }
                    if !real::type_text(text, tracked.hwnd, &view.surfaces, &self.stop)? {
                        return Err(STOPPED.to_string());
                    }
                    Ok("keyboard".to_string())
                };
                return if flags.real {
                    self.front(tracked, view, &points, loan, send)?
                } else {
                    self.front_if_already(tracked, view, &points, loan, send)?
                };
            }
            if let Some((x, y)) = place
                && win::is_chromium_at(tracked.hwnd, x, y)
            {
                let _wake = act::wake_page(tracked.hwnd);
                // The page has to know which field has focus, and a click inside it says so.
                act::post_click(tracked.hwnd, x, y, false, false)?;
                thread::sleep(FIELD_SETTLE);
                let before = field.as_ref().and_then(|node| node.element.field_text());
                let target_hwnd = win::chromium_render_window(tracked.hwnd).unwrap_or_else(|| win::child_at(tracked.hwnd, x, y).0);
                let route = act::post_text(target_hwnd, text, &self.stop)?;
                // Chromium can drop characters while its window is not active, and says nothing. A
                // field that can be read (ValuePattern or contenteditable TextPattern) is read
                // back, and pasted in the background if the text did not land.
                if let (Some(before), Some(node)) = (before, &field) {
                    let wanted = format!("{before}{text}");
                    let until = Instant::now() + TYPE_CHECK;
                    while node.element.field_text().as_deref() != Some(wanted.as_str()) && Instant::now() < until {
                        thread::sleep(SETTLE_STEP);
                    }
                    if node.element.field_text().as_deref() != Some(wanted.as_str()) {
                        if let Some(done) = replace_in_page(tracked.hwnd, node, &wanted) {
                            return done;
                        }
                        if !win::front_thread(tracked.hwnd) || win::foreground() == tracked.hwnd {
                            return act::set(node, tracked.hwnd, &wanted);
                        }
                    }
                }
                return Ok(route);
            }
            // In a classic edit box the cursor turns into a caret that follows the text in. It moves to
            // where each letter will end just before the letter goes in, so it is never behind. A
            // posted letter lands a moment later, so the box is given time to take the last one first.
            let mut last: Option<Option<usize>> = None;
            let follow = |box_hwnd: Hwnd, letter: u16| {
                let until = Instant::now() + CARET_SETTLE;
                while last.is_some_and(|was| win::caret_index(box_hwnd) == was) && Instant::now() < until {
                    thread::sleep(Duration::from_millis(1));
                }
                last = Some(win::caret_index(box_hwnd));
                let Some((x, y, h)) = win::caret_in(box_hwnd) else { return };
                let ahead = if letter == 10 { 0 } else { win::letter_width(box_hwnd, letter).unwrap_or(0) };
                self.overlay.caret(who, x + ahead, y, h);
            };
            match (target, hwnd) {
                // A control the app draws itself takes no window messages, but one that says what
                // its text is can be given its text with this added at the end.
                (Some(_), None) => match field.as_ref().and_then(|node| append_value(node, tracked.hwnd, text)) {
                    Some(done) => done,
                    None => match field.as_ref().filter(|node| win::is_wpf(tracked.hwnd) && !node.can.value && matches!(node.parts.role.as_str(), "edit" | "document" | "combobox")) {
                        Some(node) => {
                            act::focus_wpf_field(tracked.hwnd, node)?;
                            match act::post_text_focused(tracked.hwnd, text, &self.stop) {
                                Err(why) if why.contains(act::IN_FRONT) && !self.quiet.load(Ordering::Relaxed) && win::front_is(tracked.hwnd, &view.surfaces) => {
                                    self.front_if_already(tracked, view, &[], Loan::Keys, || {
                                        if !real::type_text(text, tracked.hwnd, &view.surfaces, &self.stop)? {
                                            return Err(STOPPED.to_string());
                                        }
                                        Ok("keyboard".to_string())
                                    })?
                                }
                                other => other,
                            }
                        }
                        None => Err(NEEDS_REAL.into()),
                    },
                },
                (Some(_), Some(hwnd)) => {
                    // A rich text box keeps its caret where it was, often at the start: typing adds to
                    // the end, as it does elsewhere.
                    if act::is_rich_text(hwnd) {
                        act::caret_to_end(hwnd)?;
                    }
                    act::post_text_watched(hwnd, text, &self.stop, follow)
                }
                (None, _) if win::is_chromium(tracked.hwnd) => {
                    let _wake = act::wake_page(tracked.hwnd);
                    let target_hwnd = win::chromium_render_window(tracked.hwnd).unwrap_or(tracked.hwnd);
                    act::post_text(target_hwnd, text, &self.stop)
                }
                (None, _) if win::keys_need_focus(tracked.hwnd) => act::post_text_focused(tracked.hwnd, text, &self.stop),
                (None, _) => act::post_text_watched(win::keyboard_target(tracked.hwnd), text, &self.stop, follow),
            }
    
        })
    }


    /// Run some typing while the agent's cursor shows as a caret wherever the control says its caret
    /// is: `Some(None)` watches whatever has focus in the window, `None` watches nothing.
    fn watching_caret(&self, who: &str, window: Hwnd, control: Option<Option<uia::Element>>, work: impl FnOnce() -> Reply) -> Reply {
        let Some(control) = control else { return work() };
        let done = AtomicBool::new(false);
        thread::scope(|scope| {
            scope.spawn(|| {
                uia::enter_com();
                let mut shown = None;
                loop {
                    let finished = done.load(Ordering::Relaxed);
                    // A named control gets text added at its end; otherwise the focused one's caret.
                    let place = match &control {
                        Some(element) => {
                            // Its own caret when it has one inside it, else the end of its text,
                            // kept within its edges when the text runs past them.
                            let bounds = element.rect().ok();
                            let inside = |&(x, y, _): &(i32, i32, i32)| bounds.is_none_or(|(bx, by, bw, bh)| x >= bx && x <= bx + bw && y >= by - 2 && y <= by + bh);
                            element.caret().filter(inside).or_else(|| element.text_end()).map(|caret| clamp_caret(caret, bounds))
                        }
                        None => self.uia.focused_in(window, &[]).and_then(|element| element.caret()),
                    };
                    if let Some((x, y, h)) = place.filter(|&at| Some(at) != shown) {
                        self.overlay.caret(who, x, y, h);
                        shown = Some((x, y, h));
                    }
                    if finished {
                        break;
                    }
                    thread::sleep(CARET_WATCH);
                }
            });
            let reply = work();
            done.store(true, Ordering::Relaxed);
            reply
        })
    }
    /// Store apps read Enter, Escape, Backspace and Delete only from the real keyboard: as window
    /// messages they take none of them, even in front. Where the app has a button that does what the
    /// key does, as Calculator does, that button is pressed through accessibility instead, in the back.
    fn key_by_button(&self, tracked: &Tracked, chord: &real::Chord) -> Option<Reply> {
        const BUTTONS: [(u16, &str); 4] = [(0x0D, "equalButton"), (0x1B, "clearButton"), (0x08, "backSpaceButton"), (0x2E, "clearEntryButton")];
        if chord.modified || chord.keys.len() != 1 || win::class(tracked.hwnd) != "ApplicationFrameWindow" {
            return None;
        }
        // A window found once to have none of these buttons is not read again for them.
        static WITHOUT: Mutex<std::collections::BTreeSet<Hwnd>> = Mutex::new(std::collections::BTreeSet::new());
        let (_, id) = BUTTONS.iter().find(|(vk, _)| *vk == chord.keys[0].0)?;
        if WITHOUT.lock().unwrap().contains(&tracked.hwnd) {
            return None;
        }
        let (mut nodes, mut hidden, mut quiet) = (Vec::new(), 0, Vec::new());
        self.uia.read(tracked.hwnd, false, &mut nodes, &mut hidden, &mut quiet).ok()?;
        if !nodes.iter().any(|node| BUTTONS.iter().any(|(_, id)| node.control_id == *id)) {
            WITHOUT.lock().unwrap().insert(tracked.hwnd);
            return None;
        }
        let node = nodes.iter().find(|node| node.control_id == *id)?;
        Some(act::press_behind(node, tracked.hwnd)?.map(|route| format!("{route} on '{}'", node.parts.name)))
    }

    fn key(&self, flags: &Flags, tracked: &Tracked, view: &View, spec: &str) -> Reply {
        let chord = real::chord(spec)?;
        refuse_risky(&chord, spec, flags)?;
        let hold = flags.hold.map(|ms| Duration::from_millis(ms.min(HOLD_MOST_MS)));
        let keyboard = |allow_bring: bool| -> Reply {
            let run = |work: Box<dyn FnOnce() -> Result<(), String>>| -> Result<Result<(), String>, String> {
                if allow_bring {
                    self.front(tracked, view, &[], Loan::Keys, work)
                } else {
                    self.front_if_already(tracked, view, &[], Loan::Keys, work)
                }
            };
            match hold {
                Some(hold) => run(Box::new(|| {
                    refuse_risky(&chord, spec, flags)?;
                    real::hold_chord(&chord, hold);
                    Ok(())
                }))??,
                None => run(Box::new(|| {
                    refuse_risky(&chord, spec, flags)?;
                    real::check_front(tracked.hwnd, &view.surfaces).map(|_| real::press_chord(&chord))
                }))??,
            }
            Ok("keyboard".into())
        };
        if flags.real {
            return keyboard(true);
        }
        if hold.is_none() {
            if act::is_shell_win_shortcut(&chord) && self.quiet.load(Ordering::Relaxed) {
                return Err(format!("refused: quiet mode is on ({spec} opens foreground shell UI; run 'deskcrew quiet off' to allow)"));
            }
            if let Some(direct) = act::win_shortcut(&chord, Some(tracked.hwnd)) {
                return direct;
            }
        }
        if hold.is_none()
            && let Some(pressed) = self.key_by_button(tracked, &chord)
        {
            return pressed;
        }
        let _wake = act::wake_page(tracked.hwnd);
        let ctrl_letter = (hold.is_none()
            && chord.modified
            && chord.keys.len() == 2
            && chord.keys[0].0 == windows_sys::Win32::UI::Input::KeyboardAndMouse::VK_CONTROL)
            .then(|| chord.keys[1].0 as u8);
        let clip_before = matches!(ctrl_letter, Some(b'C' | b'X')).then(apps::clipboard_seq);
        let focused_before = matches!(ctrl_letter, Some(b'C' | b'X' | b'A' | b'V'))
            .then(|| self.uia.focused_in(tracked.hwnd, &view.surfaces))
            .flatten();
        let val_before = (ctrl_letter == Some(b'V'))
            .then(|| focused_before.as_ref().filter(|el| !el.value_read_only()).map(|el| el.value_text()))
            .flatten();
        let posted = match hold {
            Some(hold) => act::post_hold(tracked.hwnd, &chord, hold, &self.stop),
            None => act::post_key(tracked.hwnd, &chord, spec),
        };
        // UIA fallbacks for copy, cut, select-all, and paste (only when `focused_in` proves the
        // focused control belongs to this exact window/surface, never another window of the app).
        if let Some(letter) = ctrl_letter {
            match letter {
                b'C' | b'X' => {
                    if posted.is_ok() {
                        thread::sleep(PASTE_SETTLE);
                    }
                    if let Some(seq_before) = clip_before
                        && apps::clipboard_seq() == seq_before
                        && let Some(focused) = focused_before.or_else(|| self.uia.focused_in(tracked.hwnd, &view.surfaces))
                        && let Some(selected) = focused.selected_text()
                    {
                        if letter == b'X' {
                            let full = focused.value_text();
                            if !focused.value_read_only() && selected == full {
                                apps::set_clipboard(&selected)?;
                                act::behind(tracked.hwnd, || focused.set_value(""))?;
                                return Ok("cut selection".into());
                            }
                        } else {
                            apps::set_clipboard(&selected)?;
                            return Ok("text selection".into());
                        }
                    }
                }
                b'A' => {
                    if posted.is_ok() {
                        thread::sleep(FIELD_SETTLE);
                    }
                    if let Some(focused) = focused_before.or_else(|| self.uia.focused_in(tracked.hwnd, &view.surfaces))
                        && (posted.is_err() || focused.selected_text().is_none())
                        && act::behind(tracked.hwnd, || focused.select_all())
                    {
                        return Ok("select all".into());
                    }
                }
                b'V' => {
                    if posted.is_ok() {
                        thread::sleep(PASTE_SETTLE);
                    }
                    if let Some(focused) = focused_before.or_else(|| self.uia.focused_in(tracked.hwnd, &view.surfaces))
                        && !focused.value_read_only()
                        && (posted.is_err() || val_before.as_deref() == Some(focused.value_text().as_str()))
                        && let Ok(clip) = apps::clipboard_text()
                        && clip != apps::NO_CLIPBOARD_TEXT
                    {
                        let current = focused.value_text();
                        let replace_all = current.is_empty() || focused.selected_text().as_deref() == Some(current.as_str());
                        if replace_all && act::behind(tracked.hwnd, || focused.set_value(&clip)).is_ok() {
                            return Ok("value paste".into());
                        }
                    }
                }
                _ => {}
            }
        }
        // A window the person has in front shares their keyboard, so its keys cannot be posted with
        // the app's own keyboard state. It already has the keyboard, which reaches it with nothing
        // moving; `allow_bring = false` guarantees we never steal focus if the person switched away.
        match posted {
            Err(why) if why.contains(act::IN_FRONT) && !self.quiet.load(Ordering::Relaxed) && win::front_is(tracked.hwnd, &view.surfaces) => keyboard(false),
            other => other,
        }
    }

    fn select(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, word: &str, item: &str) -> Reply {
        if flags.real || flags.mouse {
            return Err("select does not take --real or --mouse: click the dropdown and item instead".into());
        }
        let Target::Control(id) = self.resolve_for(word, who, tracked, view, |n| n.can.expand || n.can.select || matches!(n.parts.role.as_str(), "combobox" | "list"))? else { return Err("select needs a control".into()) };
        let (node, rect) = self.locate(id, tracked, view)?;
        if node_disabled(&node) {
            return Err("that control is disabled".into());
        }
        self.cursor_on(who, flags, rect);
        let classic = !win::draws_itself(tracked.hwnd) && !store_control(tracked.hwnd, &node, &view.surfaces, act::centre(rect));
        let (result, tried_mouse) = if classic {
            self.overlay.shielded(|| act::select_item(&node, item, &self.uia, tracked.hwnd, &view.surfaces))
        } else {
            (act::select_item(&node, item, &self.uia, tracked.hwnd, &view.surfaces), false)
        };
        if tried_mouse { Err(act::NEEDS_MOUSE.into()) } else { result }
    }

    fn scroll(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, words: &[String]) -> Reply {
        let direction = |word: &String| matches!(word.as_str(), "up" | "down" | "left" | "right");
        let at = words.iter().position(direction).ok_or("scroll [control] up|down|left|right [n]")?;
        let sideways = words[at] == "left" || words[at] == "right";
        let down = words[at] == "down" || words[at] == "right";
        let times = crate::motion::parse_scroll_count(words.get(at + 1).map(String::as_str), 1)?;
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
        // A wheel turned down scrolls down, but a sideways one turned positive scrolls right.
        let notches = if down != sideways { -times } else { times };
        if flags.mouse {
            self.front(tracked, view, &[(x, y)], Loan::Mouse, || real::wheel(x, y, notches, sideways))?;
            return Ok("mouse".into());
        }
        if flags.real {
            let page = real::chord(match (sideways, down) {
                (false, true) => "pagedown",
                (false, false) => "pageup",
                (true, true) => "right",
                (true, false) => "left",
            })?;
            self.with_front(tracked, view, &[(x, y)], || {
                for _ in 0..times {
                    if self.stop.load(Ordering::Relaxed) {
                        return Err(STOPPED.into());
                    }
                    real::press_chord(&page);
                }
                Ok::<(), String>(())
            })??;
            return Ok("keyboard".into());
        }
        if node.can.scroll {
            for _ in 0..times {
                if self.stop.load(Ordering::Relaxed) {
                    return Err(STOPPED.into());
                }
                node.element.scroll(down, true, sideways)?;
            }
            return Ok("scroll".into());
        }
        act::post_wheel(tracked.hwnd, x, y, notches, sideways)
    }

    fn point_of(&self, word: &str, who: &str, tracked: &Tracked, view: &mut View) -> Result<(i32, i32), String> {
        match self.resolve(word, who, tracked, view)? {
            Target::Point(x, y) => Ok((x, y)),
            Target::Control(id) => Ok(act::centre(self.locate(id, tracked, view)?.1)),
            Target::Menu(_) => Err("that is a menu entry, not a place".into()),
        }
    }

    /// Strokes by the quietest way the app accepts: messages to a classic window, the pen for one
    /// that draws itself, and the real mouse only for an agent allowed to borrow it.
    fn walk(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &View, strokes: Vec<Vec<(f32, f32)>>) -> Reply {
        let (button, modifiers) = (flags.mouse_button(), flags.modifiers()?);
        let points: Vec<(i32, i32)> = strokes.iter().flatten().map(|p| (p.0.round() as i32, p.1.round() as i32)).collect();
        let (x, y) = *points.first().ok_or("nothing to draw")?;
        let jump = |x: i32, y: i32| self.overlay.jump(who, x, y);
        let (route, finished) = if flags.mouse {
            let done = self.front(tracked, view, &points, Loan::Mouse, || strokes.iter().all(|path| real::stroke(path, button, modifiers, &self.stop, jump)))?;
            ("mouse", done)
        } else if act::needs_pen_at(tracked.hwnd, x, y) && (flags.real || !act::posts_with_capture(tracked.hwnd)) {
            if self.quiet.load(Ordering::Relaxed) {
                return Err("refused: quiet mode is on (synthetic pen borrows the cursor; run 'deskcrew quiet off' to allow)".into());
            }
            if !modifiers.is_empty() || button == MouseButton::Middle {
                return Err(PEN_LIMITS.into());
            }
            let draw = || -> Result<bool, String> {
                let pen = Pen::new().ok_or(act::NO_PEN)?;
                strokes.iter().try_fold(true, |done, path| Ok::<bool, String>(done && pen.stroke(path, button, &self.stop, jump)?))
            };
            let mut family = view.surfaces.clone();
            family.push(tracked.hwnd);
            let done = if flags.real {
                self.front(tracked, view, &points, Loan::Pen, draw)??
            } else if win::front_is(tracked.hwnd, &view.surfaces) && points.iter().all(|&(px, py)| win::owns_point(&family, px, py)) {
                self.front_if_already(tracked, view, &points, Loan::Pen, draw)??
            } else {
                if win::minimized(tracked.hwnd) {
                    return Err(MINIMIZED_POINT.into());
                }
                return Err(act::NEEDS_REAL.into());
            };
            ("pen", done)
        } else {
            if let Some((x, y)) = points.iter().find(|(x, y)| !win::inside(tracked.hwnd, *x, *y)) {
                if win::minimized(tracked.hwnd) {
                    return Err(MINIMIZED_POINT.into());
                }
                return Err(format!("refused: {x},{y} on screen is not on this window"));
            }
            let send = || {
                let _lock = (!flags.real && win::is_background(tracked.hwnd)).then(win::ForegroundLock::take);
                let out = strokes.iter().try_fold(true, |done, path| Ok::<bool, String>(done && act::post_stroke(tracked.hwnd, path, button, modifiers, &self.stop, jump)?));
                win::stop_flashing(tracked.hwnd);
                out
            };
            let done = if flags.real { self.with_front(tracked, view, &points, send)?? } else { send()? };
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
        // A covered browser window is drawn and takes input while an agent works in it.
        let _drawn = win::keep_drawn(tracked.hwnd, true);
        if win::minimized(tracked.hwnd) && win::is_chromium(tracked.hwnd) {
            return Err(COVERED.into());
        }
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
            (strokes, view.pids.clone())
        };
        let first = strokes[0][0];
        self.overlay.over(who, tracked.hwnd);
        self.overlay.move_to(who, first.0 as i32, first.1 as i32);
        self.focus_in(&pids);
        let _draw_lock = win::is_background(tracked.hwnd).then(win::ForegroundLock::take);
        tracked.drawers.fetch_add(1, Ordering::Relaxed);
        let mut result = Ok(true);
        let button = flags.mouse_button();
        let modifiers = flags.modifiers()?;
        'strokes: for stroke in &strokes {
            let mut start = 0;
            while start + 1 < stroke.len() {
                // Each press makes a browser ask to come forward, which flashes its taskbar button,
                // so one agent alone draws each stroke in one press.
                let piece = if tracked.drawers.load(Ordering::Relaxed) > 1 { SHARED_PIECE } else { stroke.len() };
                let end = (start + piece).min(stroke.len() - 1);
                let done = {
                    let _turn = tracked.pointer.lock().unwrap();
                    act::post_stroke(tracked.hwnd, &stroke[start..=end], button, modifiers, &self.stop, |x, y| self.overlay.jump(who, x, y))
                };
                win::stop_flashing(tracked.hwnd);
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
        tracked.drawers.fetch_sub(1, Ordering::Relaxed);
        drop(_draw_lock);
        self.focus_out();
        win::stop_flashing(tracked.hwnd);
        match result? {
            true => Ok("ok message".into()),
            false => Err(STOPPED.into()),
        }
    }

    fn plan_draw(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, words: &[String]) -> Result<Vec<Vec<(f32, f32)>>, String> {
        let (word, script) = match words {
            [] => return Err(DRAW_USAGE.into()),
            // A first word that is neither a point nor a shape names the control to draw on.
            [first, rest @ ..] if !rest.is_empty() && is_point(first).is_none() && !motion::is_shape(first) => (Some(first.clone()), rest.join(" ")),
            _ => (None, words.join(" ")),
        };
        let origin = match word {
            Some(ref word) => match self.resolve(word, who, tracked, view)? {
                Target::Control(id) => Some(self.locate(id, tracked, view)?.1),
                _ => return Err("draw needs a control".into()),
            },
            None => None,
        };
        let frames = self.shot_frames(who, tracked);
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
                    let (x, y) = is_point(word).ok_or(format!("'{word}' is not x,y"))?;
                    points.push(match (origin, frames) {
                        (Some(rect), _) => (rect.0 + x, rect.1 + y),
                        (None, Some(frames)) => frames.to_screen(x, y),
                        (None, None) => return Err("draw on a control, or take a shot first".into()),
                    });
                }
                if !points.is_empty() {
                    strokes.push(real::trace(&points, flags.smooth));
                }
            }
        }
        if strokes.iter().map(Vec::len).sum::<usize>() == 0 {
            return Err("nothing to draw".into());
        }
        Ok(strokes)
    }

    fn wait(&self, flags: &Flags, tracked: &Tracked, view: &mut View, text: &str) -> Reply {
        if let Some(ms) = sleep_ms(text, flags) {
            let limit = Duration::from_millis(ms.min(WAIT_MOST_MS));
            let started = Instant::now();
            while started.elapsed() < limit {
                if self.stop.load(Ordering::Relaxed) {
                    return Err(STOPPED.into());
                }
                thread::sleep(WAIT_STEP.min(limit.saturating_sub(started.elapsed())));
            }
            return Ok("waited".into());
        }
        self.wait_text(flags, tracked, view, text)
    }

    /// Wait until the text shows in the window, or is gone with --gone.
    fn wait_text(&self, flags: &Flags, tracked: &Tracked, view: &mut View, text: &str) -> Reply {
        let wanted = text.to_lowercase();
        let until = Instant::now() + Duration::from_millis(flags.ms.unwrap_or(WAIT_DEFAULT_MS).min(WAIT_MOST_MS));
        loop {
            if self.stop.load(Ordering::Relaxed) {
                return Err(STOPPED.into());
            }
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

    /// One step of a do, with the flags it may carry itself: its own --expect is checked after it,
    /// and flags that would need another window or another go-ahead are refused, not dropped.
    fn step(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, step: &Step) -> Reply {
        let own = &step.flags;
        if let Some(error) = &own.flag_error {
            return Err(error.clone());
        }
        if own.window.is_some() {
            return Err("a step cannot pick another window: a do runs in one, so run another command for that window".into());
        }
        if matches!(own.risk.as_deref(), Some("medium" | "high")) && !flags.force && !own.force {
            return Err("declared risky: ask the person, then add --force".into());
        }
        let verb = step.words.first().map_or("", String::as_str);
        reject_step_flags(verb, flags, Some(own))?;
        let merged = Flags {
            real: flags.real || own.real,
            mouse: flags.mouse || own.mouse,
            fast: flags.fast || own.fast,
            force: flags.force || own.force,
            right: flags.right || own.right,
            double: flags.double || own.double,
            triple: flags.triple || own.triple,
            middle: flags.middle || own.middle,
            back: flags.back || own.back,
            forward: flags.forward || own.forward,
            paste: flags.paste || own.paste,
            with: own.with.clone().or_else(|| flags.with.clone()),
            button: own.button.or(flags.button),
            hold: own.hold.or(flags.hold),
            gone: flags.gone || own.gone,
            smooth: flags.smooth || own.smooth,
            fill: flags.fill || own.fill,
            close: flags.close || own.close,
            ms: own.ms.or(flags.ms),
            ..Flags::default()
        };
        let route = self.run_step(who, &merged, tracked, view, &step.words)?;
        if let Some(text) = &own.expect {
            let waiting = Flags { ms: merged.ms, ..Flags::default() };
            self.wait_text(&waiting, tracked, view, text)?;
        }
        Ok(route)
    }

    fn run_step(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, words: &[String]) -> Reply {
        self.overlay.over(who, tracked.hwnd);
        let verb = words.first().map_or("", String::as_str);
        reject_step_flags(verb, flags, None)?;
        let rest = words.get(1..).unwrap_or(&[]);
        let direct_win = verb == "key"
            && rest.len() == 1
            && real::chord(&rest[0]).is_ok_and(|c| act::WinShortcut::from_chord(&c).is_some());
        // A minimized browser or Electron window drops or holds its input until it is shown again,
        // even one started to take input while covered. It is shown off screen while an agent works
        // in it, so this only happens when that could not be done. Direct Win shortcuts resize the
        // window or launch system shells without sending keystrokes to Chromium.
        if !flags.real && !flags.mouse && !direct_win && ACTIONS.contains(&verb) && win::is_chromium(tracked.hwnd) && win::minimized(tracked.hwnd) {
            return Err(COVERED.into());
        }
        match (verb, rest) {
            ("click", [word]) => self.click(who, flags, tracked, view, word),
            ("hover", [word]) => self.hover(who, flags, tracked, view, word),
            ("set", [word, value]) => self.set(who, flags, tracked, view, word, value),
            ("type", _) if flags.paste => self.paste(who, flags, tracked, view, rest),
            ("type", _) => self.type_text(who, flags, tracked, view, rest),
            ("clear", [word]) => self.set(who, flags, tracked, view, word, ""),
            ("toggle", [word]) => self.click(who, flags, tracked, view, word),
            ("check", [word]) => self.ensure(who, flags, tracked, view, word, ("on", "off")),
            ("uncheck", [word]) => self.ensure(who, flags, tracked, view, word, ("off", "on")),
            ("expand", [word]) => self.ensure(who, flags, tracked, view, word, ("open", "closed")),
            ("collapse", [word]) => self.ensure(who, flags, tracked, view, word, ("closed", "open")),
            ("down", _) => self.press_down(who, flags, tracked, view, rest),
            ("key", [spec]) => self.key(flags, tracked, view, spec),
            ("select", [word, item]) => self.select(who, flags, tracked, view, word, item),
            ("scroll", _) => self.scroll(who, flags, tracked, view, rest),
            ("show", [word]) => match self.resolve(word, who, tracked, view)? {
                Target::Control(id) if view.nodes[&id].can.scroll_item => view.nodes[&id].element.bring_into_view().map(|_| "show".to_string()),
                Target::Control(_) => Err("that control cannot scroll into view".into()),
                _ => Err("show needs a control".into()),
            },
            ("drag", [from, to]) => self.drag(who, flags, tracked, view, from, to),
            ("draw", _) => self.draw(who, flags, tracked, view, rest),
            ("wait", [text]) => self.wait(flags, tracked, view, text),
            ("expect", [text]) => self.wait_text(flags, tracked, view, text),
            ("pick-file", [path]) => self.pick_file(who, flags, tracked, view, path),
            _ => Err(format!("can't do '{}': see deskcrew help", words.join(" "))),
        }
    }

    /// Run steps against one window, then report the result as a single set of changes.
    fn act(&self, who: &str, flags: &Flags, steps: &[Step]) -> Reply {
        if self.stop.load(Ordering::Relaxed) {
            return Err(STOPPED.into());
        }
        let tracked = self.pick(who, flags.window.as_deref())?;
        // A covered browser window is drawn and takes input while an agent works in it.
        let _drawn = win::keep_drawn(tracked.hwnd, steps.iter().any(|step| step.words.first().is_some_and(|verb| verb == "draw") || step.words.iter().skip(1).any(|word| is_point(word).is_some())));
        let mut guard = tracked.view.lock().unwrap();
        let view = &mut *guard;
        if !view.read_once {
            self.first_read(&tracked, view, false)?;
        }
        let before = self.seen(who, tracked.hwnd).unwrap_or_else(|| view.lines.clone());
        let title_before = win::title(tracked.hwnd);
        let picture_before = shot::window(tracked.hwnd).ok().filter(|(frame, _)| !frame.is_blank());
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
        win::stop_flashing(tracked.hwnd);

        let mut out = Vec::new();
        out.push(if routes.len() > 1 { format!("ok {} steps", routes.len()) } else { format!("ok {}", routes.first().map_or("", String::as_str)) });
        match settled {
            Ok(notes) => {
                let (changes, complete) = lines::change_report(&before, &view.lines);
                let title = win::title(tracked.hwnd);
                let retitled = title != title_before && !title.is_empty();
                let nothing = changes.is_empty() && notes.is_empty() && !retitled;
                out.extend(notes);
                out.extend(changes);
                // Many apps show what an action did in their title: a file name, a saved mark.
                if retitled {
                    out.push(format!("~ title \"{}\"", lines::clip(&title, 80)));
                }
                if complete {
                    self.remember(who, tracked.hwnd, view.lines.clone());
                }
                // The controls tell nothing: the picture still can, without sending one.
                let picture = if nothing && !waited_only && !routes.iter().any(|r| r.starts_with("dialog") || r == "focus") { picture_before.as_ref().map(|before| self.picture_change(&tracked, before)) } else { None };
                match picture {
                    Some(Some(note)) => out.push(note),
                    Some(None) if routes.iter().any(|r| r == "message") => out.push("nothing changed, on screen either: try --real if it should have".into()),
                    Some(None) => out.push("nothing changed, on screen either".into()),
                    // A minimized window has no picture to tell by, and --real would restore it.
                    None if nothing && win::minimized(tracked.hwnd) => out.push("no control changed; the window is minimized, so its picture could not be checked".into()),
                    None if nothing && routes.iter().any(|r| r == "message") => out.push("nothing changed: try --real if it should have".into()),
                    None if nothing && routes.iter().any(|r| r == "pen" || r == "mouse" || r == "keyboard") => out.push("nothing changed: check with shot".into()),
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

    /// Where the window's picture changed since `before`, in window pixels, ignoring a caret.
    fn picture_change(&self, tracked: &Tracked, (before, units): &(Frame, f32)) -> Option<String> {
        let (after, _) = shot::window(tracked.hwnd).ok()?;
        let (x, y, w, h) = after.changed_from(before)?;
        if w < LEAST_CHANGE || h < LEAST_CHANGE {
            return None;
        }
        let at = |value: u32| (value as f32 * units).round() as i32;
        Some(format!("controls unchanged; the picture changed at {},{} {}x{}", at(x), at(y), at(w), at(h)))
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
        Ok(if shown.is_empty() { format!("no windows{}", out_of_sight()) } else { shown.join("\n") })
    }

    fn see(&self, who: &str, flags: &Flags, words: &[String]) -> Reply {
        let spec = words.first().map(String::as_str).or(flags.window.as_deref());
        let tracked = self.pick(who, spec)?;
        // A covered browser window is drawn and takes input while an agent works in it.
        let _drawn = win::keep_drawn(tracked.hwnd, false);
        let mut guard = tracked.view.lock().unwrap();
        let view = &mut *guard;
        self.first_read(&tracked, view, flags.all)?;
        // A minimized browser leaves its page out of accessibility until it is shown again.
        let unread = (win::is_chromium(tracked.hwnd) && win::minimized(tracked.hwnd))
            .then_some("the page itself is not read while this window is minimized: Windows would not let deskcrew show it off screen without it coming forward just now. Ask the person to restore it");
        let filtered = flags.find.is_some() || flags.from > 0;
        let seen = self.seen(who, tracked.hwnd).filter(|_| !flags.full && !filtered && !flags.all);
        if let Some(seen) = seen {
            let changes = lines::changes(&seen, &view.lines);
            if changes.len() * 2 <= view.lines.len().max(1) {
                self.remember(who, tracked.hwnd, view.lines.clone());
                let mut said = if changes.is_empty() { vec!["no change".to_string()] } else { changes };
                said.extend(unread.map(String::from));
                return Ok(said.join("\n"));
            }
        }
        if !filtered {
            self.remember(who, tracked.hwnd, view.lines.clone());
        }
        let hidden = if view.hidden > 0 { format!(", {} off screen", view.hidden) } else { String::new() };
        let mut out = vec![format!("w{} \"{}\" {} controls{hidden}", tracked.id, lines::clip(&win::title(tracked.hwnd), 50), view.lines.len())];
        out.extend(lines::page(&view.lines, flags.find.as_deref(), flags.from));
        if !filtered {
            out.extend(self.rows_off_screen(view));
            out.extend(self.blind_spot(who, &tracked, view));
        }
        out.extend(unread.map(String::from));
        Ok(out.join("\n"))
    }

    /// The rows of a table that scrolled out of the list, when they fit in the reply: one read
    /// then holds the whole table, instead of a `table` call after it.
    fn rows_off_screen(&self, view: &View) -> Vec<String> {
        let mut out = Vec::new();
        for (id, node) in view.nodes.iter().filter(|(_, n)| n.parts.role == "table") {
            let Ok(table) = node.element.grid(0, MAX_TABLE_ROWS, MAX_TABLE_COLUMNS, &self.uia) else { continue };
            let listed: HashSet<&Vec<String>> = view.nodes.values().filter(|n| n.parts.role == "row" && !n.parts.cells.is_empty()).map(|n| &n.parts.cells).collect();
            let rest: Vec<String> = table.rows.iter().filter(|row| !listed.contains(row)).map(|row| row.iter().map(|cell| lines::clip(cell, MAX_CELL_CHARS)).collect::<Vec<_>>().join(" | ")).collect();
            if rest.is_empty() || table.rows.len() < table.total_rows && table.total_rows > MAX_TABLE_ROWS {
                continue;
            }
            if rest.iter().map(|line| line.len() + 1).sum::<usize>() > TABLE_CHARS {
                out.push(format!("e{id} table: {} more rows off screen: table e{id} reads them", rest.len()));
                continue;
            }
            out.push(format!("e{id} table, {} more rows off screen:", rest.len()));
            out.extend(rest);
        }
        out
    }

    /// Point out the largest part of the window with no controls in it, and read any text there,
    /// so the agent knows when the list falls short and needs a picture of only that part.
    fn blind_spot(&self, who: &str, tracked: &Tracked, view: &mut View) -> Vec<String> {
        let window = win::rect(tracked.hwnd);
        let mut rects: Vec<_> = view.nodes.values().map(|n| (n.rect, CONTAINERS.contains(&n.parts.role.as_str()))).collect();
        rects.extend(view.quiet.iter().map(|rect| (*rect, false)));
        let Some((x, y, w, h)) = look::empty_area(window, &rects) else { return Vec::new() };
        let Ok((frame, units)) = shot::window(tracked.hwnd) else { return Vec::new() };
        let inside = |value: i32| (value as f32 / units) as u32;
        let part = frame.crop(inside(x - window.0), inside(y - window.1), inside(w).max(1), inside(h).max(1));
        let blank = frame.is_blank();
        if !blank && !part.busy() {
            return Vec::new();
        }
        let hash = part.hash();
        if let Some((seen, said)) = &view.blind
            && *seen == hash
        {
            return said.clone();
        }
        // The smallest control holding the area, so a picture of it covers the area and little else.
        let holder = view
            .nodes
            .iter()
            .filter(|(_, n)| n.rect.0 <= x + w / 8 && n.rect.1 <= y + h / 8 && n.rect.0 + n.rect.2 >= x + w * 7 / 8 && n.rect.1 + n.rect.3 >= y + h * 7 / 8)
            .min_by_key(|(_, n)| n.rect.2 as i64 * n.rect.3 as i64)
            .map(|(id, _)| *id);
        let picture = holder.map_or_else(|| format!("shot --region {x},{y},{w},{h}"), |id| format!("shot e{id}"));
        let (left, top) = (x - window.0, y - window.1);
        let texts = if blank { Vec::new() } else { look::read_text(&part).unwrap_or_default() };
        let said = if texts.is_empty() {
            vec![format!("no controls in {left},{top} {w}x{h}, only drawing: {picture}")]
        } else {
            let mut said = vec![format!("no controls in {left},{top} {w}x{h}; text read there (x,y for click):")];
            for text in &texts {
                let (tx, ty, tw, th) = text.rect;
                let centre = |at: i32, size: i32, offset: i32| offset + ((at as f32 + size as f32 / 2.0) * units).round() as i32;
                said.push(format!("\"{}\" {},{}", lines::clip(&text.text, 60), centre(tx, tw, left), centre(ty, th, top)));
            }
            said.push(format!("for drawings: {picture}"));
            // These x,y are window pixels: clicks read them that way until the next shot.
            let mut agents = self.agents.lock().unwrap();
            let state = agents.entry(who.to_string()).or_default();
            let frames = Frames { x: 0, y: 0, scale: 1.0 };
            match state.shots.get_mut(&tracked.hwnd) {
                Some(last) => {
                    last.frames = frames;
                    last.hash = 0;
                }
                None => {
                    state.shots.insert(tracked.hwnd, LastShot { hash: 0, frame: Frame { w: 0, h: 0, pixels: Vec::new() }, frames });
                }
            }
            said
        };
        view.blind = Some((hash, said.clone()));
        said
    }

    /// A table or grid as tab-separated rows, headers first, so a list of files or a data grid
    /// reads in one call.
    fn table(&self, who: &str, flags: &Flags, words: &[String]) -> Reply {
        let word = words.first().ok_or("table needs a control")?;
        let tracked = self.pick(who, flags.window.as_deref())?;
        // A covered browser window is drawn and takes input while an agent works in it.
        let _drawn = win::keep_drawn(tracked.hwnd, false);
        let mut view = tracked.view.lock().unwrap();
        if !view.read_once {
            self.first_read(&tracked, &mut view, false)?;
        }
        let Target::Control(id) = self.resolve(word, who, &tracked, &mut view)? else { return Err("table needs a control".into()) };
        // `--from N` starts at row N. As many rows follow as fit in one reply.
        let skip = flags.from.saturating_sub(1);
        let table = view.nodes[&id].element.grid(skip, MAX_TABLE_ROWS, MAX_TABLE_COLUMNS, &self.uia)?;
        // Explorer wraps dates in invisible direction marks, which only cost characters.
        let clip = |cells: &[String]| cells.iter().map(|cell| lines::clip(&cell.replace(['\u{200e}', '\u{200f}'], ""), MAX_CELL_CHARS)).collect::<Vec<_>>().join("\t");
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
        Ok(out.join("\n"))
    }

    /// Fill a Windows Open or Save dialog's file name and press its Open or Save button. The field
    /// and button are found by their ids, which are the same in every language.
    fn pick_file(&self, who: &str, flags: &Flags, tracked: &Tracked, view: &mut View, path: &str) -> Reply {
        self.read(tracked, view, false)?;
        let field = view
            .nodes
            .iter()
            .find(|(_, node)| node.parts.role == "edit" && ["1001", "1148"].contains(&node.control_id.as_str()))
            .map(|(id, _)| *id)
            .ok_or("no Open or Save dialog is showing in this window")?;
        let button = view
            .nodes
            .iter()
            .find(|(_, node)| node.parts.role == "button" && node.control_id == "1")
            .map(|(id, _)| *id)
            .ok_or("that dialog has no Open or Save button")?;
        self.set(who, flags, tracked, view, &format!("e{field}"), path)?;
        self.click(who, flags, tracked, view, &format!("e{button}"))?;
        // The dialog closes a moment after its button is pressed: wait, so the reply shows it gone.
        let started = Instant::now();
        while started.elapsed() < DIALOG_CLOSE {
            thread::sleep(WAIT_STEP);
            self.read(tracked, view, false)?;
            if !view.nodes.contains_key(&field) {
                return Ok("dialog closed".into());
            }
        }
        Ok("dialog still open: see what it asks".into())
    }

    fn get(&self, who: &str, flags: &Flags, words: &[String]) -> Reply {
        let word = words.first().ok_or("get needs a control")?;
        let tracked = self.pick(who, flags.window.as_deref())?;
        // A covered browser window is drawn and takes input while an agent works in it.
        let _drawn = win::keep_drawn(tracked.hwnd, false);
        let mut view = tracked.view.lock().unwrap();
        if !view.read_once {
            self.first_read(&tracked, &mut view, false)?;
        }
        let Target::Control(id) = self.resolve(word, who, &tracked, &mut view)? else { return Err("get needs a control".into()) };
        if view.nodes[&id].parts.states.contains(&"password") {
            return Err("that is a password field: its text stays hidden".into());
        }
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
        if let Some(region) = &flags.region {
            let Some((x, y, w, h)) = lines::parse_rect(region) else {
                return Err("--region x,y,w,h (width and height >= 1)".into());
            };
            let (small, scale) = self.capture_screen(x, y, w, h)?.shrink(edge);
            let path = shot::path_for(who, 0);
            small.save(&path)?;
            return Ok(format!("{} {}x{} scale {scale:.2}", path.display(), small.w, small.h));
        }
        if flags.screen {
            let (mut left, mut top, (mut width, mut height)) = (0, 0, crate::overlay::screen_size());
            if flags.all {
                let all = win::screens();
                left = all.iter().map(|s| s.0).min().unwrap_or(0);
                top = all.iter().map(|s| s.1).min().unwrap_or(0);
                width = all.iter().map(|s| s.0 + s.2).max().unwrap_or(width) - left;
                height = all.iter().map(|s| s.1 + s.3).max().unwrap_or(height) - top;
            }
            let frame = self.capture_screen(left, top, width, height)?;
            let (small, scale) = frame.shrink(edge);
            let path = shot::path_for(who, 0);
            small.save(&path)?;
            return Ok(format!("{} {}x{} scale {scale:.2}", path.display(), small.w, small.h));
        }
        let word = words.first();
        let control = word.and_then(|w| is_ref(w));
        let tracked = self.pick(who, if control.is_some() { flags.window.as_deref() } else { word.map(String::as_str).or(flags.window.as_deref()) })?;
        // A covered browser window is drawn and takes input while an agent works in it.
        let _drawn = win::keep_drawn(tracked.hwnd, true);
        let (left, top, ..) = win::rect(tracked.hwnd);
        let mut note = "";
        let (mut frame, mut units) = shot::window(tracked.hwnd)?;
        if frame.is_blank() {
            // A copy of the screen shows whatever is on top, so it is only worth taking when some
            // of the window is in view.
            if win::fully_covered(tracked.hwnd) {
                note = " (blank: this app draws itself some way a window copy cannot see, and the window is covered, so the screen cannot show it either)";
            } else {
                let (x, y, w, h) = win::rect(tracked.hwnd);
                let copy = self.capture_screen(x, y, w, h)?;
                if !copy.is_blank() {
                    frame = copy;
                    units = 1.0;
                    note = " (copied from the screen: anything covering the window shows)";
                }
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
            if rect.2 <= 0 || rect.3 <= 0 {
                return Err(format!("e{id} has no visible size"));
            }
            let rx0 = rect.0.max(left);
            let ry0 = rect.1.max(top);
            let vw = rect.0.saturating_add(rect.2).saturating_sub(rx0);
            let vh = rect.1.saturating_add(rect.3).saturating_sub(ry0);
            if vw <= 0 || vh <= 0 {
                return Err(format!("e{id} is outside the window picture"));
            }
            let inside = |value: i32| ((value as f32 / units).round().max(0.0)) as u32;
            frame = frame.crop(inside(rx0 - left), inside(ry0 - top), inside(vw).max(1), inside(vh).max(1));
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
        let last = state.shots.get(&tracked.hwnd);
        if control.is_none() && !flags.full && !flags.marks && !flags.rulers && last.is_some_and(|l| l.hash == hash) {
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
        let (mut small, scale) = picture.shrink(edge);
        let per_pixel = scale * units;
        if flags.marks && per_pixel.is_finite() && per_pixel > 0.0 {
            let to_image = |value: i32, from: i32| ((value - from) as f32 / per_pixel) as i32;
            for (id, (x, y, w, h)) in marked {
                small.mark(to_image(x, origin.0), to_image(y, origin.1), ((w as f32) / per_pixel) as i32, ((h as f32) / per_pixel) as i32, id);
            }
        }
        if flags.rulers {
            small.rulers();
        }
        let path = shot::path_for(who, tracked.id);
        small.save(&path)?;
        let frames = Frames { x: origin.0 - left, y: origin.1 - top, scale: scale * units };
        if control.is_none() {
            state.shots.insert(tracked.hwnd, LastShot { hash, frame, frames });
        } else if let Some(last) = state.shots.get_mut(&tracked.hwnd) {
            last.frames = frames;
        } else {
            state.shots.insert(tracked.hwnd, LastShot { hash: 0, frame: Frame { w: 0, h: 0, pixels: Vec::new() }, frames });
        }
        // An agent taking pictures of a window it has never read is pointed to the cheaper way.
        let unread = if control.is_none() && !flags.marks && !state.seen.contains_key(&tracked.hwnd) { READ_FIRST } else { "" };
        let part = if crop.is_some() { " changed part" } else { "" };
        Ok(format!("{} {}x{} scale {scale:.2}{part}{note}", path.display(), small.w, small.h) + unread)
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
        if act::is_shell_window(tracked.hwnd) {
            return Err("refused: that window is the Windows desktop or taskbar".into());
        }
        let word = words.first().ok_or("place x,y,w,h|min|max|restore|left|right|monitor N")?;
        let joined = words.join(" ");
        let joined = joined.trim();
        // A window in the back stays there: Windows' own minimize, maximize and restore activate
        // the window, which would take the person's keyboard. Their own window is theirs to change.
        let front = win::foreground();
        let theirs = front == tracked.hwnd;
        let _lock = (!theirs).then(win::ForegroundLock::take);
        let mut said = "ok";
        match word.as_str() {
            "min" => win::place_min(tracked.hwnd, theirs)?,
            "max" if theirs => win::place_max_front(tracked.hwnd)?,
            "max" => {
                if !win::fill_screen_behind(tracked.hwnd)? {
                    said = "ok: it fills the screen in the back; Windows' own maximize would have brought it forward";
                }
            }
            "restore" if theirs => win::place_restore_front(tracked.hwnd)?,
            "restore" | "show" => win::restore_behind(tracked.hwnd)?,
            "left" => win::snap_half(tracked.hwnd, false)?,
            "right" => win::snap_half(tracked.hwnd, true)?,
            _ if joined == "monitor" || joined.starts_with("monitor ") => {
                let index: usize = joined["monitor".len()..].trim().parse().ok().ok_or("place monitor N (see deskcrew screens)")?;
                win::move_to_monitor(tracked.hwnd, index)?;
            }
            _ => {
                let Some((x, y, w, h)) = lines::parse_rect(joined) else {
                    return Err("place x,y,w,h|min|max|restore|left|right|monitor N".into());
                };
                win::place_rect(tracked.hwnd, x, y, w, h)?;
            }
        }
        // Should the app have come forward all the same, the person gets their window back.
        let fg = win::foreground();
        let pids = win::app_pids(tracked.hwnd);
        if !theirs && front != 0 && fg != front && (fg == tracked.hwnd || pids.contains(&win::pid(fg)) || pids.contains(&win::content_pid(fg))) {
            thread::sleep(Duration::from_millis(30));
            if !win::bring_forward(front) {
                win::stop_flashing(front);
            }
        }
        win::stop_flashing(tracked.hwnd);
        Ok(said.into())
    }

    fn status(&self) -> Reply {
        let agents = self.agents.lock().unwrap();
        let mut names: Vec<&String> = agents.keys().collect();
        names.sort();
        let names: Vec<&str> = names.iter().map(|n| n.as_str()).collect();
        let mut out = format_status(self.stop.load(Ordering::Relaxed), &names, self.overlay.stop_keys_error());
        if self.quiet.load(Ordering::Relaxed) {
            out.push_str("; quiet: on");
        }
        Ok(out)
    }

    fn handle(&self, who: &str, client_pid: Option<u32>, words: &[String]) -> Reply {
        let caller = CallerLookup::new(client_pid);
        let (flags, rest) = parse(words);
        if let Some(err) = &flags.flag_error {
            return Err(err.clone());
        }
        flags.modifiers()?;
        let verb = rest.first().map_or("", String::as_str);
        reject_step_flags(verb, &flags, None)?;
        let args = rest.get(1..).unwrap_or(&[]);
        if stopped_command(verb, args, &flags) && self.stop.load(Ordering::Relaxed) {
            return Err(STOPPED.into());
        }
        if self.quiet.load(Ordering::Relaxed) && verb != "do"
            && let Some(msg) = quiet_blocked(verb, args, &flags, None)
        {
            return Err(msg.into());
        }
        if !(verb == "do" && flags.dry) && matches!(flags.risk.as_deref(), Some("medium" | "high")) && !flags.force {
            return Err("declared risky: ask the person, then add --force".into());
        }
        match verb {
            "windows" => self.windows_list(who, args.first()),
            "see" => self.see(who, &flags, args),
            "get" => self.get(who, &flags, args),
            "table" => self.table(who, &flags, args),
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
                [clear] if clear == "clear" => apps::set_clipboard("").map(|_| "ok".to_string()),
                _ => Err("clip, clip set \"text\", or clip clear".into()),
            },
            "is" => self.is(who, &flags, args),
            "cursor" => self.borrow.set(args.first().map(String::as_str)),
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
            "up" => {
                let count = real::release_of(who);
                let mut home = self.home.lock().unwrap();
                if home.holding_mouse == MouseHold::Held && !real::holding_button() {
                    home.holding_mouse = MouseHold::Off;
                }
                Ok(format!("let go of {count}"))
            }
            "notifications" => self.notifications(who, args),
            "apps" => {
                let list = self.windows_list(who, None)?;
                let mut names: Vec<&str> = list.lines().filter_map(|line| line.rsplit_once("\" ").map(|(_, name)| name.trim())).collect();
                names.sort_unstable();
                names.dedup();
                Ok(names.join("\n"))
            }
            "screens" => Ok(win::screens().iter().enumerate().map(|(at, s)| format!("{at}: {},{} {}x{}{}", s.0, s.1, s.2, s.3, if s.4 { " primary" } else { "" })).collect::<Vec<_>>().join("\n")),
            "status" => self.status(),
            "resume" => {
                self.stop.store(false, Ordering::Relaxed);
                Ok("ok".into())
            }
            "do" => {
                let script = args.first().ok_or("do \"click e1; key enter\"")?;
                let mut steps: Vec<Step> = lines::split_steps(script)
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
                        self.first_read(&tracked, &mut view, false)?;
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
                    step.flags.modifiers()?;
                    if step.flags.window.is_some() {
                        return Err("a step cannot pick another window: a do runs in one, so run another command for that window".into());
                    }
                    if matches!(step.flags.risk.as_deref(), Some("medium" | "high")) && !flags.force && !step.flags.force {
                        return Err("declared risky: ask the person, then add --force".into());
                    }
                    let step_verb = step.words.first().map_or("", String::as_str);
                    reject_step_flags(step_verb, &flags, Some(&step.flags))?;
                }
                if self.quiet.load(Ordering::Relaxed) {
                    if let Some(msg) = quiet_blocked("do", args, &flags, Some(&steps)) {
                        return Err(msg.into());
                    }
                    for step in &steps {
                        let step_verb = step.words.first().map_or("", String::as_str);
                        let step_args = step.words.get(1..).unwrap_or(&[]);
                        if let Some(msg) = quiet_blocked(step_verb, step_args, &step.flags, None) {
                            return Err(msg.into());
                        }
                    }
                }
                if flags.mouse || steps.iter().any(|step| step.flags.mouse) {
                    self.borrow.allow(who, caller.resolve())?;
                }
                steps.extend(expected(&flags));
                self.act(who, &flags, &steps)
            }
            "click" if !args.is_empty() => {
                if flags.mouse {
                    self.borrow.allow(who, caller.resolve())?;
                }
                let mut steps: Vec<Step> = args.iter().map(|word| Step { flags: Flags::default(), words: vec!["click".to_string(), word.clone()] }).collect();
                steps.extend(expected(&flags));
                self.act(who, &flags, &steps)
            }
            "draw" if !flags.real && !flags.mouse && self.pick(who, flags.window.as_deref()).is_ok_and(|t| win::is_chromium(t.hwnd)) => {
                let drawn = self.draw_together(who, &flags, args)?;
                // --expect is checked here too, as on every other action.
                match expected(&flags) {
                    Some(step) => self.act(who, &flags, &[step]).map(|found| format!("{drawn}\n{found}")).map_err(|error| format!("{drawn}\n{error}")),
                    None => Ok(drawn),
                }
            }
            "key" if !flags.real && flags.hold.is_none() && flags.window.is_none() && flags.expect.is_none() && args.len() == 1 => {
                let spec = &args[0];
                let chord = real::chord(spec)?;
                if act::is_shell_win_shortcut(&chord) {
                    if self.stop.load(Ordering::Relaxed) {
                        return Err(STOPPED.into());
                    }
                    if self.quiet.load(Ordering::Relaxed) {
                        return Err(format!("refused: quiet mode is on ({spec} opens foreground shell UI; run 'deskcrew quiet off' to allow)"));
                    }
                    refuse_risky(&chord, spec, &flags)?;
                    return act::win_shortcut(&chord, None).unwrap().map(|route| format!("ok {route}"));
                }
                if chord.has_win() && act::WinShortcut::from_chord(&chord).is_none() {
                    refuse_risky(&chord, spec, &flags)?;
                    return Err(format!("{spec} needs the real keyboard: add --real"));
                }
                let mut steps = vec![Step { flags: Flags::default(), words: rest.clone() }];
                steps.extend(expected(&flags));
                self.act(who, &flags, &steps)
            }
            "set" | "type" | "key" | "select" | "scroll" | "show" | "drag" | "draw" | "wait" | "click" | "hover" | "check" | "uncheck" | "toggle" | "expand" | "collapse" | "clear" | "down" | "pick-file" => {
                if flags.mouse
                    && matches!(
                        verb,
                        "click"
                            | "hover"
                            | "set"
                            | "type"
                            | "clear"
                            | "toggle"
                            | "check"
                            | "uncheck"
                            | "expand"
                            | "collapse"
                            | "down"
                            | "scroll"
                            | "drag"
                            | "draw"
                            | "pick-file"
                    )
                {
                    self.borrow.allow(who, caller.resolve())?;
                }
                let mut steps = vec![Step { flags: Flags::default(), words: rest.clone() }];
                steps.extend(expected(&flags));
                self.act(who, &flags, &steps)
            }
            "" | "help" => Ok(HELP.to_string()),
            _ => Err(format!("unknown command '{verb}': deskcrew help")),
        }
    }

    fn serve(&self, pipe: Pipe) {
        uia::enter_com();
        let client_pid = pipe.client_pid();
        if let Ok(message) = pipe.receive() {
            let active = ActiveGuard::enter(&self.active, &self.last_request);
            let text = String::from_utf8_lossy(&message);
            let mut words = text.split('\0').map(str::to_string);
            let who = words.next().unwrap_or_default();
            let words: Vec<String> = words.collect();
            let quitting = words.first().is_some_and(|w| w == "quit");
            let reply = if quitting { Ok("ok".to_string()) } else { self.handle(&who, client_pid, &words) };
            let (code, body) = match reply {
                Ok(body) => (0, body),
                Err(body) => (1, body),
            };
            let mut framed = vec![code];
            framed.extend_from_slice(body.as_bytes());
            let _ = pipe.send(&framed);
            pipe.finish();
            drop(active);
            if quitting {
                real::release_all();
                std::process::exit(0);
            }
        }
    }
}

struct ActiveGuard<'a> {
    active: &'a AtomicUsize,
    last_request: &'a Mutex<Instant>,
}

impl<'a> ActiveGuard<'a> {
    fn enter(active: &'a AtomicUsize, last_request: &'a Mutex<Instant>) -> Self {
        active.fetch_add(1, Ordering::SeqCst);
        *last_request.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
        Self { active, last_request }
    }
}

impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        *self.last_request.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
        self.active.fetch_sub(1, Ordering::SeqCst);
    }
}

fn should_idle_exit(active: usize, idle: Duration) -> bool {
    (active == 0 && idle > IDLE_EXIT) || idle > HUNG_EXIT
}

fn format_status(stopped: bool, names: &[&str], stop_keys_error: Option<u32>) -> String {
    let state = if stopped { "stopped by the person" } else { "running" };
    let stop_keys = match stop_keys_error {
        None => String::new(),
        Some(windows_sys::Win32::Foundation::ERROR_HOTKEY_ALREADY_REGISTERED) => {
            "; Ctrl+Alt+Esc stop keys unavailable: another process holds them".into()
        }
        Some(code) => format!("; Ctrl+Alt+Esc stop keys unavailable (error {code})"),
    };
    format!(
        "deskcrew {} {state}; agents: {}{stop_keys}",
        env!("CARGO_PKG_VERSION"),
        if names.is_empty() { "none".into() } else { names.join(", ") }
    )
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
        borrow: Permission::default(),
        home: Mutex::new(Home::default()),
        keeper: OnceLock::new(),
        stop,
        quiet: AtomicBool::new(env_quiet()),
        last_request: Mutex::new(Instant::now()),
        active: AtomicUsize::new(0),
    });

    win::watch_flashes();
    let keeper = service.clone();
    let _ = service.keeper.set(thread::spawn(move || keeper.keep_focus()).thread().clone());

    let watcher = service.clone();
    thread::spawn(move || {
        loop {
            thread::sleep(Duration::from_secs(30));
            let idle = watcher.last_request.lock().unwrap_or_else(|e| e.into_inner()).elapsed();
            if should_idle_exit(watcher.active.load(Ordering::SeqCst), idle) {
                real::release_all();
                std::process::exit(0);
            }
        }
    });

    loop {
        if listening.accept().is_err() {
            break;
        }
        // The next instance exists before this one is handed off, so the name never lapses and
        // a caller can never conclude the service is down and start a second one.
        let Ok(next) = Pipe::listen(false) else {
            break;
        };
        let current = std::mem::replace(&mut listening, next);
        let worker = service.clone();
        thread::spawn(move || worker.serve(current));
    }
    drop(listening);
    real::release_all();
    std::process::exit(0);
}

/// Verbs that change windows or the clipboard outside an action's own checks: refused after the
/// person pressed the stop keys, like every action. Closing the notification panel stays allowed.
fn changes_things(verb: &str, args: &[String]) -> bool {
    match verb {
        "focus" | "close" | "place" => true,
        "clip" => !args.is_empty(),
        "notifications" => args.first().is_none_or(|word| word != "close"),
        _ => false,
    }
}

/// Every command that acts on windows or state and should refuse before prompting if stopped.
fn stopped_command(verb: &str, args: &[String], flags: &Flags) -> bool {
    changes_things(verb, args)
        || (verb == "do" && !flags.dry)
        || (verb == "cursor" && args.first().is_some_and(|w| matches!(w.as_str(), "ask" | "session" | "always")))
        || (matches!(verb, "quiet" | "background-only") && args.first().map(String::as_str) == Some("off"))
        || matches!(
            verb,
            "launch"
                | "set"
                | "type"
                | "key"
                | "select"
                | "scroll"
                | "show"
                | "drag"
                | "draw"
                | "wait"
                | "click"
                | "hover"
                | "check"
                | "uncheck"
                | "toggle"
                | "expand"
                | "collapse"
                | "clear"
                | "down"
                | "pick-file"
        )
}

/// Shortcuts that end the session or open the shell need --force, however they are written and
/// whatever keys are already held.
fn refuse_risky(chord: &real::Chord, spec: &str, flags: &Flags) -> Result<(), String> {
    if real::is_risky_chord(chord) && !flags.force {
        return Err(format!("{spec} is refused: it can end the session or open the shell (--force to allow)"));
    }
    Ok(())
}

fn clamp_caret((x, y, h): (i32, i32, i32), bounds: Option<(i32, i32, i32, i32)>) -> (i32, i32, i32) {
    match bounds {
        Some((bx, _, bw, _)) if bw > 4 => (x.clamp(bx, bx + bw - 4), y, h),
        Some((bx, _, _, _)) => (bx, y, h),
        None => (x, y, h),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_flags_parsing_fill_and_close() {
        let words = vec!["rect".to_string(), "10,10".to_string(), "50,50".to_string(), "--fill".to_string(), "--close".to_string()];
        let (flags, rest) = parse(&words);
        assert!(flags.fill);
        assert!(flags.close);
        assert_eq!(rest, vec!["rect", "10,10", "50,50"]);
    }

    #[test]
    fn active_guard_decrements_on_drop() {
        let active = AtomicUsize::new(0);
        let last_request = Mutex::new(Instant::now() - Duration::from_secs(10));
        {
            let _guard = ActiveGuard::enter(&active, &last_request);
            assert_eq!(active.load(Ordering::SeqCst), 1);
            assert!(last_request.lock().unwrap().elapsed() < Duration::from_secs(1));
        }
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }

    #[cfg(panic = "unwind")]
    #[test]
    fn active_guard_decrements_on_panic() {
        let active = AtomicUsize::new(0);
        let last_request = Mutex::new(Instant::now());
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = ActiveGuard::enter(&active, &last_request);
            assert_eq!(active.load(Ordering::SeqCst), 1);
            panic!("boom");
        }));
        assert!(outcome.is_err());
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn idle_exit_waits_for_idle_and_bounds_hung_requests() {
        assert!(!should_idle_exit(0, IDLE_EXIT));
        assert!(should_idle_exit(0, IDLE_EXIT + Duration::from_secs(1)));
        assert!(!should_idle_exit(1, IDLE_EXIT + Duration::from_secs(1)));
        assert!(should_idle_exit(1, HUNG_EXIT + Duration::from_secs(1)));
    }

    #[test]
    fn status_reports_when_stop_keys_are_held_elsewhere() {
        let ok = format_status(false, &["Cursor"], None);
        assert!(ok.contains("running") && ok.contains("Cursor") && !ok.contains("unavailable"));

        let held = format_status(false, &[], Some(windows_sys::Win32::Foundation::ERROR_HOTKEY_ALREADY_REGISTERED));
        assert!(held.contains("running") && held.contains("Ctrl+Alt+Esc") && held.contains("another process holds them"));
    }

    #[test]
    fn step_flags_reject_invalid_combinations() {
        let (right, _) = parse(&["--right".into(), "--mouse".into()]);
        let (button, _) = parse(&["--button".into(), "right".into()]);
        let (real, _) = parse(&["--real".into()]);
        let (plain, _) = parse(&[]);

        assert!(click_flags(&right));
        assert!(click_flags(&button));
        assert!(!click_flags(&real));

        for verb in ["check", "uncheck", "expand", "collapse"] {
            assert!(reject_step_flags(verb, &right, None).is_err());
            assert!(reject_step_flags(verb, &plain, Some(&button)).is_err());
            assert!(reject_step_flags(verb, &real, None).is_ok());
        }
        assert!(reject_step_flags("select", &real, None).is_err());
        assert!(reject_step_flags("select", &plain, Some(&right)).is_err());
        assert!(reject_step_flags("select", &plain, None).is_ok());
    }

    #[test]
    fn clamp_caret_handles_zero_and_tiny_bounds_without_panicking() {
        assert_eq!(clamp_caret((50, 20, 16), Some((0, 0, 0, 0))), (0, 20, 16));
        assert_eq!(clamp_caret((50, 20, 16), Some((10, 0, 4, 20))), (10, 20, 16));
        assert_eq!(clamp_caret((50, 20, 16), Some((10, 0, 100, 20))), (50, 20, 16));
        assert_eq!(clamp_caret((200, 20, 16), Some((10, 0, 100, 20))), (106, 20, 16));
    }

    #[test]
    fn parse_rejects_malformed_or_missing_flag_values() {
        let bad_hold = parse(&["key".into(), "win+e".into(), "--hold".into(), "abc".into(), "--force".into()]);
        assert!(bad_hold.0.flag_error.as_deref().is_some_and(|e| e.contains("--hold")));
        let hold_flag_eat = parse(&["key".into(), "win+e".into(), "--hold".into(), "--force".into()]);
        assert!(hold_flag_eat.0.flag_error.as_deref().is_some_and(|e| e.contains("--hold")));
        let missing_win = parse(&["key".into(), "win+e".into(), "-w".into()]);
        assert!(missing_win.0.flag_error.as_deref().is_some_and(|e| e.contains("-w")));
        let ok = parse(&["key".into(), "a".into(), "--hold".into(), "250".into()]);
        assert!(ok.0.flag_error.is_none());
        assert_eq!(ok.0.hold, Some(250));
        let literal_force = parse(&["key".into(), "win+e".into(), "--".into(), "--force".into()]);
        assert!(!literal_force.0.force);
        assert_eq!(literal_force.1, vec!["key", "win+e", "--force"]);
        let upper_risk = parse(&["click".into(), "Delete".into(), "--risk".into(), "HIGH".into()]);
        assert!(upper_risk.0.flag_error.is_none());
        assert_eq!(upper_risk.0.risk.as_deref(), Some("high"));
        let bad_risk = parse(&["click".into(), "Delete".into(), "--risk".into(), "critical".into()]);
        assert!(bad_risk.0.flag_error.as_deref().is_some_and(|e| e.contains("--risk")));
    }

    #[test]
    fn literal_flag_text_is_preserved_without_double_parsing() {
        let words = vec!["type".to_string(), "e3".to_string(), "--".to_string(), "--mouse".to_string(), "--expect".to_string(), "--real".to_string()];
        let (flags, rest) = parse(&words);
        assert!(!flags.mouse);
        assert_eq!(rest, vec!["type", "e3", "--mouse", "--expect", "--real"]);

        let expect_words = vec!["click".to_string(), "e3".to_string(), "--expect".to_string(), "--real".to_string()];
        let (expect_flags, _) = parse(&expect_words);
        let step = expected(&expect_flags).unwrap();
        assert_eq!(step.words, vec!["expect", "--real"]);
        assert!(!step.flags.real);
    }

    #[test]
    fn stopped_commands_are_caught_before_prompting() {
        let flags = Flags::default();
        assert!(stopped_command("click", &[], &flags));
        assert!(stopped_command("do", &[], &flags));
        assert!(!stopped_command("do", &[], &Flags { dry: true, ..Flags::default() }));
        assert!(!stopped_command("see", &[], &flags));
        assert!(stopped_command("cursor", &["ask".into()], &flags));
        assert!(stopped_command("cursor", &["session".into()], &flags));
        assert!(stopped_command("cursor", &["always".into()], &flags));
        assert!(!stopped_command("cursor", &["off".into()], &flags));
        assert!(!stopped_command("cursor", &[], &flags));
        assert!(stopped_command("quiet", &["off".into()], &flags));
        assert!(!stopped_command("quiet", &["on".into()], &flags));
        assert!(!stopped_command("quiet", &[], &flags));
    }

    #[test]
    fn quiet_mode_blocks_focus_and_real_input() {
        let plain = Flags::default();
        assert!(quiet_blocked("focus", &[], &plain, None).is_some());
        assert!(quiet_blocked("down", &[], &plain, None).is_some());
        assert!(quiet_blocked("notifications", &[], &plain, None).is_some());
        assert!(quiet_blocked("notifications", &["close".into()], &plain, None).is_none());
        assert!(quiet_blocked("click", &[], &Flags { real: true, ..Flags::default() }, None).is_some());
        assert!(quiet_blocked("click", &[], &Flags { mouse: true, ..Flags::default() }, None).is_some());
        assert!(quiet_blocked("launch", &[], &Flags { front: true, ..Flags::default() }, None).is_some());
        assert!(quiet_blocked("click", &[], &plain, None).is_none());
        assert!(quiet_blocked("type", &[], &Flags { paste: true, ..Flags::default() }, None).is_none());
        let step_real = vec![Step { flags: Flags { real: true, ..Flags::default() }, words: vec!["click".into(), "e1".into()] }];
        assert!(quiet_blocked("do", &[], &plain, Some(&step_real)).is_some());
    }

    #[test]
    fn numeric_wait_respects_gone_flag() {
        let plain = Flags::default();
        let gone = Flags { gone: true, ..Flags::default() };
        assert_eq!(sleep_ms("1500", &plain), Some(1500));
        assert_eq!(sleep_ms("1500", &gone), None);
        assert_eq!(sleep_ms("Ready", &plain), None);
    }
}
