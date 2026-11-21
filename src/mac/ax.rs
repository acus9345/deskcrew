// Reading and acting on a Mac window through the accessibility API: the same short lines the
// Windows side builds from UI Automation.
use super::ffi::*;
use crate::lines::Parts;
use std::collections::HashSet;
use std::ffi::c_void;
use std::ptr::null;
use std::sync::Mutex;

const MAX_DEPTH: usize = 40;
const MAX_NODES: usize = 4000;
const TIMEOUT_S: f32 = 1.5;

// The order matters: values come back in this order.
const ATTRIBUTES: [&str; 15] = [
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
    "AXHelp",
    "AXPlaceholderValue",
    "AXFocused",
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

pub struct Table {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub total_rows: usize,
    pub total_columns: usize,
}

/// PIDs for which we have recently requested Chromium/Electron/WebKit accessibility trees,
/// tracked with a short TTL so exited/reused PIDs are refreshed and the map stays bounded.
static ENABLED_PIDS: Mutex<Option<std::collections::HashMap<i32, std::time::Instant>>> = Mutex::new(None);
const ENABLED_PID_TTL: std::time::Duration = std::time::Duration::from_secs(30);
const ENABLED_PID_MAX: usize = 128;

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
    // Chromium, Electron, VS Code, Slack, and Firefox only populate their AX trees once the
    // app receives AXEnhancedUserInterface or AXManualAccessibility = true.
    let should_enable = {
        let now = std::time::Instant::now();
        let mut guard = ENABLED_PIDS.lock().unwrap_or_else(|e| e.into_inner());
        let map = guard.get_or_insert_with(std::collections::HashMap::new);
        if map.len() >= ENABLED_PID_MAX {
            map.retain(|_, at| now.duration_since(*at) < ENABLED_PID_TTL);
            if map.len() >= ENABLED_PID_MAX {
                map.clear();
            }
        }
        match map.get(&pid) {
            Some(at) if now.duration_since(*at) < ENABLED_PID_TTL => false,
            _ => {
                map.insert(pid, now);
                true
            }
        }
    };
    if should_enable {
        let _ = set_bool(&app, "AXEnhancedUserInterface", true);
        let _ = set_bool(&app, "AXManualAccessibility", true);
    }
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

/// Select all text in `element` via `AXSelectedTextRange` without needing keyboard focus or Cmd+A.
pub fn select_all_text(element: &Owned) -> bool {
    if !settable(element, "AXSelectedTextRange") {
        return false;
    }
    let len = attribute(element, "AXNumberOfCharacters")
        .and_then(|v| number_of(v.0))
        .map(|n| n.max(0.0) as CFIndex)
        .or_else(|| text(element, "AXValue").map(|t| t.encode_utf16().count() as CFIndex));
    let Some(length) = len else { return false };
    let range = CFRange { location: 0, length };
    let Some(value) = Owned::new(unsafe { AXValueCreate(kAXValueCFRangeType, (&range as *const CFRange).cast()) }) else {
        return false;
    };
    set(element, "AXSelectedTextRange", value.0).is_ok()
}

/// Insert text at the current selection/caret through `AXSelectedText`. Rich web editors
/// (`contenteditable`, Monaco, ProseMirror, Slack, Discord, Notion) and some Qt/AppKit views
/// accept `AXSelectedText` in the background even when `AXValue` is read-only or ignored.
pub fn insert_selected_text(element: &Owned, text: &str) -> Result<(), String> {
    if !settable(element, "AXSelectedText") {
        return Err("that control does not accept AXSelectedText".into());
    }
    let value = cf_string(text);
    set(element, "AXSelectedText", value.0)
}

/// The currently selected text inside `element` via `AXSelectedText`, without focus or shortcuts.
pub fn selected_text(element: &Owned) -> Option<String> {
    text(element, "AXSelectedText").filter(|s| !s.is_empty())
}

/// The focused accessibility element ONLY if it belongs to `window_id`, without raising it.
/// Verifies the element's owning `AXWindow` matches `window_id` so multi-window apps (Chrome,
/// VS Code, Notes, Terminal) never leak edits or clipboard reads into a different window.
pub fn focused_element(pid: i32, window_id: u32, window: Option<&Owned>) -> Option<Owned> {
    let candidate = window
        .and_then(|win| attribute(win, "AXFocusedUIElement"))
        .or_else(|| {
            let app = application(pid);
            attribute(&app, "AXFocusedUIElement")
        })?;
    if let Some(owner) = attribute(&candidate, "AXWindow") {
        let mut num = 0u32;
        if unsafe { _AXUIElementGetWindow(owner.0, &mut num) } == kAXErrorSuccess {
            return (num == window_id).then_some(candidate);
        }
    }
    is_main_or_only_window(pid, window_id).then_some(candidate)
}

/// Find the accessibility element at screen point `(x, y)` inside `pid`, even when the window is
/// behind other apps (queries the target app's AX handle rather than the system-wide hit-test).
pub fn element_at(pid: i32, x: f32, y: f32) -> Option<Owned> {
    let app = application(pid);
    let mut found: AXUIElementRef = std::ptr::null();
    let status = unsafe { AXUIElementCopyElementAtPosition(app.0, x, y, &mut found) };
    if status == kAXErrorSuccess && !found.is_null() {
        Some(Owned(found))
    } else {
        None
    }
}

/// Convert `CGEventFlags` modifier mask to macOS `AXMenuItemCmdModifiers` bitmask:
/// bit 0 = Shift, bit 1 = Option, bit 2 = Control, bit 3 = No-Command (0 means Cmd only).
pub fn menu_modifiers_from_cg(cg_flags: u64) -> u32 {
    let mut mods = 0u32;
    if cg_flags & kCGEventFlagMaskShift != 0 {
        mods |= 1;
    }
    if cg_flags & kCGEventFlagMaskAlternate != 0 {
        mods |= 2;
    }
    if cg_flags & kCGEventFlagMaskControl != 0 {
        mods |= 4;
    }
    if cg_flags & kCGEventFlagMaskCommand == 0 {
        mods |= 8;
    }
    mods
}

/// True if `window_id` is the only window of `pid` or currently its `AXMainWindow`, so triggering
/// an app-wide `AXMenuBar` shortcut is guaranteed to target this window rather than another one.
pub fn is_main_or_only_window(pid: i32, window_id: u32) -> bool {
    let wins = windows_of(pid);
    if wins.len() <= 1 {
        return true;
    }
    let app = application(pid);
    if let Some(main_win) = attribute(&app, "AXMainWindow") {
        let mut num = 0u32;
        if unsafe { _AXUIElementGetWindow(main_win.0, &mut num) } == kAXErrorSuccess {
            return num == window_id;
        }
    }
    false
}

/// Find an editable text/combo field inside `root` (such as a Go-to-Folder sheet), explicitly
/// excluding search fields (`AXSearchField`) so `pick-file` never types a path into a search bar.
pub fn editable_in_surface(root: &Owned, max_depth: usize) -> Option<Owned> {
    let mut found = None;
    let mut stack = vec![(root.clone(), 0usize)];
    let mut visited = 0usize;
    while let Some((el, depth)) = stack.pop() {
        visited += 1;
        if visited > 300 || depth > max_depth {
            continue;
        }
        let r = text(&el, "AXRole").unwrap_or_default();
        let sub = text(&el, "AXSubrole").unwrap_or_default();
        if matches!(r.as_str(), "AXTextField" | "AXComboBox") && sub != "AXSearchField" && settable(&el, "AXValue") {
            found = Some(el.clone());
        }
        if depth < max_depth {
            for child in children(&el).into_iter().rev() {
                stack.push((child, depth + 1));
            }
        }
    }
    found
}

/// Find an enabled menu item in `pid`'s `AXMenuBar` whose keyboard shortcut matches `(key_char,
/// virtual_key, cg_flags)` and trigger it via `AXPress` without bringing the app forward.
/// Works for background shortcuts (Cmd+C/V/X/A/Z/S/F/K/N/T, etc.) across native Cocoa, Electron,
/// Chromium, Safari, Terminal, Xcode, and SwiftUI apps.
pub fn press_menu_shortcut(pid: i32, key_char: Option<char>, virtual_key: Option<u16>, cg_flags: u64) -> Option<Result<String, String>> {
    const MAX_MENU_ITEMS: usize = 500;
    const MAX_MENU_DEPTH: usize = 5;
    let app = application(pid);
    let bar = attribute(&app, "AXMenuBar")?;
    let wanted_mods = menu_modifiers_from_cg(cg_flags);
    let wanted_char = key_char.map(|c| c.to_ascii_lowercase());
    let mut stack: Vec<(Owned, usize)> = children(&bar).into_iter().rev().map(|c| (c, 0)).collect();
    let mut visited = 0usize;
    while let Some((el, depth)) = stack.pop() {
        visited += 1;
        if visited > MAX_MENU_ITEMS || depth > MAX_MENU_DEPTH {
            continue;
        }
        let r = text(&el, "AXRole").unwrap_or_default();
        if r == "AXMenuItem" {
            let enabled = attribute(&el, "AXEnabled").and_then(|v| bool_of(v.0)).unwrap_or(true);
            if enabled {
                let cmd_char = text(&el, "AXMenuItemCmdChar").and_then(|s| s.chars().next()).map(|c| c.to_ascii_lowercase());
                let cmd_vk = attribute(&el, "AXMenuItemCmdVirtualKey").and_then(|v| number_of(v.0)).map(|n| n.round() as u16);
                let key_matches = (wanted_char.is_some() && cmd_char == wanted_char)
                    || (cmd_char.is_none() && virtual_key.is_some() && cmd_vk == virtual_key);
                if key_matches {
                    let item_mods = attribute(&el, "AXMenuItemCmdModifiers")
                        .and_then(|v| number_of(v.0))
                        .map(|n| n.round() as u32)
                        .unwrap_or(0);
                    if item_mods == wanted_mods {
                        let title = text(&el, "AXTitle").unwrap_or_default();
                        return Some(perform(&el, "AXPress").map(|_| {
                            if title.is_empty() { "menu shortcut".into() } else { format!("menu shortcut ({title})") }
                        }));
                    }
                }
            }
        }
        if depth < MAX_MENU_DEPTH {
            for child in children(&el).into_iter().rev() {
                stack.push((child, depth + 1));
            }
        }
    }
    None
}

pub fn set_bool(element: &Owned, name: &str, value: bool) -> Result<(), String> {
    set(element, name, unsafe { if value { kCFBooleanTrue } else { kCFBooleanFalse } })
}

pub fn set_number(element: &Owned, name: &str, value: f64) -> Result<(), String> {
    let number = cf_number(value).ok_or("macOS would not make that number")?;
    set(element, name, number.0)
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

/// Extra popup surfaces belonging to the app or window: sheets, popovers, dialogs, and open menus.
pub fn popup_surfaces(pid: i32, main_window: &Owned) -> Vec<Owned> {
    let mut surfaces = Vec::new();
    let mut seen = HashSet::new();
    seen.insert(unsafe { CFHash(main_window.0) });
    let mut push = |el: Owned| {
        if seen.insert(unsafe { CFHash(el.0) }) {
            surfaces.push(el);
        }
    };
    for attr in ["AXSheets", "AXChildren"] {
        if let Some(list) = attribute(main_window, attr) {
            for item in items(list.0) {
                if let Some(el) = Owned::retain(item) {
                    let r = text(&el, "AXRole").unwrap_or_default();
                    let sub = text(&el, "AXSubrole").unwrap_or_default();
                    if matches!(r.as_str(), "AXSheet" | "AXPopover" | "AXMenu") || matches!(sub.as_str(), "AXDialog" | "AXSheet" | "AXSystemDialog" | "AXFloatingWindow") {
                        push(el);
                    }
                }
            }
        }
    }
    let app = application(pid);
    if let Some(list) = attribute(&app, "AXWindows") {
        for item in items(list.0) {
            if let Some(el) = Owned::retain(item) {
                let r = text(&el, "AXRole").unwrap_or_default();
                let sub = text(&el, "AXSubrole").unwrap_or_default();
                if matches!(r.as_str(), "AXSheet" | "AXPopover" | "AXMenu") || matches!(sub.as_str(), "AXDialog" | "AXSheet" | "AXSystemDialog") {
                    push(el);
                }
            }
        }
    }
    surfaces
}

/// True if `outer` holds all of `inner`.
pub fn encloses(outer: Bounds, inner: Bounds) -> bool {
    outer != inner && outer.0 <= inner.0 && outer.1 <= inner.1 && outer.0 + outer.2 >= inner.0 + inner.2 && outer.1 + outer.3 >= inner.1 + inner.3
}

/// A role for an AX role, or None for containers that only matter for what they hold.
fn role(ax_role: &str, subrole: &str) -> Option<&'static str> {
    Some(match (ax_role, subrole) {
        ("AXButton", "AXCloseButton" | "AXMinimizeButton" | "AXZoomButton" | "AXFullScreenButton") => "chrome",
        ("AXButton" | "AXMenuButton" | "AXDisclosureTriangle" | "AXDisclosureButton" | "AXColorWell", _) => "button",
        ("AXPopUpButton" | "AXComboBox", _) => "combobox",
        ("AXCheckBox" | "AXSwitch", _) => "checkbox",
        ("AXRadioButton", "AXTabButton") | ("AXTab", _) => "tab",
        ("AXRadioButton", _) => "radio",
        ("AXTabGroup", _) => "tabs",
        ("AXTable" | "AXOutline", _) => "table",
        ("AXList", _) => "list",
        ("AXMenu", _) => "menu",
        ("AXToolbar", _) => "toolbar",
        ("AXTextField" | "AXTextArea" | "AXSearchField", _) => "edit",
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

/// Parse checkbox / switch / radio state from CFBoolean, CFNumber, or CFString AXValue.
fn toggle_state(role: &str, bool_val: Option<bool>, num_val: Option<f64>, str_val: Option<&str>) -> Option<&'static str> {
    if !matches!(role, "checkbox" | "radio") {
        return None;
    }
    if let Some(b) = bool_val {
        return Some(if b { "on" } else { "off" });
    }
    if let Some(n) = num_val {
        return if n == 0.0 {
            Some("off")
        } else if n == 1.0 {
            Some("on")
        } else if role == "checkbox" {
            Some("mixed")
        } else {
            None
        };
    }
    match str_val.map(str::trim) {
        Some("0" | "false" | "off") => Some("off"),
        Some("1" | "true" | "on") => Some("on"),
        Some("2" | "mixed") if role == "checkbox" => Some("mixed"),
        _ => None,
    }
}

/// Parts that never help an agent, along with everything inside them.
fn skipped(ax_role: &str) -> bool {
    matches!(ax_role, "AXScrollBar" | "AXValueIndicator" | "AXGrowArea" | "AXMenuBar" | "AXSplitter" | "AXRuler")
}

fn is_row(ax_role: &str) -> bool {
    matches!(ax_role, "AXRow")
}

fn acts(role: &str) -> bool {
    !matches!(role, "" | "image" | "progress" | "header" | "document" | "pane" | "table" | "list" | "tabs" | "toolbar" | "menu")
}

struct Walk<'a> {
    nodes: &'a mut Vec<Node>,
    hidden: &'a mut usize,
    seen_keys: HashSet<usize>,
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
        let key = unsafe { CFHash(element) };
        if !self.seen_keys.insert(key) {
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
        let value_bool = bool_of(got[4]);
        let enabled = bool_of(got[7]).unwrap_or(true);
        let selected = bool_of(got[8]).unwrap_or(false);
        let expanded = bool_of(got[9]);
        let identifier = string_of(got[10]).unwrap_or_default();
        let children = got[11];
        let help = string_of(got[12]).unwrap_or_default();
        let placeholder = string_of(got[13]).unwrap_or_default();
        let rect = match (point_of(got[5]), size_of_value(got[6])) {
            (Some(at), Some(size)) => (at.x.round() as i32, at.y.round() as i32, size.width.round() as i32, size.height.round() as i32),
            _ => (0, 0, 0, 0),
        };
        let role = role(&ax_role, &subrole);
        if role == Some("chrome") {
            return;
        }
        let scrollable = ax_role == "AXScrollArea";
        let mut name = if !title.is_empty() {
            title
        } else if !description.is_empty() {
            description
        } else if !help.is_empty() {
            help
        } else {
            placeholder
        };
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
            self.children(element, &ax_role, children, &name, row, depth);
            return;
        }

        let listed = match role {
            Some("") => !name.is_empty() && name != parent,
            Some("image") => !name.is_empty() && depth > 0,
            Some("toolbar" | "menu" | "tabs") => false,
            Some(_) => true,
            None => scrollable,
        };
        let mut own = row;
        if listed {
            if outside(rect, self.window) && !self.all {
                *self.hidden += 1;
            } else {
                let role_str = role.unwrap_or(if scrollable { "pane" } else { "" });
                let mut states = Vec::new();
                if !enabled {
                    states.push("disabled");
                }
                if let Some(state) = toggle_state(role_str, value_bool, value_number, value_text.as_deref()) {
                    states.push(state);
                }
                if selected {
                    states.push("selected");
                }
                match expanded {
                    Some(true) => states.push("open"),
                    Some(false) if role_str == "combobox" || matches!(ax_role.as_str(), "AXDisclosureTriangle" | "AXDisclosureButton") => states.push("closed"),
                    _ => {}
                }
                if scrollable {
                    states.push("scroll");
                }
                let password = subrole == "AXSecureTextField";
                if password {
                    states.push("password");
                }
                let value = match role_str {
                    _ if password => None,
                    "slider" | "spinner" | "progress" => value_number.map(|v| format!("{v}")),
                    "edit" | "combobox" => value_text.filter(|v| !v.is_empty() && *v != name),
                    _ => None,
                };
                let shown = if name.is_empty() && !identifier.is_empty() { format!("#{identifier}") } else { name.clone() };
                let Some(kept) = Owned::retain(element) else { return };
                self.nodes.push(Node {
                    key,
                    identity: format!("{ax_role}|{identifier}|{name}"),
                    parts: Parts { role: role_str.to_string(), name: shown, value, states, cells: Vec::new() },
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
        self.children(element, &ax_role, children, &carried, own, depth);
    }

    fn children(&mut self, element: CFTypeRef, ax_role: &str, children: CFTypeRef, parent: &str, row: Option<usize>, depth: usize) {
        let mut list = items(children);
        // Virtualized tables/outlines/lists may omit rows from AXChildren and expose AXVisibleRows/AXRows.
        if list.is_empty() && matches!(ax_role, "AXTable" | "AXOutline" | "AXList") {
            if let Some(owned) = Owned::retain(element) {
                for fallback in ["AXVisibleRows", "AXRows", "AXVisibleChildren"] {
                    if let Some(found) = attribute(&owned, fallback) {
                        let extra = items(found.0);
                        if !extra.is_empty() {
                            for child in extra {
                                if unsafe { CFGetTypeID(child) } == unsafe { AXUIElementGetTypeID() } {
                                    self.walk(child, parent, row, depth + 1);
                                }
                            }
                            return;
                        }
                    }
                }
            }
            return;
        }
        for child in list.drain(..) {
            if unsafe { CFGetTypeID(child) } == unsafe { AXUIElementGetTypeID() } {
                self.walk(child, parent, row, depth + 1);
            }
        }
    }
}

/// Drop duplicate captions right before the control they name, or shells enclosing an identically named control.
fn drop_labels(nodes: &mut Vec<Node>, quiet: &mut Vec<Bounds>) {
    let mut at = 0;
    while at + 1 < nodes.len() {
        let (label, next) = (&nodes[at].parts, &nodes[at + 1].parts);
        let shell = !label.role.is_empty() && encloses(nodes[at].rect, nodes[at + 1].rect);
        if (label.role.is_empty() || shell) && label.value.is_none() && !label.name.is_empty() && !next.role.is_empty() && next.name == label.name {
            quiet.push(nodes.remove(at).rect);
        } else {
            at += 1;
        }
    }
}

/// Every control worth a line in this window (and its open sheets/popovers/menus), off-screen count,
/// and quiet rects omitted because a control already carries the same label.
pub fn read(pid: i32, window: &Owned, window_rect: Bounds, all: bool) -> (Vec<Node>, usize, Vec<Bounds>) {
    let mut nodes = Vec::new();
    let mut hidden = 0;
    let mut quiet = Vec::new();
    let keys: Vec<Owned> = ATTRIBUTES.iter().map(|name| cf_string(name)).collect();
    let raw: Vec<CFTypeRef> = keys.iter().map(|k| k.0).collect();
    let mut walk = Walk { nodes: &mut nodes, hidden: &mut hidden, seen_keys: HashSet::new(), window: window_rect, all, names: array_of(&raw) };
    walk.walk(window.0, "", None, 0);
    for surface in popup_surfaces(pid, window) {
        let surface_rect = rect(&surface).unwrap_or(window_rect);
        walk.window = surface_rect;
        walk.walk(surface.0, "", None, 0);
    }
    if !all {
        drop_labels(&mut nodes, &mut quiet);
    }
    (nodes, hidden, quiet)
}

/// The whole text of a control, for `get`.
pub fn full_text(element: &Owned) -> String {
    for name in ["AXValue", "AXTitle", "AXDescription", "AXHelp"] {
        if let Some(text) = text(element, name).filter(|t| !t.is_empty()) {
            return text;
        }
    }
    String::new()
}

/// Walk up AXParent to locate the nearest AXTable / AXOutline / AXList.
fn table_around(element: &Owned) -> Option<Owned> {
    let mut at = element.clone();
    for _ in 0..5 {
        let r = text(&at, "AXRole").unwrap_or_default();
        if matches!(r.as_str(), "AXTable" | "AXOutline" | "AXList") {
            return Some(at);
        }
        at = attribute(&at, "AXParent")?;
    }
    None
}

fn cell_text(cell: &Owned) -> String {
    let own = full_text(cell);
    if !own.is_empty() {
        return own;
    }
    let mut parts = Vec::new();
    for child in children(cell) {
        let t = full_text(&child);
        if !t.is_empty() && !parts.contains(&t) {
            parts.push(t);
        }
    }
    parts.join(" ")
}

/// Read rows and headers from an AXTable, AXOutline, or AXList.
pub fn table(element: &Owned, skip: usize, max_rows: usize, max_cols: usize) -> Result<Table, String> {
    let container = table_around(element).ok_or("that control is not a table or list, or a cell in one")?;
    let headers: Vec<String> = attribute(&container, "AXColumns")
        .map(|list| {
            items(list.0)
                .into_iter()
                .take(max_cols)
                .filter_map(Owned::retain)
                .map(|col| {
                    attribute(&col, "AXHeader")
                        .map(|h| full_text(&h))
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| full_text(&col))
                })
                .collect()
        })
        .unwrap_or_default();
    let row_elems: Vec<Owned> = attribute(&container, "AXRows")
        .or_else(|| attribute(&container, "AXVisibleRows"))
        .or_else(|| attribute(&container, "AXChildren"))
        .map(|list| {
            items(list.0)
                .into_iter()
                .filter_map(Owned::retain)
                .filter(|el| text(el, "AXRole").as_deref() == Some("AXRow"))
                .collect()
        })
        .unwrap_or_default();
    if row_elems.is_empty() {
        return Err("that table has no rows".into());
    }
    let total_rows = row_elems.len();
    let mut total_columns = headers.len();
    let mut rows = Vec::new();
    for row in row_elems.into_iter().skip(skip).take(max_rows) {
        let kids = children(&row);
        let cells: Vec<String> = if kids.is_empty() {
            vec![full_text(&row)]
        } else {
            kids.into_iter().take(max_cols).map(|c| cell_text(&c)).collect()
        };
        total_columns = total_columns.max(cells.len());
        rows.push(cells);
    }
    Ok(Table { headers, rows, total_rows, total_columns })
}

/// Find an AXScrollArea at or near `element` and move its AXScrollBar value in the background.
pub fn scroll_bar(element: &Owned, sideways: bool, down: bool, times: i32) -> Result<bool, String> {
    let attr = if sideways { "AXHorizontalScrollBar" } else { "AXVerticalScrollBar" };
    let mut area = None;
    let mut at = element.clone();
    for _ in 0..6 {
        if attribute(&at, attr).is_some() {
            area = Some(at);
            break;
        }
        let Some(parent) = attribute(&at, "AXParent") else { break };
        at = parent;
    }
    let Some(area) = area else { return Ok(false) };
    let Some(bar) = attribute(&area, attr) else { return Ok(false) };
    if !settable(&bar, "AXValue") {
        return Ok(false);
    }
    let current = attribute(&bar, "AXValue").and_then(|v| number_of(v.0)).unwrap_or(0.0);
    let step = 0.1 * times.max(1) as f64;
    let next = if down { (current + step).clamp(0.0, 1.0) } else { (current - step).clamp(0.0, 1.0) };
    set_number(&bar, "AXValue", next)?;
    Ok(true)
}

/// Read visible notification banners/alerts from macOS NotificationCenter, along with any close button.
pub fn notifications(nc_pid: i32) -> Vec<(String, Option<Owned>)> {
    let mut out = Vec::new();
    for (window, _) in windows_of(nc_pid) {
        let mut texts = Vec::new();
        let mut close_btn = None;
        let mut stack = vec![window];
        let mut visited = 0usize;
        while let Some(el) = stack.pop() {
            if visited >= 400 {
                break;
            }
            visited += 1;
            let r = text(&el, "AXRole").unwrap_or_default();
            if r == "AXStaticText" {
                let t = full_text(&el);
                if !t.is_empty() && !texts.contains(&t) {
                    texts.push(t);
                }
            } else if r == "AXButton" && close_btn.is_none() {
                let label = full_text(&el).to_lowercase();
                if label.contains("close") || label.contains("clear") || label.contains("dismiss") {
                    close_btn = Some(el.clone());
                }
            }
            for child in children(&el) {
                stack.push(child);
            }
        }
        if !texts.is_empty() {
            out.push((texts.join(" — "), close_btn));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_state_handles_bool_number_and_string_values() {
        assert_eq!(toggle_state("checkbox", Some(true), None, None), Some("on"));
        assert_eq!(toggle_state("checkbox", Some(false), None, None), Some("off"));
        assert_eq!(toggle_state("checkbox", None, Some(1.0), None), Some("on"));
        assert_eq!(toggle_state("checkbox", None, Some(0.0), None), Some("off"));
        assert_eq!(toggle_state("checkbox", None, Some(2.0), None), Some("mixed"));
        assert_eq!(toggle_state("checkbox", None, None, Some("1")), Some("on"));
        assert_eq!(toggle_state("checkbox", None, None, Some("0")), Some("off"));
        assert_eq!(toggle_state("radio", None, Some(1.0), None), Some("on"));
        assert_eq!(toggle_state("radio", None, Some(2.0), None), None);
        assert_eq!(toggle_state("button", Some(true), None, None), None);
    }

    #[test]
    fn roles_cover_switches_tabs_tables_lists_and_search_fields() {
        assert_eq!(role("AXSwitch", ""), Some("checkbox"));
        assert_eq!(role("AXCheckBox", "AXSwitch"), Some("checkbox"));
        assert_eq!(role("AXTab", ""), Some("tab"));
        assert_eq!(role("AXTabGroup", ""), Some("tabs"));
        assert_eq!(role("AXTable", ""), Some("table"));
        assert_eq!(role("AXOutline", ""), Some("table"));
        assert_eq!(role("AXList", ""), Some("list"));
        assert_eq!(role("AXTextField", "AXSearchField"), Some("edit"));
        assert_eq!(role("AXDisclosureButton", ""), Some("button"));
    }

    #[test]
    fn encloses_detects_strict_containment() {
        assert!(encloses((0, 0, 100, 50), (10, 10, 40, 20)));
        assert!(!encloses((0, 0, 100, 50), (0, 0, 100, 50)));
        assert!(!encloses((10, 10, 40, 20), (0, 0, 100, 50)));
    }

    #[test]
    fn menu_modifiers_match_apple_ax_bitmask() {
        assert_eq!(menu_modifiers_from_cg(kCGEventFlagMaskCommand), 0);
        assert_eq!(menu_modifiers_from_cg(kCGEventFlagMaskCommand | kCGEventFlagMaskShift), 1);
        assert_eq!(menu_modifiers_from_cg(kCGEventFlagMaskCommand | kCGEventFlagMaskAlternate), 2);
        assert_eq!(menu_modifiers_from_cg(kCGEventFlagMaskCommand | kCGEventFlagMaskControl), 4);
        assert_eq!(menu_modifiers_from_cg(kCGEventFlagMaskControl), 12);
    }
}
