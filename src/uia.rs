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
    // The automation id as the app gave it, numbers included: common dialogs name their parts this way.
    pub control_id: String,
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

/// The rectangles a text range covers on the screen, as left, top, width and height.
fn rects_of(range: &IUIAutomationTextRange) -> Vec<(i32, i32, i32, i32)> {
    unsafe {
        let Ok(array) = range.GetBoundingRectangles() else { return Vec::new() };
        if array.is_null() {
            return Vec::new();
        }
        let mut numbers: Vec<f64> = Vec::new();
        if let (Ok(low), Ok(high)) = (SafeArrayGetLBound(array, 1), SafeArrayGetUBound(array, 1)) {
            let mut data = std::ptr::null_mut();
            if SafeArrayAccessData(array, &mut data).is_ok() {
                numbers.extend_from_slice(std::slice::from_raw_parts(data as *const f64, (high - low + 1).max(0) as usize));
                let _ = SafeArrayUnaccessData(array);
            }
        }
        let _ = SafeArrayDestroy(array);
        numbers
            .as_chunks::<4>()
            .0
            .iter()
            .map(|r| (r[0].round() as i32, r[1].round() as i32, r[2].round() as i32, r[3].round() as i32))
            .filter(|r| r.3 > 0)
            .collect()
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
    /// The control with keyboard focus, if it belongs to this window's program.
    pub fn focused_in(&self, hwnd: Hwnd) -> Option<Element> {
        unsafe {
            let element = self.auto.GetFocusedElement().ok()?;
            (element.CurrentProcessId().ok()? as u32 == crate::win::pid(hwnd)).then_some(Element(element))
        }
    }

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
    /// `quiet` gets the bounds of controls left out because another line says the same.
    pub fn read(&self, hwnd: Hwnd, all: bool, nodes: &mut Vec<Node>, hidden: &mut usize, quiet: &mut Vec<(i32, i32, i32, i32)>) -> Result<(), String> {
        unsafe {
            let root = self.auto.ElementFromHandle(HWND(hwnd as _)).map_err(say)?;
            let tree = root.BuildUpdatedCache(&self.cache).map_err(say)?;
            walk(&tree, "", 0, None, all, nodes, hidden, 0);
            if !all {
                drop_labels(nodes, quiet);
            }
            Ok(())
        }
    }
}

/// A caption read just before the control it names, or a shell around a control of the same name,
/// says nothing the control's own line does not.
fn drop_labels(nodes: &mut Vec<Node>, quiet: &mut Vec<(i32, i32, i32, i32)>) {
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

/// The texts of an element that holds only data cells, as a grid's row does when it has no row
/// type of its own (WinForms grids), with its first cell. None for anything else.
unsafe fn cells_of(el: &IUIAutomationElement) -> Option<(Vec<String>, IUIAutomationElement)> {
    unsafe {
        let children = el.GetCachedChildren().ok()?;
        let count = children.Length().unwrap_or(0);
        if count < 2 {
            return None;
        }
        let mut cells = Vec::new();
        let first = children.GetElement(0).ok()?;
        for at in 0..count {
            let cell = children.GetElement(at).ok()?;
            if cell.CachedControlType().map(|k| k.0).unwrap_or(0) != 50029 {
                return None;
            }
            let value = cached_text(&cell, UIA_ValueValuePropertyId);
            cells.push(if value.is_empty() { text(cell.CachedName()) } else { value });
        }
        Some((cells, first))
    }
}

/// True if `outer` holds all of `inner`.
pub fn encloses(outer: (i32, i32, i32, i32), inner: (i32, i32, i32, i32)) -> bool {
    outer != inner && outer.0 <= inner.0 && outer.1 <= inner.1 && outer.0 + outer.2 >= inner.0 + inner.2 && outer.1 + outer.3 >= inner.1 + inner.3
}

#[allow(clippy::too_many_arguments)]
unsafe fn walk(
    el: &IUIAutomationElement,
    parent: &str,
    parent_kind: i32,
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
                walk_children(el, &name, kind, row, all, nodes, hidden, depth);
                return;
            }

        let role = lines::role(kind);
        // The arrows of a spinner and the open button of a dropdown, and the shells that only hold
        // them: `set` and `select` do their work. `see -a` still lists them.
        let fitting = !all
            && ((role == Some("button") && matches!(parent_kind, 50003 | 50016))
                || (kind == 50016 && !can.value && !can.range)
                || (kind == 50003 && !can.value && !can.expand && !can.range));
        let listed = match role {
            Some("") => !name.is_empty() && name != parent,
            Some("image") => acts && !name.is_empty(),
            Some(_) => true,
            // Tables and grids get a line even when idle, so `table` has something to name.
            None => acts || scrollable || matches!(kind, 50028 | 50036),
        } && !fitting;
        // A row made only of data cells reads as one line, as the rows of other grids do.
        let cells = if !listed && row.is_none() && !all { cells_of(el) } else { None };
        // The row acts through its first cell, which a click selects as a person's would.
        if let Some((cells, first)) = cells {
            if off {
                *hidden += 1;
            } else {
                nodes.push(Node {
                    runtime: runtime_id(el),
                    identity: format!("{kind}|row|{name}"),
                    control_id: text(el.CachedAutomationId()),
                    parts: Parts { role: "row".into(), name: name.clone(), value: None, states: Vec::new(), cells },
                    rect: el.CachedBoundingRectangle().map(rect_of).unwrap_or_default(),
                    native: first.CachedNativeWindowHandle().map(|h| h.0 as Hwnd).unwrap_or(0),
                    can: Can {
                        invoke: cached_bool(&first, UIA_IsInvokePatternAvailablePropertyId),
                        toggle: cached_bool(&first, UIA_IsTogglePatternAvailablePropertyId),
                        select: cached_bool(&first, UIA_IsSelectionItemPatternAvailablePropertyId),
                        expand: false,
                        value: false,
                        range: false,
                        scroll: false,
                        scroll_item: cached_bool(&first, UIA_IsScrollItemPatternAvailablePropertyId),
                        legacy: cached_bool(&first, UIA_IsLegacyIAccessiblePatternAvailablePropertyId),
                    },
                    element: Element(first.clone()),
                });
            }
            return;
        }

        let mut own = row;
        if listed {
            if off && !all {
                *hidden += 1;
            } else {
                // WinForms hands out window handles as ids, which mean nothing and change every run.
                let control_id = text(el.CachedAutomationId());
                let automation_id = Some(control_id.clone()).filter(|id| !id.bytes().all(|b| b.is_ascii_digit())).unwrap_or_default();
                if lines::is_window_chrome(&automation_id) {
                    walk_children(el, parent, kind, row, all, nodes, hidden, depth);
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
                    control_id,
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
        walk_children(el, carried, kind, own, all, nodes, hidden, depth);
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn walk_children(
    el: &IUIAutomationElement,
    parent: &str,
    parent_kind: i32,
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
                walk(&child, parent, parent_kind, row, all, nodes, hidden, depth + 1);
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

/// A grid that offers no grid pattern, such as a WinForms data grid, read as rows of children:
/// a row whose cells carry no values is the header.
fn rows_of(element: &IUIAutomationElement, skip: usize, rows: usize, columns: usize, auto: &Uia) -> Result<Table, String> {
    unsafe {
        let every = auto.auto.CreateTrueCondition().map_err(say)?;
        let children = |parent: &IUIAutomationElement| -> Vec<IUIAutomationElement> {
            parent.FindAll(TreeScope_Children, &every).ok().map(|list| (0..list.Length().unwrap_or(0)).filter_map(|at| list.GetElement(at).ok()).collect()).unwrap_or_default()
        };
        let value = |el: &IUIAutomationElement| el.GetCurrentPropertyValue(UIA_ValueValuePropertyId).ok().and_then(|v| BSTR::try_from(&v).ok()).map(|s| s.to_string()).unwrap_or_default();
        let mut headers = Vec::new();
        let mut cells = Vec::new();
        let mut total_rows = 0;
        let mut total_columns = 0;
        for row in children(element) {
            // A grid's own scroll bars sit among its rows.
            if row.CurrentControlType().map(|k| k.0).unwrap_or(0) == 50014 {
                continue;
            }
            let parts = children(&row);
            if parts.is_empty() {
                continue;
            }
            let values: Vec<String> = parts.iter().map(&value).collect();
            let names: Vec<String> = parts.iter().map(|part| text(part.CurrentName())).collect();
            // Header cells carry no value, or their own name as one.
            if values.iter().zip(&names).all(|(v, n)| v.is_empty() || v == n) {
                if headers.is_empty() && cells.is_empty() {
                    headers = parts.iter().take(columns).map(|part| text(part.CurrentName())).collect();
                }
                continue;
            }
            total_rows += 1;
            total_columns = total_columns.max(values.len());
            if total_rows > skip && cells.len() < rows {
                cells.push(values.into_iter().take(columns).collect());
            }
        }
        if total_rows == 0 {
            return Err("that control is not a table or grid, or a cell in one".into());
        }
        // Row-header cells carry no values: drop columns that are empty in every row read.
        let empty: Vec<usize> = (0..total_columns).filter(|at| cells.iter().all(|row: &Vec<String>| row.get(*at).is_none_or(String::is_empty))).collect();
        let keep = |at: &usize| !empty.contains(at);
        let cells: Vec<Vec<String>> = cells.into_iter().map(|row| row.into_iter().enumerate().filter(|(at, _)| keep(at)).map(|(_, c)| c).collect()).collect();
        let headers: Vec<String> = headers.into_iter().enumerate().filter(|(at, _)| keep(at)).map(|(_, h)| h).collect();
        Ok(Table { headers, rows: cells, total_rows, total_columns: total_columns - empty.len() })
    }
}

/// The table or grid this control sits in: itself, or one of the few elements above it.
fn table_around(element: &IUIAutomationElement, auto: &Uia) -> Option<IUIAutomationElement> {
    unsafe {
        let walker = auto.auto.ControlViewWalker().ok()?;
        let mut at = element.clone();
        for _ in 0..4 {
            let kind = at.CurrentControlType().map(|k| k.0).unwrap_or(0);
            if matches!(kind, 50028 | 50036) || at.GetCurrentPatternAs::<IUIAutomationGridPattern>(UIA_GridPatternId).is_ok() {
                return Some(at);
            }
            at = walker.GetParentElement(&at).ok()?;
        }
        None
    }
}

pub struct Table {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub total_rows: usize,
    pub total_columns: usize,
}

impl Element {
    /// Where this control's text ends, as a caret's top left on the screen and its height: where
    /// typed text goes in. An empty control puts it at its left edge.
    pub fn text_end(&self) -> Option<(i32, i32, i32)> {
        unsafe {
            let pattern = self.0.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId).ok()?;
            let all = pattern.DocumentRange().ok()?;
            if let Some(&(left, top, width, height)) = rects_of(&all).last() {
                return Some((left + width, top, height));
            }
            let (x, y, _, h) = self.rect().ok()?;
            Some((x + h / 4, y + h / 5, (h * 3 / 5).max(8)))
        }
    }

    /// Where the text caret sits in this control, as its top left on the screen and its height.
    /// The caret is an empty range with no size, so the letter after it is measured, or failing
    /// that, at the end of the text, the letter before it.
    pub fn caret(&self) -> Option<(i32, i32, i32)> {
        unsafe {
            let range = match self.0.GetCurrentPatternAs::<IUIAutomationTextPattern2>(UIA_TextPattern2Id) {
                Ok(newer) => {
                    let mut active = windows::core::BOOL(0);
                    newer.GetCaretRange(&mut active).ok()?
                }
                Err(_) => {
                    let pattern = self.0.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId).ok()?;
                    let chosen = pattern.GetSelection().ok()?;
                    if chosen.Length().ok()? < 1 {
                        return None;
                    }
                    chosen.GetElement(0).ok()?
                }
            };
            let after = range.Clone().ok()?;
            let _ = after.MoveEndpointByRange(TextPatternRangeEndpoint_Start, &range, TextPatternRangeEndpoint_End);
            if after.MoveEndpointByUnit(TextPatternRangeEndpoint_End, TextUnit_Character, 1).unwrap_or(0) == 1
                && let Some(&(left, top, _, height)) = rects_of(&after).first()
            {
                return Some((left, top, height));
            }
            let before = range.Clone().ok()?;
            let _ = before.MoveEndpointByRange(TextPatternRangeEndpoint_End, &range, TextPatternRangeEndpoint_Start);
            before.MoveEndpointByUnit(TextPatternRangeEndpoint_Start, TextUnit_Character, -1).ok()?;
            let &(left, top, width, height) = rects_of(&before).last()?;
            Some((left + width, top, height))
        }
    }

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

    /// `down` is right for a sideways scroll.
    pub fn scroll(&self, down: bool, large: bool, sideways: bool) -> Result<(), String> {
        let amount = match (down, large) {
            (true, true) => ScrollAmount_LargeIncrement,
            (true, false) => ScrollAmount_SmallIncrement,
            (false, true) => ScrollAmount_LargeDecrement,
            (false, false) => ScrollAmount_SmallDecrement,
        };
        unsafe { self.0.GetCurrentPatternAs::<IUIAutomationScrollPattern>(UIA_ScrollPatternId).and_then(|p| if sideways { p.Scroll(amount, ScrollAmount_NoAmount) } else { p.Scroll(ScrollAmount_NoAmount, amount) }).map_err(say) }
    }

    pub fn bring_into_view(&self) -> Result<(), String> {
        unsafe { self.0.GetCurrentPatternAs::<IUIAutomationScrollItemPattern>(UIA_ScrollItemPatternId).and_then(|p| p.ScrollIntoView()).map_err(say) }
    }

    /// What is in a field right now, and nothing else: an empty field gives an empty string.
    pub fn value_text(&self) -> String {
        unsafe { self.0.GetCurrentPropertyValue(UIA_ValueValuePropertyId) }.ok().and_then(|v| BSTR::try_from(&v).ok()).map(|s| s.to_string()).unwrap_or_default()
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

    /// A grid's cells as rows of text, with its column headers, and how many rows and columns it
    /// has in all. Only the first `rows` by `columns` cells are read.
    /// Rows of the table this control is or sits in, after the first `skip`.
    pub fn grid(&self, skip: usize, rows: usize, columns: usize, auto: &Uia) -> Result<Table, String> {
        unsafe {
            // A cell knows its grid, so naming any cell works too.
            let owner = match self.0.GetCurrentPatternAs::<IUIAutomationGridItemPattern>(UIA_GridItemPatternId).and_then(|item| item.CurrentContainingGrid()) {
                Ok(grid) => grid,
                Err(_) => table_around(&self.0, auto).unwrap_or_else(|| self.0.clone()),
            };
            // Some grids keep a cell's text in its value and the column's name in its name.
            let cell = |element: &IUIAutomationElement| {
                let value = element.GetCurrentPropertyValue(UIA_ValueValuePropertyId).ok().and_then(|v| BSTR::try_from(&v).ok()).map(|s| s.to_string()).unwrap_or_default();
                if value.is_empty() { text(element.CurrentName()) } else { value }
            };
            let Ok(grid) = owner.GetCurrentPatternAs::<IUIAutomationGridPattern>(UIA_GridPatternId) else {
                return rows_of(&owner, skip, rows, columns, auto);
            };
            let (total_rows, total_columns) = (grid.CurrentRowCount().map_err(say)? as usize, grid.CurrentColumnCount().map_err(say)? as usize);
            let mut headers = Vec::new();
            if let Ok(table) = owner.GetCurrentPatternAs::<IUIAutomationTablePattern>(UIA_TablePatternId)
                && let Ok(list) = table.GetCurrentColumnHeaders()
            {
                for at in 0..list.Length().unwrap_or(0).min(columns as i32) {
                    if let Ok(header) = list.GetElement(at) {
                        headers.push(cell(&header));
                    }
                }
            }
            let mut cells = Vec::new();
            for row in skip.min(total_rows)..total_rows.min(skip + rows) {
                let mut line = Vec::new();
                for column in 0..total_columns.min(columns) {
                    line.push(grid.GetItem(row as i32, column as i32).map(|item| cell(&item)).unwrap_or_default());
                }
                cells.push(line);
            }
            Ok(Table { headers, rows: cells, total_rows, total_columns })
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
