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

// AtspiStateType, bit numbers in GetState's two words.
const ENABLED: u32 = 8;
const CHECKED: u32 = 4;
const COLLAPSED: u32 = 5;
const EXPANDABLE: u32 = 9;
const EXPANDED: u32 = 10;
const FOCUSED: u32 = 12;
const SELECTED: u32 = 23;
const SENSITIVE: u32 = 24;
const SHOWING: u32 = 25;
const VISIBLE: u32 = 30;
const INDETERMINATE: u32 = 32;
const CHECKABLE: u32 = 41;

/// One object on the accessibility bus.
#[derive(Clone, Debug, PartialEq)]
pub struct Obj {
    pub bus: String,
    pub path: String,
}

pub struct Node {
    pub key: usize,
    pub identity: String,
    pub parts: Parts,
    pub rect: Bounds,
    pub element: Obj,
}

static BUS: OnceLock<Result<Connection, String>> = OnceLock::new();

/// The accessibility bus, found through the session bus. Apps started after this see that
/// someone is listening, and turn their accessibility on.
pub fn bus() -> Result<&'static Connection, String> {
    BUS.get_or_init(|| {
        let session = Connection::session().map_err(|e| format!("no D-Bus session ({e}): start one with dbus-run-session, or run inside a desktop session"))?;
        let address: String = session
            .call_method(Some("org.a11y.Bus"), "/org/a11y/bus", Some("org.a11y.Bus"), "GetAddress", &())
            .and_then(|m| m.body().deserialize())
            .map_err(|e| format!("no accessibility bus ({e}): install at-spi2-core"))?;
        let _ = session.call_method(
            Some("org.a11y.Bus"),
            "/org/a11y/bus",
            Some("org.freedesktop.DBus.Properties"),
            "Set",
            &("org.a11y.Status", "IsEnabled", zbus::zvariant::Value::from(true)),
        );
        zbus::blocking::connection::Builder::address(address.as_str())
            .and_then(|b| b.build())
            .map_err(|e| format!("could not join the accessibility bus: {e}"))
    })
    .as_ref()
    .map_err(Clone::clone)
}

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

fn states(obj: &Obj) -> u64 {
    let words: Vec<u32> = call(obj, ACCESSIBLE, "GetState", &()).unwrap_or_default();
    words.first().copied().unwrap_or(0) as u64 | (words.get(1).copied().unwrap_or(0) as u64) << 32
}

fn has(states: u64, bit: u32) -> bool {
    states & (1u64 << bit) != 0
}

/// Where the object is on screen, in X root pixels.
pub fn rect(obj: &Obj) -> Option<Bounds> {
    call::<_, (i32, i32, i32, i32)>(obj, "org.a11y.atspi.Component", "GetExtents", &(SCREEN,)).ok()
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

/// The app's top-level windows on the accessibility bus, with the process each belongs to.
pub fn frames() -> Vec<(Obj, u32)> {
    let root = Obj { bus: ROOT_BUS.into(), path: ROOT_PATH.into() };
    let mut out = Vec::new();
    for app in children(&root) {
        let Some(pid) = pid_of(&app.bus) else { continue };
        for frame in children(&app) {
            out.push((frame, pid));
        }
    }
    out
}

/// The accessible window that is this X window: same process, then the same title or place.
pub fn frame_for(pid: u32, title: &str, at: Bounds) -> Option<Obj> {
    let mine: Vec<Obj> = frames().into_iter().filter(|(_, p)| *p == pid).map(|(f, _)| f).collect();
    let showing: Vec<&Obj> = mine.iter().filter(|f| has(states(f), SHOWING) || has(states(f), VISIBLE)).collect();
    if let Some(same) = showing.iter().find(|f| !title.is_empty() && name(f) == title) {
        return Some((*same).clone());
    }
    let near = |f: &&&Obj| rect(f).is_some_and(|r| (r.0 - at.0).abs() < 60 && (r.1 - at.1).abs() < 60 && (r.2 - at.2).abs() < 60);
    if let Some(placed) = showing.iter().find(near) {
        return Some((*placed).clone());
    }
    match showing.as_slice() {
        [only] => Some((*only).clone()),
        _ => mine.into_iter().next(),
    }
}

/// A role for an AT-SPI role name, or None for containers that only matter for what they hold.
fn role(role_name: &str) -> Option<&'static str> {
    Some(match role_name {
        "push button" | "toggle button" | "button" | "push button menu" => "button",
        "combo box" => "combobox",
        "check box" | "check menu item" | "switch" => "checkbox",
        "radio button" | "radio menu item" => "radio",
        "text" | "entry" | "password text" | "editbar" | "spin button" | "date editor" | "terminal" => "edit",
        "label" | "static" | "caption" | "paragraph" => "",
        "link" => "link",
        "image" | "icon" => "image",
        "menu item" | "menu" => "menuitem",
        "page tab" => "tab",
        "slider" => "slider",
        "progress bar" | "level bar" => "progress",
        "list item" | "tree item" | "table row" | "table cell" => "item",
        "document web" | "document frame" | "document text" => "document",
        "heading" | "table column header" | "column header" => "header",
        _ => return None,
    })
}

/// Parts that never help an agent, along with everything inside them.
fn skipped(role_name: &str) -> bool {
    matches!(role_name, "scroll bar" | "separator" | "tool tip" | "menu bar")
}

fn acts(role: &str) -> bool {
    !matches!(role, "" | "image" | "progress" | "header" | "document")
}

fn outside(rect: Bounds, window: Bounds) -> bool {
    let (x, y, w, h) = rect;
    let (wx, wy, ww, wh) = window;
    w <= 0 || h <= 0 || x + w <= wx || y + h <= wy || x >= wx + ww || y >= wy + wh
}

struct Walk<'a> {
    nodes: &'a mut Vec<Node>,
    hidden: &'a mut usize,
    window: Bounds,
    all: bool,
    seen: usize,
}

impl Walk<'_> {
    fn walk(&mut self, obj: &Obj, parent: &str, row: Option<usize>, depth: usize) {
        if depth > MAX_DEPTH || self.seen >= MAX_NODES {
            return;
        }
        self.seen += 1;
        let role_name = role_name(obj);
        if skipped(&role_name) {
            return;
        }
        let state = states(obj);
        if !self.all && depth > 0 && !has(state, SHOWING) {
            return;
        }
        let role = role(&role_name);
        let mut name = name(obj);
        if name.is_empty() {
            name = description(obj);
        }
        let ifaces = if role.is_some() { interfaces(obj) } else { Vec::new() };
        let has_iface = |wanted: &str| ifaces.iter().any(|i| i == wanted);
        let value = match role {
            Some("edit") if has_iface("org.a11y.atspi.Text") => text_of(obj).map(|t| t.replace('\n', " ")),
            Some("slider" | "progress") if has_iface("org.a11y.atspi.Value") => property::<f64>(obj, "org.a11y.atspi.Value", "CurrentValue").ok().map(|v| format!("{v}")),
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
            Some(_) => true,
            None => false,
        };
        let mut own = row;
        if listed {
            if outside(rect, self.window) && !self.all {
                *self.hidden += 1;
            } else {
                let role = role.unwrap_or_default();
                let mut flags = Vec::new();
                if !has(state, ENABLED) && !has(state, SENSITIVE) && acts(role) {
                    flags.push("disabled");
                }
                match role {
                    "checkbox" | "radio" => flags.push(if has(state, CHECKED) { "on" } else if has(state, INDETERMINATE) { "mixed" } else { "off" }),
                    "button" if has(state, CHECKABLE) || role_name == "toggle button" => flags.push(if has(state, CHECKED) { "on" } else { "off" }),
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
        // A combo box's list and an edit's text runs are read through the box itself.
        if matches!(role, Some("combobox" | "edit")) {
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

/// Every control in the window, and how many were left out for being off screen.
pub fn read(frame: &Obj, window: Bounds, all: bool) -> (Vec<Node>, usize) {
    let mut nodes = Vec::new();
    let mut hidden = 0;
    let mut walk = Walk { nodes: &mut nodes, hidden: &mut hidden, window, all, seen: 0 };
    for child in children(frame) {
        walk.walk(&child, "", None, 1);
    }
    (nodes, hidden)
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

/// Insert text where the caret is in a focused field, or at the end of any other.
pub fn insert_text(obj: &Obj, text: &str) -> Result<(), String> {
    let caret: i32 = property(obj, "org.a11y.atspi.Text", "CaretOffset").unwrap_or(-1);
    let end: i32 = property(obj, "org.a11y.atspi.Text", "CharacterCount").unwrap_or(0);
    let at = if caret >= 0 && has(states(obj), FOCUSED) { caret } else { end };
    let done: bool = call(obj, "org.a11y.atspi.EditableText", "InsertText", &(at, text, text.chars().count() as i32))?;
    if done { Ok(()) } else { Err("the app refused the text".into()) }
}

pub fn set_value(obj: &Obj, value: f64) -> Result<(), String> {
    set_property(obj, "org.a11y.atspi.Value", "CurrentValue", zbus::zvariant::Value::from(value))
}

pub fn has_value(obj: &Obj) -> bool {
    interfaces(obj).iter().any(|i| i == "org.a11y.atspi.Value")
}

pub fn focus(obj: &Obj) -> Result<(), String> {
    let done: bool = call(obj, "org.a11y.atspi.Component", "GrabFocus", &())?;
    if done { Ok(()) } else { Err("that control would not take focus".into()) }
}

/// Scroll the control into view, where the toolkit supports it.
pub fn show(obj: &Obj) -> Result<(), String> {
    const ANYWHERE: u32 = 4;
    let done: bool = call(obj, "org.a11y.atspi.Component", "ScrollTo", &(ANYWHERE,))?;
    if done { Ok(()) } else { Err("that control cannot scroll into view".into()) }
}

/// A descendant whose name is this, looking a few levels down: an entry in a list or menu.
pub fn find_named(obj: &Obj, wanted: &str, depth: usize) -> Option<Obj> {
    let wanted = wanted.to_lowercase();
    let mut level = vec![obj.clone()];
    for _ in 0..depth {
        let mut next = Vec::new();
        for parent in &level {
            for child in children(parent) {
                let own = name(&child).to_lowercase();
                if own == wanted || (!own.is_empty() && own.contains(&wanted) && wanted.len() > 2) {
                    return Some(child);
                }
                next.push(child);
            }
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
