// Acting on a control without touching the real mouse or keyboard: the app's own accessibility
// actions first, then Windows messages for classic controls. Each result names the route taken.
use crate::real::{self, Button, Chord, Modifiers};
use crate::uia::{Done, Node, Uia};
use crate::win::{self, Hwnd};
use std::sync::atomic::{AtomicBool, Ordering};
use windows_sys::Win32::{
    Foundation::{GetLastError, HWND},
    UI::{
        Input::KeyboardAndMouse::{MAPVK_VK_TO_VSC, MapVirtualKeyW},
        WindowsAndMessaging::{
            PostMessageW, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_CHAR, WM_CLOSE, WM_COMMAND, WM_KEYDOWN, WM_KEYUP,
            WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE,
            WM_MOUSEWHEEL, WM_RBUTTONDBLCLK, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SETTEXT,
        },
    },
};

pub const NEEDS_REAL: &str = "no background way to do this here: add --real";
/// Part of the reply for keys that would go to the app the person has in front.
pub const IN_FRONT: &str = "goes to the window in front";
pub const NEEDS_MOUSE: &str = "this control only answers the real mouse: try click x,y on it, or --mouse to ask the person to borrow their cursor";
const MINIMIZED_WPF: &str = "refused: this WPF window is minimized, and it takes no keys until it is shown again. Windows would not let deskcrew show it off screen without it coming forward just now. Ask the person to restore it, or add --real";
/// Closing or moving one of these takes the person's desktop with it.
const SHELL_WINDOWS: [&str; 4] = ["Progman", "WorkerW", "Shell_TrayWnd", "Shell_SecondaryTrayWnd"];

pub fn is_shell_window(hwnd: Hwnd) -> bool {
    SHELL_WINDOWS.contains(&win::class(hwnd).as_str())
}
pub const NO_PEN: &str = "Windows would not take pen input here: --mouse asks the person to borrow their cursor";
const WHEEL_DELTA: i32 = 120;
const ACCESS_DENIED: u32 = 5;
const MK_LBUTTON: usize = 1;
const MK_RBUTTON: usize = 2;
const MK_MBUTTON: usize = 0x10;
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
    (rect.0.saturating_add(rect.2 / 2), rect.1.saturating_add(rect.3 / 2))
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

static PAGE_WAKE_DEPTH: std::sync::Mutex<std::collections::BTreeMap<Hwnd, usize>> = std::sync::Mutex::new(std::collections::BTreeMap::new());

/// Keeps a background Chromium/Electron/WebView2 window awake for posted clicks/keys, and hands
/// Chromium's internal active state back to the person's foreground Chromium window (if another
/// window of the same browser, e.g. AI Studio covering a Gemini PWA, was in front) once the
/// outermost `PageWake` guard for that foreground window drops.
#[must_use = "bind PageWake to `let _wake = act::wake_page(...)` so the background page stays awake across the action"]
pub struct PageWake {
    prev_chromium_front: Hwnd,
}

impl Drop for PageWake {
    fn drop(&mut self) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{WA_ACTIVE, WM_ACTIVATE};
        if self.prev_chromium_front == 0 {
            return;
        }
        let restore = {
            let mut depths = PAGE_WAKE_DEPTH.lock().unwrap();
            match depths.get_mut(&self.prev_chromium_front) {
                Some(count) if *count > 1 => {
                    *count -= 1;
                    false
                }
                Some(_) => {
                    depths.remove(&self.prev_chromium_front);
                    true
                }
                None => false,
            }
        };
        if restore && win::foreground() == self.prev_chromium_front {
            let host = win::chromium_host(self.prev_chromium_front).unwrap_or(self.prev_chromium_front);
            let mut ignored = 0;
            unsafe {
                SendMessageTimeoutW(
                    host as HWND,
                    WM_ACTIVATE,
                    WA_ACTIVE as usize,
                    0,
                    SMTO_ABORTIFHUNG,
                    300,
                    &mut ignored,
                );
            }
        }
    }
}

/// Give a Chromium/Electron/WebView2/CEF page in the back the keyboard/click target state.
/// Chromium routes keys and mouse events by its own internal Aura focus, not Windows', and gives
/// it to the page when its `Chrome_WidgetWin_1` host receives `WM_ACTIVATE(WA_ACTIVE)`: that
/// message does not change Windows' foreground window or Z-order. Works even when another window
/// of the same browser (e.g. AI Studio covering a Gemini PWA) is in front.
pub fn wake_page(root: Hwnd) -> PageWake {
    use windows_sys::Win32::UI::WindowsAndMessaging::{WA_ACTIVE, WM_ACTIVATE};
    let front = win::foreground();
    if !win::is_chromium(root) || front == root {
        return PageWake { prev_chromium_front: 0 };
    }
    let host = win::chromium_host(root).unwrap_or(root);
    if front == host {
        return PageWake { prev_chromium_front: 0 };
    }
    let _keep = KeepBehind::new(root);
    let _lock = win::is_background(root).then(win::ForegroundLock::take);
    let mut ignored = 0;
    unsafe { SendMessageTimeoutW(host as HWND, WM_ACTIVATE, WA_ACTIVE as usize, 0, SMTO_ABORTIFHUNG, 500, &mut ignored) };
    let prev_chromium_front = if front != 0 && win::is_chromium(front) {
        *PAGE_WAKE_DEPTH.lock().unwrap().entry(front).or_insert(0) += 1;
        front
    } else {
        0
    };
    PageWake { prev_chromium_front }
}

/// How long a Store or WinUI app takes to bring its window forward after an accessibility action
/// runs, which it does on Invoke, Toggle, SelectionItem and ExpandCollapse.
const STORE_ACTIVATION: std::time::Duration = std::time::Duration::from_millis(250);

/// Run an accessibility action on `root`, keeping its app behind regardless of toolkit. A Store or
/// WinUI app such as Calculator, Settings, Photos or the new Notepad comes forward a moment after
/// accessibility acts on one of its controls, and WPF/Chromium/classic apps can try to activate as
/// well: Windows' foreground lock + immediate foreground restoration stops focus steals across all
/// app types, while never yanking focus if the person intentionally switched windows.
pub fn behind<T>(root: Hwnd, work: impl FnOnce() -> T) -> T {
    let front = win::foreground();
    if front == root {
        return work();
    }
    let _keep = KeepBehind::new(root);
    let store = win::is_store(root);
    let lock = win::ForegroundLock::try_take();
    let result = work();
    if lock.is_some() && store {
        std::thread::sleep(STORE_ACTIVATION);
    }
    result
}

/// Press a control of `root`, keeping its app behind.
pub fn press_behind(node: &Node, root: Hwnd) -> Option<Result<String, String>> {
    if !node.can.toggle && !node.can.invoke && !node.can.select && !node.can.expand && !node.can.legacy {
        return None;
    }
    behind(root, || press(node))
}

/// Expand or collapse a control of `root` to `open`, keeping its app behind.
pub fn expand_behind(node: &Node, root: Hwnd, open: bool) -> Result<String, String> {
    behind(root, || node.element.expand(open)).map(|d| route("expand", d))
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

/// True when posted mouse messages do nothing at `child` inside `root`. An embedded Chromium /
/// WebView2 / CEF child takes posted mouse messages directly even when hosted inside WinUI or WPF.
fn mouse_ignored_at(root: Hwnd, child: Hwnd) -> bool {
    if win::is_chromium_class(&win::class(child)) {
        return false;
    }
    win::ignores_posted_mouse(child) || win::ignores_posted_mouse(root)
}

/// True for a point where posted mouse messages do nothing, so only pen input can reach it.
pub fn needs_pen_at(root: Hwnd, x: i32, y: i32) -> bool {
    mouse_ignored_at(root, win::child_at(root, x, y).0)
}

pub fn post_command(root: Hwnd, id: usize) -> Result<String, String> {
    post(root, WM_COMMAND, id & 0xFFFF, 0).map(|_| "command".to_string())
}

/// How long an app gets to take posted messages while keys are held down for it.
const HELD_SETTLE: std::time::Duration = std::time::Duration::from_millis(60);

/// Run `work` with these keys or mouse buttons down in the app's own copy of the keyboard state,
/// for apps that ask that state whether Ctrl or the left button is down rather than reading the
/// message, as WinForms does. The app's thread is joined to this one for those moments; the
/// person's own keyboard and mouse belong to another thread and are not touched. An app whose
/// window is in front shares its state with the person, so it is left alone.
fn with_state_held<T>(hwnd: Hwnd, keys: &[u16], work: impl FnOnce() -> T) -> T {
    use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
    if keys.is_empty() {
        return work();
    }
    unsafe {
        let theirs = GetWindowThreadProcessId(hwnd as HWND, std::ptr::null_mut());
        let mine = GetCurrentThreadId();
        if theirs == 0 || theirs == GetWindowThreadProcessId(GetForegroundWindow(), std::ptr::null_mut()) {
            return work();
        }
        let _lock = win::ForegroundLock::take();
        if AttachThreadInput(mine, theirs, 1) == 0 {
            return work();
        }
        let result = held_down(keys, work);
        AttachThreadInput(mine, theirs, 0);
        result
    }
}

/// Run `work` with these keys down in the keyboard state of the thread this one is joined to,
/// and put that state back after.
fn held_down<T>(keys: &[u16], work: impl FnOnce() -> T) -> T {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetKeyboardState, SetKeyboardState, VK_CONTROL, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_MENU, VK_SHIFT};
    let mut saved = [0u8; 256];
    unsafe { GetKeyboardState(saved.as_mut_ptr()) };
    let mut state = saved;
    for vk in keys {
        state[*vk as usize] |= 0x80;
        let side = match *vk {
            VK_CONTROL => VK_LCONTROL,
            VK_SHIFT => VK_LSHIFT,
            VK_MENU => VK_LMENU,
            other => other,
        };
        state[side as usize] |= 0x80;
    }
    unsafe { SetKeyboardState(state.as_ptr()) };
    let result = work();
    std::thread::sleep(HELD_SETTLE);
    unsafe { SetKeyboardState(saved.as_ptr()) };
    result
}

/// WPF reads posted mouse messages only while its window holds the mouse capture, and asks its
/// keyboard state whether the button is down. Both are given to it while `work` posts them, from
/// this thread joined to the app's, with the foreground lock held so the window stays behind. None
/// when the app is the one in front: its input is the person's.
fn with_capture<T>(hwnd: Hwnd, buttons: &[u16], work: impl FnOnce() -> T) -> Option<T> {
    use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
    use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
    unsafe {
        let theirs = GetWindowThreadProcessId(hwnd as HWND, std::ptr::null_mut());
        let mine = GetCurrentThreadId();
        if theirs == 0 || win::front_thread(hwnd) {
            return None;
        }
        let _lock = win::ForegroundLock::take();
        if AttachThreadInput(mine, theirs, 1) == 0 {
            return None;
        }
        let result = held_down(buttons, || {
            SetCapture(hwnd as HWND);
            let result = work();
            // The messages are taken in turn: the capture has to last until the last one is read.
            std::thread::sleep(HELD_SETTLE);
            ReleaseCapture();
            result
        });
        AttachThreadInput(mine, theirs, 0);
        Some(result)
    }
}

/// WPF in the back takes a posted press or drag while it holds the mouse capture. In front its
/// input is the person's, and the pen is used instead.
pub fn posts_with_capture(root: Hwnd) -> bool {
    win::is_wpf(root) && !win::minimized(root) && !win::front_thread(root)
}

/// A whole press, drag and release as window messages, into the window under the first point,
/// with the chosen button and modifiers down in the app's keyboard state while it lasts.
pub fn post_stroke(
    root: Hwnd,
    path: &[(f32, f32)],
    button: Button,
    modifiers: Modifiers,
    stop: &AtomicBool,
    moved: impl FnMut(i32, i32),
) -> Result<bool, String> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{VK_LBUTTON, VK_MBUTTON, VK_RBUTTON};
    let _keep = KeepBehind::new(root);
    let _lock = win::is_background(root).then(win::ForegroundLock::take);
    let Some(first) = path.first() else { return Ok(true) };
    let hwnd = win::child_at(root, first.0 as i32, first.1 as i32).0;
    let chromium = win::is_chromium_class(&win::class(hwnd)) || win::is_chromium_class(&win::class(root));
    let _wake = chromium.then(|| wake_page(root));
    let button_vk = match button {
        Button::Right => VK_RBUTTON,
        Button::Middle => VK_MBUTTON,
        _ => VK_LBUTTON,
    };
    // Chromium/Electron/WebView2, Qt and Firefox read the button directly from wParam (MK_*).
    // Skipping AttachThreadInput on them avoids activation/titlebar flicker and missed releases.
    let mut held = if win::reads_posted_mouse(hwnd) || win::reads_posted_mouse(root) { Vec::new() } else { vec![button_vk] };
    held.extend(modifiers.virtual_keys());
    if win::is_wpf(root) && !chromium {
        // WPF wakes its mouse on the first message and looks for what is under it on the real
        // screen, where another window covers it: a press then has nothing to go to. A move first,
        // one pixel off, takes that, and the stroke's own first move finds the canvas by itself.
        let (_, cx, cy) = win::child_at(root, first.0 as i32, first.1 as i32);
        let stroke = || {
            post(hwnd, WM_MOUSEMOVE, modifiers.mouse_keys(), win::lparam(cx + 1, cy))?;
            post_stroke_now(root, path, button, modifiers.mouse_keys(), stop, moved)
        };
        with_capture(hwnd, &held, stroke).unwrap_or_else(|| Err(NEEDS_REAL.into()))
    } else {
        with_state_held(hwnd, &held, || post_stroke_now(root, path, button, modifiers.mouse_keys(), stop, moved))
    }
}

fn post_stroke_now(
    root: Hwnd,
    path: &[(f32, f32)],
    button: Button,
    keys: usize,
    stop: &AtomicBool,
    mut moved: impl FnMut(i32, i32),
) -> Result<bool, String> {
    let Some(first) = path.first() else { return Ok(true) };
    let (hwnd, x, y) = win::child_at(root, first.0 as i32, first.1 as i32);
    let (down, up, button_bit) = match button {
        Button::Right => (WM_RBUTTONDOWN, WM_RBUTTONUP, MK_RBUTTON),
        Button::Middle => (WM_MBUTTONDOWN, WM_MBUTTONUP, MK_MBUTTON),
        _ => (WM_LBUTTONDOWN, WM_LBUTTONUP, MK_LBUTTON),
    };
    post(hwnd, WM_MOUSEMOVE, keys, win::lparam(x, y))?;
    post(hwnd, down, button_bit | keys, win::lparam(x, y))?;
    let mut last = (x, y);
    let finished = real::pace(path, stop, |at, x, y| {
        last = win::to_client(hwnd, x, y);
        let _ = post(hwnd, WM_MOUSEMOVE, button_bit | keys, win::lparam(last.0, last.1));
        if at % 3 == 0 {
            moved(x, y);
        }
    });
    post(hwnd, up, keys, win::lparam(last.0, last.1))?;
    Ok(finished)
}

/// Brief frame pause between posted move, press, and release on Chromium so Blink's async
/// hit-testing and Angular/React pointer listeners register background clicks cleanly.
const CHROMIUM_CLICK_STEP: std::time::Duration = std::time::Duration::from_millis(15);

/// A click as window messages, into the innermost window under the point.
pub fn post_click(root: Hwnd, x: i32, y: i32, right: bool, double: bool) -> Result<String, String> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{VK_LBUTTON, VK_RBUTTON};
    let _keep = KeepBehind::new(root);
    let _lock = win::is_background(root).then(win::ForegroundLock::take);
    let (hwnd, cx, cy) = win::child_at(root, x, y);
    let chromium = win::is_chromium_class(&win::class(hwnd)) || win::is_chromium_class(&win::class(root));
    let _wake = chromium.then(|| wake_page(root));
    let wpf_bg = posts_with_capture(root) && !chromium;
    if !wpf_bg && mouse_ignored_at(root, hwnd) {
        return Err(NEEDS_REAL.into());
    }
    let at = win::lparam(cx, cy);
    let (down, up, again, held) = if right {
        (WM_RBUTTONDOWN, WM_RBUTTONUP, WM_RBUTTONDBLCLK, MK_RBUTTON)
    } else {
        (WM_LBUTTONDOWN, WM_LBUTTONUP, WM_LBUTTONDBLCLK, MK_LBUTTON)
    };
    let click = || {
        post(hwnd, WM_MOUSEMOVE, 0, at)?;
        if chromium {
            std::thread::sleep(CHROMIUM_CLICK_STEP);
        }
        post(hwnd, down, held, at)?;
        if chromium {
            std::thread::sleep(CHROMIUM_CLICK_STEP);
        }
        post(hwnd, up, 0, at)?;
        if double {
            if chromium {
                std::thread::sleep(CHROMIUM_CLICK_STEP);
            }
            post(hwnd, again, held, at)?;
            if chromium {
                std::thread::sleep(CHROMIUM_CLICK_STEP);
            }
            post(hwnd, up, 0, at)?;
        }
        Ok::<(), String>(())
    };
    if wpf_bg {
        let vk = if right { VK_RBUTTON } else { VK_LBUTTON };
        with_capture(hwnd, &[vk], || {
            post(hwnd, WM_MOUSEMOVE, 0, win::lparam(cx + 1, cy))?;
            click()
        })
        .unwrap_or_else(|| Err(NEEDS_REAL.into()))?;
    } else if win::is_winforms(hwnd) && !win::minimized(root) && !win::owns_point(&[root], x, y) {
        // WinForms raises Click only where its window really shows, so a covered one is moved off
        // every screen, where nothing covers it, until it has read the click.
        win::uncovered(root, || {
            let sent = click();
            std::thread::sleep(UNCOVERED_READ);
            sent
        })?;
    } else {
        click()?;
    }
    win::remember_field(hwnd);
    Ok("message".into())
}

/// How long a window moved off screen for a click stays there for its app to read the click.
const UNCOVERED_READ: std::time::Duration = std::time::Duration::from_millis(120);

/// A middle click or a triple click, which have no accessibility equivalent, or a click with Shift,
/// Ctrl or Alt held: those are set down in the app's keyboard state too, for apps that ask it.
pub fn post_press(root: Hwnd, x: i32, y: i32, button: Button, times: u32, keys: usize, modifiers: &[u16]) -> Result<String, String> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{VK_LBUTTON, VK_MBUTTON, VK_RBUTTON};
    use windows_sys::Win32::UI::WindowsAndMessaging::{WM_MBUTTONDBLCLK, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_XBUTTONDBLCLK, WM_XBUTTONDOWN, WM_XBUTTONUP};
    const MK_XBUTTON1: usize = 0x20;
    const MK_XBUTTON2: usize = 0x40;
    let _keep = KeepBehind::new(root);
    let _lock = win::is_background(root).then(win::ForegroundLock::take);
    let (hwnd, cx, cy) = win::child_at(root, x, y);
    let chromium = win::is_chromium_class(&win::class(hwnd)) || win::is_chromium_class(&win::class(root));
    let _wake = chromium.then(|| wake_page(root));
    let wpf_bg = posts_with_capture(root) && !chromium;
    if !wpf_bg && mouse_ignored_at(root, hwnd) {
        return Err(NEEDS_REAL.into());
    }
    let at = win::lparam(cx, cy);
    let (down, up, again, button_bit, which) = match button {
        Button::Left => (WM_LBUTTONDOWN, WM_LBUTTONUP, WM_LBUTTONDBLCLK, MK_LBUTTON, 0),
        Button::Right => (WM_RBUTTONDOWN, WM_RBUTTONUP, WM_RBUTTONDBLCLK, MK_RBUTTON, 0),
        Button::Middle => (WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MBUTTONDBLCLK, MK_MBUTTON, 0),
        Button::Back => (WM_XBUTTONDOWN, WM_XBUTTONUP, WM_XBUTTONDBLCLK, MK_XBUTTON1, 1usize << 16),
        Button::Forward => (WM_XBUTTONDOWN, WM_XBUTTONUP, WM_XBUTTONDBLCLK, MK_XBUTTON2, 2usize << 16),
    };
    let send = || {
        post(hwnd, WM_MOUSEMOVE, keys, at)?;
        for time in 0..times {
            if chromium {
                std::thread::sleep(CHROMIUM_CLICK_STEP);
            }
            post(hwnd, if time == 1 { again } else { down }, button_bit | which | keys, at)?;
            if chromium {
                std::thread::sleep(CHROMIUM_CLICK_STEP);
            }
            post(hwnd, up, which | keys, at)?;
        }
        Ok("message".into())
    };
    if wpf_bg {
        let mut held = vec![match button {
            Button::Right => VK_RBUTTON,
            Button::Middle => VK_MBUTTON,
            _ => VK_LBUTTON,
        }];
        held.extend_from_slice(modifiers);
        with_capture(hwnd, &held, || {
            post(hwnd, WM_MOUSEMOVE, keys, win::lparam(cx + 1, cy))?;
            send()
        })
        .unwrap_or_else(|| Err(NEEDS_REAL.into()))
    } else if win::is_winforms(hwnd) && !win::minimized(root) && !win::owns_point(&[root], x, y) {
        win::uncovered(root, || {
            let sent = with_state_held(hwnd, modifiers, send);
            std::thread::sleep(UNCOVERED_READ);
            sent
        })
    } else {
        with_state_held(hwnd, modifiers, send)
    }
}

/// The pointer resting on a spot, without pressing.
pub fn post_hover(root: Hwnd, x: i32, y: i32) -> Result<String, String> {
    let _keep = KeepBehind::new(root);
    let _lock = win::is_background(root).then(win::ForegroundLock::take);
    let (hwnd, cx, cy) = win::child_at(root, x, y);
    if mouse_ignored_at(root, hwnd) {
        return Err(NEEDS_REAL.into());
    }
    post(hwnd, WM_MOUSEMOVE, 0, win::lparam(cx, cy))?;
    Ok("message".into())
}

/// Guard that restores the previous foreground window if `root` sneaks to the front during a
/// background action — unless the person moved/clicked/typed or is actively switching apps.
struct KeepBehind {
    root: Hwnd,
    front: Hwnd,
    input_before: u32,
}

impl KeepBehind {
    fn new(hwnd: Hwnd) -> Self {
        use windows_sys::Win32::UI::WindowsAndMessaging::{GA_ROOT, GetAncestor};
        let resolved = unsafe { GetAncestor(hwnd as HWND, GA_ROOT) } as Hwnd;
        let root = if resolved != 0 { resolved } else { hwnd };
        Self { root, front: win::foreground(), input_before: real::last_input_tick() }
    }
}

impl Drop for KeepBehind {
    fn drop(&mut self) {
        restore_if_stole(self.root, self.front, self.input_before);
    }
}

/// If a background app sneaked into the foreground during a posted message or shortcut, put the
/// person's window right back in front, lower the intruder behind it, and cancel any taskbar flash.
/// Skips restoration if the person interacted (clicked, typed, or held Alt/Win) during the action,
/// so intentional app switches are never undone.
fn restore_if_stole(root: Hwnd, front: Hwnd, input_before: u32) {
    if front != 0 && !win::person_switching() && real::last_input_tick() == input_before {
        let now = win::foreground();
        if now != front && win::same_app(now, root) {
            if !win::bring_forward(front) {
                win::stop_flashing(front);
            }
            win::lower_behind(root, front);
            if now != root {
                win::lower_behind(now, front);
            }
        }
    }
    win::stop_flashing(root);
}

/// Ask a text box to paste, which needs no keyboard and no focus.
pub fn post_paste(hwnd: Hwnd) -> Result<String, String> {
    const WM_PASTE: u32 = 0x0302;
    let _keep = KeepBehind::new(hwnd);
    let _lock = win::is_background(hwnd).then(win::ForegroundLock::take);
    post(hwnd, WM_PASTE, 0, 0)?;
    Ok("paste message".into())
}

/// Wheel notches: positive is up, or right for a sideways wheel.
pub fn post_wheel(root: Hwnd, x: i32, y: i32, notches: i32, sideways: bool) -> Result<String, String> {
    const WM_MOUSEHWHEEL: u32 = 0x020E;
    let _keep = KeepBehind::new(root);
    let _lock = win::is_background(root).then(win::ForegroundLock::take);
    let (hwnd, _, _) = win::child_at(root, x, y);
    let chromium = win::is_chromium_class(&win::class(hwnd)) || win::is_chromium_class(&win::class(root));
    let _wake = chromium.then(|| wake_page(root));
    if mouse_ignored_at(root, hwnd) {
        return Err(NEEDS_REAL.into());
    }
    let wheel = ((notches * WHEEL_DELTA) as u32 & 0xFFFF) << 16;
    post(hwnd, if sideways { WM_MOUSEHWHEEL } else { WM_MOUSEWHEEL }, wheel as usize, win::lparam(x, y))?;
    Ok("message".into())
}

/// A long press: the button stays down for `hold` before it lets go.
pub fn post_long_press(root: Hwnd, x: i32, y: i32, right: bool, hold: std::time::Duration) -> Result<String, String> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{VK_LBUTTON, VK_RBUTTON};
    let _keep = KeepBehind::new(root);
    let _lock = win::is_background(root).then(win::ForegroundLock::take);
    let (hwnd, cx, cy) = win::child_at(root, x, y);
    let chromium = win::is_chromium_class(&win::class(hwnd)) || win::is_chromium_class(&win::class(root));
    let _wake = chromium.then(|| wake_page(root));
    let wpf_bg = posts_with_capture(root) && !chromium;
    if !wpf_bg && mouse_ignored_at(root, hwnd) {
        return Err(NEEDS_REAL.into());
    }
    let at = win::lparam(cx, cy);
    let (down, up, held) = if right { (WM_RBUTTONDOWN, WM_RBUTTONUP, MK_RBUTTON) } else { (WM_LBUTTONDOWN, WM_LBUTTONUP, MK_LBUTTON) };
    let send = || {
        post(hwnd, WM_MOUSEMOVE, 0, at)?;
        if chromium {
            std::thread::sleep(CHROMIUM_CLICK_STEP);
        }
        post(hwnd, down, held, at)?;
        std::thread::sleep(hold);
        post(hwnd, up, 0, at)?;
        Ok::<(), String>(())
    };
    if wpf_bg {
        let vk = if right { VK_RBUTTON } else { VK_LBUTTON };
        with_capture(hwnd, &[vk], || {
            post(hwnd, WM_MOUSEMOVE, 0, win::lparam(cx + 1, cy))?;
            send()
        })
        .unwrap_or_else(|| Err(NEEDS_REAL.into()))?;
    } else {
        send()?;
    }
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

/// A rich text box with a window of its own, such as WordPad's or a WinForms RichTextBox.
pub fn is_rich_text(hwnd: Hwnd) -> bool {
    hwnd != 0 && win::class(hwnd).to_uppercase().contains("RICHEDIT")
}

/// A key's release, which has nowhere to go once its press closed the window, as Escape closes a
/// dialog, and needs nowhere then.
fn release(hwnd: Hwnd, message: u32, wparam: usize, lparam: isize) -> Result<(), String> {
    post(hwnd, message, wparam, lparam).or_else(|e| if win::exists(hwnd) { Err(e) } else { Ok(()) })
}

fn send(hwnd: Hwnd, message: u32, wparam: usize, lparam: isize) -> Result<isize, String> {
    let mut result = 0;
    let sent = unsafe { SendMessageTimeoutW(hwnd as HWND, message, wparam, lparam, SMTO_ABORTIFHUNG, 1000, &mut result) };
    if sent != 0 {
        return Ok(result as isize);
    }
    Err(if unsafe { GetLastError() } == ACCESS_DENIED {
        "blocked: the window runs as administrator".to_string()
    } else {
        "that window stopped taking messages".to_string()
    })
}

/// Put a text box's caret after its last letter, so typing adds to the end.
pub fn caret_to_end(hwnd: Hwnd) -> Result<(), String> {
    const WM_GETTEXTLENGTH: u32 = 0x000E;
    const EM_SETSEL: u32 = 0x00B1;
    let length = send(hwnd, WM_GETTEXTLENGTH, 0, 0)?.max(0) as usize;
    send(hwnd, EM_SETSEL, length, length as isize).map(|_| ())
}

/// Replace all the text of a rich text box as an edit of its own, the way typing over it would:
/// the app is told the text changed and that there are changes to save, and can undo it.
fn replace_rich_text(hwnd: Hwnd, value: &str) -> Result<String, String> {
    const EM_SETSEL: u32 = 0x00B1;
    const EM_REPLACESEL: u32 = 0x00C2;
    send(hwnd, EM_SETSEL, 0, -1)?;
    let wide: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
    send(hwnd, EM_REPLACESEL, 1, wide.as_ptr() as isize)?;
    win::remember_field(hwnd);
    Ok("message".into())
}

pub fn set(node: &Node, root: Hwnd, value: &str) -> Result<String, String> {
    if is_rich_text(node.native) && !win::draws_itself(root) {
        return replace_rich_text(node.native, value);
    }
    // Accessibility would focus a classic text box first, which activates its window.
    if is_classic_text(node, root) {
        win::remember_field(node.native);
        return set_window_text(node.native, value).map(|_| "message".into());
    }
    if node.can.range
        && let Ok(number) = value.parse::<f64>()
    {
        return behind(root, || node.element.set_range(number)).map(|_| "range".into());
    }
    if node.can.value && !node.element.value_read_only() {
        return behind(root, || node.element.set_value(value)).map(|_| "value".into());
    }
    Err(NEEDS_REAL.into())
}

/// Characters as window messages, one at a time, where the app's own text handling picks them up.
pub fn post_text(hwnd: Hwnd, text: &str, stop: &AtomicBool) -> Result<String, String> {
    post_text_watched(hwnd, text, stop, |_, _| {})
}

/// Type as window messages, telling `next` before each key which window takes it and what it is.
pub fn post_text_watched(hwnd: Hwnd, text: &str, stop: &AtomicBool, next: impl FnMut(Hwnd, u16)) -> Result<String, String> {
    let _keep = KeepBehind::new(hwnd);
    let _lock = win::is_background(hwnd).then(win::ForegroundLock::take);
    if !win::takes_posted_keys(hwnd) {
        return Err(NEEDS_REAL.into());
    }
    win::remember_field(hwnd);
    post_chars(hwnd, text, stop, next)
}

fn post_chars(hwnd: Hwnd, text: &str, stop: &AtomicBool, mut next: impl FnMut(Hwnd, u16)) -> Result<String, String> {
    // WPF checks keyboard focus when dispatching posted WM_CHAR messages: if focus leaves right
    // after PostMessageW, trailing characters still in the queue are dropped. Sending WM_CHAR
    // synchronously delivers every character while focus is held, while newlines in WPF controls
    // require a posted Return keydown/keyup.
    const VK_RETURN: usize = 0x0D;
    let sync = win::is_wpf(hwnd);
    let mut units = text.encode_utf16().peekable();
    while let Some(unit) = units.next() {
        if stop.load(Ordering::Relaxed) {
            return Err("stopped by the person with Ctrl+Alt+Esc".into());
        }
        let started = std::time::Instant::now();
        next(hwnd, unit);
        if sync && (unit == 10 || (unit == 13 && units.peek() == Some(&10))) {
            if unit == 13 {
                units.next();
            }
            let scan = unsafe { MapVirtualKeyW(VK_RETURN as u32, MAPVK_VK_TO_VSC) } as isize;
            send(hwnd, WM_KEYDOWN, VK_RETURN, 1 | (scan << 16))?;
            let _ = send(hwnd, WM_KEYUP, VK_RETURN, 1 | (scan << 16) | (1 << 30) | (1 << 31));
        } else {
            let ch = if unit == 10 { 13 } else { unit as usize };
            if sync {
                send(hwnd, WM_CHAR, ch, 1)?;
            } else {
                post(hwnd, WM_CHAR, ch, 1)?;
            }
        }
        std::thread::sleep(crate::motion::KEY_EVERY.saturating_sub(started.elapsed()));
    }
    Ok("message".into())
}

/// Direct classic Edit/RichEdit clipboard and selection messages that work completely in the
/// background with zero `AttachThreadInput`, zero keyboard-state mutation, and zero focus change.
fn direct_edit_shortcut(target: Hwnd, chord: &Chord) -> Option<Result<String, String>> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::VK_CONTROL;
    const WM_CUT: u32 = 0x0300;
    const WM_COPY: u32 = 0x0301;
    const WM_PASTE: u32 = 0x0302;
    const WM_UNDO: u32 = 0x0304;
    const EM_SETSEL: u32 = 0x00B1;
    if !chord.modified || chord.keys.len() != 2 || chord.keys[0].0 != VK_CONTROL {
        return None;
    }
    let raw_class = win::class(target);
    let class = raw_class.to_uppercase();
    if win::draws_itself(target) || win::is_chromium_class(&raw_class) || !(class.contains("EDIT") || class.contains("COMBO")) {
        return None;
    }
    match chord.keys[1].0 as u8 {
        b'C' => Some(send(target, WM_COPY, 0, 0).map(|_| "copy message".into())),
        b'X' => Some(send(target, WM_CUT, 0, 0).map(|_| "cut message".into())),
        b'V' => Some(post(target, WM_PASTE, 0, 0).map(|_| "paste message".into())),
        b'Z' => Some(send(target, WM_UNDO, 0, 0).map(|_| "undo message".into())),
        b'A' if class.contains("EDIT") => Some(send(target, EM_SETSEL, 0, -1).map(|_| "select-all message".into())),
        _ => None,
    }
}

/// One key or shortcut as window messages, to the control with focus in the app's window.
pub fn post_key(root: Hwnd, chord: &Chord, spec: &str) -> Result<String, String> {
    let _keep = KeepBehind::new(root);
    let _lock = win::is_background(root).then(win::ForegroundLock::take);
    if chord.has_win() {
        return Err(format!("{spec} needs the real keyboard: add --real"));
    }
    // WPF reads keys only while its window has keyboard focus, which a minimized one cannot get.
    if win::is_wpf(root) && win::minimized(root) {
        return Err(MINIMIZED_WPF.into());
    }
    if win::keys_need_focus(root) {
        return post_chord(win::key_window(root), chord, spec, true);
    }
    let target = win::keyboard_target(root);
    if let Some(direct) = direct_edit_shortcut(target, chord) {
        return direct;
    }
    if !win::takes_posted_keys(target) {
        return Err(NEEDS_REAL.into());
    }
    if chord.modified {
        return post_chord(target, chord, spec, false);
    }
    let vk = chord.keys[0].0 as u32;
    let scan = unsafe { MapVirtualKeyW(vk, MAPVK_VK_TO_VSC) } as isize;
    post(target, WM_KEYDOWN, vk as usize, 1 | (scan << 16))?;
    // XAML makes the character from the key itself, so it would get Enter twice.
    if matches!(vk, 0x08 | 0x09 | 0x0D | 0x1B | 0x20) && !win::key_site(target) {
        post(target, WM_CHAR, vk as usize, 1 | (scan << 16))?;
    }
    release(target, WM_KEYUP, vk as usize, 1 | (scan << 16) | (1 << 30) | (1 << 31))?;
    Ok("message".into())
}

/// Pick an entry of an open classic popup menu, with the app in the back, where a click on the
/// menu needs the real mouse. The menu is closed and its owner gets the entry's command, as it
/// would from a real pick: a menu opened while its app is in the back does not take posted keys.
/// The arrow keys and Enter are left for an entry with no command of its own. None if the menu
/// has no entry by that name.
pub fn choose_in_popup(popup: Hwnd, label: &str) -> Option<Result<String, String>> {
    let entries = win::menu_entries(win::menu_of_popup(popup));
    let wanted = label.to_lowercase();
    let at = entries.iter().position(|entry| entry.label.to_lowercase() == wanted)?;
    if entries[at].greyed {
        return Some(Err(format!("'{}' is disabled", entries[at].label)));
    }
    let entry = &entries[at];
    if entry.submenu == 0
        && let Some(owner) = win::menu_owner(popup)
    {
        const WM_CANCELMODE: u32 = 0x001F;
        let mut ignored = 0;
        unsafe { SendMessageTimeoutW(owner as _, WM_CANCELMODE, 0, 0, SMTO_ABORTIFHUNG, 500, &mut ignored) };
        // A command that arrives while the app is still in its menu is swallowed by it.
        let until = std::time::Instant::now() + std::time::Duration::from_millis(1000);
        while win::in_menu(owner) && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if win::in_menu(owner) {
            return Some(Err("the menu would not close: try key escape, then another way".into()));
        }
        return Some(post(owner, WM_COMMAND, entry.command & 0xFFFF, 0).map(|_| "menu command".to_string()));
    }
    const VK_DOWN: usize = 0x28;
    const VK_RETURN: usize = 0x0D;
    let key = |vk: usize, release: bool| {
        let scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) } as isize;
        1 | (scan << 16) | if release { (1 << 30) | (1 << 31) } else { 0 }
    };
    let pressed = (|| {
        // Separators are skipped by the arrow keys, and menu_entries leaves them out too.
        for _ in 0..=at {
            post(popup, WM_KEYDOWN, VK_DOWN, key(VK_DOWN, false))?;
            post(popup, WM_KEYUP, VK_DOWN, key(VK_DOWN, true))?;
        }
        post(popup, WM_KEYDOWN, VK_RETURN, key(VK_RETURN, false))?;
        post(popup, WM_KEYUP, VK_RETURN, key(VK_RETURN, true))?;
        Ok("menu keys".to_string())
    })();
    Some(pressed)
}

/// How long a held key waits before it repeats, and how often it repeats then, as a keyboard does.
const REPEAT_DELAY: std::time::Duration = std::time::Duration::from_millis(500);
const REPEAT_EVERY: std::time::Duration = std::time::Duration::from_millis(33);

/// A key held down for a while as window messages: down, repeats as a keyboard sends them, then up,
/// with it set down in the app's keyboard state the whole time for apps that ask that state.
pub fn post_hold(root: Hwnd, chord: &Chord, hold: std::time::Duration, stop: &AtomicBool) -> Result<String, String> {
    let _keep = KeepBehind::new(root);
    if chord.has_win() {
        return Err("holding the Windows key needs the real keyboard: add --real".into());
    }
    if win::is_wpf(root) && win::minimized(root) {
        return Err(MINIMIZED_WPF.into());
    }
    let all: Vec<u16> = chord.keys.iter().map(|(vk, _)| *vk).collect();
    // WPF, Qt and Firefox take keys only with focus inside their own app, held for the whole time.
    if win::keys_need_focus(root) {
        let window = win::key_window(root);
        attached(window, "a held key", true, || held_down(&all, || hold_keys(window, chord, hold, stop)))
    } else {
        let target = win::keyboard_target(root);
        if !win::takes_posted_keys(target) {
            return Err(NEEDS_REAL.into());
        }
        with_state_held(target, &all, || hold_keys(target, chord, hold, stop))
    }
}

fn hold_keys(target: Hwnd, chord: &Chord, hold: std::time::Duration, stop: &AtomicBool) -> Result<String, String> {
    let Some(((key, extended), modifiers)) = chord.keys.split_last() else { return Err("no key given".into()) };
    let bits = |vk: u16, extended: bool, again: bool, release: bool| {
        let scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) } as isize;
        let mut lparam = 1 | (scan << 16);
        if extended {
            lparam |= 1 << 24;
        }
        if again {
            lparam |= 1 << 30;
        }
        if release {
            lparam |= (1 << 30) | (1 << 31);
        }
        lparam
    };
    for (vk, e) in modifiers {
        post(target, WM_KEYDOWN, *vk as usize, bits(*vk, *e, false, false))?;
    }
    post(target, WM_KEYDOWN, *key as usize, bits(*key, *extended, false, false))?;
    let started = std::time::Instant::now();
    let mut next = REPEAT_DELAY;
    while started.elapsed() < hold {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        if started.elapsed() >= next {
            post(target, WM_KEYDOWN, *key as usize, bits(*key, *extended, true, false))?;
            next += REPEAT_EVERY;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    release(target, WM_KEYUP, *key as usize, bits(*key, *extended, false, true))?;
    for (vk, e) in modifiers.iter().rev() {
        release(target, WM_KEYUP, *vk as usize, bits(*vk, *e, false, true))?;
    }
    Ok("message".to_string())
}

/// How long an app gets to take each half of a posted shortcut while its Ctrl, Shift or Alt is
/// held down for it.
const CHORD_SETTLE: std::time::Duration = std::time::Duration::from_millis(40);

/// A shortcut such as ctrl+s as window messages. A posted key does not move the keyboard's state,
/// and apps ask that state whether Ctrl is down, so the app's thread is joined to this one for a
/// moment and its Ctrl, Shift or Alt set down while the keys go in. The person's own keyboard is
/// another thread's, and is not touched.
///
/// With `focus`, the window is also given keyboard focus inside its own app for those moments, as
/// WPF needs. That makes it the active window of its app only: the lock keeps Windows from
/// bringing it forward, and the person's window keeps their keys.
fn post_chord(target: Hwnd, chord: &Chord, spec: &str, focus: bool) -> Result<String, String> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetKeyboardState, SetKeyboardState, VK_CONTROL, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_MENU, VK_SHIFT};
    use windows_sys::Win32::UI::WindowsAndMessaging::{WM_SYSKEYDOWN, WM_SYSKEYUP};
    let Some(((key, extended), held)) = chord.keys.split_last() else { return Err("no key given".into()) };
    let left = |vk: u16| match vk {
        VK_CONTROL => Some(VK_LCONTROL),
        VK_SHIFT => Some(VK_LSHIFT),
        VK_MENU => Some(VK_LMENU),
        _ => None,
    };
    if chord.has_win() {
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
    attached(target, spec, focus, || unsafe {
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
            release(target, up, *key as usize, bits(*key, *extended, true))?;
            for (vk, e) in held.iter().rev() {
                release(target, up, *vk as usize, bits(*vk, *e, true))?;
            }
            std::thread::sleep(CHORD_SETTLE);
            Ok::<(), String>(())
        })();
        SetKeyboardState(saved.as_ptr());
        sent
    })?;
    Ok("message".into())
}

/// The windows `attached` has given focus to, and how long one given focus for the first time gets.
static FOCUSED: std::sync::Mutex<std::collections::BTreeSet<Hwnd>> = std::sync::Mutex::new(std::collections::BTreeSet::new());
const FIRST_FOCUS_SETTLE: std::time::Duration = std::time::Duration::from_millis(100);

/// Run `work` with this thread's input joined to the app's, so it shares the app's keyboard state.
/// With `focus`, the window also gets keyboard focus inside its own app for that moment, with the
/// foreground lock held so it stays behind. It keeps that focus after, unless another of the app's
/// windows had it: Windows cannot make the app inactive again, and a window left active with nothing
/// focused takes none of the person's keys when they bring it forward, since Windows sees it already
/// active and does not focus it again.
fn attached<T>(target: Hwnd, spec: &str, focus: bool, work: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetFocus, SetFocus};
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
    unsafe {
        let theirs = GetWindowThreadProcessId(target as HWND, std::ptr::null_mut());
        // An app the person is using shares its thread keyboard state with them (including
        // other browser/PWA windows on the same UI thread): never mutate its keyboard state.
        if theirs == 0 || theirs == GetWindowThreadProcessId(GetForegroundWindow(), std::ptr::null_mut()) {
            return Err(format!("{spec} {IN_FRONT}: add --real"));
        }
        let _lock = focus.then(win::ForegroundLock::take);
        let mine = GetCurrentThreadId();
        if AttachThreadInput(mine, theirs, 1) == 0 {
            return Err(NEEDS_REAL.into());
        }
        let before = GetFocus();
        if focus {
            SetFocus(target as HWND);
            if GetFocus() != target as HWND {
                AttachThreadInput(mine, theirs, 0);
                return Err(NEEDS_REAL.into());
            }
            // A window focused for the first time, such as a dialog just opened in the back, picks
            // the control that takes its keys a moment later. Qt drops a key that comes before.
            if FOCUSED.lock().unwrap().insert(target) {
                std::thread::sleep(FIRST_FOCUS_SETTLE);
            }
        }
        let done = work();
        // WPF keeps no focused window after, as it always has, and takes the person's keys anyway
        // once they bring it forward.
        if focus && (!before.is_null() || win::is_wpf(target)) {
            SetFocus(before);
        }
        AttachThreadInput(mine, theirs, 0);
        done
    }
}

/// Characters for an app that reads keys only while it has focus inside itself (WPF, Qt and
/// Firefox), sent while it has that focus for the moment. They go to whatever the app last had
/// focused, in a dialog it opened if there is one.
pub fn post_text_focused(root: Hwnd, text: &str, stop: &AtomicBool) -> Result<String, String> {
    if win::is_wpf(root) && win::minimized(root) {
        return Err(MINIMIZED_WPF.into());
    }
    let window = win::key_window(root);
    attached(window, "typing", true, || post_chars(window, text, stop, |_, _| {}))
}

/// Put the caret in a WPF text box in the back, as a click on it would: the window gets keyboard
/// focus inside its own app for a moment, with the foreground lock held so it stays behind, and
/// the box is focused through accessibility then. WPF keeps it as the focused box, so the keys an
/// agent sends later go to it.
pub fn focus_wpf_field(root: Hwnd, node: &Node) -> Result<String, String> {
    use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetFocus, SetFocus};
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
    if win::is_wpf(root) && win::minimized(root) {
        return Err(MINIMIZED_WPF.into());
    }
    let window = win::key_window(root);
    let front = win::foreground();
    if front == root || front == window {
        return if node.element.set_focus().is_ok() {
            Ok("focus".into())
        } else {
            Err(NEEDS_REAL.into())
        };
    }
    unsafe {
        let theirs = GetWindowThreadProcessId(window as HWND, std::ptr::null_mut());
        if theirs == 0 || theirs == GetWindowThreadProcessId(GetForegroundWindow(), std::ptr::null_mut()) {
            return Err(NEEDS_REAL.into());
        }
        let _lock = win::ForegroundLock::take();
        let mine = GetCurrentThreadId();
        if AttachThreadInput(mine, theirs, 1) == 0 {
            return Err(NEEDS_REAL.into());
        }
        SetFocus(window as HWND);
        let focused = GetFocus() == window as HWND && node.element.set_focus().is_ok();
        SetFocus(std::ptr::null_mut());
        AttachThreadInput(mine, theirs, 0);
        // Should the app have come forward all the same, the person gets their window back.
        let now = win::foreground();
        if front != 0 && now != front && win::same_app(now, root) && !win::bring_forward(front) {
            win::stop_flashing(front);
        }
        if !focused {
            return Err(NEEDS_REAL.into());
        }
        Ok("focus".into())
    }
}

fn run_sta<T: Send>(work: impl FnOnce() -> Result<T, String> + Send) -> Result<T, String> {
    std::thread::scope(|s| {
        s.spawn(|| unsafe {
            use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
            let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let result = work();
            if hr.is_ok() {
                CoUninitialize();
            }
            result
        })
        .join()
        .unwrap_or_else(|_| Err("shell call failed".into()))
    })
}

fn shell_dispatch<I: windows::core::Interface>(work: impl FnOnce(&I) -> windows::core::Result<()> + Send) -> Result<(), String> {
    run_sta(|| unsafe {
        use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
        use windows::Win32::UI::Shell::Shell;
        let shell: I = CoCreateInstance(&Shell, None, CLSCTX_INPROC_SERVER).map_err(|e| format!("shell COM refused: {e}"))?;
        work(&shell).map_err(|e| format!("shell action failed: {e}"))
    })
}

fn windows_dir_path(windows_root: bool) -> Result<String, String> {
    use windows_sys::Win32::System::SystemInformation::{GetSystemDirectoryW, GetWindowsDirectoryW};
    let query = |buf: &mut [u16]| unsafe {
        if windows_root {
            GetWindowsDirectoryW(buf.as_mut_ptr(), buf.len() as u32)
        } else {
            GetSystemDirectoryW(buf.as_mut_ptr(), buf.len() as u32)
        }
    };
    let mut buf = vec![0u16; 260];
    let mut len = query(&mut buf);
    if len as usize >= buf.len() {
        buf.resize(len as usize + 1, 0);
        len = query(&mut buf);
    }
    if len == 0 || len as usize >= buf.len() {
        return Err("Windows would not report its system folder".into());
    }
    Ok(String::from_utf16_lossy(&buf[..len as usize]))
}

fn system_file(windows_root: bool, name: &str) -> Result<String, String> {
    Ok(format!("{}\\{name}", windows_dir_path(windows_root)?.trim_end_matches('\\')))
}

fn shell_launch(file: &str) -> Result<(), String> {
    let file = file.to_string();
    let dir = windows_dir_path(false)?;
    run_sta(move || {
        let wide = |t: &str| t.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
        let (verb, target, work_dir) = (wide("open"), wide(&file), wide(&dir));
        let code = unsafe {
            windows_sys::Win32::UI::Shell::ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                target.as_ptr(),
                std::ptr::null(),
                work_dir.as_ptr(),
                windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
            )
        };
        if code as isize > 32 { Ok(()) } else { Err(format!("Windows could not open '{file}'")) }
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WinShortcut {
    Explorer,
    ToggleDesktop,
    MinimizeAll,
    UndoMinimizeAll,
    RunDialog,
    TaskView,
    Search,
    Settings,
    Accessibility,
    About,
    ActionCenter,
    Project,
    Cast,
    ScreenSnip,
    GameBar,
    OnScreenKeyboard,
    Magnifier,
    Lock,
    MaximizeWindow,
    MinimizeOrRestoreWindow,
    SnapLeft,
    SnapRight,
    PrevMonitor,
    NextMonitor,
}

impl WinShortcut {
    pub fn from_chord(chord: &Chord) -> Option<Self> {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
            VK_A, VK_D, VK_DOWN, VK_E, VK_G, VK_I, VK_K, VK_L, VK_LEFT, VK_M, VK_O, VK_OEM_PLUS, VK_P, VK_PAUSE, VK_Q, VK_R,
            VK_RIGHT, VK_S, VK_TAB, VK_U, VK_UP,
        };
        let (ctrl, shift, alt, vk) = chord.win_pattern()?;
        if alt {
            return None;
        }
        Some(match (ctrl, shift, vk) {
            (false, false, VK_E) => Self::Explorer,
            (false, false, VK_D) => Self::ToggleDesktop,
            (false, false, VK_M) => Self::MinimizeAll,
            (false, true, VK_M) => Self::UndoMinimizeAll,
            (false, false, VK_R) => Self::RunDialog,
            (false, false, VK_TAB) => Self::TaskView,
            (false, false, VK_S | VK_Q) => Self::Search,
            (false, false, VK_I) => Self::Settings,
            (false, false, VK_U) => Self::Accessibility,
            (false, false, VK_PAUSE) => Self::About,
            (false, false, VK_A) => Self::ActionCenter,
            (false, false, VK_P) => Self::Project,
            (false, false, VK_K) => Self::Cast,
            (false, true, VK_S) => Self::ScreenSnip,
            (false, false, VK_G) => Self::GameBar,
            (true, false, VK_O) => Self::OnScreenKeyboard,
            (false, false, VK_OEM_PLUS) => Self::Magnifier,
            (false, false, VK_L) => Self::Lock,
            (false, false, VK_UP) => Self::MaximizeWindow,
            (false, false, VK_DOWN) => Self::MinimizeOrRestoreWindow,
            (false, false, VK_LEFT) => Self::SnapLeft,
            (false, false, VK_RIGHT) => Self::SnapRight,
            (false, true, VK_LEFT) => Self::PrevMonitor,
            (false, true, VK_RIGHT) => Self::NextMonitor,
            _ => return None,
        })
    }

    pub fn needs_window(self) -> bool {
        matches!(
            self,
            Self::MaximizeWindow | Self::MinimizeOrRestoreWindow | Self::SnapLeft | Self::SnapRight | Self::PrevMonitor | Self::NextMonitor
        )
    }
}

/// True if this chord is a desktop/shell-level Windows shortcut with a direct equivalent (needs no target window).
pub fn is_shell_win_shortcut(chord: &Chord) -> bool {
    WinShortcut::from_chord(chord).is_some_and(|action| !action.needs_window())
}

/// Run a Windows-key shortcut through its direct shell COM, protocol URI, Win32, or background
/// window equivalent without using the real keyboard. `None` if no direct equivalent exists.
pub fn win_shortcut(chord: &Chord, target: Option<Hwnd>) -> Option<Result<String, String>> {
    use windows::Win32::UI::Shell::{IShellDispatch, IShellDispatch4, IShellDispatch5, IShellDispatch6};
    let action = WinShortcut::from_chord(chord)?;
    if action.needs_window() {
        let hwnd = target?;
        if is_shell_window(hwnd) {
            return Some(Err("refused: that window is the Windows desktop or taskbar".into()));
        }
        return Some(match action {
            WinShortcut::MaximizeWindow => (|| {
                if win::foreground() == hwnd {
                    win::place_max_front(hwnd)?;
                } else {
                    win::fill_screen_behind(hwnd)?;
                }
                Ok("direct (maximize)".into())
            })(),
            WinShortcut::MinimizeOrRestoreWindow => win::minimize_or_restore(hwnd).map(|_| "direct (restore or minimize)".into()),
            WinShortcut::SnapLeft => win::snap_half(hwnd, false).map(|_| "direct (snap left)".into()),
            WinShortcut::SnapRight => win::snap_half(hwnd, true).map(|_| "direct (snap right)".into()),
            WinShortcut::PrevMonitor => win::move_monitor(hwnd, false).map(|moved| {
                if moved { "direct (previous monitor)" } else { "direct (only one monitor)" }.into()
            }),
            WinShortcut::NextMonitor => win::move_monitor(hwnd, true).map(|moved| {
                if moved { "direct (next monitor)" } else { "direct (only one monitor)" }.into()
            }),
            _ => unreachable!(),
        });
    }
    Some(match action {
        WinShortcut::Explorer => system_file(true, "explorer.exe").and_then(|p| shell_launch(&p)).map(|_| "direct (explorer)".into()),
        WinShortcut::ToggleDesktop => shell_dispatch::<IShellDispatch4>(|s| unsafe { s.ToggleDesktop() }).map(|_| "direct (toggle desktop)".into()),
        WinShortcut::MinimizeAll => shell_dispatch::<IShellDispatch>(|s| unsafe { s.MinimizeAll() }).map(|_| "direct (minimize all)".into()),
        WinShortcut::UndoMinimizeAll => shell_dispatch::<IShellDispatch>(|s| unsafe { s.UndoMinimizeALL() }).map(|_| "direct (restore minimized)".into()),
        WinShortcut::RunDialog => shell_dispatch::<IShellDispatch>(|s| unsafe { s.FileRun() }).map(|_| "direct (run dialog)".into()),
        WinShortcut::TaskView => shell_dispatch::<IShellDispatch5>(|s| unsafe { s.WindowSwitcher() }).map(|_| "direct (task view)".into()),
        WinShortcut::Search => shell_dispatch::<IShellDispatch6>(|s| unsafe { s.SearchCommand() })
            .or_else(|_| shell_launch("ms-search:"))
            .map(|_| "direct (search)".into()),
        WinShortcut::Settings => shell_launch("ms-settings:").map(|_| "direct (settings)".into()),
        WinShortcut::Accessibility => shell_launch("ms-settings:easeofaccess").map(|_| "direct (accessibility)".into()),
        WinShortcut::About => shell_launch("ms-settings:about").map(|_| "direct (about)".into()),
        WinShortcut::ActionCenter => shell_launch("ms-actioncenter:").map(|_| "direct (action center)".into()),
        WinShortcut::Project => system_file(false, "DisplaySwitch.exe").and_then(|p| shell_launch(&p)).map(|_| "direct (project)".into()),
        WinShortcut::Cast => shell_launch("ms-settings-connectabledevices:devicediscovery").map(|_| "direct (cast)".into()),
        WinShortcut::ScreenSnip => shell_launch("ms-screenclip:").map(|_| "direct (screen snip)".into()),
        WinShortcut::GameBar => shell_launch("ms-gamebar:").map(|_| "direct (game bar)".into()),
        WinShortcut::OnScreenKeyboard => system_file(false, "osk.exe").and_then(|p| shell_launch(&p)).map(|_| "direct (on-screen keyboard)".into()),
        WinShortcut::Magnifier => system_file(false, "Magnify.exe").and_then(|p| shell_launch(&p)).map(|_| "direct (magnifier)".into()),
        WinShortcut::Lock => {
            if unsafe { windows_sys::Win32::System::Shutdown::LockWorkStation() } != 0 {
                Ok("direct (lock)".into())
            } else {
                Err("Windows refused to lock the workstation".into())
            }
        }
        _ => unreachable!(),
    })
}

pub fn close_window(hwnd: Hwnd) -> Result<(), String> {
    if is_shell_window(hwnd) {
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
    let result = post(node.native, CB_SETCURSEL, index, 0).and_then(|_| {
        if parent != 0 {
            post(parent, WM_COMMAND, (CBN_SELCHANGE << 16) | (id as usize & 0xFFFF), node.native)
        } else {
            Ok(())
        }
    });
    Some(result.map(|_| "message".to_string()))
}

/// Open a dropdown or list, pick the named entry, and close it again.
pub fn select_item(node: &Node, item: &str, uia: &Uia, root: Hwnd, surfaces: &[Hwnd]) -> Result<String, String> {
    if !win::draws_itself(root) && !win::draws_itself(node.native) && let Some(result) = select_in_classic_combo(node, item) {
        return result;
    }
    behind(root, || {
        let already_open = if node.can.expand {
            node.element.is_expanded().unwrap_or_else(|| node.parts.states.contains(&"open"))
        } else {
            node.parts.states.contains(&"open")
        };
        let mut opened = false;
        let mut opening = false;
        if node.can.expand && !already_open {
            match node.element.open() {
                Ok(done) => {
                    opened = true;
                    opening = matches!(done, Done::StillRunning);
                    std::thread::sleep(std::time::Duration::from_millis(120));
                }
                Err(error) if error.contains("administrator") || error.contains("gone") => return Err(error),
                Err(_) => {}
            }
        }
        let find_once = || {
            node.element.find_named(item, uia).or_else(|| {
                (opened || already_open || !node.can.expand)
                    .then(|| uia.find_popup_item(root, surfaces, item))
                    .flatten()
            })
        };
        let mut found = find_once();
        if found.is_none() && (opening || (opened && node.element.is_expanded() == Some(false))) {
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(900);
            while std::time::Instant::now() < deadline {
                let expanded = node.element.is_expanded().unwrap_or(false);
                std::thread::sleep(std::time::Duration::from_millis(100));
                found = find_once();
                if found.is_some() || expanded {
                    break;
                }
            }
        }
        let done = match found {
            Some(entry) => entry.select(),
            None => Err(format!("no entry named '{item}'")),
        };
        if opened && !matches!(done, Ok(Done::StillRunning)) {
            let _ = node.element.close();
        }
        done.map(|d| route("select", d))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn win_shortcuts_map_known_chords_and_reject_unmapped() {
        assert_eq!(WinShortcut::from_chord(&real::chord("win+e").unwrap()), Some(WinShortcut::Explorer));
        assert_eq!(WinShortcut::from_chord(&real::chord("cmd+d").unwrap()), Some(WinShortcut::ToggleDesktop));
        assert_eq!(WinShortcut::from_chord(&real::chord("win+shift+m").unwrap()), Some(WinShortcut::UndoMinimizeAll));
        assert_eq!(WinShortcut::from_chord(&real::chord("shift+win+s").unwrap()), Some(WinShortcut::ScreenSnip));
        assert_eq!(WinShortcut::from_chord(&real::chord("win+ctrl+o").unwrap()), Some(WinShortcut::OnScreenKeyboard));
        assert_eq!(WinShortcut::from_chord(&real::chord("win+pause").unwrap()), Some(WinShortcut::About));
        assert_eq!(WinShortcut::from_chord(&real::chord("win+plus").unwrap()), Some(WinShortcut::Magnifier));
        assert_eq!(WinShortcut::from_chord(&real::chord("win+up").unwrap()), Some(WinShortcut::MaximizeWindow));
        assert_eq!(WinShortcut::from_chord(&real::chord("win+shift+left").unwrap()), Some(WinShortcut::PrevMonitor));
        assert!(is_shell_win_shortcut(&real::chord("win+i").unwrap()));
        assert!(!is_shell_win_shortcut(&real::chord("win+left").unwrap()));
        assert_eq!(WinShortcut::from_chord(&real::chord("win").unwrap()), None);
        assert_eq!(WinShortcut::from_chord(&real::chord("win+x").unwrap()), None);
        assert_eq!(WinShortcut::from_chord(&real::chord("win+alt+e").unwrap()), None);
        assert_eq!(WinShortcut::from_chord(&real::chord("ctrl+s").unwrap()), None);
    }
}
