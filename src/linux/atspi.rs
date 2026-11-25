// Reading and acting on a Linux window through AT-SPI, the accessibility bus GTK, Qt, Firefox,
// Chromium and LibreOffice speak: the same short lines the Windows and Mac sides build.
use crate::lines::Parts;
use std::sync::OnceLock;
use zbus::{blocking::Connection, zvariant::OwnedObjectPath};

pub type Bounds = (i32, i32, i32, i32);

const MAX_DEPTH: usize = 40;
const MAX_NODES: usize = 4000;
const ROOT_BUS: &str = "org.a11y.atspi.Registry";
const ROOT_PATH: &str = "/org/a11y/atspi/accessible/root";
const ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
const SCREEN: u32 = 0;
const WINDOW: u32 = 1;

// AtspiStateType, bit numbers in GetState's two words.
const ACTIVE: u32 = 1;
const CHECKED: u32 = 4;
const COLLAPSED: u32 = 5;
const DEFUNCT: u32 = 6;
const EDITABLE: u32 = 7;
const ENABLED: u32 = 8;
const EXPANDABLE: u32 = 9;
const EXPANDED: u32 = 10;
const FOCUSED: u32 = 12;
const HORIZONTAL: u32 = 14;
const PRESSED: u32 = 20;
const SELECTED: u32 = 23;
const SENSITIVE: u32 = 24;
const SHOWING: u32 = 25;
const VERTICAL: u32 = 29;
const VISIBLE: u32 = 30;
const INDETERMINATE: u32 = 32;
const IS_DEFAULT: u32 = 39;
const CHECKABLE: u32 = 41;
const READ_ONLY: u32 = 43;

/// One object on the accessibility bus.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Obj {
    pub bus: String,
    pub path: String,
}

/// A top-level window discovered on the accessibility bus.
pub struct FrameWindow {
    pub element: Obj,
    pub pid: u32,
    pub app: String,
    pub title: String,
    pub rect: Bounds,
}

pub struct Node {
    pub key: usize,
    pub identity: String,
    pub parts: Parts,
    pub rect: Bounds,
    pub element: Obj,
}

static BUS: OnceLock<Result<Connection, String>> = OnceLock::new();

/// Turn on desktop accessibility status properties on the session bus so Chromium, Electron,
/// Firefox, GTK and Qt publish their full accessibility trees.
fn enable_session_a11y(session: &Connection) {
    for prop in ["IsEnabled", "ScreenReaderEnabled"] {
        let _ = session.call_method(
            Some("org.a11y.Bus"),
            "/org/a11y/bus",
            Some("org.freedesktop.DBus.Properties"),
            "Set",
            &("org.a11y.Status", prop, zbus::zvariant::Value::from(true)),
        );
    }
}

/// The accessibility bus: the one AT_SPI_BUS_ADDRESS or the X display names, or the one the
/// session bus hands out. Apps started after this see that someone is listening, and turn their
/// accessibility on.
pub fn bus() -> Result<&'static Connection, String> {
    BUS.get_or_init(|| {
        let named = std::env::var("AT_SPI_BUS_ADDRESS").ok().filter(|a| !a.is_empty()).or_else(super::x::accessibility_bus);
        let address = match named {
            Some(address) => {
                // Even when the bus address is already known, ensure IsEnabled and
                // ScreenReaderEnabled are set so browsers/Electron expose full trees.
                if let Ok(session) = Connection::session() {
                    enable_session_a11y(&session);
                }
                address
            }
            None => {
                let session = Connection::session().map_err(|e| format!("no D-Bus session ({e}): start one with dbus-run-session, or run inside a desktop session"))?;
                let address: String = session
                    .call_method(Some("org.a11y.Bus"), "/org/a11y/bus", Some("org.a11y.Bus"), "GetAddress", &())
                    .and_then(|m| m.body().deserialize())
                    .map_err(|e| format!("no accessibility bus ({e}): install at-spi2-core, or set AT_SPI_BUS_ADDRESS"))?;
                enable_session_a11y(&session);
                address
            }
        };
        // One frozen app must cost a timeout, not hang every read: libatspi waits 800 ms.
        zbus::blocking::connection::Builder::address(address.as_str())
            .map(|b| b.method_timeout(ATSPI_TIMEOUT))
            .and_then(|b| b.build())
            .map_err(|e| format!("could not join the accessibility bus: {e}"))
    })
    .as_ref()
    .map_err(Clone::clone)
}

/// How long one accessibility call may take before the app counts as not answering.
const ATSPI_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

fn call<B, R>(obj: &Obj, iface: &str, method: &str, body: &B) -> Result<R, String>
where
    B: zbus::export::serde::ser::Serialize + zbus::zvariant::DynamicType,
    R: for<'d> zbus::zvariant::DynamicDeserialize<'d>,
{
    let bus = bus()?;
    let reply = bus.call_method(Some(obj.bus.as_str()), obj.path.as_str(), Some(iface), method, body).map_err(|e| e.to_string())?;
    reply.body().deserialize().map_err(|e| e.to_string())
}

fn property<R>(obj: &Obj, iface: &str, name: &str) -> Result<R, String>
where
    R: TryFrom<zbus::zvariant::OwnedValue>,
{
    let value: zbus::zvariant::OwnedValue = call(obj, "org.freedesktop.DBus.Properties", "Get", &(iface, name))?;
    R::try_from(value).map_err(|_| format!("{name} has an unexpected type"))
}

fn set_property(obj: &Obj, iface: &str, name: &str, value: zbus::zvariant::Value<'_>) -> Result<(), String> {
    call::<_, ()>(obj, "org.freedesktop.DBus.Properties", "Set", &(iface, name, value))
}

pub fn children(obj: &Obj) -> Vec<Obj> {
    call::<_, Vec<(String, OwnedObjectPath)>>(obj, ACCESSIBLE, "GetChildren", &())
        .unwrap_or_default()
        .into_iter()
        .map(|(bus, path)| Obj { bus, path: path.as_str().to_string() })
        .filter(|o| o.path != "/org/a11y/atspi/null")
        .collect()
}

pub fn name(obj: &Obj) -> String {
    property(obj, ACCESSIBLE, "Name").unwrap_or_default()
}

fn description(obj: &Obj) -> String {
    property(obj, ACCESSIBLE, "Description").unwrap_or_default()
}

pub fn role_name(obj: &Obj) -> String {
    call(obj, ACCESSIBLE, "GetRoleName", &()).unwrap_or_default()
}

fn interfaces(obj: &Obj) -> Vec<String> {
    call(obj, ACCESSIBLE, "GetInterfaces", &()).unwrap_or_default()
}

fn try_states(obj: &Obj) -> Result<u64, String> {
    let words: Vec<u32> = call(obj, ACCESSIBLE, "GetState", &())?;
    if words.is_empty() {
        return Err("that control did not report its state".into());
    }
    Ok(words.first().copied().unwrap_or(0) as u64 | (words.get(1).copied().unwrap_or(0) as u64) << 32)
}

fn states(obj: &Obj) -> u64 {
    try_states(obj).unwrap_or(0)
}

fn has(states: u64, bit: u32) -> bool {
    states & (1u64 << bit) != 0
}

/// Where the object is on screen (or relative to its surface on Wayland).
pub fn rect(obj: &Obj) -> Option<Bounds> {
    let valid = |r: &Bounds| r.0 != i32::MIN && r.1 != i32::MIN && r.2 > 0 && r.3 > 0;
    let screen = call::<_, (i32, i32, i32, i32)>(obj, "org.a11y.atspi.Component", "GetExtents", &(SCREEN,)).ok();
    if screen.as_ref().is_some_and(valid) || (super::x::has_display() && !super::x::is_xwayland()) {
        return screen.filter(|r| r.0 != i32::MIN && r.1 != i32::MIN);
    }
    call::<_, (i32, i32, i32, i32)>(obj, "org.a11y.atspi.Component", "GetExtents", &(WINDOW,))
        .ok()
        .filter(valid)
        .or_else(|| screen.filter(|r| r.0 != i32::MIN && r.1 != i32::MIN))
}

/// The process behind a bus name on the accessibility bus.
fn pid_of(bus_name: &str) -> Option<u32> {
    let bus = bus().ok()?;
    bus.call_method(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", Some("org.freedesktop.DBus"), "GetConnectionUnixProcessID", &(bus_name,))
        .ok()?
        .body()
        .deserialize()
        .ok()
}

/// Every app on the bus: the registry's list, or, with no registry running, each connection
/// that answers at the usual root path.
fn applications() -> Vec<Obj> {
    let root = Obj { bus: ROOT_BUS.into(), path: ROOT_PATH.into() };
    let listed = children(&root);
    if !listed.is_empty() {
        return listed;
    }
    let Ok(bus) = bus() else { return Vec::new() };
    let mine = bus.unique_name().map(|n| n.to_string()).unwrap_or_default();
    let names: Vec<String> = bus
        .call_method(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", Some("org.freedesktop.DBus"), "ListNames", &())
        .ok()
        .and_then(|m| m.body().deserialize().ok())
        .unwrap_or_default();
    names
        .into_iter()
        .filter(|n| n.starts_with(':') && *n != mine)
        .map(|bus| Obj { bus, path: ROOT_PATH.into() })
        .filter(|app| role_name(app) == "application")
        .collect()
}

fn is_window_role(role: &str) -> bool {
    matches!(role, "frame" | "window" | "dialog" | "file chooser" | "alert" | "color chooser" | "font chooser" | "page setup")
}

fn app_name(app: Option<&Obj>, pid: u32) -> String {
    let from_bus = app.map(name).filter(|n| !n.is_empty());
    let from_proc = (pid > 0).then(|| std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()).flatten().map(|c| c.trim().to_string());
    from_bus.or(from_proc).unwrap_or_default()
}

/// Top-level visible windows on the accessibility bus, active windows first.
pub fn windows() -> Result<Vec<FrameWindow>, String> {
    bus()?;
    let me = std::process::id();
    let mut out: Vec<(bool, FrameWindow)> = Vec::new();
    for app in applications() {
        let Some(pid) = pid_of(&app.bus) else { continue };
        if pid == me {
            continue;
        }
        let app_label = app_name(Some(&app), pid);
        for frame in children(&app) {
            if !is_window_role(&role_name(&frame)) {
                continue;
            }
            let state = states(&frame);
            if !has(state, SHOWING) && !has(state, VISIBLE) {
                continue;
            }
            let title = name(&frame);
            let bounds = rect(&frame).unwrap_or((0, 0, 0, 0));
            if title.is_empty() && bounds.2 <= 0 && bounds.3 <= 0 && children(&frame).is_empty() {
                continue;
            }
            out.push((
                has(state, ACTIVE),
                FrameWindow { element: frame, pid, app: app_label.clone(), title, rect: bounds },
            ));
        }
    }
    out.sort_by_key(|(active, _)| !*active);
    Ok(out.into_iter().map(|(_, window)| window).collect())
}

/// Live properties of a top-level accessible window, or None if it closed.
pub fn frame_info(obj: &Obj) -> Option<FrameWindow> {
    if !is_window_role(&role_name(obj)) {
        return None;
    }
    let state = states(obj);
    if !has(state, SHOWING) && !has(state, VISIBLE) {
        return None;
    }
    let pid = pid_of(&obj.bus).unwrap_or(0);
    let app = app_name(parent(obj).as_ref(), pid);
    let title = name(obj);
    let bounds = rect(obj).unwrap_or((0, 0, 0, 0));
    Some(FrameWindow { element: obj.clone(), pid, app, title, rect: bounds })
}

/// The app's top-level windows on the accessibility bus, with the process each belongs to.
pub fn frames() -> Vec<(Obj, u32)> {
    let mut out = Vec::new();
    for app in applications() {
        let Some(pid) = pid_of(&app.bus) else { continue };
        for frame in children(&app) {
            out.push((frame, pid));
        }
    }
    out
}

/// Visible popup windows, menus or dialogs belonging to the same process (excluding `main`).
pub fn popup_frames(pid: u32, main: &Obj) -> Vec<Obj> {
    if pid == 0 {
        return Vec::new();
    }
    frames()
        .into_iter()
        .filter(|(f, p)| *p == pid && f != main)
        .map(|(f, _)| f)
        .filter(|f| {
            let r = role_name(f);
            let popup_role = matches!(r.as_str(), "window" | "dialog" | "alert" | "file chooser" | "menu" | "popup menu" | "panel");
            if !popup_role {
                return false;
            }
            let st = states(f);
            !has(st, DEFUNCT) && (has(st, SHOWING) || has(st, VISIBLE))
        })
        .collect()
}

/// The accessible window that is this X window: same process first, then matching title or bounds
/// (which also catches Flatpak/Snap PID namespaces, multi-process launchers and apps with no
/// `_NET_WM_PID`).
pub fn frame_for(pid: u32, title: &str, at: Bounds) -> Option<Obj> {
    let all = frames();
    let near = |f: &&Obj| at.2 > 0 && at.3 > 0 && rect(f).is_some_and(|r| r.2 > 0 && r.3 > 0 && r.0.abs_diff(at.0) < 60 && r.1.abs_diff(at.1) < 60 && r.2.abs_diff(at.2) < 60);
    let pick = |candidates: &[Obj], allow_single: bool| -> Option<Obj> {
        let showing: Vec<&Obj> = candidates.iter().filter(|f| has(states(f), SHOWING) || has(states(f), VISIBLE)).collect();
        if let Some(same) = showing.iter().find(|f| !title.is_empty() && name(f) == title) {
            return Some((*same).clone());
        }
        if let Some(placed) = showing.iter().find(|f| near(f)) {
            return Some((*placed).clone());
        }
        if allow_single {
            return match showing.as_slice() {
                [only] => Some((*only).clone()),
                _ => candidates.first().cloned(),
            };
        }
        None
    };
    if pid > 0 {
        let mine: Vec<Obj> = all.iter().filter(|(_, p)| *p == pid).map(|(f, _)| f.clone()).collect();
        if let Some(found) = pick(&mine, true) {
            return Some(found);
        }
    }
    let everyone: Vec<Obj> = all.into_iter().map(|(f, _)| f).collect();
    pick(&everyone, false)
}

/// A role for an AT-SPI role name, or None for containers that only matter for what they hold.
/// `is_editable` distinguishes editable text inputs and contenteditable paragraphs from
/// read-only/static text nodes in browsers, Electron, LibreOffice, Qt and GTK.
fn role(role_name: &str, is_editable: bool) -> Option<&'static str> {
    Some(match role_name {
        "push button" | "toggle button" | "button" | "push button menu" => "button",
        "combo box" | "autocomplete" => "combobox",
        "check box" | "check menu item" | "switch" => "checkbox",
        "radio button" | "radio menu item" => "radio",
        "entry" | "password text" | "editbar" | "date editor" | "terminal" => "edit",
        "text" | "paragraph" => if is_editable { "edit" } else { "" },
        "spin button" | "spin box" => "spinner",
        "label" | "static" | "caption" => "",
        "link" => "link",
        "image" | "icon" => "image",
        "drawing area" | "canvas" => "canvas",
        "table" | "tree table" => "table",
        "list" | "list box" => "list",
        "tree" => "tree",
        "menu item" | "menu" => "menuitem",
        "page tab" | "tab" => "tab",
        "page tab list" => "tabs",
        "tool bar" => "toolbar",
        "scroll pane" => "pane",
        "slider" => "slider",
        "progress bar" | "level bar" => "progress",
        "list item" | "tree item" | "table row" | "table cell" | "flow box child" => "item",
        "document web" | "document frame" | "document text" | "document email" | "document presentation" | "document spreadsheet" => "document",
        "heading" | "table column header" | "column header" => "header",
        _ => return None,
    })
}

/// Parts that never help an agent, along with everything inside them.
fn skipped(role_name: &str) -> bool {
    matches!(role_name, "scroll bar" | "separator" | "tool tip")
}

fn acts(role: &str) -> bool {
    !matches!(role, "" | "image" | "progress" | "header" | "document" | "table" | "pane" | "tabs" | "toolbar" | "list" | "tree")
}

/// Whether `outer` encloses `inner` (with a small slack for border padding).
pub fn encloses(outer: Bounds, inner: Bounds) -> bool {
    if outer.2 <= 0 || outer.3 <= 0 || inner.2 <= 0 || inner.3 <= 0 {
        return false;
    }
    const SLACK: i64 = 4;
    let (ox0, oy0, ox1, oy1) = (outer.0 as i64, outer.1 as i64, outer.0 as i64 + outer.2 as i64, outer.1 as i64 + outer.3 as i64);
    let (ix0, iy0, ix1, iy1) = (inner.0 as i64, inner.1 as i64, inner.0 as i64 + inner.2 as i64, inner.1 as i64 + inner.3 as i64);
    ix0 >= ox0 - SLACK && iy0 >= oy0 - SLACK && ix1 <= ox1 + SLACK && iy1 <= oy1 + SLACK
}

/// Drop static text nodes whose text duplicates a named control enclosing them, returning
/// `(kept, dropped_count, quiet_rects)` so `blind_spot` still treats those rects as covered.
pub fn drop_labels(nodes: Vec<Node>) -> (Vec<Node>, usize, Vec<Bounds>) {
    let boxes: Vec<(Bounds, String)> = nodes
        .iter()
        .filter(|n| !n.parts.role.is_empty() && !n.parts.name.is_empty())
        .map(|n| (n.rect, n.parts.name.to_lowercase()))
        .collect();
    let mut kept = Vec::with_capacity(nodes.len());
    let mut quiet = Vec::new();
    let mut dropped = 0;
    for node in nodes {
        if node.parts.role.is_empty() {
            let lower = node.parts.name.to_lowercase();
            let duplicate = !lower.is_empty() && boxes.iter().any(|(b, name)| name == &lower && encloses(*b, node.rect));
            if duplicate {
                quiet.push(node.rect);
                dropped += 1;
                continue;
            }
        }
        kept.push(node);
    }
    (kept, dropped, quiet)
}

fn outside(rect: Bounds, window: Bounds) -> bool {
    if window.2 <= 0 || window.3 <= 0 {
        return false;
    }
    let (x, y, w, h) = (rect.0 as i64, rect.1 as i64, rect.2 as i64, rect.3 as i64);
    let (wx, wy, ww, wh) = (window.0 as i64, window.1 as i64, window.2 as i64, window.3 as i64);
    w <= 0 || h <= 0 || x + w <= wx || y + h <= wy || x >= wx + ww || y >= wy + wh
}

struct Walk<'a> {
    nodes: &'a mut Vec<Node>,
    hidden: &'a mut usize,
    window: Bounds,
    all: bool,
    seen: usize,
    visited: std::collections::HashSet<Obj>,
}

impl Walk<'_> {
    fn walk(&mut self, obj: &Obj, parent: &str, row: Option<usize>, depth: usize) {
        if depth > MAX_DEPTH || self.seen >= MAX_NODES || !self.visited.insert(obj.clone()) {
            return;
        }
        self.seen += 1;
        let role_name = role_name(obj);
        if skipped(&role_name) {
            return;
        }
        let state = states(obj);
        if has(state, DEFUNCT) {
            return;
        }
        // Only prune subtrees that are neither VISIBLE nor SHOWING: Chromium, Electron, Qt and GTK
        // containers routinely set VISIBLE without SHOWING while their child controls are SHOWING.
        if !self.all && depth > 0 && !has(state, VISIBLE) && !has(state, SHOWING) {
            return;
        }
        let need_ifaces = matches!(role_name.as_str(), "text" | "paragraph") || role(&role_name, false).is_some();
        let ifaces = if need_ifaces { interfaces(obj) } else { Vec::new() };
        let has_iface = |wanted: &str| ifaces.iter().any(|i| i == wanted);
        let is_editable = (has(state, EDITABLE) || has_iface("org.a11y.atspi.EditableText")) && !has(state, READ_ONLY);
        let role = role(&role_name, is_editable);
        let mut name = name(obj);
        if name.is_empty() {
            name = description(obj);
        }
        let value = match role {
            Some("edit") if has_iface("org.a11y.atspi.Text") => text_of(obj).map(|t| t.replace('\n', " ")),
            Some("document") if is_editable && has_iface("org.a11y.atspi.Text") => text_of(obj).filter(|t| !t.is_empty()).map(|t| t.replace('\n', " ")),
            Some("slider" | "progress" | "spinner") if has_iface("org.a11y.atspi.Value") => property::<f64>(obj, "org.a11y.atspi.Value", "CurrentValue").ok().map(|v| format!("{v}")),
            Some("spinner") if has_iface("org.a11y.atspi.Text") => text_of(obj).map(|t| t.replace('\n', " ")),
            Some("combobox") => {
                // The chosen entry, then whatever the box shows; a GTK box often names itself by it.
                selected_name(obj).or_else(|| text_of(obj).filter(|t| !t.is_empty())).or_else(|| (!name.is_empty()).then(|| name.clone()))
            }
            _ => None,
        };
        if role == Some("") && name.is_empty() && has_iface("org.a11y.atspi.Text") {
            name = text_of(obj).unwrap_or_default();
        }
        let rect = rect(obj).unwrap_or((0, 0, 0, 0));

        // Inside a row, plain text belongs to the row's own line.
        if let Some(at) = row
            && role.is_none_or(|r| !acts(r))
        {
            if !name.is_empty() && name != parent && !self.nodes[at].parts.cells.contains(&name) {
                if self.nodes[at].parts.name.is_empty() {
                    self.nodes[at].parts.name = name.clone();
                } else {
                    self.nodes[at].parts.cells.push(name.clone());
                }
            }
            for child in children(obj) {
                self.walk(&child, &name, row, depth + 1);
            }
            return;
        }

        let listed = match role {
            Some("") => !name.is_empty() && name != parent,
            Some("image") => !name.is_empty() && depth > 0,
            Some("pane") => role_name == "scroll pane" && depth > 0,
            Some("list" | "tree" | "tabs" | "toolbar") => !name.is_empty() && depth > 0,
            Some(_) => true,
            None => false,
        };
        let mut own = row;
        if listed {
            if !self.all && ((!has(state, SHOWING) && role != Some("pane")) || outside(rect, self.window)) {
                *self.hidden += 1;
            } else {
                let role = role.unwrap_or_default();
                let mut flags = Vec::new();
                if !has(state, ENABLED) && !has(state, SENSITIVE) && acts(role) {
                    flags.push("disabled");
                }
                match role {
                    "checkbox" | "radio" => flags.push(if has(state, CHECKED) { "on" } else if has(state, INDETERMINATE) { "mixed" } else { "off" }),
                    "button" if has(state, CHECKABLE) || role_name == "toggle button" => flags.push(if has(state, CHECKED) || has(state, PRESSED) { "on" } else { "off" }),
                    _ => {}
                }
                if role == "combobox" || has(state, EXPANDABLE) {
                    if has(state, EXPANDED) {
                        flags.push("open");
                    } else if has(state, COLLAPSED) || role == "combobox" {
                        flags.push("closed");
                    }
                }
                if has(state, SELECTED) && matches!(role, "item" | "tab" | "menuitem") {
                    flags.push("selected");
                }
                if has(state, FOCUSED) {
                    flags.push("focused");
                }
                if role_name == "password text" {
                    flags.push("password");
                }
                if role_name == "scroll pane" {
                    flags.push("scroll");
                }
                let value = if role_name == "password text" { None } else { value.clone() };
                let identity = format!("{role}|{name}");
                self.nodes.push(Node {
                    key: hash_of(&obj.bus, &obj.path),
                    identity,
                    parts: Parts { role: role.to_string(), name: name.clone(), value, states: flags, cells: Vec::new() },
                    rect,
                    element: obj.clone(),
                });
                if role == "item" {
                    own = Some(self.nodes.len() - 1);
                }
            }
        }
        // A collapsed combo box's list and an edit's text runs are read through the box itself;
        // an expanded combo box or editable paragraph (which can hold inline links) is descended into.
        if (role == Some("edit") && role_name != "paragraph") || (role == Some("combobox") && !has(state, EXPANDED)) {
            return;
        }
        let shown = if listed { name.as_str() } else { parent };
        for child in children(obj) {
            self.walk(&child, shown, own, depth + 1);
        }
    }
}

fn hash_of(bus: &str, path: &str) -> usize {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (bus, path).hash(&mut hasher);
    hasher.finish() as usize
}

/// Every control in the window (and any visible popup surfaces), and how many were left out for
/// being off screen.
pub fn read(frame: &Obj, window: Bounds, all: bool) -> (Vec<Node>, usize) {
    let mut nodes = Vec::new();
    let mut hidden = 0;
    let mut walk = Walk { nodes: &mut nodes, hidden: &mut hidden, window, all, seen: 0, visited: std::collections::HashSet::new() };
    for child in children(frame) {
        walk.walk(&child, "", None, 1);
    }
    (nodes, hidden)
}

/// Read controls from a main frame plus any additional popup/dialog frames of the same window.
pub fn read_surfaces(surfaces: &[Obj], window: Bounds, all: bool) -> (Vec<Node>, usize, Vec<Bounds>) {
    let mut nodes = Vec::new();
    let mut hidden = 0;
    let mut seen_keys = std::collections::HashSet::new();
    for (index, surface) in surfaces.iter().enumerate() {
        let bounds = if index == 0 { window } else { rect(surface).unwrap_or(window) };
        let (surface_nodes, surface_hidden) = read(surface, bounds, all);
        hidden += surface_hidden;
        for node in surface_nodes {
            if seen_keys.insert(node.key) {
                nodes.push(node);
            }
        }
    }
    let (kept, _, quiet) = drop_labels(nodes);
    (kept, hidden, quiet)
}

pub fn text_of(obj: &Obj) -> Option<String> {
    call::<_, String>(obj, "org.a11y.atspi.Text", "GetText", &(0i32, -1i32)).ok()
}

/// All of a control's text: its text, then its value, then its name.
pub fn full_text(obj: &Obj) -> String {
    if let Some(text) = text_of(obj).filter(|t| !t.is_empty()) {
        return text;
    }
    if let Ok(value) = property::<f64>(obj, "org.a11y.atspi.Value", "CurrentValue") {
        return format!("{value}");
    }
    name(obj)
}

/// The app's own actions for this control, such as click, press, toggle or activate.
fn actions(obj: &Obj) -> Vec<String> {
    call::<_, Vec<(String, String, String)>>(obj, "org.a11y.atspi.Action", "GetActions", &()).map(|list| list.into_iter().map(|a| a.0).collect()).unwrap_or_default()
}

/// Perform the first of these actions the control has.
pub fn perform(obj: &Obj, wanted: &[&str]) -> Result<String, String> {
    let have = actions(obj);
    let at = wanted
        .iter()
        .find_map(|w| have.iter().position(|a| a.eq_ignore_ascii_case(w)))
        .or_else(|| (!have.is_empty() && wanted.contains(&"*")).then_some(0))
        .ok_or_else(|| if have.is_empty() { "that control has no action".to_string() } else { format!("that control can only {}", have.join(", ")) })?;
    let done: bool = call(obj, "org.a11y.atspi.Action", "DoAction", &(at as i32))?;
    if done { Ok(have[at].clone()) } else { Err(format!("the app refused {}", have[at])) }
}

pub fn press(obj: &Obj) -> Result<String, String> {
    perform(obj, &["click", "press", "activate", "toggle", "jump", "select", "*"])
}

pub fn editable(obj: &Obj) -> bool {
    interfaces(obj).iter().any(|i| i == "org.a11y.atspi.EditableText")
}

pub fn set_text(obj: &Obj, text: &str) -> Result<(), String> {
    let done: bool = call(obj, "org.a11y.atspi.EditableText", "SetTextContents", &(text,))?;
    if done { Ok(()) } else { Err("the app refused the text".into()) }
}

/// Add text to the end of a field, as typing into it does on Windows and macOS, and leave the
/// caret after it.
pub fn insert_text(obj: &Obj, text: &str) -> Result<(), String> {
    let bytes = i32::try_from(text.len()).map_err(|_| "that text is too long to insert at once".to_string())?;
    let end: i32 = property(obj, "org.a11y.atspi.Text", "CharacterCount").unwrap_or(0).max(0);
    // The length is in bytes: GTK hands it to gtk_editable_insert_text, which counts bytes.
    let done: bool = call(obj, "org.a11y.atspi.EditableText", "InsertText", &(end, text, bytes))?;
    if !done {
        return Err("the app refused the text".into());
    }
    let count = i32::try_from(text.chars().count()).unwrap_or(i32::MAX);
    let _ = call::<_, bool>(obj, "org.a11y.atspi.Text", "SetCaretOffset", &(end.saturating_add(count),));
    Ok(())
}

/// Append text to an editable control in the background: tries `EditableText.InsertText` first,
/// then falls back to `SetTextContents(existing + text)` for toolkits (Qt, LibreOffice, Java ATK,
/// Electron) that implement `SetTextContents` without `InsertText`.
pub fn append_text(obj: &Obj, text: &str) -> Result<(), String> {
    if insert_text(obj, text).is_ok() {
        return Ok(());
    }
    let Some(existing) = text_of(obj) else {
        return Err("could not read existing text to append safely".into());
    };
    set_text(obj, &(existing + text))
}

/// Read the first active selection range `(start, end)` from `org.a11y.atspi.Text`.
fn selection_range(obj: &Obj) -> Option<(i32, i32)> {
    let n: i32 = call(obj, "org.a11y.atspi.Text", "GetNSelections", &()).ok()?;
    if n <= 0 {
        return None;
    }
    let (start, end): (i32, i32) = call(obj, "org.a11y.atspi.Text", "GetSelection", &(0i32,)).ok()?;
    let (lo, hi) = if start <= end { (start, end) } else { (end, start) };
    (hi > lo).then_some((lo, hi))
}

/// Currently selected text in `obj` via `org.a11y.atspi.Text`, with no focus or shortcut needed.
pub fn selected_text(obj: &Obj) -> Option<String> {
    let (lo, hi) = selection_range(obj)?;
    let text: String = call(obj, "org.a11y.atspi.Text", "GetText", &(lo, hi)).ok()?;
    (!text.is_empty()).then_some(text)
}

/// Select all text in `obj` via `org.a11y.atspi.Text` without touching keyboard focus.
pub fn select_all(obj: &Obj) -> bool {
    let Ok(count) = property::<i32>(obj, "org.a11y.atspi.Text", "CharacterCount") else { return false };
    if count <= 0 {
        return false;
    }
    let replaced = call::<_, bool>(obj, "org.a11y.atspi.Text", "SetSelection", &(0i32, 0i32, count)).unwrap_or(false);
    if replaced {
        return true;
    }
    call::<_, bool>(obj, "org.a11y.atspi.Text", "AddSelection", &(0i32, count)).unwrap_or(false)
}

/// Delete the active selection in `obj` and return the cut text, completely in the background.
pub fn delete_selection(obj: &Obj) -> Option<String> {
    let (lo, hi) = selection_range(obj)?;
    let text: String = call(obj, "org.a11y.atspi.Text", "GetText", &(lo, hi)).ok()?;
    if text.is_empty() {
        return None;
    }
    let deleted: bool = call(obj, "org.a11y.atspi.EditableText", "DeleteText", &(lo, hi)).ok()?;
    deleted.then_some(text)
}

/// Insert `text` at the current caret (replacing any active selection) via `EditableText`,
/// leaving the window completely in the background.
pub fn insert_at_caret(obj: &Obj, text: &str) -> Result<(), String> {
    if !editable(obj) {
        return Err("that control is not editable".into());
    }
    let bytes = i32::try_from(text.len()).map_err(|_| "that text is too long to insert at once".to_string())?;
    let count: i32 = property(obj, "org.a11y.atspi.Text", "CharacterCount").unwrap_or(0).max(0);
    let pos = if let Some((lo, hi)) = selection_range(obj) {
        let _ = call::<_, bool>(obj, "org.a11y.atspi.EditableText", "DeleteText", &(lo, hi));
        lo.clamp(0, count)
    } else {
        property::<i32>(obj, "org.a11y.atspi.Text", "CaretOffset").unwrap_or(count).clamp(0, count)
    };
    let done: bool = call(obj, "org.a11y.atspi.EditableText", "InsertText", &(pos, text, bytes))?;
    if !done {
        return append_text(obj, text);
    }
    let chars = i32::try_from(text.chars().count()).unwrap_or(i32::MAX);
    let _ = call::<_, bool>(obj, "org.a11y.atspi.Text", "SetCaretOffset", &(pos.saturating_add(chars),));
    Ok(())
}

pub fn set_value(obj: &Obj, value: f64) -> Result<(), String> {
    set_property(obj, "org.a11y.atspi.Value", "CurrentValue", zbus::zvariant::Value::from(value))
}

pub fn has_value(obj: &Obj) -> bool {
    interfaces(obj).iter().any(|i| i == "org.a11y.atspi.Value")
}

/// True if the control already has the keyboard focus inside its app.
pub fn focused(obj: &Obj) -> bool {
    has(states(obj), FOCUSED)
}

pub fn focus(obj: &Obj) -> Result<(), String> {
    let done: bool = call(obj, "org.a11y.atspi.Component", "GrabFocus", &())?;
    if done { Ok(()) } else { Err("that control would not take focus".into()) }
}

fn parent(obj: &Obj) -> Option<Obj> {
    let (bus, path): (String, OwnedObjectPath) = property(obj, ACCESSIBLE, "Parent").ok()?;
    (path.as_str() != "/org/a11y/atspi/null" && path.as_str() != "/").then(|| Obj { bus, path: path.as_str().to_string() })
}

fn value_of(obj: &Obj, name: &str) -> Option<f64> {
    property(obj, "org.a11y.atspi.Value", name).ok()
}

/// A scroll bar of this pane that runs this way and can be moved by its value.
fn bar_of(pane: &Obj, vertical: bool) -> Option<Obj> {
    children(pane).into_iter().find(|child| {
        role_name(child) == "scroll bar" && has(states(child), if vertical { VERTICAL } else { HORIZONTAL }) && has_value(child)
    })
}

/// The first pane with such a scroll bar, looking a little way into this control, breadth first.
fn bar_inside(obj: &Obj, vertical: bool) -> Option<Obj> {
    const LOOKED_AT_MOST: usize = 120;
    let mut queue = std::collections::VecDeque::from([obj.clone()]);
    let mut visited = std::collections::HashSet::from([obj.clone()]);
    let mut looked = 0;
    while let Some(here) = queue.pop_front() {
        looked += 1;
        if looked > LOOKED_AT_MOST {
            return None;
        }
        if let Some(bar) = bar_of(&here, vertical) {
            return Some(bar);
        }
        for child in children(&here) {
            if visited.insert(child.clone()) {
                queue.push_back(child);
            }
        }
    }
    None
}

/// Scroll by moving a scroll bar's value, which needs no pointer and no focus: the bar of this
/// control or of the nearest pane around it, or the first one inside it, as for a whole window.
/// Positive notches scroll down or right, each a tenth of the way or one step, whichever is more.
pub fn scroll(obj: &Obj, vertical: bool, notches: i32) -> Result<(), String> {
    let mut around = Some(obj.clone());
    let mut bar = None;
    for _ in 0..5 {
        let Some(here) = around else { break };
        bar = bar_of(&here, vertical);
        if bar.is_some() {
            break;
        }
        around = parent(&here);
    }
    let bar = bar.or_else(|| bar_inside(obj, vertical)).ok_or("no scroll bar here")?;
    let (Some(now), Some(low), Some(high)) = (value_of(&bar, "CurrentValue"), value_of(&bar, "MinimumValue"), value_of(&bar, "MaximumValue")) else {
        return Err("that scroll bar does not say where it is".into());
    };
    if !now.is_finite() || !low.is_finite() || !high.is_finite() || low > high {
        return Err("that scroll bar reported invalid bounds".into());
    }
    let inc = value_of(&bar, "MinimumIncrement").filter(|v| v.is_finite() && *v >= 0.0).unwrap_or(0.0);
    let step = ((high - low) / 10.0).max(inc);
    let target = now + step * notches as f64;
    let wanted = if target.is_finite() { target.clamp(low, high) } else if notches > 0 { high } else { low };
    set_value(&bar, wanted)?;
    if (value_of(&bar, "CurrentValue").unwrap_or(now) - now).abs() < f64::EPSILON && wanted != now {
        return Err("that scroll bar did not move".into());
    }
    Ok(())
}

/// Scroll the control into view, where the toolkit supports it.
pub fn show(obj: &Obj) -> Result<(), String> {
    const ANYWHERE: u32 = 4;
    let done: bool = call(obj, "org.a11y.atspi.Component", "ScrollTo", &(ANYWHERE,))?;
    if done { Ok(()) } else { Err("that control cannot scroll into view".into()) }
}

/// A descendant whose name is this, looking a few levels down: an entry in a list or menu.
pub fn find_named(obj: &Obj, wanted: &str, depth: usize) -> Option<Obj> {
    const MAX_FIND_VISITED: usize = 1000;
    let wanted = wanted.to_lowercase();
    let mut level = vec![obj.clone()];
    let mut visited = std::collections::HashSet::from([obj.clone()]);
    for _ in 0..depth {
        let mut next = Vec::new();
        for parent in &level {
            for child in children(parent) {
                if !visited.insert(child.clone()) {
                    continue;
                }
                let own = name(&child).to_lowercase();
                if own == wanted || (!own.is_empty() && own.contains(&wanted) && wanted.len() > 2) {
                    return Some(child);
                }
                if visited.len() >= MAX_FIND_VISITED {
                    return None;
                }
                next.push(child);
            }
        }
        if next.is_empty() {
            break;
        }
        level = next;
    }
    None
}

/// The name of the entry a combo box or list has chosen.
pub fn selected_name(obj: &Obj) -> Option<String> {
    let (bus, path) = call::<_, (String, OwnedObjectPath)>(obj, "org.a11y.atspi.Selection", "GetSelectedChild", &(0i32,)).ok()?;
    let chosen = Obj { bus, path: path.as_str().to_string() };
    (chosen.path != "/org/a11y/atspi/null").then(|| name(&chosen)).filter(|n| !n.is_empty())
}

/// Choose a combo box's entry by name: GTK's box selects by the entry's place in its list.
pub fn choose(combo: &Obj, wanted: &str) -> Result<(), String> {
    let lower = wanted.to_lowercase();
    let mut lists = vec![combo.clone()];
    lists.extend(children(combo));
    for list in &lists {
        let entries = children(list);
        if let Some(at) = entries.iter().position(|e| name(e).to_lowercase() == lower) {
            let done: bool = call(combo, "org.a11y.atspi.Selection", "SelectChild", &(at as i32))?;
            if done && selected_name(combo).is_some_and(|n| n.to_lowercase() == lower) {
                return Ok(());
            }
        }
    }
    Err(format!("no entry named '{wanted}'"))
}

/// Select a child of a list or combo box through the Selection interface.
pub fn select_child(parent: &Obj, child: &Obj) -> Result<(), String> {
    let index: i32 = call(child, ACCESSIBLE, "GetIndexInParent", &())?;
    let mut holder = parent.clone();
    // The entry's own parent holds the selection: a combo box's menu, or the list itself.
    if let Ok((bus, path)) = property::<(String, OwnedObjectPath)>(child, ACCESSIBLE, "Parent") {
        holder = Obj { bus, path: path.as_str().to_string() };
    }
    let done: bool = call(&holder, "org.a11y.atspi.Selection", "SelectChild", &(index,))?;
    if done { Ok(()) } else { Err("the app refused that entry".into()) }
}

/// A table's cells as text: the rows asked for, and how many there are in all.
pub struct Table {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub total_rows: usize,
    pub total_columns: usize,
}

const TABLE: &str = "org.a11y.atspi.Table";

fn object(reference: (String, OwnedObjectPath)) -> Option<Obj> {
    let (bus, path) = reference;
    (path.as_str() != "/org/a11y/atspi/null").then(|| Obj { bus, path: path.as_str().to_string() })
}

/// Rows `skip` onward of the table this control is, or is in, as GTK's and Qt's tables and
/// LibreOffice's sheets give them: a cell or a row names its table too. A list with no table
/// behind it gives one column, a row per item.
pub fn table(obj: &Obj, skip: usize, rows: usize, columns: usize) -> Result<Table, String> {
    let mut owner = Some(obj.clone());
    for _ in 0..4 {
        match &owner {
            Some(at) if interfaces(at).iter().any(|i| i == TABLE) => break,
            Some(at) => owner = parent(at),
            None => break,
        }
    }
    let Some(owner) = owner.filter(|o| interfaces(o).iter().any(|i| i == TABLE)) else {
        let items: Vec<Obj> = children(obj);
        let total_rows = items.len();
        let rows = items.iter().skip(skip).take(rows).map(|item| {
            let cells = children(item);
            if cells.is_empty() { vec![full_text(item)] } else { cells.iter().take(columns).map(full_text).collect() }
        });
        let rows: Vec<Vec<String>> = rows.collect();
        let total_columns = rows.iter().map(Vec::len).max().unwrap_or(0);
        return Ok(Table { headers: Vec::new(), rows, total_rows, total_columns });
    };
    let total_rows = property::<i32>(&owner, TABLE, "NRows")?.max(0) as usize;
    let total_columns = property::<i32>(&owner, TABLE, "NColumns")?.max(0) as usize;
    let mut headers = Vec::new();
    for column in 0..total_columns.min(columns) as i32 {
        let header = call::<_, (String, OwnedObjectPath)>(&owner, TABLE, "GetColumnHeader", &(column,)).ok().and_then(object).map(|h| full_text(&h)).unwrap_or_default();
        let header = if header.is_empty() { call::<_, String>(&owner, TABLE, "GetColumnDescription", &(column,)).unwrap_or_default() } else { header };
        headers.push(header);
    }
    if headers.iter().all(String::is_empty) {
        headers.clear();
    }
    let mut cells = Vec::new();
    for row in skip.min(total_rows)..total_rows.min(skip.saturating_add(rows)) {
        let mut line = Vec::new();
        for column in 0..total_columns.min(columns) {
            let cell = call::<_, (String, OwnedObjectPath)>(&owner, TABLE, "GetAccessibleAt", &(row as i32, column as i32)).ok().and_then(object);
            line.push(cell.map(|c| full_text(&c)).unwrap_or_default());
        }
        cells.push(line);
    }
    Ok(Table { headers, rows: cells, total_rows, total_columns })
}

/// Live checked state (`"on"`, `"mixed"`, `"off"`) of a checkbox, radio or toggle button.
pub fn toggle_state(obj: &Obj, role: &str) -> Result<Option<&'static str>, String> {
    let state = try_states(obj)?;
    Ok(match role {
        "checkbox" | "radio" => Some(if has(state, CHECKED) { "on" } else if has(state, INDETERMINATE) { "mixed" } else { "off" }),
        "button" if has(state, CHECKABLE) || role_name(obj) == "toggle button" => {
            Some(if has(state, CHECKED) || has(state, PRESSED) { "on" } else { "off" })
        }
        _ => None,
    })
}

/// Live expanded state (`"open"` or `"closed"`) of an expandable control or combo box.
pub fn expand_state(obj: &Obj, role: &str) -> Result<Option<&'static str>, String> {
    let state = try_states(obj)?;
    if role == "combobox" || has(state, EXPANDABLE) {
        if has(state, EXPANDED) {
            return Ok(Some("open"));
        }
        if has(state, COLLAPSED) || role == "combobox" {
            return Ok(Some("closed"));
        }
    }
    Ok(None)
}

/// Where a field's caret is on the screen, as its top left and height, after the last character
/// when it is at the end.
pub fn caret(obj: &Obj) -> Option<(i32, i32, i32)> {
    const TEXT: &str = "org.a11y.atspi.Text";
    let offset: i32 = property(obj, TEXT, "CaretOffset").ok()?;
    let count: i32 = property(obj, TEXT, "CharacterCount").unwrap_or(0);
    let extents = |at: i32| {
        call::<_, (i32, i32, i32, i32)>(obj, TEXT, "GetCharacterExtents", &(at, SCREEN))
            .ok()
            .filter(|e| e.0 != i32::MIN && e.1 != i32::MIN && e.3 > 0)
    };
    if offset >= 0
        && offset < count
        && let Some((x, y, _, h)) = extents(offset)
    {
        return Some((x, y, h));
    }
    if let Some((x, y, w, h)) = extents(offset.saturating_sub(1).max(0)).filter(|_| count > 0) {
        return Some((x.saturating_add(w), y, h));
    }
    // An empty field: the start of its box, a little in.
    let (x, y, _, h) = rect(obj).filter(|r| r.0 != i32::MIN && r.1 != i32::MIN)?;
    (h > 4).then_some((x.saturating_add(3), y.saturating_add(3), h - 6))
}

/// Whether this is the button a dialog presses when its Enter is pressed, as Open or Save is.
pub fn is_default(obj: &Obj) -> bool {
    has(states(obj), IS_DEFAULT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(key: usize, role: &str, name: &str, rect: Bounds) -> Node {
        Node {
            key,
            identity: format!("{role}|{name}"),
            parts: Parts { role: role.to_string(), name: name.to_string(), value: None, states: Vec::new(), cells: Vec::new() },
            rect,
            element: Obj { bus: ":1.1".into(), path: format!("/o/{key}") },
        }
    }

    #[test]
    fn text_role_distinguishes_editable_from_static() {
        assert_eq!(role("text", true), Some("edit"));
        assert_eq!(role("text", false), Some(""));
        assert_eq!(role("paragraph", true), Some("edit"));
        assert_eq!(role("paragraph", false), Some(""));
        assert_eq!(role("spin button", false), Some("spinner"));
        assert_eq!(role("spin box", false), Some("spinner"));
        assert_eq!(role("autocomplete", false), Some("combobox"));
        assert_eq!(role("flow box child", false), Some("item"));
        assert_eq!(role("drawing area", false), Some("canvas"));
        assert_eq!(role("document text", true), Some("document"));
        assert_eq!(role("document spreadsheet", false), Some("document"));
        assert_eq!(role("scroll pane", false), Some("pane"));
        assert!(!skipped("menu bar"));
        assert!(skipped("scroll bar"));
    }

    #[test]
    fn duplicate_static_label_inside_button_is_dropped() {
        let nodes = vec![
            node(1, "button", "Save", (10, 10, 80, 30)),
            node(2, "", "Save", (20, 15, 40, 20)),
            node(3, "", "Status: ready", (10, 60, 120, 20)),
        ];
        let (kept, dropped, quiet) = drop_labels(nodes);
        assert_eq!(dropped, 1);
        assert_eq!(quiet, vec![(20, 15, 40, 20)]);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].parts.role, "button");
        assert_eq!(kept[1].parts.name, "Status: ready");
    }

    #[test]
    fn encloses_handles_extreme_bounds_without_overflow() {
        assert!(encloses((0, 0, 100, 50), (10, 10, 40, 20)));
        assert!(encloses((i32::MIN, i32::MIN, 100, 100), (i32::MIN + 2, i32::MIN + 2, 10, 10)));
        assert!(encloses((i32::MAX - 50, i32::MAX - 50, 50, 50), (i32::MAX - 40, i32::MAX - 40, 30, 30)));
        assert!(!encloses((i32::MAX - 10, 0, 10, 10), (0, 0, 10, 10)));
    }
}
