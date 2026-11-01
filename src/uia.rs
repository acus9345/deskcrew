// Reading a window through Windows accessibility: one cached fetch of the whole tree, so a
// read costs one trip into the app however many controls it has.
use crate::lines::{self, Parts};
use crate::win::Hwnd;
use std::{
    sync::mpsc,
    thread,
    time::Duration,
};
use windows::{
    Win32::{
        Foundation::{HWND, RECT},
        System::{
            Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx},
            Ole::{SafeArrayAccessData, SafeArrayDestroy, SafeArrayGetLBound, SafeArrayGetUBound, SafeArrayUnaccessData},
            Variant::VARIANT,
        },
        UI::Accessibility::*,
    },
    core::{BSTR, Interface},
};

/// A click that opens a modal dialog does not return until the dialog closes, so stop waiting.
const INVOKE_WAIT: Duration = Duration::from_millis(1500);
const MAX_TEXT: i32 = 200_000;
const MAX_DEPTH: usize = 64;

pub fn enter_com() {
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
}

#[derive(Clone)]
pub struct Element(pub IUIAutomationElement);

// Controls are only used from multithreaded COM, where these pointers are free to move.
unsafe impl Send for Element {}
unsafe impl Sync for Element {}

#[derive(Clone, Copy, Default)]
pub struct Can {
    pub invoke: bool,
    pub toggle: bool,
    pub select: bool,
    pub expand: bool,
    pub value: bool,
    pub range: bool,
    pub scroll: bool,
    pub scroll_item: bool,
    pub legacy: bool,
}

#[derive(Clone)]
pub struct Node {
    pub runtime: Vec<i32>,
    pub identity: String,
    pub parts: Parts,
    pub rect: (i32, i32, i32, i32),
    pub native: Hwnd,
    pub can: Can,
    pub element: Element,
}

pub struct Uia {
    auto: IUIAutomation,
    cache: IUIAutomationCacheRequest,
}

unsafe impl Send for Uia {}
unsafe impl Sync for Uia {}

fn text(result: windows::core::Result<BSTR>) -> String {
    result.map(|s| s.to_string()).unwrap_or_default()
}

fn flag(result: windows::core::Result<windows::core::BOOL>) -> bool {
    result.map(|b| b.as_bool()).unwrap_or(false)
}

unsafe fn cached_bool(el: &IUIAutomationElement, id: UIA_PROPERTY_ID) -> bool {
    unsafe { el.GetCachedPropertyValue(id) }.ok().and_then(|v: VARIANT| bool::try_from(&v).ok()).unwrap_or(false)
}

unsafe fn cached_i32(el: &IUIAutomationElement, id: UIA_PROPERTY_ID) -> Option<i32> {
    unsafe { el.GetCachedPropertyValue(id) }.ok().and_then(|v: VARIANT| i32::try_from(&v).ok())
}

unsafe fn cached_f64(el: &IUIAutomationElement, id: UIA_PROPERTY_ID) -> Option<f64> {
    unsafe { el.GetCachedPropertyValue(id) }.ok().and_then(|v: VARIANT| f64::try_from(&v).ok())
}

unsafe fn cached_text(el: &IUIAutomationElement, id: UIA_PROPERTY_ID) -> String {
    unsafe { el.GetCachedPropertyValue(id) }.ok().and_then(|v: VARIANT| BSTR::try_from(&v).ok()).map(|s| s.to_string()).unwrap_or_default()
}

unsafe fn runtime_id(el: &IUIAutomationElement) -> Vec<i32> {
    unsafe {
        let Ok(array) = el.GetRuntimeId() else { return Vec::new() };
        if array.is_null() {
            return Vec::new();
        }
        let mut out = Vec::new();
        if let (Ok(low), Ok(high)) = (SafeArrayGetLBound(array, 1), SafeArrayGetUBound(array, 1)) {
            let mut data = std::ptr::null_mut();
            if SafeArrayAccessData(array, &mut data).is_ok() {
                out.extend_from_slice(std::slice::from_raw_parts(data as *const i32, (high - low + 1).max(0) as usize));
                let _ = SafeArrayUnaccessData(array);
            }
        }
        let _ = SafeArrayDestroy(array);
        out
    }
}

fn rect_of(r: RECT) -> (i32, i32, i32, i32) {
    (r.left, r.top, r.right - r.left, r.bottom - r.top)
}

fn say(error: windows::core::Error) -> String {
    let code = error.code().0 as u32;
    match code {
        0x8004_0201 | 0x8004_0200 => "that control is gone".into(),
        0x8007_0005 => "blocked: the window runs as administrator".into(),
        _ => format!("Windows refused: {code:#x} {}", error.message()),
    }
}

impl Uia {
    pub fn new() -> Result<Uia, String> {
        unsafe {
            let auto: IUIAutomation = CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER).map_err(say)?;
            // A hung app must cost a timeout, not a hung agent.
            if let Ok(newer) = auto.cast::<IUIAutomation2>() {
                let _ = newer.SetConnectionTimeout(2000);
                let _ = newer.SetTransactionTimeout(8000);
            }
            let cache = auto.CreateCacheRequest().map_err(say)?;
            for id in [
                UIA_ControlTypePropertyId,
                UIA_NamePropertyId,
                UIA_AutomationIdPropertyId,
                UIA_BoundingRectanglePropertyId,
                UIA_IsEnabledPropertyId,
                UIA_IsOffscreenPropertyId,
                UIA_IsPasswordPropertyId,
                UIA_IsKeyboardFocusablePropertyId,
                UIA_NativeWindowHandlePropertyId,
                UIA_IsInvokePatternAvailablePropertyId,
                UIA_IsTogglePatternAvailablePropertyId,
                UIA_IsSelectionItemPatternAvailablePropertyId,
                UIA_IsExpandCollapsePatternAvailablePropertyId,
                UIA_IsValuePatternAvailablePropertyId,
                UIA_IsRangeValuePatternAvailablePropertyId,
                UIA_IsScrollPatternAvailablePropertyId,
                UIA_IsScrollItemPatternAvailablePropertyId,
                UIA_IsLegacyIAccessiblePatternAvailablePropertyId,
                UIA_ValueValuePropertyId,
                UIA_ValueIsReadOnlyPropertyId,
                UIA_ToggleToggleStatePropertyId,
                UIA_SelectionItemIsSelectedPropertyId,
                UIA_ExpandCollapseExpandCollapseStatePropertyId,
                UIA_RangeValueValuePropertyId,
                UIA_ScrollVerticallyScrollablePropertyId,
                UIA_ScrollHorizontallyScrollablePropertyId,
            ] {
                cache.AddProperty(id).map_err(say)?;
            }
            cache.SetTreeScope(TreeScope_Subtree).map_err(say)?;
            cache.SetTreeFilter(&auto.ControlViewCondition().map_err(say)?).map_err(say)?;
            cache.SetAutomationElementMode(AutomationElementMode_Full).map_err(say)?;
            Ok(Uia { auto, cache })
        }
    }

    /// Every control worth a line in this window, in reading order. Off-screen ones are counted.
    pub fn read(&self, hwnd: Hwnd, all: bool, nodes: &mut Vec<Node>, hidden: &mut usize) -> Result<(), String> {
        unsafe {
            let root = self.auto.ElementFromHandle(HWND(hwnd as _)).map_err(say)?;
            let tree = root.BuildUpdatedCache(&self.cache).map_err(say)?;
            walk(&tree, "", None, all, nodes, hidden, 0);
            Ok(())
        }
    }
}

unsafe fn walk(
    el: &IUIAutomationElement,
    parent: &str,
    row: Option<usize>,
    all: bool,
    nodes: &mut Vec<Node>,
    hidden: &mut usize,
    depth: usize,
) {
    unsafe {
        if depth > MAX_DEPTH {
            return;
        }
        let kind = el.CachedControlType().map(|k| k.0).unwrap_or(0);
        if lines::skipped(kind) {
            return;
        }
        let name = text(el.CachedName());
        let can = Can {
            invoke: cached_bool(el, UIA_IsInvokePatternAvailablePropertyId),
            toggle: cached_bool(el, UIA_IsTogglePatternAvailablePropertyId),
            select: cached_bool(el, UIA_IsSelectionItemPatternAvailablePropertyId),
            expand: cached_bool(el, UIA_IsExpandCollapsePatternAvailablePropertyId),
            value: cached_bool(el, UIA_IsValuePatternAvailablePropertyId),
            range: cached_bool(el, UIA_IsRangeValuePatternAvailablePropertyId),
            scroll: cached_bool(el, UIA_IsScrollPatternAvailablePropertyId),
            scroll_item: cached_bool(el, UIA_IsScrollItemPatternAvailablePropertyId),
            legacy: cached_bool(el, UIA_IsLegacyIAccessiblePatternAvailablePropertyId),
        };
        let off = flag(el.CachedIsOffscreen());
        let focusable = flag(el.CachedIsKeyboardFocusable());
        let scrollable = can.scroll
            && (cached_bool(el, UIA_ScrollVerticallyScrollablePropertyId) || cached_bool(el, UIA_ScrollHorizontallyScrollablePropertyId));
        let read_only = cached_bool(el, UIA_ValueIsReadOnlyPropertyId);
        let acts = can.invoke || can.toggle || can.select || can.expand || (can.value && !read_only) || can.range;

        // Inside a row, plain text belongs to the row's own line.
        if let Some(at) = row
            && !acts && !focusable {
                let shown = if name.is_empty() { cached_text(el, UIA_ValueValuePropertyId) } else { name.clone() };
                if !shown.is_empty() && shown != parent && !nodes[at].parts.cells.contains(&shown) {
                    nodes[at].parts.cells.push(shown);
                }
                walk_children(el, &name, row, all, nodes, hidden, depth);
                return;
            }

        let role = lines::role(kind);
        let listed = match role {
            Some("") => !name.is_empty() && name != parent,
            Some("image") => acts && !name.is_empty(),
            Some(_) => true,
            None => acts || scrollable,
        };

        let mut own = row;
        if listed {
            if off && !all {
                *hidden += 1;
            } else {
                // WinForms hands out window handles as ids, which mean nothing and change every run.
                let automation_id = Some(text(el.CachedAutomationId())).filter(|id| !id.bytes().all(|b| b.is_ascii_digit())).unwrap_or_default();
                if lines::is_window_chrome(&automation_id) {
                    walk_children(el, parent, row, all, nodes, hidden, depth);
                    return;
                }
                let mut states = Vec::new();
                if !flag(el.CachedIsEnabled()) {
                    states.push("disabled");
                }
                match cached_i32(el, UIA_ToggleToggleStatePropertyId) {
                    Some(0) if can.toggle => states.push("off"),
                    Some(1) if can.toggle => states.push("on"),
                    Some(2) if can.toggle => states.push("mixed"),
                    _ => {}
                }
                if can.select && cached_bool(el, UIA_SelectionItemIsSelectedPropertyId) {
                    states.push("selected");
                }
                match cached_i32(el, UIA_ExpandCollapseExpandCollapseStatePropertyId) {
                    Some(0) if can.expand => states.push("closed"),
                    Some(1) if can.expand => states.push("open"),
                    _ => {}
                }
                if scrollable {
                    states.push("scroll");
                }
                let password = flag(el.CachedIsPassword());
                if password {
                    states.push("password");
                }
                let value = if password {
                    None
                } else if can.range {
                    cached_f64(el, UIA_RangeValueValuePropertyId).map(|v| format!("{v}"))
                } else if can.value {
                    Some(cached_text(el, UIA_ValueValuePropertyId)).filter(|v| !v.is_empty() && *v != name)
                } else {
                    None
                };
                let shown = if name.is_empty() && !automation_id.is_empty() { format!("#{automation_id}") } else { name.clone() };
                let parts = Parts {
                    role: role.map_or_else(|| lines::container_role(kind).to_string(), str::to_string),
                    name: shown,
                    value,
                    states,
                    cells: Vec::new(),
                };
                nodes.push(Node {
                    runtime: runtime_id(el),
                    identity: format!("{kind}|{automation_id}|{name}"),
                    parts,
                    rect: el.CachedBoundingRectangle().map(rect_of).unwrap_or_default(),
                    native: el.CachedNativeWindowHandle().map(|h| h.0 as Hwnd).unwrap_or(0),
                    can,
                    element: Element(el.clone()),
                });
                if lines::is_row(kind) {
                    own = Some(nodes.len() - 1);
                }
            }
        }
        let carried = if listed { name.as_str() } else { parent };
        walk_children(el, carried, own, all, nodes, hidden, depth);
    }
}

unsafe fn walk_children(
    el: &IUIAutomationElement,
    parent: &str,
    row: Option<usize>,
    all: bool,
    nodes: &mut Vec<Node>,
    hidden: &mut usize,
    depth: usize,
) {
    unsafe {
        let Ok(children) = el.GetCachedChildren() else { return };
        for at in 0..children.Length().unwrap_or(0) {
            if let Ok(child) = children.GetElement(at) {
                walk(&child, parent, row, all, nodes, hidden, depth + 1);
            }
        }
    }
}

/// What the app answered, or that it is still busy with a dialog of its own.
pub enum Done {
    Finished,
    StillRunning,
}

/// Run a pattern call where a blocked app cannot block the agent.
fn patiently(element: &Element, call: impl FnOnce(&IUIAutomationElement) -> windows::core::Result<()> + Send + 'static) -> Result<Done, String> {
    let element = element.clone();
    let (done, wait) = mpsc::channel();
    thread::spawn(move || {
        enter_com();
        let owned = element;
        let _ = done.send(call(&owned.0).map_err(say));
    });
    match wait.recv_timeout(INVOKE_WAIT) {
        Ok(Ok(())) => Ok(Done::Finished),
        Ok(Err(error)) => Err(error),
        Err(_) => Ok(Done::StillRunning),
    }
}

impl Element {
    pub fn rect(&self) -> Result<(i32, i32, i32, i32), String> {
        unsafe { self.0.CurrentBoundingRectangle().map(rect_of).map_err(say) }
    }

    pub fn invoke(&self) -> Result<Done, String> {
        patiently(self, |el| unsafe { el.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)?.Invoke() })
    }

    pub fn toggle(&self) -> Result<Done, String> {
        patiently(self, |el| unsafe { el.GetCurrentPatternAs::<IUIAutomationTogglePattern>(UIA_TogglePatternId)?.Toggle() })
    }

    pub fn select(&self) -> Result<Done, String> {
        patiently(self, |el| unsafe { el.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId)?.Select() })
    }

    pub fn default_action(&self) -> Result<Done, String> {
        patiently(self, |el| unsafe {
            el.GetCurrentPatternAs::<IUIAutomationLegacyIAccessiblePattern>(UIA_LegacyIAccessiblePatternId)?.DoDefaultAction()
        })
    }

    pub fn open_or_close(&self) -> Result<Done, String> {
        patiently(self, |el| unsafe {
            let pattern = el.GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(UIA_ExpandCollapsePatternId)?;
            if pattern.CurrentExpandCollapseState()? == ExpandCollapseState_Expanded { pattern.Collapse() } else { pattern.Expand() }
        })
    }

    pub fn open(&self) -> Result<(), String> {
        unsafe { self.0.GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(UIA_ExpandCollapsePatternId).and_then(|p| p.Expand()).map_err(say) }
    }

    pub fn close(&self) -> Result<(), String> {
        unsafe { self.0.GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(UIA_ExpandCollapsePatternId).and_then(|p| p.Collapse()).map_err(say) }
    }

    pub fn set_value(&self, value: &str) -> Result<(), String> {
        let value = BSTR::from(value);
        unsafe { self.0.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId).and_then(|p| p.SetValue(&value)).map_err(say) }
    }

    pub fn value_read_only(&self) -> bool {
        unsafe { self.0.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId).and_then(|p| p.CurrentIsReadOnly()) }
            .map(|b| b.as_bool())
            .unwrap_or(true)
    }

    pub fn set_range(&self, value: f64) -> Result<(), String> {
        unsafe { self.0.GetCurrentPatternAs::<IUIAutomationRangeValuePattern>(UIA_RangeValuePatternId).and_then(|p| p.SetValue(value)).map_err(say) }
    }

    pub fn scroll(&self, down: bool, large: bool) -> Result<(), String> {
        let amount = match (down, large) {
            (true, true) => ScrollAmount_LargeIncrement,
            (true, false) => ScrollAmount_SmallIncrement,
            (false, true) => ScrollAmount_LargeDecrement,
            (false, false) => ScrollAmount_SmallDecrement,
        };
        unsafe { self.0.GetCurrentPatternAs::<IUIAutomationScrollPattern>(UIA_ScrollPatternId).and_then(|p| p.Scroll(ScrollAmount_NoAmount, amount)).map_err(say) }
    }

    pub fn bring_into_view(&self) -> Result<(), String> {
        unsafe { self.0.GetCurrentPatternAs::<IUIAutomationScrollItemPattern>(UIA_ScrollItemPatternId).and_then(|p| p.ScrollIntoView()).map_err(say) }
    }

    /// All of a control's text: its document, then its value, then its name.
    pub fn full_text(&self) -> String {
        unsafe {
            if let Ok(pattern) = self.0.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId)
                && let Ok(body) = pattern.DocumentRange().and_then(|r| r.GetText(MAX_TEXT))
                    && !body.is_empty() {
                        return body.to_string();
                    }
            if let Ok(value) = self.0.GetCurrentPropertyValue(UIA_ValueValuePropertyId)
                && let Ok(value) = BSTR::try_from(&value)
                    && !value.is_empty() {
                        return value.to_string();
                    }
            text(self.0.CurrentName())
        }
    }

    /// Find a named item below this control, such as a dropdown's entry.
    pub fn find_named(&self, name: &str, auto: &Uia) -> Option<Element> {
        unsafe {
            let wanted = VARIANT::from(BSTR::from(name));
            let condition = auto.auto.CreatePropertyCondition(UIA_NamePropertyId, &wanted).ok()?;
            self.0.FindFirst(TreeScope_Descendants, &condition).ok().map(Element)
        }
    }
}
