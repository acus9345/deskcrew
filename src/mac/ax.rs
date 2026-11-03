// Reading and acting on a Mac window through the accessibility API: the same short lines the
// Windows side builds from UI Automation.
use super::ffi::*;
use crate::lines::Parts;
use std::ffi::c_void;
use std::ptr::null;

const MAX_DEPTH: usize = 40;
const MAX_NODES: usize = 4000;
const TIMEOUT_S: f32 = 1.5;

// The order matters: values come back in this order.
const ATTRIBUTES: [&str; 12] = [
    "AXRole",
    "AXSubrole",
    "AXTitle",
    "AXDescription",
    "AXValue",
    "AXPosition",
    "AXSize",
    "AXEnabled",
    "AXSelected",
    "AXExpanded",
    "AXIdentifier",
    "AXChildren",
];

pub type Bounds = (i32, i32, i32, i32);

pub struct Node {
    pub key: usize,
    pub identity: String,
    pub parts: Parts,
    pub rect: Bounds,
    pub ax_role: String,
    pub element: Owned,
}

/// Whether this process may use the accessibility API. With `ask`, macOS shows its own prompt.
pub fn trusted(ask: bool) -> bool {
    unsafe {
        if !ask {
            return AXIsProcessTrusted() != 0;
        }
        let options = dictionary_of(&[kAXTrustedCheckOptionPrompt], &[kCFBooleanTrue]);
        AXIsProcessTrustedWithOptions(options.0) != 0
    }
}

pub fn application(pid: i32) -> Owned {
    let app = Owned(unsafe { AXUIElementCreateApplication(pid) });
    unsafe { AXUIElementSetMessagingTimeout(app.0, TIMEOUT_S) };
    app
}

pub fn attribute(element: &Owned, name: &str) -> Option<Owned> {
    let key = cf_string(name);
    let mut value: CFTypeRef = null();
    let error = unsafe { AXUIElementCopyAttributeValue(element.0, key.0, &mut value) };
    if error == kAXErrorSuccess { Owned::new(value) } else { None }
}

pub fn text(element: &Owned, name: &str) -> Option<String> {
    attribute(element, name).and_then(|value| string_of(value.0))
}

pub fn settable(element: &Owned, name: &str) -> bool {
    let key = cf_string(name);
    let mut yes = 0u8;
    unsafe { AXUIElementIsAttributeSettable(element.0, key.0, &mut yes) == kAXErrorSuccess && yes != 0 }
}

pub fn set(element: &Owned, name: &str, value: CFTypeRef) -> Result<(), String> {
    let key = cf_string(name);
    match unsafe { AXUIElementSetAttributeValue(element.0, key.0, value) } {
        error if error == kAXErrorSuccess => Ok(()),
        error => Err(format!("the app refused to change {name} (error {error})")),
    }
}

pub fn set_text(element: &Owned, text: &str) -> Result<(), String> {
    let value = cf_string(text);
    set(element, "AXValue", value.0)
}

pub fn set_bool(element: &Owned, name: &str, value: bool) -> Result<(), String> {
    set(element, name, unsafe { if value { kCFBooleanTrue } else { kCFBooleanFalse } })
}

/// Move a window, in top-left points.
pub fn set_position(element: &Owned, x: i32, y: i32) -> Result<(), String> {
    let point = CGPoint { x: x as f64, y: y as f64 };
    let value = Owned::new(unsafe { AXValueCreate(kAXValueCGPointType, (&point as *const CGPoint).cast()) }).ok_or("macOS would not make that position")?;
    set(element, "AXPosition", value.0)
}

pub fn set_size(element: &Owned, w: i32, h: i32) -> Result<(), String> {
    let size = CGSize { width: w as f64, height: h as f64 };
    let value = Owned::new(unsafe { AXValueCreate(kAXValueCGSizeType, (&size as *const CGSize).cast()) }).ok_or("macOS would not make that size")?;
    set(element, "AXSize", value.0)
}

/// An element's children, such as the items of an open menu.
pub fn children(element: &Owned) -> Vec<Owned> {
    attribute(element, "AXChildren").map(|list| items(list.0).into_iter().filter_map(Owned::retain).collect()).unwrap_or_default()
}

/// The first element under this one, at most `depth` levels down, with a matching title or value.
pub fn find_named(element: &Owned, wanted: &str, depth: usize) -> Option<Owned> {
    let wanted = wanted.to_lowercase();
    let mut level = vec![element.clone()];
    for _ in 0..depth {
        let mut next = Vec::new();
        for parent in &level {
            for child in children(parent) {
                let name = full_text(&child).to_lowercase();
                if name == wanted {
                    return Some(child);
                }
                next.push(child);
            }
        }
        level = next;
    }
    None
}

pub fn focus(element: &Owned) -> Result<(), String> {
    set(element, "AXFocused", unsafe { kCFBooleanTrue })
}

pub fn perform(element: &Owned, action: &str) -> Result<(), String> {
    let name = cf_string(action);
    match unsafe { AXUIElementPerformAction(element.0, name.0) } {
        error if error == kAXErrorSuccess => Ok(()),
        error => Err(format!("the app refused {action} (error {error})")),
    }
}

fn point_of(value: CFTypeRef) -> Option<CGPoint> {
    let mut point = CGPoint::default();
    unsafe { (!value.is_null() && CFGetTypeID(value) == AXValueGetTypeID() && AXValueGetValue(value, kAXValueCGPointType, (&mut point as *mut CGPoint).cast::<c_void>()) != 0).then_some(point) }
}

fn size_of_value(value: CFTypeRef) -> Option<CGSize> {
    let mut size = CGSize::default();
    unsafe { (!value.is_null() && CFGetTypeID(value) == AXValueGetTypeID() && AXValueGetValue(value, kAXValueCGSizeType, (&mut size as *mut CGSize).cast::<c_void>()) != 0).then_some(size) }
}

/// Where the element is on screen, in points from the top left of the main display.
pub fn rect(element: &Owned) -> Option<Bounds> {
    let at = attribute(element, "AXPosition").and_then(|v| point_of(v.0))?;
    let size = attribute(element, "AXSize").and_then(|v| size_of_value(v.0))?;
    Some((at.x.round() as i32, at.y.round() as i32, size.width.round() as i32, size.height.round() as i32))
}

/// The app's windows, with the window number each one has on screen.
pub fn windows_of(pid: i32) -> Vec<(Owned, u32)> {
    let app = application(pid);
    let Some(list) = attribute(&app, "AXWindows") else { return Vec::new() };
    items(list.0)
        .into_iter()
        .filter_map(|window| {
            let window = Owned::retain(window)?;
            let mut number = 0u32;
            unsafe { _AXUIElementGetWindow(window.0, &mut number) };
            Some((window, number))
        })
        .collect()
}

/// A role for an AX role, or None for containers that only matter for what they hold.
fn role(ax_role: &str, subrole: &str) -> Option<&'static str> {
    Some(match (ax_role, subrole) {
        ("AXButton", "AXCloseButton" | "AXMinimizeButton" | "AXZoomButton" | "AXFullScreenButton") => "chrome",
        ("AXButton" | "AXMenuButton" | "AXDisclosureTriangle" | "AXColorWell", _) => "button",
        ("AXPopUpButton" | "AXComboBox", _) => "combobox",
        ("AXCheckBox", _) => "checkbox",
        ("AXRadioButton", "AXTabButton") => "tab",
        ("AXRadioButton", _) => "radio",
        ("AXTextField" | "AXTextArea", _) => "edit",
        ("AXStaticText", _) => "",
        ("AXLink", _) => "link",
        ("AXImage", _) => "image",
        ("AXMenuItem" | "AXMenuBarItem", _) => "menuitem",
        ("AXSlider", _) => "slider",
        ("AXIncrementor" | "AXStepper", _) => "spinner",
        ("AXProgressIndicator" | "AXBusyIndicator" | "AXLevelIndicator", _) => "progress",
        ("AXRow", _) => "item",
        ("AXWebArea", _) => "document",
        ("AXHeading", _) => "header",
        ("AXDateField" | "AXTimeField", _) => "edit",
        _ => return None,
    })
}

/// Parts that never help an agent, along with everything inside them.
fn skipped(ax_role: &str) -> bool {
    matches!(ax_role, "AXScrollBar" | "AXValueIndicator" | "AXGrowArea" | "AXMenuBar" | "AXSplitter" | "AXRuler")
}

fn is_row(ax_role: &str) -> bool {
    matches!(ax_role, "AXRow")
}

fn acts(role: &str) -> bool {
    !matches!(role, "" | "image" | "progress" | "header" | "document")
}

struct Walk<'a> {
    nodes: &'a mut Vec<Node>,
    hidden: &'a mut usize,
    window: Bounds,
    all: bool,
    names: Owned,
}

fn outside(rect: Bounds, window: Bounds) -> bool {
    let (x, y, w, h) = rect;
    let (wx, wy, ww, wh) = window;
    w <= 0 || h <= 0 || x + w <= wx || y + h <= wy || x >= wx + ww || y >= wy + wh
}

impl Walk<'_> {
    fn values(&self, element: CFTypeRef) -> Option<Owned> {
        let mut values: CFArrayRef = null();
        let error = unsafe { AXUIElementCopyMultipleAttributeValues(element, self.names.0, 0, &mut values) };
        if error == kAXErrorSuccess { Owned::new(values) } else { None }
    }

    fn walk(&mut self, element: CFTypeRef, parent: &str, row: Option<usize>, depth: usize) {
        if depth > MAX_DEPTH || self.nodes.len() >= MAX_NODES {
            return;
        }
        let Some(values) = self.values(element) else { return };
        let got = items(values.0);
        if got.len() < ATTRIBUTES.len() {
            return;
        }
        let ax_role = string_of(got[0]).unwrap_or_default();
        if skipped(&ax_role) {
            return;
        }
        let subrole = string_of(got[1]).unwrap_or_default();
        let title = string_of(got[2]).unwrap_or_default();
        let description = string_of(got[3]).unwrap_or_default();
        let value_text = string_of(got[4]);
        let value_number = number_of(got[4]);
        let enabled = bool_of(got[7]).unwrap_or(true);
        let selected = bool_of(got[8]).unwrap_or(false);
        let expanded = bool_of(got[9]);
        let identifier = string_of(got[10]).unwrap_or_default();
        let children = got[11];
        let rect = match (point_of(got[5]), size_of_value(got[6])) {
            (Some(at), Some(size)) => (at.x.round() as i32, at.y.round() as i32, size.width.round() as i32, size.height.round() as i32),
            _ => (0, 0, 0, 0),
        };
        let role = role(&ax_role, &subrole);
        if role == Some("chrome") {
            return;
        }
        let mut name = if !title.is_empty() { title } else { description };
        if role == Some("") && name.is_empty() {
            name = value_text.clone().unwrap_or_default();
        }

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
            self.children(children, &name, row, depth);
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
                let mut states = Vec::new();
                if !enabled {
                    states.push("disabled");
                }
                match (role, value_number) {
                    ("checkbox" | "radio", Some(0.0)) => states.push("off"),
                    ("checkbox" | "radio", Some(1.0)) => states.push("on"),
                    ("checkbox", Some(_)) => states.push("mixed"),
                    _ => {}
                }
                if selected {
                    states.push("selected");
                }
                match expanded {
                    Some(true) => states.push("open"),
                    Some(false) if role == "combobox" || ax_role == "AXDisclosureTriangle" => states.push("closed"),
                    _ => {}
                }
                let password = subrole == "AXSecureTextField";
                if password {
                    states.push("password");
                }
                let value = match role {
                    _ if password => None,
                    "slider" | "spinner" | "progress" => value_number.map(|v| format!("{v}")),
                    "edit" | "combobox" => value_text.filter(|v| !v.is_empty() && *v != name),
                    _ => None,
                };
                let shown = if name.is_empty() && !identifier.is_empty() { format!("#{identifier}") } else { name.clone() };
                let Some(kept) = Owned::retain(element) else { return };
                self.nodes.push(Node {
                    key: unsafe { CFHash(element) },
                    identity: format!("{ax_role}|{identifier}|{name}"),
                    parts: Parts { role: role.to_string(), name: shown, value, states, cells: Vec::new() },
                    rect,
                    ax_role: ax_role.clone(),
                    element: kept,
                });
                if is_row(&ax_role) {
                    own = Some(self.nodes.len() - 1);
                }
            }
        }
        let carried = if listed { name.clone() } else { parent.to_string() };
        self.children(children, &carried, own, depth);
    }

    fn children(&mut self, children: CFTypeRef, parent: &str, row: Option<usize>, depth: usize) {
        for child in items(children) {
            if unsafe { CFGetTypeID(child) } == unsafe { AXUIElementGetTypeID() } {
                self.walk(child, parent, row, depth + 1);
            }
        }
    }
}

/// Every control worth a line in this window, and how many were left out for being off screen.
pub fn read(window: &Owned, window_rect: Bounds, all: bool) -> (Vec<Node>, usize) {
    let mut nodes = Vec::new();
    let mut hidden = 0;
    let keys: Vec<Owned> = ATTRIBUTES.iter().map(|name| cf_string(name)).collect();
    let raw: Vec<CFTypeRef> = keys.iter().map(|k| k.0).collect();
    let mut walk = Walk { nodes: &mut nodes, hidden: &mut hidden, window: window_rect, all, names: array_of(&raw) };
    walk.walk(window.0, "", None, 0);
    (nodes, hidden)
}

/// The whole text of a control, for `get`.
pub fn full_text(element: &Owned) -> String {
    for name in ["AXValue", "AXTitle", "AXDescription"] {
        if let Some(text) = text(element, name).filter(|t| !t.is_empty()) {
            return text;
        }
    }
    String::new()
}
