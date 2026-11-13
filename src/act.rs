// Acting on a control without touching the real mouse or keyboard: the app's own accessibility
// actions first, then Windows messages for classic controls. Each result names the route taken.
use crate::real::{self, Button, Chord};
use crate::uia::{Done, Element, Node, Uia};
use crate::win::{self, Hwnd};
use std::sync::atomic::{AtomicBool, Ordering};
use windows_sys::Win32::{
    Foundation::{GetLastError, HWND},
    UI::{
        Input::KeyboardAndMouse::{MAPVK_VK_TO_VSC, MapVirtualKeyW},
        WindowsAndMessaging::{
            PostMessageW, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_CHAR, WM_CLOSE, WM_COMMAND, WM_KEYDOWN, WM_KEYUP,
            WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDBLCLK, WM_RBUTTONDOWN, WM_RBUTTONUP,
            WM_SETTEXT,
        },
    },
};

pub const NEEDS_REAL: &str = "no background way to do this here: add --real";
pub const NEEDS_MOUSE: &str = "this control only answers the real mouse: try click x,y on it, or --mouse to ask the person to borrow their cursor";
/// Closing one of these takes the person's desktop with it.
const SHELL_WINDOWS: [&str; 4] = ["Progman", "WorkerW", "Shell_TrayWnd", "Shell_SecondaryTrayWnd"];
pub const NO_PEN: &str = "Windows would not take pen input here: --mouse asks the person to borrow their cursor";
const WHEEL_DELTA: i32 = 120;
const ACCESS_DENIED: u32 = 5;
const MK_LBUTTON: usize = 1;
const MK_RBUTTON: usize = 2;
const BN_CLICKED: usize = 0;
const CB_FINDSTRINGEXACT: u32 = 0x0158;
const CB_SETCURSEL: u32 = 0x014E;
const CBN_SELCHANGE: usize = 1;

fn post(hwnd: Hwnd, message: u32, wparam: usize, lparam: isize) -> Result<(), String> {
    if unsafe { PostMessageW(hwnd as HWND, message, wparam, lparam) } != 0 {
        return Ok(());
    }
    Err(if unsafe { GetLastError() } == ACCESS_DENIED {
        "blocked: the window runs as administrator".to_string()
    } else {
        "that window stopped taking messages".to_string()
    })
}

fn route(name: &'static str, done: Done) -> String {
    match done {
        Done::Finished => name.to_string(),
        Done::StillRunning => format!("{name} (the app is still busy, maybe showing a dialog)"),
    }
}

pub fn centre(rect: (i32, i32, i32, i32)) -> (i32, i32) {
    (rect.0 + rect.2 / 2, rect.1 + rect.3 / 2)
}

/// The control's own default action, picking the most direct one it offers.
pub fn press(node: &Node) -> Option<Result<String, String>> {
    let (can, element) = (node.can, &node.element);
    if can.toggle {
        return Some(element.toggle().map(|d| route("toggle", d)));
    }
    if can.invoke {
        return Some(element.invoke().map(|d| route("invoke", d)));
    }
    if can.select {
        return Some(element.select().map(|d| route("select", d)));
    }
    if can.expand {
        return Some(element.open_or_close().map(|d| route("expand", d)));
    }
    if can.legacy {
        return Some(element.default_action().map(|d| route("default", d)));
    }
    None
}

/// A check box or radio button WinForms draws itself: its own default action flips it in place.
/// Accessibility's toggle needs the window active, and WinForms drops a posted click on a spot
/// another window covers.
pub fn flip_self_drawn(node: &Node) -> Option<Result<String, String>> {
    if !matches!(node.parts.role.as_str(), "checkbox" | "radio") || !node.can.legacy || !win::self_drawn_button(node.native) {
        return None;
    }
    Some(node.element.default_action().map(|d| route("default", d)))
}

/// A classic push button with its own window: tell its parent it was clicked. Accessibility would
/// activate the window and wait out any dialog, and a posted click is dropped when it is covered.
pub fn post_button(node: &Node, root: Hwnd) -> Option<Result<String, String>> {
    if node.native == 0 || node.parts.role != "button" || win::draws_itself(node.native) || win::draws_itself(root) {
        return None;
    }
    let (parent, id) = win::parent_and_id(node.native);
    if parent == 0 {
        return None;
    }
    let command = (BN_CLICKED << 16) | (id as usize & 0xFFFF);
    Some(post(parent, WM_COMMAND, command, node.native).map(|_| "command".to_string()))
}

/// Rows of a classic list: a list's accessibility select skips the app's own change event.
pub fn is_classic_row(node: &Node, root: Hwnd) -> bool {
    let (x, y) = centre(node.rect);
    let (hwnd, _, _) = win::child_at(root, x, y);
    let class = win::class(hwnd).to_uppercase();
    node.parts.role == "item" && ["LISTBOX", "SYSLISTVIEW32", "SYSTREEVIEW32"].iter().any(|name| class.contains(name))
}

/// True for a point drawn by an engine of the app's own, where only pen input can reach.
pub fn draws_at(root: Hwnd, x: i32, y: i32) -> bool {
    win::draws_itself(win::child_at(root, x, y).0) || win::draws_itself(root)
}

pub fn post_command(root: Hwnd, id: usize) -> Result<String, String> {
    post(root, WM_COMMAND, id & 0xFFFF, 0).map(|_| "command".to_string())
}

/// A whole press, drag and release as window messages, into the window under the first point.
pub fn post_stroke(root: Hwnd, path: &[(f32, f32)], stop: &AtomicBool, mut moved: impl FnMut(i32, i32)) -> Result<bool, String> {
    let Some(first) = path.first() else { return Ok(true) };
    let (hwnd, x, y) = win::child_at(root, first.0 as i32, first.1 as i32);
    post(hwnd, WM_MOUSEMOVE, 0, win::lparam(x, y))?;
    post(hwnd, WM_LBUTTONDOWN, MK_LBUTTON, win::lparam(x, y))?;
    let mut last = (x, y);
    let finished = real::pace(path, stop, |at, x, y| {
        last = win::to_client(hwnd, x, y);
        let _ = post(hwnd, WM_MOUSEMOVE, MK_LBUTTON, win::lparam(last.0, last.1));
        if at % 3 == 0 {
            moved(x, y);
        }
    });
    post(hwnd, WM_LBUTTONUP, 0, win::lparam(last.0, last.1))?;
    Ok(finished)
}

/// A click as window messages, into the innermost window under the point.
pub fn post_click(root: Hwnd, x: i32, y: i32, right: bool, double: bool) -> Result<String, String> {
    let (hwnd, cx, cy) = win::child_at(root, x, y);
    if win::draws_itself(hwnd) || win::draws_itself(root) {
        return Err(NEEDS_REAL.into());
    }
    let at = win::lparam(cx, cy);
    let (down, up, again, held) = if right {
        (WM_RBUTTONDOWN, WM_RBUTTONUP, WM_RBUTTONDBLCLK, MK_RBUTTON)
    } else {
        (WM_LBUTTONDOWN, WM_LBUTTONUP, WM_LBUTTONDBLCLK, MK_LBUTTON)
    };
    post(hwnd, WM_MOUSEMOVE, 0, at)?;
    post(hwnd, down, held, at)?;
    post(hwnd, up, 0, at)?;
    win::remember_field(hwnd);
    if double {
        post(hwnd, again, held, at)?;
        post(hwnd, up, 0, at)?;
    }
    Ok("message".into())
}

/// A middle click or a triple click, which have no accessibility equivalent.
pub fn post_press(root: Hwnd, x: i32, y: i32, button: Button, times: u32, keys: usize) -> Result<String, String> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{WM_MBUTTONDBLCLK, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_XBUTTONDBLCLK, WM_XBUTTONDOWN, WM_XBUTTONUP};
    const MK_MBUTTON: usize = 0x10;
    const MK_XBUTTON1: usize = 0x20;
    const MK_XBUTTON2: usize = 0x40;
    let (hwnd, cx, cy) = win::child_at(root, x, y);
    if win::draws_itself(hwnd) || win::draws_itself(root) {
        return Err(NEEDS_REAL.into());
    }
    let at = win::lparam(cx, cy);
    let (down, up, again, held, which) = match button {
        Button::Left => (WM_LBUTTONDOWN, WM_LBUTTONUP, WM_LBUTTONDBLCLK, MK_LBUTTON, 0),
        Button::Right => (WM_RBUTTONDOWN, WM_RBUTTONUP, WM_RBUTTONDBLCLK, MK_RBUTTON, 0),
        Button::Middle => (WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MBUTTONDBLCLK, MK_MBUTTON, 0),
        Button::Back => (WM_XBUTTONDOWN, WM_XBUTTONUP, WM_XBUTTONDBLCLK, MK_XBUTTON1, 1usize << 16),
        Button::Forward => (WM_XBUTTONDOWN, WM_XBUTTONUP, WM_XBUTTONDBLCLK, MK_XBUTTON2, 2usize << 16),
    };
    post(hwnd, WM_MOUSEMOVE, 0, at)?;
    for time in 0..times {
        post(hwnd, if time == 1 { again } else { down }, held | which | keys, at)?;
        post(hwnd, up, which | keys, at)?;
    }
    Ok("message".into())
}

/// The pointer resting on a spot, without pressing.
pub fn post_hover(root: Hwnd, x: i32, y: i32) -> Result<String, String> {
    let (hwnd, cx, cy) = win::child_at(root, x, y);
    if win::draws_itself(hwnd) || win::draws_itself(root) {
        return Err(NEEDS_REAL.into());
    }
    post(hwnd, WM_MOUSEMOVE, 0, win::lparam(cx, cy))?;
    Ok("message".into())
}

/// Ask a text box to paste, which needs no keyboard and no focus.
pub fn post_paste(hwnd: Hwnd) -> Result<String, String> {
    const WM_PASTE: u32 = 0x0302;
    post(hwnd, WM_PASTE, 0, 0)?;
    Ok("paste message".into())
}

/// Wheel notches: positive is up, or right for a sideways wheel.
pub fn post_wheel(root: Hwnd, x: i32, y: i32, notches: i32, sideways: bool) -> Result<String, String> {
    const WM_MOUSEHWHEEL: u32 = 0x020E;
    let (hwnd, _, _) = win::child_at(root, x, y);
    if win::draws_itself(hwnd) || win::draws_itself(root) {
        return Err(NEEDS_REAL.into());
    }
    let wheel = ((notches * WHEEL_DELTA) as u32 & 0xFFFF) << 16;
    post(hwnd, if sideways { WM_MOUSEHWHEEL } else { WM_MOUSEWHEEL }, wheel as usize, win::lparam(x, y))?;
    Ok("message".into())
}

/// A long press: the button stays down for `hold` before it lets go.
pub fn post_long_press(root: Hwnd, x: i32, y: i32, right: bool, hold: std::time::Duration) -> Result<String, String> {
    let (hwnd, cx, cy) = win::child_at(root, x, y);
    if win::draws_itself(hwnd) || win::draws_itself(root) {
        return Err(NEEDS_REAL.into());
    }
    let at = win::lparam(cx, cy);
    let (down, up, held) = if right { (WM_RBUTTONDOWN, WM_RBUTTONUP, MK_RBUTTON) } else { (WM_LBUTTONDOWN, WM_LBUTTONUP, MK_LBUTTON) };
    post(hwnd, WM_MOUSEMOVE, 0, at)?;
    post(hwnd, down, held, at)?;
    std::thread::sleep(hold);
    post(hwnd, up, 0, at)?;
    Ok("message".into())
}

fn set_window_text(hwnd: Hwnd, text: &str) -> Result<(), String> {
    let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    let mut result = 0;
    let sent = unsafe { SendMessageTimeoutW(hwnd as HWND, WM_SETTEXT, 0, wide.as_ptr() as isize, SMTO_ABORTIFHUNG, 1000, &mut result) };
    if sent != 0 { Ok(()) } else { Err("that window did not take the text".into()) }
}

/// The classic window behind a control, if it has one of its own.
pub fn native_window(node: &Node, root: Hwnd) -> Option<Hwnd> {
    let hwnd = if node.native != 0 {
        node.native
    } else {
        let (x, y) = centre(node.rect);
        win::child_at(root, x, y).0
    };
    (!win::draws_itself(hwnd) && !win::draws_itself(root)).then_some(hwnd)
}

/// A plain text box. Rich text boxes are left out: replacing their text this way leaves the app
/// unaware of the edit, so it would not offer to save it.
fn is_classic_text(node: &Node, root: Hwnd) -> bool {
    let class = win::class(node.native).to_uppercase();
    node.native != 0 && !win::draws_itself(root) && class.contains("EDIT") && !class.contains("RICH")
}

pub fn set(node: &Node, root: Hwnd, value: &str) -> Result<String, String> {
    // Accessibility would focus a classic text box first, which activates its window.
    if is_classic_text(node, root) {
        win::remember_field(node.native);
        return set_window_text(node.native, value).map(|_| "message".into());
    }
    if node.can.range
        && let Ok(number) = value.parse::<f64>() {
            return node.element.set_range(number).map(|_| "range".into());
        }
    if node.can.value && !node.element.value_read_only() {
        return node.element.set_value(value).map(|_| "value".into());
    }
    Err(NEEDS_REAL.into())
}

/// Characters as window messages, one at a time, where the app's own text handling picks them up.
pub fn post_text(hwnd: Hwnd, text: &str, stop: &AtomicBool) -> Result<String, String> {
    post_text_watched(hwnd, text, stop, |_, _| {})
}

/// Type as window messages, telling `next` before each key which window takes it and what it is.
pub fn post_text_watched(hwnd: Hwnd, text: &str, stop: &AtomicBool, mut next: impl FnMut(Hwnd, u16)) -> Result<String, String> {
    if !win::takes_posted_keys(hwnd) {
        return Err(NEEDS_REAL.into());
    }
    win::remember_field(hwnd);
    for unit in text.encode_utf16() {
        if stop.load(Ordering::Relaxed) {
            return Err("stopped by the person with Ctrl+Alt+Esc".into());
        }
        let started = std::time::Instant::now();
        next(hwnd, unit);
        post(hwnd, WM_CHAR, if unit == 10 { 13 } else { unit as usize }, 1)?;
        std::thread::sleep(crate::motion::KEY_EVERY.saturating_sub(started.elapsed()));
    }
    Ok("message".into())
}

/// One key or shortcut as window messages, to the control with focus in the app's window.
pub fn post_key(root: Hwnd, chord: &Chord, spec: &str) -> Result<String, String> {
    let target = win::keyboard_target(root);
    if !win::takes_posted_keys(target) {
        return Err(NEEDS_REAL.into());
    }
    if chord.modified {
        return post_chord(target, chord, spec);
    }
    let vk = chord.keys[0].0 as u32;
    let scan = unsafe { MapVirtualKeyW(vk, MAPVK_VK_TO_VSC) } as isize;
    post(target, WM_KEYDOWN, vk as usize, 1 | (scan << 16))?;
    // XAML makes the character from the key itself, so it would get Enter twice.
    if matches!(vk, 0x08 | 0x09 | 0x0D | 0x1B | 0x20) && !win::key_site(target) {
        post(target, WM_CHAR, vk as usize, 1 | (scan << 16))?;
    }
    post(target, WM_KEYUP, vk as usize, 1 | (scan << 16) | (1 << 30) | (1 << 31))?;
    Ok("message".into())
}

/// How long an app gets to take each half of a posted shortcut while its Ctrl, Shift or Alt is
/// held down for it.
const CHORD_SETTLE: std::time::Duration = std::time::Duration::from_millis(40);

/// A shortcut such as ctrl+s as window messages. A posted key does not move the keyboard's state,
/// and apps ask that state whether Ctrl is down, so the app's thread is joined to this one for a
/// moment and its Ctrl, Shift or Alt set down while the keys go in. The person's own keyboard is
/// another thread's, and is not touched.
fn post_chord(target: Hwnd, chord: &Chord, spec: &str) -> Result<String, String> {
    use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetKeyboardState, SetKeyboardState, VK_CONTROL, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_MENU, VK_SHIFT,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId, WM_SYSKEYDOWN, WM_SYSKEYUP};
    let Some(((key, extended), held)) = chord.keys.split_last() else { return Err("no key given".into()) };
    let left = |vk: u16| match vk {
        VK_CONTROL => Some(VK_LCONTROL),
        VK_SHIFT => Some(VK_LSHIFT),
        VK_MENU => Some(VK_LMENU),
        _ => None,
    };
    // The Windows key belongs to the shell, and a second ordinary key cannot be held this way.
    if held.iter().any(|(vk, _)| left(*vk).is_none()) {
        return Err(format!("{spec} needs the real keyboard: add --real"));
    }
    let alt = held.iter().any(|(vk, _)| *vk == VK_MENU);
    let ctrl = held.iter().any(|(vk, _)| *vk == VK_CONTROL);
    // Alt on its own makes system keys, which is how menus and Alt shortcuts are found.
    let (down, up) = if alt && !ctrl { (WM_SYSKEYDOWN, WM_SYSKEYUP) } else { (WM_KEYDOWN, WM_KEYUP) };
    let bits = |vk: u16, extended: bool, release: bool| {
        let scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) } as isize;
        let mut lparam = 1 | (scan << 16);
        if extended {
            lparam |= 1 << 24;
        }
        if alt {
            lparam |= 1 << 29;
        }
        if release {
            lparam |= (1 << 30) | (1 << 31);
        }
        lparam
    };
    unsafe {
        let theirs = GetWindowThreadProcessId(target as HWND, std::ptr::null_mut());
        // An app the person is using shares its keyboard state with them: leave that one alone.
        if theirs == 0 || theirs == GetWindowThreadProcessId(GetForegroundWindow(), std::ptr::null_mut()) {
            return Err(format!("{spec} goes to the window in front: add --real"));
        }
        let mine = GetCurrentThreadId();
        if AttachThreadInput(mine, theirs, 1) == 0 {
            return Err(NEEDS_REAL.into());
        }
        let mut saved = [0u8; 256];
        GetKeyboardState(saved.as_mut_ptr());
        let mut state = saved;
        for (vk, _) in held {
            state[*vk as usize] |= 0x80;
            if let Some(side) = left(*vk) {
                state[side as usize] |= 0x80;
            }
        }
        SetKeyboardState(state.as_ptr());
        let sent = (|| {
            for (vk, e) in held {
                post(target, down, *vk as usize, bits(*vk, *e, false))?;
            }
            post(target, down, *key as usize, bits(*key, *extended, false))?;
            std::thread::sleep(CHORD_SETTLE);
            post(target, up, *key as usize, bits(*key, *extended, true))?;
            for (vk, e) in held.iter().rev() {
                post(target, up, *vk as usize, bits(*vk, *e, true))?;
            }
            std::thread::sleep(CHORD_SETTLE);
            Ok::<(), String>(())
        })();
        SetKeyboardState(saved.as_ptr());
        AttachThreadInput(mine, theirs, 0);
        sent?;
    }
    Ok("message".into())
}

pub fn close_window(hwnd: Hwnd) -> Result<(), String> {
    if SHELL_WINDOWS.contains(&win::class(hwnd).as_str()) {
        return Err("refused: that window is the Windows desktop or taskbar".into());
    }
    post(hwnd, WM_CLOSE, 0, 0)
}

/// A classic dropdown takes the entry directly, and is told about it as a click would.
fn select_in_classic_combo(node: &Node, item: &str) -> Option<Result<String, String>> {
    let class = win::class(node.native).to_uppercase();
    if node.native == 0 || !class.contains("COMBOBOX") {
        return None;
    }
    let wide: Vec<u16> = item.encode_utf16().chain(Some(0)).collect();
    let mut index = 0;
    let asked = unsafe {
        SendMessageTimeoutW(node.native as HWND, CB_FINDSTRINGEXACT, usize::MAX, wide.as_ptr() as isize, SMTO_ABORTIFHUNG, 1000, &mut index)
    };
    if asked == 0 {
        return Some(Err("that dropdown did not answer".into()));
    }
    if index as isize == -1 {
        return Some(Err(format!("no entry named '{item}'")));
    }
    let (parent, id) = win::parent_and_id(node.native);
    let result = post(node.native, CB_SETCURSEL, index, 0)
        .and_then(|_| post(parent, WM_COMMAND, (CBN_SELCHANGE << 16) | (id as usize & 0xFFFF), node.native));
    Some(result.map(|_| "message".to_string()))
}

/// Open a dropdown or list, pick the named entry, and close it again.
pub fn select_item(node: &Node, item: &str, uia: &Uia, root: Hwnd) -> Result<String, String> {
    if !win::draws_itself(root) && let Some(result) = select_in_classic_combo(node, item) {
        return result;
    }
    let opened = node.can.expand && node.element.open().is_ok();
    if opened {
        std::thread::sleep(std::time::Duration::from_millis(120));
    }
    let found: Option<Element> = node.element.find_named(item, uia);
    let result = match found {
        Some(entry) => entry.select().map(|_| "select".to_string()),
        None => Err(format!("no entry named '{item}'")),
    };
    if opened {
        let _ = node.element.close();
    }
    result
}
