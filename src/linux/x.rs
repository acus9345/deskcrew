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
        composite::ConnectionExt as _,
        randr::ConnectionExt as _,
        xproto::{self, Atom, AtomEnum, ClientMessageEvent, ConfigureWindowAux, ConnectionExt as _, EventMask, ImageFormat, KeyButMask, StackMode, Window as XWindow},
        xtest::ConnectionExt as _,
    },
    rust_connection::RustConnection,
};

pub type Bounds = (i32, i32, i32, i32);

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
    screen: usize,
    root: XWindow,
    atoms: Mutex<HashMap<&'static str, Atom>>,
    started: Instant,
    last_time: Mutex<u32>,
    composite_ok: OnceLock<bool>,
}

static X: OnceLock<Result<X, String>> = OnceLock::new();

pub fn has_display() -> bool {
    std::env::var_os("DISPLAY").is_some_and(|d| !d.is_empty())
}

pub fn has_wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some_and(|d| !d.is_empty())
}

/// Wayland without X11 still drives accessible controls through AT-SPI; only a session with neither
/// DISPLAY nor WAYLAND_DISPLAY has nothing to talk to.
pub fn display_problem() -> Option<String> {
    if has_display() || has_wayland() {
        None
    } else {
        Some("no X display: set DISPLAY, or start one with Xvfb (for example: Xvfb :99 & export DISPLAY=:99)".into())
    }
}

fn x() -> Result<&'static X, String> {
    X.get_or_init(|| {
        if !has_display() {
            if has_wayland() {
                return Err("that needs X11, and this Wayland session has no DISPLAY (screenshots, global keys/pointer and window placement need X11 or desktop portals)".into());
            }
            return Err("no X display: set DISPLAY, or start one with Xvfb (for example: Xvfb :99 & export DISPLAY=:99)".into());
        }
        let (conn, screen) = x11rb::connect(None).map_err(|e| format!("could not open the X display: {e}"))?;
        let root = conn.setup().roots[screen].root;
        Ok(X {
            conn,
            screen,
            root,
            atoms: Mutex::new(HashMap::new()),
            started: Instant::now(),
            last_time: Mutex::new(1),
            composite_ok: OnceLock::new(),
        })
    })
    .as_ref()
    .map_err(Clone::clone)
}

/// True when WAYLAND_DISPLAY is set and DISPLAY points at XWayland rather than a standalone X
/// server such as Xvfb.
pub fn is_xwayland() -> bool {
    if !has_wayland() || !has_display() {
        return false;
    }
    let Ok(x) = x() else { return false };
    static XWAYLAND: OnceLock<bool> = OnceLock::new();
    *XWAYLAND.get_or_init(|| detect_xwayland(x))
}

fn detect_xwayland(x: &X) -> bool {
    if let Some(monitors) = x.conn.randr_get_monitors(x.root, true).ok().and_then(|c| c.reply().ok()) {
        for monitor in monitors.monitors {
            if let Some(name) = x.conn.get_atom_name(monitor.name).ok().and_then(|c| c.reply().ok())
                && name.name.starts_with(b"XWAYLAND")
            {
                return true;
            }
        }
    }
    if let Some(res) = x.conn.randr_get_screen_resources_current(x.root).ok().and_then(|c| c.reply().ok()) {
        for output in res.outputs {
            if let Some(info) = x.conn.randr_get_output_info(output, res.config_timestamp).ok().and_then(|c| c.reply().ok())
                && info.name.starts_with(b"XWAYLAND")
            {
                return true;
            }
        }
    }
    false
}

/// The accessibility bus the desktop published on the root window, as at-spi does.
pub fn accessibility_bus() -> Option<String> {
    if !has_display() {
        return None;
    }
    let x = x().ok()?;
    let bytes = x.property(x.root, "AT_SPI_BUS", AtomEnum::STRING.into())?;
    Some(String::from_utf8_lossy(&bytes).trim_end_matches('\0').to_string()).filter(|a| !a.is_empty())
}

pub fn ready() -> Result<(), String> {
    x().map(|_| ())
}

impl X {
    fn timestamp(&self) -> u32 {
        let mut last = self.last_time.lock().unwrap();
        let next = next_timestamp(*last, self.started.elapsed().as_millis() as u64);
        *last = next;
        next
    }

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
/// Give a window the keyboard without raising it or making it the active window: on X the keyboard
/// focus and the stacking are separate, so the window in front stays in front. Returns the window
/// that had the keyboard before.
pub fn focus_keys(id: u32) -> Result<u32, String> {
    let x = x()?;
    let before = x.conn.get_input_focus().map_err(|e| e.to_string())?.reply().map_err(|e| e.to_string())?.focus;
    x.conn.set_input_focus(xproto::InputFocus::PARENT, id, x11rb::CURRENT_TIME).map_err(|e| e.to_string())?;
    x.conn.flush().map_err(|e| e.to_string())?;
    Ok(before)
}

/// Hand the keyboard back to the window that had it.
pub fn restore_keys(window: u32) {
    if let Ok(x) = x()
        && window > 1
    {
        let _ = x.conn.set_input_focus(xproto::InputFocus::PARENT, window, x11rb::CURRENT_TIME);
        let _ = x.conn.flush();
    }
}

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

/// Walk up from `id` to the WM frame sitting directly under the root window.
fn wm_frame(x: &X, id: u32) -> u32 {
    let mut at = id;
    for _ in 0..8 {
        let Some(tree) = x.conn.query_tree(at).ok().and_then(|c| c.reply().ok()) else { return at };
        if tree.parent == x.root || tree.parent == x11rb::NONE {
            return at;
        }
        at = tree.parent;
    }
    at
}

/// Keep `id` stacked behind `front_id` without activating or stealing focus.
pub fn lower_behind(id: u32, front_id: u32) {
    let Ok(x) = x() else { return };
    if id == front_id {
        return;
    }
    let target = wm_frame(x, id);
    let sibling = wm_frame(x, front_id);
    if target == sibling {
        return;
    }
    let aux = ConfigureWindowAux::new().sibling(sibling).stack_mode(StackMode::BELOW);
    let _ = x.conn.configure_window(target, &aux);
    let _ = x.conn.flush();
}

pub fn place(id: u32, how: &str) -> Result<(), String> {
    let x = x()?;
    let state = |add: bool, a: &'static str, b: &'static str| x.message(id, "_NET_WM_STATE", [add as u32, x.atom(a), x.atom(b), 2, 0]);
    let unmax = || {
        if x.has_wm() {
            let _ = state(false, "_NET_WM_STATE_MAXIMIZED_VERT", "_NET_WM_STATE_MAXIMIZED_HORZ");
        }
    };
    let how = how.trim();
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
        "left" | "right" => {
            let (sx, sy, sw, sh) = monitor_of(x, id);
            unmax();
            let half = sw / 2;
            let (bx, bw) = if how == "left" { (sx, half) } else { (sx + half, sw - half) };
            configure(x, id, (bx, sy, bw, sh))
        }
        other if other == "monitor" || other.starts_with("monitor ") => {
            let index: usize = other["monitor".len()..].trim().parse().map_err(|_| "place monitor N needs a monitor number from deskcrew screens".to_string())?;
            let list = screens();
            let (sx, sy, sw, sh) = list.get(index).ok_or_else(|| format!("no monitor {index}: run deskcrew screens"))?.0;
            unmax();
            let (bw, bh) = x.rect(id).map_or((sw, sh), |r| (r.2.min(sw).max(200.min(sw.max(1))), r.3.min(sh).max(200.min(sh.max(1)))));
            configure(x, id, (sx + (sw - bw) / 2, sy + (sh - bh) / 2, bw, bh))
        }
        box_text => {
            let Some((bx, by, bw, bh)) = crate::lines::parse_rect(box_text) else {
                return Err("place x,y,w,h|min|max|restore|left|right|monitor N".into());
            };
            unmax();
            configure(x, id, (bx, by, bw, bh))
        }
    }
}

/// The monitor currently containing the center of `id`, or the primary monitor.
fn monitor_of(x: &X, id: u32) -> Bounds {
    let list = screens();
    if let Some((wx, wy, ww, wh)) = x.rect(id) {
        let (cx, cy) = (wx.saturating_add(ww / 2), wy.saturating_add(wh / 2));
        if let Some((bounds, _)) = list.iter().find(|((sx, sy, sw, sh), _)| cx >= *sx && cy >= *sy && cx < sx.saturating_add(*sw) && cy < sy.saturating_add(*sh)) {
            return *bounds;
        }
    }
    list.into_iter().find(|s| s.1).map_or((0, 0, 1024, 768), |s| s.0)
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
        let mut out: Vec<(Bounds, bool)> = monitors.monitors.iter().map(|m| ((m.x as i32, m.y as i32, m.width as i32, m.height as i32), m.primary)).collect();
        // Xvfb and some setups name no primary monitor: the first one is it then.
        if !out.iter().any(|m| m.1) {
            out[0].1 = true;
        }
        return out;
    }
    let screen = &x.conn.setup().roots[0];
    vec![((0, 0, screen.width_in_pixels as i32, screen.height_in_pixels as i32), true)]
}

static CAPTURE: Mutex<()> = Mutex::new(());

/// A picture of part of the screen, as it is shown: windows covering it are in it too.
pub fn capture(rect: Bounds) -> Result<Frame, String> {
    let x = x()?;
    let screen = &x.conn.setup().roots[0];
    let (sw, sh) = (screen.width_in_pixels as i32, screen.height_in_pixels as i32);
    if sw < 1 || sh < 1 {
        return Err("the screen has no size".into());
    }
    let (left, top) = (rect.0.max(0), rect.1.max(0));
    let right = (rect.0 as i64 + rect.2 as i64).min(sw as i64);
    let bottom = (rect.1 as i64 + rect.3 as i64).min(sh as i64);
    let (w, h) = ((right - left as i64).min(i32::MAX as i64) as i32, (bottom - top as i64).min(i32::MAX as i64) as i32);
    if w < 1 || h < 1 {
        return Err("that part is off the screen".into());
    }
    let byte_len = (w as usize).checked_mul(h as usize).and_then(|n| n.checked_mul(4)).ok_or("that region is too big")?;
    let _capture = CAPTURE.lock().unwrap();
    // The agents' drawn cursors stay out of the picture.
    let overlay = super::overlay::Overlay::running();
    if let Some(overlay) = &overlay {
        overlay.aside(true);
    }
    let image = x.conn.get_image(ImageFormat::Z_PIXMAP, x.root, left as i16, top as i16, w as u16, h as u16, !0).map_err(|e| e.to_string()).and_then(|c| c.reply().map_err(|e| format!("X would not copy the screen: {e}")));
    if let Some(overlay) = &overlay {
        overlay.aside(false);
    }
    let image = image?;
    if image.depth < 24 || image.data.len() < byte_len {
        return Err(format!("the screen's colour depth ({}) is not supported", image.depth));
    }
    // 24 and 32 bit visuals keep pixels as blue, green, red, padding: the order Frame uses.
    let mut pixels = image.data;
    pixels.truncate(byte_len);
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel[3] = 255;
    }
    Ok(Frame { w: w as u32, h: h as u32, pixels })
}

/// Capture a window's redirected backing pixmap via `XCompositeNameWindowPixmap` when an X11
/// compositor (`_NET_WM_CM_S0`) is active, so covered or partially off-screen background windows
/// can be read without overlapping windows or cursor overlays.
fn capture_window_pixmap(x: &X, id: u32) -> Option<Frame> {
    let cm_name = format!("_NET_WM_CM_S{}", x.screen);
    let cm = x.conn.intern_atom(false, cm_name.as_bytes()).ok()?.reply().ok()?.atom;
    let owner = x.conn.get_selection_owner(cm).ok()?.reply().ok()?.owner;
    if owner == x11rb::NONE {
        return None;
    }
    let has_composite = *x.composite_ok.get_or_init(|| {
        x.conn
            .composite_query_version(0, 4)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some_and(|v| v.major_version > 0 || v.minor_version >= 2)
    });
    if !has_composite {
        return None;
    }
    let geom = x.conn.get_geometry(id).ok()?.reply().ok()?;
    let (w, h) = (geom.width as i32, geom.height as i32);
    if w < 1 || h < 1 {
        return None;
    }
    let byte_len = (w as usize).checked_mul(h as usize)?.checked_mul(4)?;
    let pixmap = x.conn.generate_id().ok()?;
    if x.conn.composite_name_window_pixmap(id, pixmap).ok()?.check().is_err() {
        return None;
    }
    let image = x.conn.get_image(ImageFormat::Z_PIXMAP, pixmap, 0, 0, geom.width, geom.height, !0).ok().and_then(|c| c.reply().ok());
    let _ = x.conn.free_pixmap(pixmap);
    let _ = x.conn.flush();
    let image = image?;
    if image.depth < 24 || image.data.len() < byte_len {
        return None;
    }
    let mut pixels = image.data;
    pixels.truncate(byte_len);
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel[3] = 255;
    }
    let frame = Frame { w: w as u32, h: h as u32, pixels };
    (!frame.is_blank()).then_some(frame)
}

/// A window's picture: prefers its composited backing pixmap (so covered windows stay readable),
/// falling back to the screen region where it is shown.
pub fn window_picture(id: u32) -> Result<(Frame, f32), String> {
    let window = find(id).ok_or("that window closed: run deskcrew windows")?;
    if let Ok(x) = x()
        && let Some(frame) = capture_window_pixmap(x, id)
    {
        return Ok((frame, 1.0));
    }
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
    pub fn named(word: &str) -> Option<Button> {
        match word.to_lowercase().as_str() {
            "left" => Some(Button::Left),
            "right" => Some(Button::Right),
            "middle" => Some(Button::Middle),
            "back" => Some(Button::Back),
            "forward" => Some(Button::Forward),
            _ => None,
        }
    }

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

pub fn click(by: By, px: i32, py: i32, which: Button, count: u32, hold: Duration) -> Result<(), String> {
    move_by(by, px, py)?;
    thread::sleep(MOVE_SETTLE);
    for _ in 0..count.max(1) {
        button_by(by, which.number(), true)?;
        thread::sleep(hold.max(KEY_GAP));
        button_by(by, which.number(), false)?;
        thread::sleep(KEY_GAP);
    }
    Ok(())
}

/// Wheel notches: positive is up, or right when sideways.
pub fn wheel(by: By, px: i32, py: i32, notches: i32, sideways: bool) -> Result<(), String> {
    move_by(by, px, py)?;
    thread::sleep(MOVE_SETTLE);
    let number = match (sideways, notches > 0) {
        (false, true) => 4,
        (false, false) => 5,
        (true, true) => 7,
        (true, false) => 6,
    };
    for _ in 0..notches.unsigned_abs() {
        button_by(by, number, true)?;
        button_by(by, number, false)?;
    }
    Ok(())
}

pub fn stroke(by: By, path: &[(f32, f32)], button: Button, stop: &dyn Fn() -> bool, moved: &mut dyn FnMut(usize, i32, i32)) -> Result<bool, String> {
    let Some(first) = path.first() else { return Ok(true) };
    move_by(by, first.0 as i32, first.1 as i32)?;
    thread::sleep(MOVE_SETTLE);
    button_by(by, button.number(), true)?;
    let mut finished = true;
    // The handwriting rhythm the Windows and Mac strokes keep too.
    let due = crate::motion::schedule(path, crate::motion::PIXELS_PER_SECOND);
    let started = std::time::Instant::now();
    let mut step_err = None;
    for (at, point) in path.iter().enumerate().skip(1) {
        if stop() {
            finished = false;
            break;
        }
        let secs = due[at];
        if let Ok(wait_for) = Duration::try_from_secs_f32(secs)
            && let Some(wait) = wait_for.checked_sub(started.elapsed())
        {
            thread::sleep(wait);
        }
        if let Err(err) = move_by(by, point.0 as i32, point.1 as i32) {
            step_err = Some(err);
            break;
        }
        moved(at, point.0 as i32, point.1 as i32);
    }
    let up_res = button_by(by, button.number(), false);
    if let Some(err) = step_err {
        return Err(err);
    }
    up_res?;
    Ok(finished)
}

// ---- Background XSendEvent input: zero WM raise, zero focus steal, zero drawing flicker. ----

fn mod_mask(modifiers: &[u32]) -> KeyButMask {
    let mut mask = KeyButMask::default();
    for &sym in modifiers {
        match sym {
            0xffe1 | 0xffe2 => mask |= KeyButMask::SHIFT,
            0xffe3 | 0xffe4 => mask |= KeyButMask::CONTROL,
            0xffe9 | 0xffea => mask |= KeyButMask::MOD1,
            0xffeb | 0xffec => mask |= KeyButMask::MOD4,
            _ => {}
        }
    }
    mask
}

fn button_mask(button: Button) -> KeyButMask {
    match button {
        Button::Left => KeyButMask::BUTTON1,
        Button::Middle => KeyButMask::BUTTON2,
        Button::Right => KeyButMask::BUTTON3,
        Button::Back | Button::Forward => KeyButMask::default(),
    }
}

/// Monotonic non-zero X11 event timestamp (milliseconds). Unlike real server events, the X server
/// does NOT rewrite `CurrentTime` (0) on `XSendEvent`, causing GTK, Qt, Chromium, Firefox and Java
/// to drop events or miscalculate double-click/drag timing.
fn next_timestamp(prev: u32, elapsed_ms: u64) -> u32 {
    let elapsed = (elapsed_ms as u32).max(1);
    let candidate = if elapsed > prev { elapsed } else { prev.wrapping_add(1) };
    if candidate == 0 { 1 } else { candidate }
}

/// Find the deepest child window inside `window` under root coordinates `(px, py)` and the
/// local coordinates inside that child.
fn child_at(x: &X, window: XWindow, px: i32, py: i32) -> (XWindow, i16, i16) {
    let Ok(initial) = x.conn.translate_coordinates(x.root, window, px as i16, py as i16).map(|c| c.reply()) else {
        return (window, 0, 0);
    };
    let Ok(mut cur_reply) = initial else { return (window, 0, 0) };
    let mut cur = window;
    let mut lx = cur_reply.dst_x;
    let mut ly = cur_reply.dst_y;
    for _ in 0..16 {
        let next_child = cur_reply.child;
        if next_child == x11rb::NONE {
            break;
        }
        let Ok(step) = x.conn.translate_coordinates(cur, next_child, lx, ly).map(|c| c.reply()) else {
            break;
        };
        let Ok(reply) = step else { break };
        cur = next_child;
        lx = reply.dst_x;
        ly = reply.dst_y;
        cur_reply = reply;
    }
    (cur, lx, ly)
}

/// Send an EnterNotify to `target` with `focus: false` and `EventMask::NO_EVENT` so toolkits that
/// track pointer presence (Qt, GTK, Tk, FLTK, Java/Swing, Electron) accept subsequent clicks and
/// motions without moving the real pointer or notifying the WM frame.
fn send_enter(x: &X, target: XWindow, px: i32, py: i32, lx: i16, ly: i16, state: KeyButMask) -> Result<(), String> {
    let event = xproto::EnterNotifyEvent {
        response_type: xproto::ENTER_NOTIFY_EVENT,
        detail: xproto::NotifyDetail::NONLINEAR,
        sequence: 0,
        time: x.timestamp(),
        root: x.root,
        event: target,
        child: x11rb::NONE,
        root_x: px as i16,
        root_y: py as i16,
        event_x: lx,
        event_y: ly,
        state,
        mode: xproto::NotifyMode::NORMAL,
        same_screen_focus: 1, // same_screen = true (bit 0), focus = false (bit 1)
    };
    x.conn.send_event(false, target, EventMask::NO_EVENT, event).map_err(|e| e.to_string())?;
    Ok(())
}

/// Send a synthetic FocusIn/FocusOut directly to `target` without calling `XSetInputFocus`.
/// OS/WM focus never moves, while toolkits that require internal focus to process KeyPress
/// (Qt, GTK, Firefox, Electron, LibreOffice, Java, Tk) accept the synthetic keys.
fn send_focus(x: &X, target: XWindow, focus_in: bool) -> Result<(), String> {
    if focus_in {
        let event = xproto::FocusInEvent {
            response_type: xproto::FOCUS_IN_EVENT,
            detail: xproto::NotifyDetail::NONLINEAR,
            sequence: 0,
            event: target,
            mode: xproto::NotifyMode::NORMAL,
        };
        x.conn.send_event(false, target, EventMask::NO_EVENT, event).map_err(|e| e.to_string())?;
    } else {
        let event = xproto::FocusOutEvent {
            response_type: xproto::FOCUS_OUT_EVENT,
            detail: xproto::NotifyDetail::NONLINEAR,
            sequence: 0,
            event: target,
            mode: xproto::NotifyMode::NORMAL,
        };
        x.conn.send_event(false, target, EventMask::NO_EVENT, event).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn send_motion(x: &X, target: XWindow, px: i32, py: i32, lx: i16, ly: i16, state: KeyButMask) -> Result<(), String> {
    let event = xproto::MotionNotifyEvent {
        response_type: xproto::MOTION_NOTIFY_EVENT,
        detail: xproto::Motion::NORMAL,
        sequence: 0,
        time: x.timestamp(),
        root: x.root,
        event: target,
        child: x11rb::NONE,
        root_x: px as i16,
        root_y: py as i16,
        event_x: lx,
        event_y: ly,
        state,
        same_screen: true,
    };
    // `propagate: false` + `EventMask::NO_EVENT` delivers unconditionally to the client that
    // created `target` (even when it selects XInput 2 instead of core masks) and never bubbles up
    // to the WM frame, preventing click-to-focus raises.
    x.conn.send_event(false, target, EventMask::NO_EVENT, event).map_err(|e| e.to_string())?;
    Ok(())
}

fn send_button(x: &X, target: XWindow, button_num: u8, down: bool, root_at: (i32, i32), local_at: (i16, i16), state: KeyButMask) -> Result<(), String> {
    let ((px, py), (lx, ly)) = (root_at, local_at);
    if down {
        let event = xproto::ButtonPressEvent {
            response_type: xproto::BUTTON_PRESS_EVENT,
            detail: button_num,
            sequence: 0,
            time: x.timestamp(),
            root: x.root,
            event: target,
            child: x11rb::NONE,
            root_x: px as i16,
            root_y: py as i16,
            event_x: lx,
            event_y: ly,
            state,
            same_screen: true,
        };
        x.conn.send_event(false, target, EventMask::NO_EVENT, event).map_err(|e| e.to_string())?;
    } else {
        let event = xproto::ButtonReleaseEvent {
            response_type: xproto::BUTTON_RELEASE_EVENT,
            detail: button_num,
            sequence: 0,
            time: x.timestamp(),
            root: x.root,
            event: target,
            child: x11rb::NONE,
            root_x: px as i16,
            root_y: py as i16,
            event_x: lx,
            event_y: ly,
            state,
            same_screen: true,
        };
        x.conn.send_event(false, target, EventMask::NO_EVENT, event).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Click a point inside `window` via XSendEvent without raising it, moving the cursor, or
/// stealing focus.
pub fn send_click(window: u32, px: i32, py: i32, which: Button, count: u32, hold: Duration, modifiers: &[u32]) -> Result<(), String> {
    let x = x()?;
    let (target, lx, ly) = child_at(x, window, px, py);
    let mods = mod_mask(modifiers);
    let pressed = mods | button_mask(which);
    send_enter(x, target, px, py, lx, ly, mods)?;
    send_motion(x, target, px, py, lx, ly, mods)?;
    x.conn.flush().map_err(|e| e.to_string())?;
    for _ in 0..count.max(1) {
        send_button(x, target, which.number(), true, (px, py), (lx, ly), mods)?;
        x.conn.flush().map_err(|e| e.to_string())?;
        thread::sleep(hold.max(KEY_GAP));
        send_button(x, target, which.number(), false, (px, py), (lx, ly), pressed)?;
        x.conn.flush().map_err(|e| e.to_string())?;
        thread::sleep(KEY_GAP);
    }
    Ok(())
}

/// Press or release a mouse button at `(px, py)` inside `window` via XSendEvent without raising
/// it or taking focus (used for `down left|right|middle` / `up`).
pub fn send_button_at(window: u32, px: i32, py: i32, which: Button, down: bool, modifiers: &[u32]) -> Result<(), String> {
    let x = x()?;
    let (target, lx, ly) = child_at(x, window, px, py);
    let mods = mod_mask(modifiers);
    let state = if down { mods } else { mods | button_mask(which) };
    if down {
        send_enter(x, target, px, py, lx, ly, mods)?;
        send_motion(x, target, px, py, lx, ly, mods)?;
    }
    send_button(x, target, which.number(), down, (px, py), (lx, ly), state)?;
    x.conn.flush().map_err(|e| e.to_string())
}

/// Hover a point inside `window` via XSendEvent without raising it or touching the cursor.
pub fn send_hover(window: u32, px: i32, py: i32) -> Result<(), String> {
    let x = x()?;
    let (target, lx, ly) = child_at(x, window, px, py);
    send_enter(x, target, px, py, lx, ly, KeyButMask::default())?;
    send_motion(x, target, px, py, lx, ly, KeyButMask::default())?;
    x.conn.flush().map_err(|e| e.to_string())
}

/// Scroll at a point inside `window` via XSendEvent without raising it or touching the cursor.
pub fn send_wheel(window: u32, px: i32, py: i32, notches: i32, sideways: bool) -> Result<(), String> {
    let x = x()?;
    let (target, lx, ly) = child_at(x, window, px, py);
    let number = match (sideways, notches > 0) {
        (false, true) => 4,
        (false, false) => 5,
        (true, true) => 7,
        (true, false) => 6,
    };
    send_enter(x, target, px, py, lx, ly, KeyButMask::default())?;
    send_motion(x, target, px, py, lx, ly, KeyButMask::default())?;
    for _ in 0..notches.unsigned_abs() {
        send_button(x, target, number, true, (px, py), (lx, ly), KeyButMask::default())?;
        send_button(x, target, number, false, (px, py), (lx, ly), KeyButMask::default())?;
    }
    x.conn.flush().map_err(|e| e.to_string())
}

/// Draw or drag a stroke inside `window` via XSendEvent: never raises the window, never steals
/// focus, and never triggers bring-to-front flicker.
pub fn send_stroke(window: u32, path: &[(f32, f32)], button: Button, modifiers: &[u32], stop: &dyn Fn() -> bool, moved: &mut dyn FnMut(usize, i32, i32)) -> Result<bool, String> {
    let Some(first) = path.first() else { return Ok(true) };
    let x = x()?;
    let (fx, fy) = (first.0 as i32, first.1 as i32);
    let (target, flx, fly) = child_at(x, window, fx, fy);
    let origin_dx = fx - flx as i32;
    let origin_dy = fy - fly as i32;
    let mods = mod_mask(modifiers);
    let held_state = mods | button_mask(button);
    send_enter(x, target, fx, fy, flx, fly, mods)?;
    send_motion(x, target, fx, fy, flx, fly, mods)?;
    send_button(x, target, button.number(), true, (fx, fy), (flx, fly), mods)?;
    if let Err(err) = x.conn.flush() {
        let (llx, lly) = ((fx - origin_dx) as i16, (fy - origin_dy) as i16);
        let _ = send_button(x, target, button.number(), false, (fx, fy), (llx, lly), held_state);
        let _ = x.conn.flush();
        return Err(err.to_string());
    }
    let mut finished = true;
    let (mut last_x, mut last_y) = (fx, fy);
    let mut step_err = None;
    for (at, point) in path.iter().enumerate().skip(1) {
        if stop() {
            finished = false;
            break;
        }
        let (px, py) = (point.0 as i32, point.1 as i32);
        let (lx, ly) = ((px - origin_dx) as i16, (py - origin_dy) as i16);
        if let Err(err) = send_motion(x, target, px, py, lx, ly, held_state).and_then(|()| x.conn.flush().map_err(|e| e.to_string())) {
            step_err = Some(err);
            break;
        }
        moved(at, px, py);
        last_x = px;
        last_y = py;
        thread::sleep(Duration::from_millis(4));
    }
    let (llx, lly) = ((last_x - origin_dx) as i16, (last_y - origin_dy) as i16);
    let up_res = send_button(x, target, button.number(), false, (last_x, last_y), (llx, lly), held_state);
    let flush_res = x.conn.flush().map_err(|e| e.to_string());
    if let Some(err) = step_err {
        return Err(err);
    }
    up_res?;
    flush_res?;
    Ok(finished)
}

// ---- The agents' own pointer. ----
//
// X can have more than one pointer: XInput 2 adds a "master" pointer with its own place on the
// screen, and a test device whose made-up presses and moves are real input to every app, XInput 2
// ones such as GTK, Qt and Chrome included. Agents use one of their own, so the person's pointer
// never moves. Moving it has to be a made-up move of that device: a warp makes no motion events.

const AGENT_POINTER: &str = "deskcrew";

struct Agent {
    /// The test device of the agents' master pointer: the one made-up input goes through.
    device: u8,
    xtest_opcode: u8,
    /// XInput's first event number, which device event numbers count from.
    first_event: u8,
}

static AGENT: OnceLock<Option<Agent>> = OnceLock::new();

/// The agents' pointer, made the first time it is needed and kept by the X server for the next
/// run. None where the X server has no XInput 2, as some remote displays do.
fn agent() -> Option<&'static Agent> {
    AGENT.get_or_init(|| make_agent().ok()).as_ref()
}

fn make_agent() -> Result<Agent, String> {
    use x11rb::{connection::RequestConnection as _, protocol::xinput::{self, ConnectionExt as _}};
    let x = x()?;
    let err = |e: &dyn std::fmt::Display| e.to_string();
    x.conn.xinput_xi_query_version(2, 2).map_err(|e| err(&e))?.reply().map_err(|e| err(&e))?;
    let find = || -> Result<Option<u8>, String> {
        let devices = x.conn.xinput_xi_query_device(0u16).map_err(|e| err(&e))?.reply().map_err(|e| err(&e))?.infos;
        let master = devices.iter().find(|d| d.type_ == xinput::DeviceType::MASTER_POINTER && d.name == format!("{AGENT_POINTER} pointer").as_bytes());
        Ok(master.and_then(|m| {
            devices
                .iter()
                .find(|d| d.type_ == xinput::DeviceType::SLAVE_POINTER && d.attachment == m.deviceid && d.name.ends_with(b"XTEST pointer"))
                .and_then(|d| u8::try_from(d.deviceid).ok())
        }))
    };
    let device = match find()? {
        Some(device) => device,
        None => {
            let add = xinput::HierarchyChange {
                len: 0,
                data: xinput::HierarchyChangeData::AddMaster(xinput::HierarchyChangeDataAddMaster { send_core: true, enable: true, name: AGENT_POINTER.as_bytes().to_vec() }),
            };
            x.conn.xinput_xi_change_hierarchy(&[add]).map_err(|e| err(&e))?.check().map_err(|e| err(&e))?;
            find()?.ok_or("the X server made no test device for the agents' pointer")?
        }
    };
    let opcode = |name| x.conn.extension_information(name).ok().flatten().ok_or(format!("no {name} extension"));
    Ok(Agent { device, xtest_opcode: opcode(x11rb::protocol::xtest::X11_EXTENSION_NAME)?.major_opcode, first_event: opcode(xinput::X11_EXTENSION_NAME)?.first_event })
}

/// Whether agents have a pointer of their own here.
pub fn own_pointer() -> bool {
    agent().is_some()
}

/// One made-up event of the agents' test device, with the place for a move. XTest takes a device's
/// position as a second event after the first, which x11rb's request does not carry, so the
/// request is put together here.
fn agent_event(agent: &Agent, kind: u8, detail: u8, at: Option<(i32, i32)>) -> Result<(), String> {
    use std::io::IoSlice;
    use x11rb::connection::RequestConnection;
    let x = x()?;
    let words: u16 = if at.is_some() { 17 } else { 9 };
    let mut request = Vec::with_capacity(words as usize * 4);
    request.extend([agent.xtest_opcode, 2]);
    request.extend(words.to_ne_bytes());
    request.extend([agent.first_event + kind, detail, 0, 0]);
    request.extend([0u8; 28]);
    request[35] = agent.device;
    if let Some((px, py)) = at {
        // The device's place, as its first two values, in screen pixels.
        request.extend([agent.first_event, agent.device, 0, 0, 0, 0, 2, 0]);
        for value in [px, py, 0, 0, 0, 0] {
            request.extend(value.to_ne_bytes());
        }
    }
    x.conn.send_request_without_reply(&[IoSlice::new(&request)], Vec::new()).map_err(|e| e.to_string())?;
    Ok(())
}

const DEVICE_BUTTON_PRESS: u8 = 3;
const DEVICE_BUTTON_RELEASE: u8 = 4;
const DEVICE_MOTION: u8 = 5;

/// Input by the agents' pointer, or by the display's own one where there is no other.
#[derive(Clone, Copy, PartialEq)]
pub enum By {
    Person,
    Agent,
}

pub fn move_by(by: By, px: i32, py: i32) -> Result<(), String> {
    match (by, agent()) {
        (By::Agent, Some(agent)) => {
            agent_event(agent, DEVICE_MOTION, 0, Some((px, py)))?;
            x()?.conn.flush().map_err(|e| e.to_string())
        }
        _ => move_to(px, py),
    }
}

fn button_by(by: By, number: u8, down: bool) -> Result<(), String> {
    match (by, agent()) {
        (By::Agent, Some(agent)) => {
            agent_event(agent, if down { DEVICE_BUTTON_PRESS } else { DEVICE_BUTTON_RELEASE }, number, None)?;
            x()?.conn.flush().map_err(|e| e.to_string())
        }
        _ => {
            let x = x()?;
            fake(x, if down { xproto::BUTTON_PRESS_EVENT } else { xproto::BUTTON_RELEASE_EVENT }, number, None)?;
            x.conn.flush().map_err(|e| e.to_string())
        }
    }
}

/// Whether the window (or a popup/dialog belonging to it), rather than another app on top of it,
/// shows at this point of the screen, so a press there reaches it without bringing it forward.
pub fn shows_at(window: u32, px: i32, py: i32) -> bool {
    let (Ok(px16), Ok(py16)) = (i16::try_from(px), i16::try_from(py)) else {
        return false;
    };
    let Ok(x) = x() else { return false };
    let Some(top) = x.conn.translate_coordinates(x.root, x.root, px16, py16).ok().and_then(|c| c.reply().ok()).map(|r| r.child) else {
        return false;
    };
    if top == x11rb::NONE {
        return false;
    }
    // The window manager's frame around it is on top, not the window itself.
    let mut at = window;
    for _ in 0..8 {
        if at == top {
            return true;
        }
        match x.conn.query_tree(at).ok().and_then(|c| c.reply().ok()) {
            Some(tree) if tree.parent != x.root && tree.parent != x11rb::NONE => at = tree.parent,
            _ => break,
        }
    }
    // A dialog transient for this window, or an override-redirect menu/dropdown of the same app.
    let mut owner = top;
    for _ in 0..8 {
        let Some(&transient) = x.numbers(owner, "WM_TRANSIENT_FOR", AtomEnum::WINDOW.into()).first() else {
            break;
        };
        if transient == window {
            return true;
        }
        if transient == x11rb::NONE || transient == owner {
            break;
        }
        owner = transient;
    }
    if x.conn.get_window_attributes(top).ok().and_then(|c| c.reply().ok()).is_some_and(|a| a.override_redirect) {
        let win_pid = x.numbers(window, "_NET_WM_PID", AtomEnum::CARDINAL.into()).first().copied().unwrap_or(0);
        let top_pid = x.numbers(top, "_NET_WM_PID", AtomEnum::CARDINAL.into()).first().copied().unwrap_or(0);
        if win_pid > 0 && win_pid == top_pid {
            return true;
        }
    }
    false
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
        "space" | "spacebar" => 0x20,
        "pause" | "break" => 0xff13,
        "plus" => 0x002b,
        "minus" => 0x002d,
        "equal" | "equals" => 0x003d,
        "comma" => 0x002c,
        "period" => 0x002e,
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
pub fn keysym_of(c: char) -> u32 {
    let code = c as u32;
    match code {
        0x0a | 0x0d => 0xff0d,
        0x09 => 0xff09,
        0x20..=0x7e | 0xa0..=0xff => code,
        _ => 0x0100_0000 + code,
    }
}

pub fn chord(spec: &str) -> Result<Chord, String> {
    if spec.trim().is_empty() {
        return Err("which key?".into());
    }
    let mut modifiers = Vec::new();
    let mut key = None;
    let lower = spec.to_lowercase();
    for part in lower.split('+').map(str::trim) {
        if part.is_empty() {
            return Err(format!("empty key in '{spec}': use 'plus' for '+'"));
        }
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

fn keymap_unlocked(x: &X) -> Result<Keymap, String> {
    let setup = x.conn.setup();
    let (min, max) = (setup.min_keycode, setup.max_keycode);
    let reply = x.conn.get_keyboard_mapping(min, max - min + 1).map_err(|e| e.to_string())?.reply().map_err(|e| e.to_string())?;
    Ok(Keymap { min, per: reply.keysyms_per_keycode as usize, syms: reply.keysyms })
}

fn keymap(x: &X) -> Result<Keymap, String> {
    let _guard = KEYMAP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    keymap_unlocked(x)
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
    let mut _keymap_guard = None;
    let mut active_per = map.per.max(1);
    let (code, needs_shift, borrowed) = match map.find(sym) {
        Some((code, shifted)) => (code, shifted, false),
        None => {
            _keymap_guard = Some(KEYMAP_LOCK.lock().unwrap_or_else(|e| e.into_inner()));
            let fresh = keymap_unlocked(x)?;
            if let Some((code, shifted)) = fresh.find(sym) {
                (code, shifted, false)
            } else {
                let code = fresh.spare().ok_or("no free key to type that character with")?;
                active_per = fresh.per.max(1);
                let row = vec![sym; active_per];
                x.conn.change_keyboard_mapping(1, code, active_per as u8, &row).map_err(|e| e.to_string())?;
                (code, false, true)
            }
        }
    };
    let shift_now = needs_shift && !held.contains(&shift.unwrap_or(0));
    let mut shift_down = false;
    let sent = (|| -> Result<(), String> {
        if borrowed {
            x.conn.flush().map_err(|e| e.to_string())?;
            thread::sleep(KEY_GAP);
        }
        if shift_now && let Some(s) = shift {
            key_event(x, s, true)?;
            shift_down = true;
        }
        key_event(x, code, true)?;
        key_event(x, code, false)?;
        x.conn.flush().map_err(|e| e.to_string())?;
        thread::sleep(KEY_GAP);
        Ok(())
    })();
    let shift_up_res = if shift_down && let Some(s) = shift {
        let res = key_event(x, s, false);
        let _ = x.conn.flush();
        res
    } else {
        Ok(())
    };
    let restore_res = if borrowed {
        let row = vec![0u32; active_per];
        let res = x.conn.change_keyboard_mapping(1, code, active_per as u8, &row).map(|_| ()).map_err(|e| e.to_string());
        let _ = x.conn.flush();
        res
    } else {
        Ok(())
    };
    sent?;
    shift_up_res?;
    restore_res
}

/// Press a chord in the focused window, holding the key for `hold`.
pub fn press(chord: &Chord, hold: Duration) -> Result<(), String> {
    let x = x()?;
    let map = keymap(x)?;
    let mods: Vec<u8> = chord.modifiers.iter().filter_map(|m| map.find(*m).map(|k| k.0)).collect();
    let mut pressed = Vec::with_capacity(mods.len());
    let mut mod_err = None;
    for &m in &mods {
        if let Err(err) = key_event(x, m, true) {
            mod_err = Some(err);
            break;
        }
        pressed.push(m);
    }
    let result = if let Some(err) = mod_err {
        Err(err)
    } else {
        match chord.key {
            Some(sym) if hold.is_zero() => tap(x, &map, sym, &mods),
            Some(sym) => match map.find(sym) {
                Some((code, _)) => match key_event(x, code, true) {
                    Ok(()) => {
                        let flush_res = x.conn.flush().map_err(|e| e.to_string());
                        thread::sleep(hold);
                        let up_res = key_event(x, code, false);
                        flush_res.and(up_res)
                    }
                    Err(err) => Err(err),
                },
                None => tap(x, &map, sym, &mods),
            },
            None => {
                thread::sleep(hold);
                Ok(())
            }
        }
    };
    let mut up_err = None;
    for &m in pressed.iter().rev() {
        if let Err(err) = key_event(x, m, false)
            && up_err.is_none()
        {
            up_err = Some(err);
        }
    }
    let flush_res = x.conn.flush().map_err(|e| e.to_string());
    result?;
    if let Some(err) = up_err {
        return Err(err);
    }
    flush_res
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
        // A tap already waits a moment; the rest of the gap keeps the typing speed.
        thread::sleep(crate::motion::KEY_EVERY.saturating_sub(KEY_GAP));
    }
    Ok(typed)
}

/// Resolve the X11 subwindow and interior `(root_x, root_y, event_x, event_y)` coordinates for
/// synthetic key delivery: uses `at` when a control spot is known, or the window's centre.
fn key_target(x: &X, window: XWindow, at: Option<(i32, i32)>) -> (XWindow, i16, i16, i16, i16) {
    let (px, py) = at.unwrap_or_else(|| {
        x.rect(window)
            .map(|(wx, wy, ww, wh)| (wx + ww / 2, wy + wh / 2))
            .unwrap_or((1, 1))
    });
    let (target, lx, ly) = child_at(x, window, px, py);
    (target, px as i16, py as i16, lx, ly)
}

fn send_key_event(x: &X, target: XWindow, coords: (i16, i16, i16, i16), code: u8, down: bool, state: KeyButMask) -> Result<(), String> {
    let (root_x, root_y, event_x, event_y) = coords;
    if down {
        let event = xproto::KeyPressEvent {
            response_type: xproto::KEY_PRESS_EVENT,
            detail: code,
            sequence: 0,
            time: x.timestamp(),
            root: x.root,
            event: target,
            child: x11rb::NONE,
            root_x,
            root_y,
            event_x,
            event_y,
            state,
            same_screen: true,
        };
        x.conn.send_event(false, target, EventMask::NO_EVENT, event).map_err(|e| e.to_string())?;
    } else {
        let event = xproto::KeyReleaseEvent {
            response_type: xproto::KEY_RELEASE_EVENT,
            detail: code,
            sequence: 0,
            time: x.timestamp(),
            root: x.root,
            event: target,
            child: x11rb::NONE,
            root_x,
            root_y,
            event_x,
            event_y,
            state,
            same_screen: true,
        };
        x.conn.send_event(false, target, EventMask::NO_EVENT, event).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Serializes temporary spare-keycode mapping edits so concurrent agents never race on the X11 keymap.
static KEYMAP_LOCK: Mutex<()> = Mutex::new(());

fn send_tap(x: &X, target: XWindow, coords: (i16, i16, i16, i16), map: &Keymap, sym: u32, base_state: KeyButMask, hold: Duration) -> Result<(), String> {
    let mut _keymap_guard = None;
    let mut active_per = map.per.max(1);
    let (code, needs_shift, borrowed) = match map.find(sym) {
        Some((code, shifted)) => (code, shifted, false),
        None => {
            _keymap_guard = Some(KEYMAP_LOCK.lock().unwrap_or_else(|e| e.into_inner()));
            let fresh = keymap_unlocked(x)?;
            if let Some((code, shifted)) = fresh.find(sym) {
                (code, shifted, false)
            } else {
                let code = fresh.spare().ok_or("no free key to type that character with")?;
                active_per = fresh.per.max(1);
                let row = vec![sym; active_per];
                x.conn.change_keyboard_mapping(1, code, active_per as u8, &row).map_err(|e| e.to_string())?;
                (code, false, true)
            }
        }
    };
    let shift_code = (needs_shift && !base_state.contains(KeyButMask::SHIFT))
        .then(|| map.find(0xffe1).map(|k| k.0))
        .flatten();
    let state = if needs_shift { base_state | KeyButMask::SHIFT } else { base_state };
    let sent = (|| -> Result<(), String> {
        if borrowed {
            x.conn.flush().map_err(|e| e.to_string())?;
            thread::sleep(KEY_GAP);
        }
        if let Some(sc) = shift_code {
            send_key_event(x, target, coords, sc, true, base_state)?;
        }
        send_key_event(x, target, coords, code, true, state)?;
        x.conn.flush().map_err(|e| e.to_string())?;
        if !hold.is_zero() {
            thread::sleep(hold);
        }
        send_key_event(x, target, coords, code, false, state)?;
        if let Some(sc) = shift_code {
            send_key_event(x, target, coords, sc, false, state)?;
        }
        x.conn.flush().map_err(|e| e.to_string())?;
        thread::sleep(KEY_GAP);
        Ok(())
    })();
    if borrowed {
        let row = vec![0u32; active_per];
        let _ = x.conn.change_keyboard_mapping(1, code, active_per as u8, &row);
        let _ = x.conn.flush();
    }
    sent
}

/// Run synthetic key delivery inside a window-local synthetic FocusIn/FocusOut pair (when `window`
/// is in the background). Never calls `XSetInputFocus`, so OS/WM focus never changes.
fn with_synthetic_focus<T>(x: &X, window: XWindow, target: XWindow, work: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let background = front() != Some(window);
    if background {
        let _ = send_focus(x, window, true);
        if target != window {
            let _ = send_focus(x, target, true);
        }
        let _ = x.conn.flush();
    }
    let result = work();
    if background {
        if target != window {
            let _ = send_focus(x, target, false);
        }
        let _ = send_focus(x, window, false);
        let _ = x.conn.flush();
    }
    result
}

/// Send a key chord directly to `window` (optionally targeting the subwindow at screen spot `at`)
/// via XSendEvent without calling XSetInputFocus or raising.
pub fn send_key_at(window: u32, at: Option<(i32, i32)>, chord: &Chord, hold: Duration) -> Result<(), String> {
    let x = x()?;
    let map = keymap(x)?;
    let (target, rx, ry, lx, ly) = key_target(x, window, at);
    let coords = (rx, ry, lx, ly);
    let state = mod_mask(&chord.modifiers);
    with_synthetic_focus(x, window, target, || match chord.key {
        Some(sym) => {
            let mods: Vec<(u32, u8)> = chord.modifiers.iter().filter_map(|&m| map.find(m).map(|k| (m, k.0))).collect();
            let mut running = KeyButMask::default();
            for &(m, code) in &mods {
                send_key_event(x, target, coords, code, true, running)?;
                running |= mod_mask(&[m]);
            }
            let sent = send_tap(x, target, coords, &map, sym, state, hold);
            for &(m, code) in mods.iter().rev() {
                let _ = send_key_event(x, target, coords, code, false, running);
                running = KeyButMask::from(u16::from(running) & !u16::from(mod_mask(&[m])));
            }
            let _ = x.conn.flush();
            sent
        }
        None => {
            let mods: Vec<(u32, u8)> = chord.modifiers.iter().filter_map(|&m| map.find(m).map(|k| (m, k.0))).collect();
            let mut running = KeyButMask::default();
            for &(m, code) in &mods {
                send_key_event(x, target, coords, code, true, running)?;
                running |= mod_mask(&[m]);
            }
            x.conn.flush().map_err(|e| e.to_string())?;
            if !hold.is_zero() {
                thread::sleep(hold);
            }
            for &(m, code) in mods.iter().rev() {
                send_key_event(x, target, coords, code, false, running)?;
                running = KeyButMask::from(u16::from(running) & !u16::from(mod_mask(&[m])));
            }
            x.conn.flush().map_err(|e| e.to_string())
        }
    })
}

/// Send a key chord directly to `window` via XSendEvent without calling XSetInputFocus or raising.
pub fn send_key(window: u32, chord: &Chord, hold: Duration) -> Result<(), String> {
    send_key_at(window, None, chord, hold)
}

/// Type text directly into `window` (optionally at subwindow spot `at`) via XSendEvent without
/// taking keyboard focus.
pub fn send_text_at(window: u32, at: Option<(i32, i32)>, text: &str, stop: &dyn Fn() -> bool) -> Result<usize, String> {
    let x = x()?;
    let map = keymap(x)?;
    let (target, rx, ry, lx, ly) = key_target(x, window, at);
    let coords = (rx, ry, lx, ly);
    with_synthetic_focus(x, window, target, || {
        let mut typed = 0;
        for c in text.chars() {
            if stop() {
                break;
            }
            send_tap(x, target, coords, &map, keysym_of(c), KeyButMask::default(), Duration::ZERO)?;
            typed += 1;
            thread::sleep(crate::motion::KEY_EVERY.saturating_sub(KEY_GAP));
        }
        Ok(typed)
    })
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

/// Start an app in its own process group and return its PID.
pub fn spawn(app: &str, args: &[String]) -> Result<u32, String> {
    use std::os::unix::process::CommandExt;
    let mut child = Command::new(app)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("could not start {app}: {e}"))?;
    let pid = child.id();
    thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(pid)
}

fn use_wayland_clipboard() -> bool {
    (!has_display() && has_wayland()) || is_xwayland()
}

fn wayland_clipboard_types() -> Option<Result<Vec<String>, String>> {
    let types = match Command::new("wl-paste").arg("--list-types").stderr(Stdio::piped()).output() {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => return Some(Err(format!("could not run wl-paste: {e}"))),
    };
    if !types.status.success() {
        let err = String::from_utf8_lossy(&types.stderr);
        if err.contains("Nothing is copied") || err.trim().is_empty() {
            return Some(Ok(Vec::new()));
        }
        return Some(Err(format!("could not read the Wayland clipboard: {}", err.trim())));
    }
    let list = String::from_utf8_lossy(&types.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    Some(Ok(list))
}

fn wayland_clipboard() -> Option<Result<Option<String>, String>> {
    let list = match wayland_clipboard_types()? {
        Ok(list) => list,
        Err(e) => return Some(Err(e)),
    };
    if list.is_empty() {
        return Some(Ok(None));
    }
    let kind = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain", "STRING"]
        .into_iter()
        .find_map(|wanted| list.iter().find(|item| item.eq_ignore_ascii_case(wanted)).cloned())
        .or_else(|| list.iter().find(|k| k.starts_with("text/")).cloned());
    let Some(kind) = kind else {
        return Some(Ok(None));
    };
    let out = match Command::new("wl-paste").args(["--no-newline", "--type", &kind]).stderr(Stdio::null()).output() {
        Ok(out) => out,
        Err(e) => return Some(Err(format!("could not run wl-paste: {e}"))),
    };
    Some(Ok(out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())))
}

fn wayland_set_clipboard(text: Option<&str>) -> Option<Result<(), String>> {
    let Some(text) = text else {
        return match Command::new("wl-copy").arg("--clear").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).output() {
            Ok(out) if out.status.success() => Some(Ok(())),
            Ok(out) => Some(Err(format!("could not clear the Wayland clipboard: {}", String::from_utf8_lossy(&out.stderr).trim()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => Some(Err(format!("could not run wl-copy: {e}"))),
        };
    };
    own_clipboard("wl-copy", &[], text.as_bytes(), Some(text))
}

/// The clipboard through xclip or xsel on X11, or wl-clipboard on Wayland.
pub fn clipboard() -> Result<Option<String>, String> {
    if use_wayland_clipboard() {
        if let Some(res) = wayland_clipboard() {
            return res;
        }
        if !has_display() {
            return Err("the clipboard on Wayland needs wl-clipboard: install it (sudo apt install wl-clipboard)".into());
        }
    }
    for (program, args) in [("xclip", &["-selection", "clipboard", "-o"][..]), ("xsel", &["--clipboard", "--output"][..])] {
        if let Ok(out) = Command::new(program).args(args).stderr(Stdio::null()).output() {
            return Ok(out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned()));
        }
    }
    Err("the clipboard needs xclip or xsel: install one (sudo apt install xclip)".into())
}

fn set_clipboard_unlocked(text: Option<&str>) -> Result<(), String> {
    if use_wayland_clipboard() {
        if let Some(res) = wayland_set_clipboard(text) {
            return res;
        }
        if !has_display() {
            return Err("the clipboard on Wayland needs wl-clipboard: install it (sudo apt install wl-clipboard)".into());
        }
    }
    let text = text.unwrap_or("");
    for (program, args) in [("xclip", &["-selection", "clipboard", "-i"][..]), ("xsel", &["--clipboard", "--input"][..])] {
        if let Some(done) = own_clipboard(program, args, text.as_bytes(), Some(text)) {
            return done;
        }
    }
    Err("the clipboard needs xclip or xsel: install one (sudo apt install xclip)".into())
}

pub fn set_clipboard(text: Option<&str>) -> Result<(), String> {
    let _loan = CLIPBOARD_LOAN.lock().unwrap_or_else(|e| e.into_inner());
    set_clipboard_unlocked(text)
}

/// Hand the clipboard to xclip, xsel or wl-copy holding these bytes. None if the program is missing.
fn own_clipboard(program: &str, args: &[&str], bytes: &[u8], text: Option<&str>) -> Option<Result<(), String>> {
    let mut child = Command::new(program).args(args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().ok()?;
    if let Some(mut input) = child.stdin.take()
        && let Err(e) = input.write_all(bytes)
    {
        let _ = child.wait();
        return Some(Err(e.to_string()));
    }
    // The program leaves a copy of itself behind to own the clipboard: wait until it does,
    // so the next read sees the new contents.
    let started = Instant::now();
    let mut exited = false;
    while started.elapsed() < Duration::from_secs(1) {
        if !exited
            && let Ok(Some(status)) = child.try_wait()
        {
            exited = true;
            if !status.success() {
                return Some(Err(format!("{program} exited with {status}")));
            }
        }
        if exited && text.is_none_or(|text| clipboard().ok().flatten().unwrap_or_default() == text) {
            return Some(Ok(()));
        }
        thread::sleep(Duration::from_millis(20));
    }
    if exited {
        return Some(Err(format!("{program} did not update the clipboard")));
    }
    thread::spawn(move || child.wait());
    Some(Ok(()))
}

/// The kinds of data on the clipboard, as X apps name them.
fn clipboard_kinds() -> Option<(bool, Vec<String>)> {
    if use_wayland_clipboard()
        && let Some(Ok(kinds)) = wayland_clipboard_types()
    {
        return Some((true, kinds));
    }
    let out = Command::new("xclip").args(["-selection", "clipboard", "-t", "TARGETS", "-o"]).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| {
        (
            false,
            String::from_utf8_lossy(&out.stdout).lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect(),
        )
    })
}

/// The kind of clipboard data worth keeping most: a picture, then copied files, then text.
/// xclip can offer only one kind, so the others an app put there alongside it are not kept.
fn main_kind(kinds: &[String]) -> Option<&String> {
    const NOT_DATA: [&str; 6] = ["TARGETS", "TIMESTAMP", "MULTIPLE", "SAVE_TARGETS", "DELETE", "INCR"];
    let first = |test: &dyn Fn(&str) -> bool| kinds.iter().find(|k| test(k));
    first(&|k| k == "image/png")
        .or_else(|| first(&|k| k.starts_with("image/")))
        .or_else(|| first(&|k| k == "x-special/gnome-copied-files"))
        .or_else(|| first(&|k| k == "text/uri-list"))
        .or_else(|| first(&|k| k == "UTF8_STRING"))
        .or_else(|| first(&|k| k.starts_with("text/plain")))
        .or_else(|| first(&|k| !NOT_DATA.contains(&k)))
}

/// Serializes clipboard loans across concurrent agents so one agent's paste cannot overwrite
/// another agent's lent text or corrupt the person's saved clipboard.
static CLIPBOARD_LOAN: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// What was on the clipboard before an agent borrowed it to paste: its picture, copied files or
/// text, whichever it held. Automatically restores on drop if `put_back` was not reached.
pub struct SavedClipboard {
    _loan: std::sync::MutexGuard<'static, ()>,
    kept: Option<(String, Vec<u8>)>,
    lent: Option<String>,
    restored: bool,
}

impl SavedClipboard {
    pub fn take() -> SavedClipboard {
        let loan = CLIPBOARD_LOAN.lock().unwrap_or_else(|e| e.into_inner());
        let kept = match clipboard_kinds() {
            Some((wayland, kinds)) => main_kind(&kinds).and_then(|kind| {
                let out = if wayland {
                    let mut cmd = Command::new("wl-paste");
                    if kind == "UTF8_STRING" || kind.starts_with("text/plain") {
                        cmd.arg("--no-newline");
                    }
                    cmd.args(["--type", kind]).stderr(Stdio::null()).output().ok()?
                } else {
                    Command::new("xclip").args(["-selection", "clipboard", "-t", kind, "-o"]).stderr(Stdio::null()).output().ok()?
                };
                out.status.success().then(|| (kind.clone(), out.stdout))
            }),
            None => clipboard().ok().flatten().map(|text| ("UTF8_STRING".to_string(), text.into_bytes())),
        };
        SavedClipboard { _loan: loan, kept, lent: None, restored: false }
    }

    pub fn lend(&mut self, text: &str) -> Result<(), String> {
        set_clipboard_unlocked(Some(text))?;
        self.lent = Some(text.to_string());
        Ok(())
    }

    fn restore_inner(&mut self) -> Result<(), String> {
        if self.restored {
            return Ok(());
        }
        self.restored = true;
        let Some(lent) = &self.lent else {
            return Ok(());
        };
        if clipboard().ok().flatten().as_deref() != Some(lent.as_str()) {
            return Ok(());
        }
        let Some((kind, bytes)) = self.kept.take() else { return set_clipboard_unlocked(None) };
        if kind == "UTF8_STRING" || kind.starts_with("text/plain") {
            return set_clipboard_unlocked(Some(&String::from_utf8_lossy(&bytes)));
        }
        if use_wayland_clipboard()
            && let Some(done) = own_clipboard("wl-copy", &["--type", &kind], &bytes, None)
        {
            return done;
        }
        own_clipboard("xclip", &["-selection", "clipboard", "-t", &kind, "-i"], &bytes, None)
            .unwrap_or_else(|| Err("the clipboard needs xclip to put a picture or files back".into()))
    }

    /// Put the person's clipboard back, unless they copied something new in the meantime.
    pub fn put_back(mut self) -> Result<(), String> {
        self.restore_inner()
    }
}

impl Drop for SavedClipboard {
    fn drop(&mut self) {
        let _ = self.restore_inner();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifier_and_button_masks_match_x11() {
        let mask = mod_mask(&[0xffe1, 0xffe3, 0xffe9, 0xffeb]);
        assert!(mask.contains(KeyButMask::SHIFT));
        assert!(mask.contains(KeyButMask::CONTROL));
        assert!(mask.contains(KeyButMask::MOD1));
        assert!(mask.contains(KeyButMask::MOD4));
        assert_eq!(button_mask(Button::Left), KeyButMask::BUTTON1);
        assert_eq!(button_mask(Button::Middle), KeyButMask::BUTTON2);
        assert_eq!(button_mask(Button::Right), KeyButMask::BUTTON3);
    }

    #[test]
    fn risky_chords_are_flagged() {
        assert!(is_risky(&chord("alt+f4").unwrap()));
        assert!(is_risky(&chord("ctrl+q").unwrap()));
        assert!(is_risky(&chord("ctrl+w").unwrap()));
        assert!(!is_risky(&chord("ctrl+c").unwrap()));
    }

    #[test]
    fn synthetic_timestamps_are_nonzero_and_monotonic() {
        let t1 = next_timestamp(1, 0);
        assert!(t1 > 1);
        let t2 = next_timestamp(t1, 0);
        assert_eq!(t2, t1 + 1);
        let t3 = next_timestamp(t2, 500);
        assert_eq!(t3, 500);
        assert_eq!(next_timestamp(u32::MAX, 0), 1);
    }

    #[test]
    fn chord_aliases_and_empty_parts_behave_consistently() {
        assert_eq!(chord("plus").unwrap().key, Some(0x002b));
        assert_eq!(chord("minus").unwrap().key, Some(0x002d));
        assert_eq!(chord("equal").unwrap().key, Some(0x003d));
        assert_eq!(chord("equals").unwrap().key, Some(0x003d));
        assert_eq!(chord("comma").unwrap().key, Some(0x002c));
        assert_eq!(chord("period").unwrap().key, Some(0x002e));
        assert_eq!(chord("pause").unwrap().key, Some(0xff13));
        assert_eq!(chord("spacebar").unwrap().key, Some(0x20));
        assert_eq!(chord("pgup").unwrap().key, Some(0xff55));
        assert_eq!(chord("pgdn").unwrap().key, Some(0xff56));
        assert_eq!(chord("ins").unwrap().key, Some(0xff63));
        assert_eq!(chord("back").unwrap().key, Some(0xff08));
        assert_eq!(chord("del").unwrap().key, Some(0xffff));
        assert_eq!(chord("delete").unwrap().key, Some(0xffff));
        let spaced = chord("ctrl + s").unwrap();
        assert_eq!(spaced.modifiers, vec![0xffe3]);
        assert_eq!(spaced.key, Some(b's' as u32));
        assert!(chord("").is_err());
        assert!(chord("  ").is_err());
        assert!(chord("ctrl++").is_err());
        assert!(chord("win+").is_err());
        assert!(chord("ctrl+a+b").is_err());
    }
}
