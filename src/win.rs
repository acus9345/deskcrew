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
            Gdi::{MONITOR_DEFAULTTONEAREST, MonitorFromWindow, ScreenToClient},
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
            IsWindow, IsWindowVisible, SW_RESTORE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetForegroundWindow, SetWindowPos,
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
        hwnd
    }
}

pub fn parent_and_id(hwnd: Hwnd) -> (Hwnd, i32) {
    unsafe { (GetParent(raw(hwnd)) as Hwnd, GetDlgCtrlID(raw(hwnd))) }
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
        bring_forward(self.front);
        true
    }
}

pub fn lparam(x: i32, y: i32) -> LPARAM {
    (((y as u32 & 0xFFFF) << 16) | (x as u32 & 0xFFFF)) as i32 as LPARAM
}

