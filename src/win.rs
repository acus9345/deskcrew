// Top-level windows: finding them, naming them, and leaving focus where the person had it.
use std::{
    collections::HashSet,
    mem::zeroed,
    ptr::null_mut,
    thread,
    time::{Duration, Instant},
};
use windows_sys::{
    Win32::{
        Foundation::{CloseHandle, HWND, LPARAM, POINT, RECT},
        Graphics::{
            Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute},
            Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow, ScreenToClient},
        },
        System::Threading::{
            AttachThreadInput, GetCurrentThreadId, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
            QueryFullProcessImageNameW,
        },
        UI::HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI},
        UI::WindowsAndMessaging::{
            BringWindowToTop, ChildWindowFromPointEx, CWP_SKIPINVISIBLE, CWP_SKIPTRANSPARENT, EnumChildWindows,
            EnumWindows, GUITHREADINFO, GWL_EXSTYLE, GetClassNameW, GetForegroundWindow, GetGUIThreadInfo,
            GA_ROOT, GW_HWNDPREV, GW_OWNER, GetAncestor, GetDlgCtrlID, GetMenu, GetMenuItemCount, GetMenuItemID, GetMenuState, GetMenuStringW,
            GetParent, GetSubMenu, GetWindow, GetWindowLongPtrW, HMENU, MF_BYPOSITION, MF_DISABLED, MF_GRAYED, MF_SEPARATOR, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
            GetWindowThreadProcessId, IsIconic,
            IsWindow, IsWindowVisible, LSFW_LOCK, LSFW_UNLOCK, LockSetForegroundWindow, SW_RESTORE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetForegroundWindow, SetWindowPos,
            ShowWindow, SwitchToThisWindow, WS_EX_TOOLWINDOW, WindowFromPoint,
        },
    },
    core::BOOL,
};

pub type Hwnd = isize;

fn raw(hwnd: Hwnd) -> HWND {
    hwnd as HWND
}

#[derive(Clone, Debug)]
pub struct Win {
    pub hwnd: Hwnd,
    pub title: String,
    pub app: String,
}

fn utf16(buffer: &[u16]) -> String {
    String::from_utf16_lossy(buffer)
}

pub fn title(hwnd: Hwnd) -> String {
    unsafe {
        let mut buffer = vec![0; GetWindowTextLengthW(raw(hwnd)).max(0) as usize + 1];
        let got = GetWindowTextW(raw(hwnd), buffer.as_mut_ptr(), buffer.len() as i32);
        utf16(&buffer[..got.max(0) as usize])
    }
}

pub fn class(hwnd: Hwnd) -> String {
    unsafe {
        let mut buffer = [0; 128];
        let got = GetClassNameW(raw(hwnd), buffer.as_mut_ptr(), buffer.len() as i32);
        utf16(&buffer[..got.max(0) as usize])
    }
}

pub fn pid(hwnd: Hwnd) -> u32 {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(raw(hwnd), &mut pid) };
    pid
}

pub fn exists(hwnd: Hwnd) -> bool {
    unsafe { IsWindow(raw(hwnd)) != 0 }
}

pub fn minimized(hwnd: Hwnd) -> bool {
    unsafe { IsIconic(raw(hwnd)) != 0 }
}

fn image_name(pid: u32) -> String {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return String::new();
        }
        let mut buffer = [0; 520];
        let mut size = buffer.len() as u32;
        let ok = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, buffer.as_mut_ptr(), &mut size);
        CloseHandle(process);
        if ok == 0 {
            return String::new();
        }
        let path = utf16(&buffer[..size as usize]);
        path.rsplit('\\').next().unwrap_or("").trim_end_matches(".exe").to_string()
    }
}

unsafe extern "system" fn collect(hwnd: HWND, list: LPARAM) -> BOOL {
    unsafe { (*(list as *mut Vec<Hwnd>)).push(hwnd as Hwnd) };
    1
}

fn children(hwnd: Hwnd) -> Vec<Hwnd> {
    let mut list: Vec<Hwnd> = Vec::new();
    unsafe { EnumChildWindows(raw(hwnd), Some(collect), &mut list as *mut _ as LPARAM) };
    list
}

pub fn top_level() -> Vec<Hwnd> {
    let mut list: Vec<Hwnd> = Vec::new();
    unsafe { EnumWindows(Some(collect), &mut list as *mut _ as LPARAM) };
    list
}

/// Store apps sit inside a frame owned by another process, so ask the frame's children.
pub fn content_pid(hwnd: Hwnd) -> u32 {
    let own = pid(hwnd);
    if class(hwnd) != "ApplicationFrameWindow" {
        return own;
    }
    children(hwnd).into_iter().map(pid).find(|p| *p != own).unwrap_or(own)
}

fn shown(hwnd: Hwnd) -> bool {
    unsafe {
        if IsWindowVisible(raw(hwnd)) == 0 {
            return false;
        }
        let mut cloaked = 0u32;
        DwmGetWindowAttribute(raw(hwnd), DWMWA_CLOAKED as u32, &mut cloaked as *mut _ as *mut _, 4);
        cloaked == 0
    }
}

/// The windows a person could see or switch to, front first.
pub fn list() -> Vec<Win> {
    top_level()
        .into_iter()
        .filter(|&hwnd| shown(hwnd))
        .filter(|&hwnd| unsafe { GetWindowLongPtrW(raw(hwnd), GWL_EXSTYLE) } & WS_EX_TOOLWINDOW as isize == 0)
        .filter(|&hwnd| class(hwnd) != "deskcrew")
        .filter_map(|hwnd| {
            let title = title(hwnd);
            if title.is_empty() {
                return None;
            }
            Some(Win { hwnd, title, app: image_name(content_pid(hwnd)) })
        })
        .collect()
}

/// Every visible window of these processes, popups included.
pub fn of_processes(pids: &[u32]) -> Vec<Hwnd> {
    top_level().into_iter().filter(|&hwnd| shown(hwnd) && pids.contains(&pid(hwnd)) && class(hwnd) != "deskcrew").collect()
}

/// A screen point in a window's own client coordinates.
pub fn to_client(hwnd: Hwnd, x: i32, y: i32) -> (i32, i32) {
    let mut point = POINT { x, y };
    unsafe { ScreenToClient(raw(hwnd), &mut point) };
    (point.x, point.y)
}

pub fn inside(hwnd: Hwnd, x: i32, y: i32) -> bool {
    let (left, top, w, h) = outer_rect(hwnd);
    x >= left && y >= top && x < left + w && y < top + h
}

/// One entry of a classic menu bar, which can be run without ever opening the menu.
pub struct MenuEntry {
    pub label: String,
    pub command: usize,
    pub submenu: isize,
    pub greyed: bool,
}

pub fn menu_bar(hwnd: Hwnd) -> isize {
    unsafe { GetMenu(raw(hwnd)) as isize }
}

pub fn menu_entries(menu: isize) -> Vec<MenuEntry> {
    let handle = menu as HMENU;
    let mut out = Vec::new();
    unsafe {
        for at in 0..GetMenuItemCount(handle).max(0) {
            let state = GetMenuState(handle, at as u32, MF_BYPOSITION);
            if state & MF_SEPARATOR != 0 {
                continue;
            }
            let mut text = [0u16; 128];
            let got = GetMenuStringW(handle, at as u32, text.as_mut_ptr(), text.len() as i32, MF_BYPOSITION);
            let label = utf16(&text[..got.max(0) as usize]);
            let label = label.split('\t').next().unwrap_or("").replace('&', "");
            out.push(MenuEntry {
                label,
                command: GetMenuItemID(handle, at) as usize,
                submenu: GetSubMenu(handle, at) as isize,
                greyed: state & (MF_GRAYED | MF_DISABLED) != 0,
            });
        }
    }
    out
}

/// The entry with this label anywhere in a menu.
pub fn find_menu(menu: isize, wanted: &str) -> Option<MenuEntry> {
    if menu == 0 {
        return None;
    }
    let wanted = wanted.to_lowercase();
    let entries = menu_entries(menu);
    let submenus: Vec<isize> = entries.iter().map(|e| e.submenu).filter(|s| *s != 0).collect();
    entries.into_iter().find(|e| e.label.to_lowercase() == wanted).or_else(|| submenus.into_iter().find_map(|s| find_menu(s, &wanted)))
}

/// True if a real click at this point would reach one of these windows, or a popup one of them
/// owns, and nothing else.
pub fn owns_point(family: &[Hwnd], x: i32, y: i32) -> bool {
    unsafe {
        let hit = WindowFromPoint(POINT { x, y });
        if hit.is_null() {
            return false;
        }
        let mut at = GetAncestor(hit, GA_ROOT);
        while !at.is_null() {
            if family.contains(&(at as Hwnd)) {
                return true;
            }
            at = GetWindow(at, GW_OWNER);
        }
        false
    }
}

/// A popup, menu or dialog of this window rather than an unrelated window of the same app.
pub fn belongs(hwnd: Hwnd, family: &[Hwnd]) -> bool {
    let owner = unsafe { GetWindow(raw(hwnd), GW_OWNER) } as Hwnd;
    let name = class(hwnd);
    family.contains(&owner) || name == "#32768" || name == "#32770" || name.contains("Popup") || name.contains("DropDown")
}

/// Put an overlay window directly above this window, or above the highest of its menus and popups,
/// and under whatever else covers them. Only overlay windows may sit in between.
pub fn stack_above(layer: Hwnd, root: Hwnd, force: bool) {
    use windows_sys::Win32::{
        System::Threading::GetCurrentProcessId,
        UI::WindowsAndMessaging::{HWND_NOTOPMOST, HWND_TOPMOST, IsWindow, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetWindowPos, WS_EX_TOOLWINDOW, WS_EX_TOPMOST},
    };
    unsafe {
        let (layer, root) = (raw(layer), raw(root));
        if IsWindow(root) == 0 {
            return;
        }
        let process = |window: HWND| {
            let mut id = 0;
            GetWindowThreadProcessId(window, &mut id);
            id
        };
        let topmost = |window: HWND| GetWindowLongPtrW(window, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST != 0;
        let (me, theirs) = (GetCurrentProcessId(), process(root));
        let mut top = root;
        let mut at = GetWindow(root, GW_HWNDPREV);
        while !at.is_null() {
            let popup = GetWindow(at, GW_OWNER) == root || (process(at) == theirs && GetWindowLongPtrW(at, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW != 0);
            let shown = IsWindowVisible(at) != 0;
            if popup && shown {
                top = at;
            } else if shown && process(at) != me && process(at) != theirs {
                // Someone else's window covers this one: a popup above that, such as a tooltip
                // kept on top, would lift the cursor over the person's window too.
                break;
            }
            at = GetWindow(at, GW_HWNDPREV);
        }
        let mut above = GetWindow(top, GW_HWNDPREV);
        while !above.is_null() && process(above) == me {
            if above == layer && !force {
                return;
            }
            above = GetWindow(above, GW_HWNDPREV);
        }
        let after = match (topmost(top), above.is_null()) {
            (true, true) => HWND_TOPMOST,
            (false, true) => HWND_NOTOPMOST,
            (false, false) if topmost(above) => HWND_NOTOPMOST,
            _ => above,
        };
        SetWindowPos(layer, after, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
    }
}

/// Which process owns this window, and when that process started. A handle can be given to a new
/// window after the old one closes, so a handle alone does not prove it is the same window.
pub fn identity(hwnd: Hwnd) -> (u32, u64) {
    use windows_sys::Win32::{
        Foundation::FILETIME,
        System::Threading::{GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
    };
    let owner = pid(hwnd);
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, owner);
        if process.is_null() {
            return (owner, 0);
        }
        let (mut made, mut ended, mut kernel, mut user): (FILETIME, FILETIME, FILETIME, FILETIME) = (zeroed(), zeroed(), zeroed(), zeroed());
        let read = GetProcessTimes(process, &mut made, &mut ended, &mut kernel, &mut user);
        CloseHandle(process);
        (owner, if read != 0 { (made.dwHighDateTime as u64) << 32 | made.dwLowDateTime as u64 } else { 0 })
    }
}

/// Whether the window in front is this one or a dialog or popup of its own. Being the same process
/// is not enough: two windows of one app share a process, and keys must not reach the other one.
pub fn front_is(root: Hwnd, surfaces: &[Hwnd]) -> bool {
    let front = foreground();
    if front == root || surfaces.contains(&front) {
        return true;
    }
    let mut owner = front;
    while owner != 0 {
        owner = unsafe { GetWindow(raw(owner), GW_OWNER) } as Hwnd;
        if owner == root {
            return true;
        }
    }
    false
}

/// Every monitor as x, y, width, height, and whether it is the primary one.
pub fn screens() -> Vec<(i32, i32, i32, i32, bool)> {
    unsafe extern "system" fn each(monitor: HMONITOR, _: HDC, _: *mut RECT, found: LPARAM) -> i32 {
        unsafe {
            let mut info: MONITORINFO = zeroed();
            info.cbSize = size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(monitor, &mut info) != 0 {
                let r = info.rcMonitor;
                (*(found as *mut Vec<(i32, i32, i32, i32, bool)>)).push((r.left, r.top, r.right - r.left, r.bottom - r.top, info.dwFlags & 1 != 0));
            }
        }
        1
    }
    let mut found = Vec::new();
    unsafe { EnumDisplayMonitors(null_mut(), std::ptr::null(), Some(each), &mut found as *mut _ as LPARAM) };
    found
}

pub fn rect(hwnd: Hwnd) -> (i32, i32, i32, i32) {
    unsafe {
        let mut r: RECT = zeroed();
        let got = DwmGetWindowAttribute(raw(hwnd), DWMWA_EXTENDED_FRAME_BOUNDS as u32, &mut r as *mut _ as *mut _, size_of::<RECT>() as u32);
        if got != 0 {
            GetWindowRect(raw(hwnd), &mut r);
        }
        (r.left, r.top, r.right - r.left, r.bottom - r.top)
    }
}

pub fn outer_rect(hwnd: Hwnd) -> (i32, i32, i32, i32) {
    unsafe {
        let mut r: RECT = zeroed();
        GetWindowRect(raw(hwnd), &mut r);
        (r.left, r.top, r.right - r.left, r.bottom - r.top)
    }
}

pub fn place(hwnd: Hwnd, x: i32, y: i32, w: i32, h: i32) {
    unsafe { SetWindowPos(raw(hwnd), null_mut(), x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE) };
}

pub fn show(hwnd: Hwnd, how: i32) {
    unsafe { ShowWindow(raw(hwnd), how) };
}

pub fn foreground() -> Hwnd {
    unsafe { GetForegroundWindow() as Hwnd }
}

/// The window keyboard input goes to inside this window's thread.
pub fn keyboard_target(hwnd: Hwnd) -> Hwnd {
    unsafe {
        let thread = GetWindowThreadProcessId(raw(hwnd), null_mut());
        let mut info: GUITHREADINFO = zeroed();
        info.cbSize = size_of::<GUITHREADINFO>() as u32;
        if GetGUIThreadInfo(thread, &mut info) != 0 && !info.hwndFocus.is_null() {
            return info.hwndFocus as Hwnd;
        }
        // A window in the back has no focused control, so keys go where an agent last clicked or
        // typed in it, as they would after a real click there.
        let field = FIELDS.lock().unwrap().get(&hwnd).copied();
        if let Some(field) = field
            && IsWindow(raw(field)) != 0
            && GetAncestor(raw(field), GA_ROOT) as Hwnd == hwnd
        {
            return field;
        }
        // An app with its own engine takes keys in one window of its own.
        children(hwnd).into_iter().find(|child| key_site(*child)).unwrap_or(hwnd)
    }
}

/// The window a Store app or hosted XAML (Windows Terminal, for one) reads its keys from.
pub fn key_site(hwnd: Hwnd) -> bool {
    let name = class(hwnd);
    name == "Windows.UI.Core.CoreWindow" || name.contains("InputSite")
}

/// Windows that take posted keys and characters: every classic window, and the key windows of
/// apps that draw themselves. Their pointer input is another matter.
pub fn takes_posted_keys(hwnd: Hwnd) -> bool {
    !draws_itself(hwnd) || key_site(hwnd)
}

/// The control in each window that an agent last clicked or typed in.
static FIELDS: std::sync::Mutex<std::collections::BTreeMap<Hwnd, Hwnd>> = std::sync::Mutex::new(std::collections::BTreeMap::new());

/// Note that an agent clicked or typed in this control, so later keys go to it.
pub fn remember_field(control: Hwnd) {
    let root = unsafe { GetAncestor(raw(control), GA_ROOT) } as Hwnd;
    if root != 0 && root != control {
        FIELDS.lock().unwrap().insert(root, control);
    }
}

pub fn parent_and_id(hwnd: Hwnd) -> (Hwnd, i32) {
    unsafe { (GetParent(raw(hwnd)) as Hwnd, GetDlgCtrlID(raw(hwnd))) }
}

/// A check box or radio button an app draws itself on a classic button window, as WinForms does.
pub fn self_drawn_button(hwnd: Hwnd) -> bool {
    const BS_TYPEMASK: u32 = 0x0F;
    const BS_OWNERDRAW: u32 = 0x0B;
    let style = unsafe { GetWindowLongPtrW(raw(hwnd), windows_sys::Win32::UI::WindowsAndMessaging::GWL_STYLE) } as u32;
    hwnd != 0 && class(hwnd).to_uppercase().contains("BUTTON") && style & BS_TYPEMASK == BS_OWNERDRAW
}

/// Screen pixels per app unit. An app that predates high-DPI screens works in larger units than
/// the pixels Windows shows it at, so its messages and its own pictures are in those units.
pub fn app_scale(hwnd: Hwnd) -> f32 {
    unsafe {
        let mut screen_x = 96;
        let mut screen_y = 96;
        GetDpiForMonitor(MonitorFromWindow(raw(hwnd), MONITOR_DEFAULTTONEAREST), MDT_EFFECTIVE_DPI, &mut screen_x, &mut screen_y);
        screen_x.max(1) as f32 / GetDpiForWindow(raw(hwnd)).max(1) as f32
    }
}

/// The innermost child window under a screen point, and that point in its client area. Windows
/// has already converted it to the app's own units, so a message can carry it as it is.
pub fn child_at(root: Hwnd, x: i32, y: i32) -> (Hwnd, i32, i32) {
    let mut at = root;
    loop {
        let mut point = POINT { x, y };
        unsafe {
            ScreenToClient(raw(at), &mut point);
            let child = ChildWindowFromPointEx(raw(at), point, CWP_SKIPINVISIBLE | CWP_SKIPTRANSPARENT) as Hwnd;
            if child == 0 || child == at {
                return (at, point.x, point.y);
            }
            at = child;
        }
    }
}

/// Browsers and Electron apps. They draw themselves, but they do read posted mouse and character
/// messages, even while covered, so several can be worked at once.
pub fn is_chromium(hwnd: Hwnd) -> bool {
    class(hwnd).starts_with("Chrome_")
}

/// Windows drawn by their own engine: posted messages never reach their controls.
pub fn draws_itself(hwnd: Hwnd) -> bool {
    let name = class(hwnd);
    ["Xaml", "CoreWindow", "DesktopChildSiteBridge", "InputSite", "ApplicationFrame", "Mozilla", "HwndWrapper", "Qt"]
        .iter()
        .any(|part| name.contains(part))
}

fn holds_front(hwnd: Hwnd, ms: u64) -> bool {
    let until = Instant::now() + Duration::from_millis(ms);
    loop {
        if foreground() == hwnd {
            thread::sleep(Duration::from_millis(15));
            if foreground() == hwnd {
                return true;
            }
        }
        if Instant::now() >= until {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// Bring a window forward, and report whether Windows really did.
pub fn bring_forward(hwnd: Hwnd) -> bool {
    if foreground() == hwnd {
        return true;
    }
    unsafe {
        if IsIconic(raw(hwnd)) != 0 {
            ShowWindow(raw(hwnd), SW_RESTORE);
        }
        let front = GetForegroundWindow();
        let their_thread = if front.is_null() { 0 } else { GetWindowThreadProcessId(front, null_mut()) };
        let mine = GetCurrentThreadId();
        let attached = their_thread != 0 && their_thread != mine && AttachThreadInput(mine, their_thread, 1) != 0;
        SetForegroundWindow(raw(hwnd));
        BringWindowToTop(raw(hwnd));
        if attached {
            AttachThreadInput(mine, their_thread, 0);
        }
        if holds_front(hwnd, 250) {
            return true;
        }
        SwitchToThisWindow(raw(hwnd), 1);
    }
    holds_front(hwnd, 350)
}

/// Cancel the taskbar flash Windows starts when an app, woken by an agent's input, tries to come
/// forward and is not allowed to. The person never asked for that window.
pub fn stop_flashing(hwnd: Hwnd) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{FLASHW_STOP, FLASHWINFO, FlashWindowEx};
    if foreground() == hwnd {
        return;
    }
    let info = FLASHWINFO { cbSize: size_of::<FLASHWINFO>() as u32, hwnd: raw(hwnd), dwFlags: FLASHW_STOP, uCount: 0, dwTimeout: 0 };
    unsafe { FlashWindowEx(&info) };
}

/// No app can take the foreground while this is held.
pub struct ForegroundLock;

impl ForegroundLock {
    pub fn take() -> ForegroundLock {
        unsafe { LockSetForegroundWindow(LSFW_LOCK) };
        ForegroundLock
    }
}

impl Drop for ForegroundLock {
    fn drop(&mut self) {
        unsafe { LockSetForegroundWindow(LSFW_UNLOCK) };
    }
}

/// Processes that took focus when an agent pressed a control through accessibility. A posted
/// click does not, so these are pressed that way from then on.
static PULLED: std::sync::Mutex<Vec<u32>> = std::sync::Mutex::new(Vec::new());

/// True if one of these processes has already pulled its window forward over the person's.
pub fn pulls_forward(pids: &[u32]) -> bool {
    PULLED.lock().unwrap().iter().any(|pid| pids.contains(pid))
}

/// Remembers the person's window, and takes focus back if the app under control grabbed it.
#[derive(Clone)]
pub struct FocusGuard {
    front: Hwnd,
}

impl FocusGuard {
    pub fn take() -> FocusGuard {
        // A Store app reports its inner window, which cannot be brought forward on its own.
        let front = foreground();
        let outer = unsafe { GetAncestor(raw(front), GA_ROOT) } as Hwnd;
        FocusGuard { front: if outer != 0 { outer } else { front } }
    }

    /// Guard a window already known to be the person's.
    pub fn of(front: Hwnd) -> FocusGuard {
        FocusGuard { front }
    }

    pub fn front(&self) -> Hwnd {
        self.front
    }

    /// True if the window in front belongs to one of these processes.
    pub fn is_one_of(&self, pids: &[u32]) -> bool {
        pids.contains(&pid(self.front)) || pids.contains(&content_pid(self.front))
    }

    /// While an app starts: a window that did not exist before taking the front is that app, so
    /// the person gets it back. Going to a window that already existed was their own doing.
    pub fn hold_against(&self, existing: &HashSet<Hwnd>) {
        let now = foreground();
        let outer = unsafe { GetAncestor(raw(now), GA_ROOT) } as Hwnd;
        if now != 0 && now != self.front && outer != self.front && !existing.contains(&now) && !existing.contains(&outer) && exists(self.front) {
            bring_forward(self.front);
        }
    }

    /// Put a window directly behind the person's, if it has landed in front of it.
    pub fn keep_behind(&self, hwnd: Hwnd) {
        if self.front == 0 || !exists(self.front) {
            return;
        }
        unsafe {
            let mut above = GetWindow(raw(self.front), GW_HWNDPREV);
            while !above.is_null() {
                if above as Hwnd == hwnd {
                    SetWindowPos(raw(hwnd), raw(self.front), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                    return;
                }
                above = GetWindow(above, GW_HWNDPREV);
            }
        }
    }

    /// True if focus had to be taken back. A switch to any other app was the person's, so it stays.
    pub fn restore(&self, target_pids: &[u32]) -> bool {
        let now = foreground();
        let outer = unsafe { GetAncestor(raw(now), GA_ROOT) } as Hwnd;
        if now == self.front || outer == self.front || now == 0 || self.front == 0 || !exists(self.front) {
            return false;
        }
        if !target_pids.contains(&pid(now)) && !target_pids.contains(&content_pid(now)) {
            return false;
        }
        // A button or Alt held down means the person is switching to that app themself.
        if person_switching() {
            return false;
        }
        let mut pulled = PULLED.lock().unwrap();
        for owner in [pid(now), content_pid(now)] {
            if !pulled.contains(&owner) {
                pulled.push(owner);
            }
        }
        drop(pulled);
        if !bring_forward(self.front) {
            stop_flashing(self.front);
        }
        true
    }
}

impl FocusGuard {
    /// The window the person has just switched to themself, when it belongs to the app agents work
    /// in: clicked on it or its taskbar button, or reached it with Alt or the Windows key. It is
    /// theirs from then on, to use and move, and is not taken away from them.
    pub fn chosen(&self, target_pids: &[u32]) -> Option<Hwnd> {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_LWIN, VK_MBUTTON, VK_MENU, VK_RBUTTON, VK_RWIN};
        use windows_sys::Win32::UI::WindowsAndMessaging::{GetCursorPos, WindowFromPoint};
        let held = |keys: &[u16]| keys.iter().any(|&key| unsafe { GetAsyncKeyState(key as i32) } < 0);
        // Where the person's current press began. An app that jumps forward under a button they
        // are already holding is under the pointer now, but they did not press on it.
        let pressing = held(&[VK_LBUTTON, VK_RBUTTON, VK_MBUTTON]);
        let began_on = {
            let mut start = PRESS_BEGAN_ON.lock().unwrap();
            if !pressing {
                *start = None;
            } else if start.is_none() {
                let mut point = POINT { x: 0, y: 0 };
                *start = Some(unsafe {
                    GetCursorPos(&mut point);
                    GetAncestor(WindowFromPoint(point), GA_ROOT) as Hwnd
                });
            }
            *start
        };
        let now = foreground();
        let outer = unsafe { GetAncestor(raw(now), GA_ROOT) } as Hwnd;
        let theirs = if outer != 0 { outer } else { now };
        if now == 0 || now == self.front || outer == self.front {
            return None;
        }
        if !target_pids.contains(&pid(now)) && !target_pids.contains(&content_pid(now)) {
            return None;
        }
        if held(&[VK_MENU, VK_LWIN, VK_RWIN]) {
            return Some(theirs);
        }
        // The press has to have begun on that window, or on the taskbar that switches to it.
        let began_on = began_on?;
        (began_on == theirs || matches!(class(began_on).as_str(), "Shell_TrayWnd" | "Shell_SecondaryTrayWnd")).then_some(theirs)
    }
}

/// The top window under the pointer when the person's current mouse press began, as the focus
/// keeper last saw it.
static PRESS_BEGAN_ON: std::sync::Mutex<Option<Hwnd>> = std::sync::Mutex::new(None);

/// The person is pressing a mouse button, or holding Alt or the Windows key to switch apps.
fn person_switching() -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_LWIN, VK_MBUTTON, VK_MENU, VK_RBUTTON, VK_RWIN};
    [VK_LBUTTON, VK_RBUTTON, VK_MBUTTON, VK_MENU, VK_LWIN, VK_RWIN].iter().any(|&key| unsafe { GetAsyncKeyState(key as i32) } < 0)
}

pub fn lparam(x: i32, y: i32) -> LPARAM {
    (((y as u32 & 0xFFFF) << 16) | (x as u32 & 0xFFFF)) as i32 as LPARAM
}


/// Where the next letter goes in a classic edit box, as the caret's top left on the screen and its
/// height. The box need not have focus. None for other kinds of field.
pub fn caret_in(hwnd: Hwnd) -> Option<(i32, i32, i32)> {
    use windows_sys::Win32::Foundation::SIZE;
    use windows_sys::Win32::Graphics::Gdi::{ClientToScreen, CreateFontIndirectW, DeleteObject, GetDC, GetObjectW, GetTextExtentPoint32W, HFONT, LOGFONTW, ReleaseDC, SelectObject};
    use windows_sys::Win32::UI::WindowsAndMessaging::{WM_GETTEXT, WM_GETTEXTLENGTH};
    const LINE_PER_EM: f32 = 1.33;
    use windows_sys::Win32::UI::WindowsAndMessaging::{SMTO_ABORTIFHUNG, SendMessageTimeoutW};
    const EM_GETSEL: u32 = 0x00B0;
    const EM_POSFROMCHAR: u32 = 0x00D6;
    const WM_GETFONT: u32 = 0x0031;
    let kind = class(hwnd).to_lowercase();
    if kind != "edit" && !kind.contains(".edit.") {
        return None;
    }
    let ask = |message: u32, wparam: usize| -> Option<isize> {
        let mut result = 0;
        let sent = unsafe { SendMessageTimeoutW(raw(hwnd), message, wparam, 0, SMTO_ABORTIFHUNG, 200, &mut result) };
        (sent != 0).then_some(result as isize)
    };
    let selection = ask(EM_GETSEL, 0)?;
    let end = ((selection >> 16) & 0xFFFF) as usize;
    let unpack = |packed: isize| ((packed & 0xFFFF) as i16 as i32, ((packed >> 16) & 0xFFFF) as i16 as i32);
    // The box's font belongs to its own program, so its size is read rather than measured.
    let font = ask(WM_GETFONT, 0).unwrap_or(0);
    let mut logfont: LOGFONTW = unsafe { zeroed() };
    let read = unsafe { GetObjectW(font as HFONT, size_of::<LOGFONTW>() as i32, (&mut logfont as *mut LOGFONTW).cast()) };
    let em = if read > 0 && logfont.lfHeight != 0 { logfont.lfHeight.abs() } else { 16 };
    // A negative size is the letters' own height; the caret spans the whole line around them.
    let height = if logfont.lfHeight < 0 { (em as f32 * LINE_PER_EM).round() as i32 } else { em };
    // The last letter's width, measured in the box's font, for a caret just past it.
    let last_width = |index: usize| -> i32 {
        let mut text = [0u16; 2];
        let mut result = 0;
        // EM_GETLINE can't pick one letter, so the whole text is read and the letter picked out.
        let length = unsafe { SendMessageTimeoutW(raw(hwnd), WM_GETTEXTLENGTH, 0, 0, SMTO_ABORTIFHUNG, 200, &mut result) };
        if length == 0 || result == 0 {
            return em / 2;
        }
        let mut all = vec![0u16; result + 1];
        let mut got = 0;
        unsafe { SendMessageTimeoutW(raw(hwnd), WM_GETTEXT, all.len(), all.as_mut_ptr() as isize, SMTO_ABORTIFHUNG, 200, &mut got) };
        let Some(&letter) = all.get(index) else { return em / 2 };
        text[0] = letter;
        unsafe {
            let made = CreateFontIndirectW(&logfont);
            let dc = GetDC(null_mut());
            let old = SelectObject(dc, made);
            let mut size: SIZE = zeroed();
            GetTextExtentPoint32W(dc, text.as_ptr(), 1, &mut size);
            SelectObject(dc, old);
            ReleaseDC(null_mut(), dc);
            DeleteObject(made);
            if size.cx > 0 { size.cx } else { em / 2 }
        }
    };
    // Past the last letter a box may not answer, so the last letter's place and width stand in.
    let (x, y) = match ask(EM_POSFROMCHAR, end).filter(|&at| at != -1) {
        Some(at) => unpack(at),
        None if end > 0 => {
            let (x, y) = unpack(ask(EM_POSFROMCHAR, end - 1).filter(|&at| at != -1)?);
            (x + last_width(end - 1), y)
        }
        None => (2, 2),
    };
    // A window Windows enlarges for a high-DPI screen answers in its own small units: the corner is
    // found on the screen, and the rest scaled up from there.
    let scale = app_scale(hwnd);
    let mut corner = POINT { x: 0, y: 0 };
    unsafe { ClientToScreen(raw(hwnd), &mut corner) };
    let grow = |n: i32| (n as f32 * scale).round() as i32;
    Some((corner.x + grow(x), corner.y + grow(y), grow(height.max(8))))
}


/// Where the caret is in a classic edit box, counted in letters, or None for other fields.
pub fn caret_index(hwnd: Hwnd) -> Option<usize> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SMTO_ABORTIFHUNG, SendMessageTimeoutW};
    const EM_GETSEL: u32 = 0x00B0;
    let mut result = 0;
    let sent = unsafe { SendMessageTimeoutW(raw(hwnd), EM_GETSEL, 0, 0, SMTO_ABORTIFHUNG, 200, &mut result) };
    (sent != 0).then_some((result >> 16) & 0xFFFF)
}

/// How wide one letter is in a classic edit box's font, in screen pixels.
pub fn letter_width(hwnd: Hwnd, letter: u16) -> Option<i32> {
    use windows_sys::Win32::Foundation::SIZE;
    use windows_sys::Win32::Graphics::Gdi::{CreateFontIndirectW, DeleteObject, GetDC, GetObjectW, GetTextExtentPoint32W, HFONT, LOGFONTW, ReleaseDC, SelectObject};
    use windows_sys::Win32::UI::WindowsAndMessaging::{SMTO_ABORTIFHUNG, SendMessageTimeoutW};
    const WM_GETFONT: u32 = 0x0031;
    let mut font = 0;
    if unsafe { SendMessageTimeoutW(raw(hwnd), WM_GETFONT, 0, 0, SMTO_ABORTIFHUNG, 200, &mut font) } == 0 {
        return None;
    }
    let mut logfont: LOGFONTW = unsafe { zeroed() };
    if unsafe { GetObjectW(font as HFONT, size_of::<LOGFONTW>() as i32, (&mut logfont as *mut LOGFONTW).cast()) } == 0 {
        return None;
    }
    let width = unsafe {
        let made = CreateFontIndirectW(&logfont);
        let dc = GetDC(null_mut());
        let old = SelectObject(dc, made);
        let mut size: SIZE = zeroed();
        GetTextExtentPoint32W(dc, &letter, 1, &mut size);
        SelectObject(dc, old);
        ReleaseDC(null_mut(), dc);
        DeleteObject(made);
        size.cx
    };
    Some((width as f32 * app_scale(hwnd)).round() as i32)
}

/// The processes agents worked in, and when they last did.
static WORKED: std::sync::Mutex<Vec<(u32, Instant)>> = std::sync::Mutex::new(Vec::new());
/// How long after an agent's last action its app's taskbar flashes are still put out.
const FLASHES_WATCHED_FOR: Duration = Duration::from_secs(15 * 60);
/// A flash can start again a moment after it was stopped, as the app finishes what it was doing.
const FLASH_AGAIN: Duration = Duration::from_millis(600);

/// Note that agents are working in this window's app. A Store app's frame belongs to a process
/// that frames every Store app, so only the app inside it counts.
pub fn note_worked_in(hwnd: Hwnd) {
    let app = content_pid(hwnd);
    if class(hwnd) == "ApplicationFrameWindow" && app == pid(hwnd) {
        return;
    }
    note_worked(&[app]);
}

/// Note that agents are working in these processes.
pub fn note_worked(pids: &[u32]) {
    let pids: Vec<u32> = pids.iter().copied().filter(|p| !image_name(*p).to_lowercase().ends_with("applicationframehost.exe")).collect();
    let pids = &pids[..];
    let mut worked = WORKED.lock().unwrap();
    worked.retain(|(pid, at)| !pids.contains(pid) && at.elapsed() < FLASHES_WATCHED_FOR);
    worked.extend(pids.iter().map(|pid| (*pid, Instant::now())));
}

fn worked_in(hwnd: Hwnd) -> bool {
    let app = content_pid(hwnd);
    WORKED.lock().unwrap().iter().any(|(pid, at)| *pid == app && at.elapsed() < FLASHES_WATCHED_FOR)
}

/// Windows flashes an app's taskbar button when the app tries to come forward and is not allowed
/// to. Apps do that after an agent's input, though the person never asked for them, so those
/// flashes are put out as soon as the shell reports them.
pub fn watch_flashes() {
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, MSG, RegisterClassW, RegisterShellHookWindow, RegisterWindowMessageW,
        TranslateMessage, WNDCLASSW, WS_EX_TOOLWINDOW, WS_POPUP,
    };
    static SHELL_MESSAGE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    const HSHELL_FLASH: usize = 0x8006;
    unsafe extern "system" fn heard(hwnd: HWND, message: u32, w: usize, l: LPARAM) -> isize {
        let shell = SHELL_MESSAGE.load(std::sync::atomic::Ordering::Relaxed);
        if shell != 0 && message == shell && w == HSHELL_FLASH {
            let flashing = l as Hwnd;
            if worked_in(flashing) {
                stop_flashing(flashing);
                thread::spawn(move || {
                    thread::sleep(FLASH_AGAIN);
                    stop_flashing(flashing);
                });
            }
            return 0;
        }
        unsafe { DefWindowProcW(hwnd, message, w, l) }
    }
    thread::spawn(|| unsafe {
        let wide = |text: &str| text.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
        let module = GetModuleHandleW(null_mut());
        let class = wide("deskcrew-flash-watch");
        let mut kind: WNDCLASSW = zeroed();
        kind.lpfnWndProc = Some(heard);
        kind.hInstance = module;
        kind.lpszClassName = class.as_ptr();
        RegisterClassW(&kind);
        let window = CreateWindowExW(WS_EX_TOOLWINDOW, class.as_ptr(), class.as_ptr(), WS_POPUP, 0, 0, 0, 0, null_mut(), null_mut(), module, null_mut());
        if window.is_null() {
            return;
        }
        SHELL_MESSAGE.store(RegisterWindowMessageW(wide("SHELLHOOK").as_ptr()), std::sync::atomic::Ordering::Relaxed);
        if RegisterShellHookWindow(window) == 0 {
            return;
        }
        let mut message: MSG = zeroed();
        while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    });
}
