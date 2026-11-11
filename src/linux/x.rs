// The X11 side of Linux: the window list from the window manager (or the root window when there is
// none, as on a bare Xvfb), pictures, and XTest input for what accessibility cannot do.
use crate::shot::Frame;
use std::{
    collections::HashMap,
    io::Write,
    process::{Command, Stdio},
    sync::{Mutex, OnceLock},
    thread,
    time::{Duration, Instant},
};
use x11rb::{
    connection::Connection,
    protocol::{
        randr::ConnectionExt as _,
        xproto::{self, Atom, AtomEnum, ClientMessageEvent, ConfigureWindowAux, ConnectionExt as _, EventMask, ImageFormat, StackMode, Window as XWindow},
        xtest::ConnectionExt as _,
    },
    rust_connection::RustConnection,
};

pub type Bounds = (i32, i32, i32, i32);

const LAUNCH_WAIT: Duration = Duration::from_secs(10);
const LAUNCH_STEP: Duration = Duration::from_millis(150);
const KEY_GAP: Duration = Duration::from_millis(8);
const MOVE_SETTLE: Duration = Duration::from_millis(15);

#[derive(Clone)]
pub struct Window {
    pub id: u32,
    pub pid: u32,
    pub app: String,
    pub title: String,
    pub rect: Bounds,
}

struct X {
    conn: RustConnection,
    root: XWindow,
    atoms: Mutex<HashMap<&'static str, Atom>>,
}

static X: OnceLock<Result<X, String>> = OnceLock::new();

/// Wayland stops one app from reading or driving another, so deskcrew needs X11.
pub fn display_problem() -> Option<String> {
    let x = std::env::var_os("DISPLAY").is_some_and(|d| !d.is_empty());
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some_and(|d| !d.is_empty());
    match (x, wayland) {
        (true, _) => None,
        (false, true) => Some("deskcrew needs X11, and this is a Wayland session: Wayland does not let one app read or drive another. Log in to an X11 (Xorg) session, or run the apps on Xvfb with DISPLAY set.".into()),
        (false, false) => Some("no X display: set DISPLAY, or start one with Xvfb (for example: Xvfb :99 & export DISPLAY=:99)".into()),
    }
}

fn x() -> Result<&'static X, String> {
    X.get_or_init(|| {
        if let Some(problem) = display_problem() {
            return Err(problem);
        }
        let (conn, screen) = x11rb::connect(None).map_err(|e| format!("could not open the X display: {e}"))?;
        let root = conn.setup().roots[screen].root;
        Ok(X { conn, root, atoms: Mutex::new(HashMap::new()) })
    })
    .as_ref()
    .map_err(Clone::clone)
}

pub fn ready() -> Result<(), String> {
    x().map(|_| ())
}

impl X {
    fn atom(&self, name: &'static str) -> Atom {
        if let Some(atom) = self.atoms.lock().unwrap().get(name) {
            return *atom;
        }
        let atom = self.conn.intern_atom(false, name.as_bytes()).ok().and_then(|c| c.reply().ok()).map_or(0, |r| r.atom);
        self.atoms.lock().unwrap().insert(name, atom);
        atom
    }

    fn property(&self, window: XWindow, name: &'static str, kind: Atom) -> Option<Vec<u8>> {
        let reply = self.conn.get_property(false, window, self.atom(name), kind, 0, 1 << 20).ok()?.reply().ok()?;
        (reply.type_ != 0).then_some(reply.value)
    }

    fn numbers(&self, window: XWindow, name: &'static str, kind: Atom) -> Vec<u32> {
        let reply = self.conn.get_property(false, window, self.atom(name), kind, 0, 1 << 16).ok().and_then(|c| c.reply().ok());
        reply.and_then(|r| r.value32().map(|v| v.collect())).unwrap_or_default()
    }

    fn text(&self, window: XWindow, name: &'static str) -> Option<String> {
        let utf8 = self.atom("UTF8_STRING");
        let bytes = self.property(window, name, utf8).or_else(|| self.property(window, name, AtomEnum::STRING.into()))?;
        Some(String::from_utf8_lossy(&bytes).trim_end_matches('\0').to_string())
    }

    fn supports(&self, name: &'static str) -> bool {
        let wanted = self.atom(name);
        self.numbers(self.root, "_NET_SUPPORTED", AtomEnum::ATOM.into()).contains(&wanted)
    }

    fn has_wm(&self) -> bool {
        !self.numbers(self.root, "_NET_SUPPORTING_WM_CHECK", AtomEnum::WINDOW.into()).is_empty()
    }

    fn message(&self, window: XWindow, kind: &'static str, data: [u32; 5]) -> Result<(), String> {
        let event = ClientMessageEvent::new(32, window, self.atom(kind), data);
        self.conn
            .send_event(false, self.root, EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY, event)
            .map_err(|e| e.to_string())?;
        self.conn.flush().map_err(|e| e.to_string())
    }

    fn rect(&self, window: XWindow) -> Option<Bounds> {
        let geometry = self.conn.get_geometry(window).ok()?.reply().ok()?;
        let at = self.conn.translate_coordinates(window, self.root, 0, 0).ok()?.reply().ok()?;
        Some((at.dst_x as i32, at.dst_y as i32, geometry.width as i32, geometry.height as i32))
    }

    /// Top-level app windows: the window manager's list, or the root's mapped children without one.
    fn clients(&self) -> Vec<XWindow> {
        let listed = self.numbers(self.root, "_NET_CLIENT_LIST_STACKING", AtomEnum::WINDOW.into());
        if !listed.is_empty() {
            return listed.into_iter().rev().collect();
        }
        let listed = self.numbers(self.root, "_NET_CLIENT_LIST", AtomEnum::WINDOW.into());
        if !listed.is_empty() {
            return listed;
        }
        let Some(tree) = self.conn.query_tree(self.root).ok().and_then(|c| c.reply().ok()) else { return Vec::new() };
        let mut out = Vec::new();
        for child in tree.children.into_iter().rev() {
            let Some(attributes) = self.conn.get_window_attributes(child).ok().and_then(|c| c.reply().ok()) else { continue };
            if attributes.override_redirect || attributes.map_state != xproto::MapState::VIEWABLE {
                continue;
            }
            if self.text(child, "WM_NAME").is_some() || self.property(child, "WM_CLASS", AtomEnum::STRING.into()).is_some() {
                out.push(child);
            }
        }
        out
    }

    fn describe(&self, window: XWindow) -> Option<Window> {
        let types = self.numbers(window, "_NET_WM_WINDOW_TYPE", AtomEnum::ATOM.into());
        let unwanted = ["_NET_WM_WINDOW_TYPE_DOCK", "_NET_WM_WINDOW_TYPE_DESKTOP", "_NET_WM_WINDOW_TYPE_TOOLTIP", "_NET_WM_WINDOW_TYPE_NOTIFICATION"];
        if types.iter().any(|t| unwanted.iter().any(|u| self.atom(u) == *t)) {
            return None;
        }
        let rect = self.rect(window)?;
        if rect.2 < 2 || rect.3 < 2 {
            return None;
        }
        let title = self.text(window, "_NET_WM_NAME").or_else(|| self.text(window, "WM_NAME")).unwrap_or_default();
        let pid = self.numbers(window, "_NET_WM_PID", AtomEnum::CARDINAL.into()).first().copied().unwrap_or(0);
        let class = self.property(window, "WM_CLASS", AtomEnum::STRING.into()).map(|b| {
            let parts: Vec<String> = b.split(|c| *c == 0).filter(|p| !p.is_empty()).map(|p| String::from_utf8_lossy(p).into_owned()).collect();
            parts.last().cloned().unwrap_or_default()
        });
        let program = (pid > 0).then(|| std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()).flatten().map(|c| c.trim().to_string());
        let app = class.filter(|c| !c.is_empty()).or(program).unwrap_or_default();
        Some(Window { id: window, pid, app, title, rect })
    }
}

/// App windows on screen, front first.
pub fn list() -> Vec<Window> {
    let Ok(x) = x() else { return Vec::new() };
    let me = std::process::id();
    x.clients().into_iter().filter_map(|w| x.describe(w)).filter(|w| w.pid != me).collect()
}

pub fn find(id: u32) -> Option<Window> {
    let x = x().ok()?;
    let attributes = x.conn.get_window_attributes(id).ok()?.reply().ok()?;
    (attributes.map_state == xproto::MapState::VIEWABLE).then(|| x.describe(id)).flatten()
}

/// The window with the keyboard now.
pub fn front() -> Option<u32> {
    let x = x().ok()?;
    if let Some(active) = x.numbers(x.root, "_NET_ACTIVE_WINDOW", AtomEnum::WINDOW.into()).first().filter(|w| **w != 0) {
        return Some(*active);
    }
    let focus = x.conn.get_input_focus().ok()?.reply().ok()?.focus;
    (focus > 1).then_some(focus)
}

/// Raise the window and give it the keyboard, through the window manager when there is one.
pub fn activate(id: u32) -> Result<(), String> {
    let x = x()?;
    if x.has_wm() && x.supports("_NET_ACTIVE_WINDOW") {
        x.message(id, "_NET_ACTIVE_WINDOW", [2, 0, 0, 0, 0])?;
    } else {
        x.conn.configure_window(id, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE)).map_err(|e| e.to_string())?;
        x.conn.set_input_focus(xproto::InputFocus::PARENT, id, x11rb::CURRENT_TIME).map_err(|e| e.to_string())?;
        x.conn.flush().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Ask the window to close, the way its own close button would.
pub fn close(id: u32) -> Result<(), String> {
    let x = x()?;
    if x.has_wm() && x.supports("_NET_CLOSE_WINDOW") {
        return x.message(id, "_NET_CLOSE_WINDOW", [0, 2, 0, 0, 0]);
    }
    let delete = x.atom("WM_DELETE_WINDOW");
    if !x.numbers(id, "WM_PROTOCOLS", AtomEnum::ATOM.into()).contains(&delete) {
        return Err("that window cannot be asked to close".into());
    }
    let event = ClientMessageEvent::new(32, id, x.atom("WM_PROTOCOLS"), [delete, 0, 0, 0, 0]);
    x.conn.send_event(false, id, EventMask::NO_EVENT, event).map_err(|e| e.to_string())?;
    x.conn.flush().map_err(|e| e.to_string())
}

pub fn place(id: u32, how: &str) -> Result<(), String> {
    let x = x()?;
    let state = |add: bool, a: &'static str, b: &'static str| x.message(id, "_NET_WM_STATE", [add as u32, x.atom(a), x.atom(b), 2, 0]);
    match how {
        "min" => x.message(id, "WM_CHANGE_STATE", [3, 0, 0, 0, 0]),
        "max" if x.has_wm() => state(true, "_NET_WM_STATE_MAXIMIZED_VERT", "_NET_WM_STATE_MAXIMIZED_HORZ"),
        "max" => {
            let (sx, sy, sw, sh) = screens().into_iter().find(|s| s.1).map_or((0, 0, 1024, 768), |s| s.0);
            configure(x, id, (sx, sy, sw, sh))
        }
        "restore" | "show" => {
            x.conn.map_window(id).map_err(|e| e.to_string())?;
            if x.has_wm() {
                state(false, "_NET_WM_STATE_MAXIMIZED_VERT", "_NET_WM_STATE_MAXIMIZED_HORZ")?;
            }
            x.conn.flush().map_err(|e| e.to_string())
        }
        box_text => {
            let parts: Vec<i32> = box_text.split(',').filter_map(|n| n.trim().parse().ok()).collect();
            let [bx, by, bw, bh] = parts[..] else { return Err("place x,y,w,h|min|max|restore".into()) };
            configure(x, id, (bx, by, bw, bh))
        }
    }
}

fn configure(x: &X, id: u32, (bx, by, bw, bh): Bounds) -> Result<(), String> {
    let aux = ConfigureWindowAux::new().x(bx).y(by).width(bw.max(1) as u32).height(bh.max(1) as u32);
    x.conn.configure_window(id, &aux).map_err(|e| e.to_string())?;
    x.conn.flush().map_err(|e| e.to_string())
}

/// Each monitor, and whether it is the primary one.
pub fn screens() -> Vec<(Bounds, bool)> {
    let Ok(x) = x() else { return Vec::new() };
    if let Some(monitors) = x.conn.randr_get_monitors(x.root, true).ok().and_then(|c| c.reply().ok())
        && !monitors.monitors.is_empty()
    {
        return monitors.monitors.iter().map(|m| ((m.x as i32, m.y as i32, m.width as i32, m.height as i32), m.primary)).collect();
    }
    let screen = &x.conn.setup().roots[0];
    vec![((0, 0, screen.width_in_pixels as i32, screen.height_in_pixels as i32), true)]
}

/// A picture of part of the screen, as it is shown: windows covering it are in it too.
pub fn capture(rect: Bounds) -> Result<Frame, String> {
    let x = x()?;
    let screen = &x.conn.setup().roots[0];
    let (sw, sh) = (screen.width_in_pixels as i32, screen.height_in_pixels as i32);
    let (left, top) = (rect.0.clamp(0, sw - 1), rect.1.clamp(0, sh - 1));
    let (w, h) = ((rect.0 + rect.2).min(sw) - left, (rect.1 + rect.3).min(sh) - top);
    if w < 1 || h < 1 {
        return Err("that part is off the screen".into());
    }
    let image = x
        .conn
        .get_image(ImageFormat::Z_PIXMAP, x.root, left as i16, top as i16, w as u16, h as u16, !0)
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| format!("X would not copy the screen: {e}"))?;
    if image.depth < 24 || image.data.len() < (w * h * 4) as usize {
        return Err(format!("the screen's colour depth ({}) is not supported", image.depth));
    }
    // 24 and 32 bit visuals keep pixels as blue, green, red, padding: the order Frame uses.
    let mut pixels = image.data;
    pixels.truncate((w * h * 4) as usize);
    for pixel in pixels.chunks_exact_mut(4) {
        pixel[3] = 255;
    }
    Ok(Frame { w: w as u32, h: h as u32, pixels })
}

/// A window's picture, from the screen where it is shown.
pub fn window_picture(id: u32) -> Result<(Frame, f32), String> {
    let window = find(id).ok_or("that window closed: run deskcrew windows")?;
    Ok((capture(window.rect)?, 1.0))
}

// ---- XTest input: the pointer and keyboard of the X display. ----

#[derive(Clone, Copy, PartialEq)]
pub enum Button {
    Left,
    Middle,
    Right,
    Back,
    Forward,
}

impl Button {
    fn number(self) -> u8 {
        match self {
            Button::Left => 1,
            Button::Middle => 2,
            Button::Right => 3,
            Button::Back => 8,
            Button::Forward => 9,
        }
    }
}

fn fake(x: &X, kind: u8, detail: u8, at: Option<(i32, i32)>) -> Result<(), String> {
    let (px, py) = at.unwrap_or((0, 0));
    x.conn.xtest_fake_input(kind, detail, x11rb::CURRENT_TIME, if at.is_some() { x.root } else { x11rb::NONE }, px as i16, py as i16, 0).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn move_to(px: i32, py: i32) -> Result<(), String> {
    let x = x()?;
    fake(x, xproto::MOTION_NOTIFY_EVENT, 0, Some((px, py)))?;
    x.conn.flush().map_err(|e| e.to_string())
}

pub fn pointer() -> Option<(i32, i32)> {
    let x = x().ok()?;
    let reply = x.conn.query_pointer(x.root).ok()?.reply().ok()?;
    Some((reply.root_x as i32, reply.root_y as i32))
}

pub fn button(which: Button, down: bool) -> Result<(), String> {
    let x = x()?;
    fake(x, if down { xproto::BUTTON_PRESS_EVENT } else { xproto::BUTTON_RELEASE_EVENT }, which.number(), None)?;
    x.conn.flush().map_err(|e| e.to_string())
}

pub fn click(px: i32, py: i32, which: Button, count: u32, hold: Duration) -> Result<(), String> {
    move_to(px, py)?;
    thread::sleep(MOVE_SETTLE);
    for _ in 0..count.max(1) {
        button(which, true)?;
        thread::sleep(hold.max(KEY_GAP));
        button(which, false)?;
        thread::sleep(KEY_GAP);
    }
    Ok(())
}

/// Wheel notches: positive is up, or right when sideways.
pub fn wheel(px: i32, py: i32, notches: i32, sideways: bool) -> Result<(), String> {
    move_to(px, py)?;
    thread::sleep(MOVE_SETTLE);
    let number = match (sideways, notches > 0) {
        (false, true) => 4,
        (false, false) => 5,
        (true, true) => 7,
        (true, false) => 6,
    };
    let x = x()?;
    for _ in 0..notches.unsigned_abs() {
        fake(x, xproto::BUTTON_PRESS_EVENT, number, None)?;
        fake(x, xproto::BUTTON_RELEASE_EVENT, number, None)?;
    }
    x.conn.flush().map_err(|e| e.to_string())
}

pub fn stroke(path: &[(f32, f32)], stop: &dyn Fn() -> bool, moved: &mut dyn FnMut(usize, i32, i32)) -> Result<bool, String> {
    let Some(first) = path.first() else { return Ok(true) };
    move_to(first.0 as i32, first.1 as i32)?;
    thread::sleep(MOVE_SETTLE);
    button(Button::Left, true)?;
    let mut finished = true;
    for (at, point) in path.iter().enumerate().skip(1) {
        if stop() {
            finished = false;
            break;
        }
        move_to(point.0 as i32, point.1 as i32)?;
        moved(at, point.0 as i32, point.1 as i32);
        thread::sleep(Duration::from_millis(4));
    }
    button(Button::Left, false)?;
    Ok(finished)
}

/// A key and the modifiers held with it, as X keysyms.
pub struct Chord {
    pub modifiers: Vec<u32>,
    pub key: Option<u32>,
}

fn named(name: &str) -> Option<u32> {
    Some(match name {
        "enter" | "return" => 0xff0d,
        "tab" => 0xff09,
        "esc" | "escape" => 0xff1b,
        "backspace" | "back" => 0xff08,
        "delete" | "del" => 0xffff,
        "insert" | "ins" => 0xff63,
        "home" => 0xff50,
        "end" => 0xff57,
        "left" => 0xff51,
        "up" => 0xff52,
        "right" => 0xff53,
        "down" => 0xff54,
        "pageup" | "pgup" => 0xff55,
        "pagedown" | "pgdn" => 0xff56,
        "space" => 0x20,
        "menu" | "apps" => 0xff67,
        "printscreen" | "print" => 0xff61,
        "capslock" => 0xffe5,
        _ => {
            if let Some(n) = name.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()).filter(|n| (1..=24).contains(n)) {
                return Some(0xffbe + n - 1);
            }
            return None;
        }
    })
}

fn modifier(name: &str) -> Option<u32> {
    Some(match name {
        "shift" => 0xffe1,
        "ctrl" | "control" => 0xffe3,
        "alt" | "option" => 0xffe9,
        "win" | "super" | "cmd" | "meta" => 0xffeb,
        _ => return None,
    })
}

/// The keysym that types this character.
fn keysym_of(c: char) -> u32 {
    let code = c as u32;
    match code {
        0x0a | 0x0d => 0xff0d,
        0x09 => 0xff09,
        0x20..=0x7e | 0xa0..=0xff => code,
        _ => 0x0100_0000 + code,
    }
}

pub fn chord(spec: &str) -> Result<Chord, String> {
    let mut modifiers = Vec::new();
    let mut key = None;
    for part in spec.to_lowercase().split('+').map(str::trim).filter(|p| !p.is_empty()) {
        if let Some(m) = modifier(part) {
            modifiers.push(m);
        } else if key.is_none() {
            key = Some(named(part).or_else(|| {
                let mut chars = part.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => Some(keysym_of(c)),
                    _ => None,
                }
            }).ok_or(format!("unknown key '{part}'"))?);
        } else {
            return Err(format!("'{spec}' has two keys: press them one at a time"));
        }
    }
    if key.is_none() && modifiers.is_empty() {
        return Err("which key?".into());
    }
    Ok(Chord { modifiers, key })
}

/// Shortcuts that close windows or log out.
pub fn is_risky(chord: &Chord) -> bool {
    let has = |m: u32| chord.modifiers.contains(&m);
    let key = chord.key.unwrap_or(0);
    (has(0xffe9) && key == 0xffc1) // alt+f4
        || (has(0xffe3) && has(0xffe9) && matches!(key, 0xffff | 0xff08)) // ctrl+alt+delete or backspace
        || (has(0xffe3) && matches!(key, 0x71 | 0x77)) // ctrl+q, ctrl+w
}

struct Keymap {
    min: u8,
    per: usize,
    syms: Vec<u32>,
}

fn keymap(x: &X) -> Result<Keymap, String> {
    let setup = x.conn.setup();
    let (min, max) = (setup.min_keycode, setup.max_keycode);
    let reply = x.conn.get_keyboard_mapping(min, max - min + 1).map_err(|e| e.to_string())?.reply().map_err(|e| e.to_string())?;
    Ok(Keymap { min, per: reply.keysyms_per_keycode as usize, syms: reply.keysyms })
}

impl Keymap {
    /// The keycode for a keysym, and whether it needs shift.
    fn find(&self, sym: u32) -> Option<(u8, bool)> {
        for (at, row) in self.syms.chunks(self.per.max(1)).enumerate() {
            for (level, s) in row.iter().take(2).enumerate() {
                if *s == sym {
                    return Some((self.min + at as u8, level == 1));
                }
            }
        }
        None
    }

    /// A keycode with nothing on it, to borrow for a character the keyboard lacks.
    fn spare(&self) -> Option<u8> {
        self.syms.chunks(self.per.max(1)).enumerate().rev().find(|(_, row)| row.iter().all(|s| *s == 0)).map(|(at, _)| self.min + at as u8)
    }
}

fn key_event(x: &X, code: u8, down: bool) -> Result<(), String> {
    fake(x, if down { xproto::KEY_PRESS_EVENT } else { xproto::KEY_RELEASE_EVENT }, code, None)
}

/// Press a keysym, borrowing a spare keycode if no key types it.
fn tap(x: &X, map: &Keymap, sym: u32, held: &[u8]) -> Result<(), String> {
    let shift = map.find(0xffe1).map(|k| k.0);
    let (code, needs_shift, borrowed) = match map.find(sym) {
        Some((code, shifted)) => (code, shifted, false),
        None => {
            let code = map.spare().ok_or("no free key to type that character with")?;
            let row = vec![sym; map.per.max(1)];
            x.conn.change_keyboard_mapping(1, code, map.per as u8, &row).map_err(|e| e.to_string())?;
            x.conn.flush().map_err(|e| e.to_string())?;
            thread::sleep(KEY_GAP);
            (code, false, true)
        }
    };
    let shift_now = needs_shift && !held.contains(&shift.unwrap_or(0));
    if shift_now && let Some(s) = shift {
        key_event(x, s, true)?;
    }
    key_event(x, code, true)?;
    key_event(x, code, false)?;
    if shift_now && let Some(s) = shift {
        key_event(x, s, false)?;
    }
    x.conn.flush().map_err(|e| e.to_string())?;
    thread::sleep(KEY_GAP);
    if borrowed {
        let row = vec![0u32; map.per.max(1)];
        x.conn.change_keyboard_mapping(1, code, map.per as u8, &row).map_err(|e| e.to_string())?;
        x.conn.flush().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Press a chord in the focused window, holding the key for `hold`.
pub fn press(chord: &Chord, hold: Duration) -> Result<(), String> {
    let x = x()?;
    let map = keymap(x)?;
    let mods: Vec<u8> = chord.modifiers.iter().filter_map(|m| map.find(*m).map(|k| k.0)).collect();
    for m in &mods {
        key_event(x, *m, true)?;
    }
    let result = match chord.key {
        Some(sym) if hold.is_zero() => tap(x, &map, sym, &mods),
        Some(sym) => match map.find(sym) {
            Some((code, _)) => {
                key_event(x, code, true)?;
                x.conn.flush().map_err(|e| e.to_string())?;
                thread::sleep(hold);
                key_event(x, code, false)
            }
            None => tap(x, &map, sym, &mods),
        },
        None => {
            thread::sleep(hold);
            Ok(())
        }
    };
    for m in mods.iter().rev() {
        key_event(x, *m, false)?;
    }
    x.conn.flush().map_err(|e| e.to_string())?;
    result
}

/// Hold or let go of modifier keys.
pub fn modifiers(names: &[u32], down: bool) -> Result<(), String> {
    let x = x()?;
    let map = keymap(x)?;
    for m in names {
        if let Some((code, _)) = map.find(*m) {
            key_event(x, code, down)?;
        }
    }
    x.conn.flush().map_err(|e| e.to_string())
}

/// Type text into the focused window. Returns how many characters went before a stop.
pub fn type_text(text: &str, stop: &dyn Fn() -> bool) -> Result<usize, String> {
    let x = x()?;
    let map = keymap(x)?;
    let mut typed = 0;
    for c in text.chars() {
        if stop() {
            break;
        }
        tap(x, &map, keysym_of(c), &[])?;
        typed += 1;
    }
    Ok(typed)
}

/// Whether Ctrl+Alt+Esc is down now, read from the key state without grabbing anything.
pub fn stop_keys_down() -> bool {
    let Ok(x) = x() else { return false };
    let Ok(map) = keymap(x) else { return false };
    let Some(keys) = x.conn.query_keymap().ok().and_then(|c| c.reply().ok()) else { return false };
    let down = |sym: u32| map.find(sym).is_some_and(|(code, _)| keys.keys[code as usize / 8] & (1 << (code % 8)) != 0);
    (down(0xffe3) || down(0xffe4)) && (down(0xffe9) || down(0xffea)) && down(0xff1b)
}

// ---- Apps and the clipboard. ----

/// Start an app and wait for its first window: a new window of its process, or any new window,
/// since single-instance apps hand the work to a process already running.
pub fn launch(app: &str, args: &[String]) -> Result<Option<Window>, String> {
    use std::os::unix::process::CommandExt;
    let before: Vec<u32> = list().iter().map(|w| w.id).collect();
    let child = Command::new(app)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("could not start {app}: {e}"))?;
    let pid = child.id();
    let started = Instant::now();
    while started.elapsed() < LAUNCH_WAIT {
        thread::sleep(LAUNCH_STEP);
        let now = list();
        if let Some(own) = now.iter().find(|w| w.pid == pid) {
            return Ok(Some(own.clone()));
        }
        if let Some(new) = now.into_iter().find(|w| !before.contains(&w.id)) {
            return Ok(Some(new));
        }
    }
    Ok(None)
}

/// The clipboard through xclip or xsel: X keeps a selection only while its owner runs.
pub fn clipboard() -> Result<Option<String>, String> {
    for (program, args) in [("xclip", &["-selection", "clipboard", "-o"][..]), ("xsel", &["--clipboard", "--output"][..])] {
        if let Ok(out) = Command::new(program).args(args).stderr(Stdio::null()).output() {
            return Ok(out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned()));
        }
    }
    Err("the clipboard needs xclip or xsel: install one (sudo apt install xclip)".into())
}

pub fn set_clipboard(text: Option<&str>) -> Result<(), String> {
    let text = text.unwrap_or("");
    for (program, args) in [("xclip", &["-selection", "clipboard", "-i"][..]), ("xsel", &["--clipboard", "--input"][..])] {
        let Ok(mut child) = Command::new(program).args(args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() else { continue };
        if let Some(mut input) = child.stdin.take() {
            input.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
        }
        // xclip stays behind to own the selection; xsel does too. Neither needs waiting for.
        thread::spawn(move || child.wait());
        return Ok(());
    }
    Err("the clipboard needs xclip or xsel: install one (sudo apt install xclip)".into())
}
