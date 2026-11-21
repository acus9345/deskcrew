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
        Foundation::{CloseHandle, ERROR_ACCESS_DENIED, GetLastError, HWND, LPARAM, POINT, RECT, SetLastError},
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
            BringWindowToTop, ChildWindowFromPointEx, CWP_SKIPDISABLED, CWP_SKIPINVISIBLE, CWP_SKIPTRANSPARENT, EnumChildWindows,
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
        let name = path.rsplit('\\').next().unwrap_or("");
        // Office and others are stored in capitals: WINWORD.EXE is winword too.
        match name.len().checked_sub(4).and_then(|cut| Some((cut, name.get(cut..)?))) {
            Some((cut, extension)) if extension.eq_ignore_ascii_case(".exe") => name[..cut].to_string(),
            _ => name.to_string(),
        }
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
    children(hwnd).into_iter().map(pid).find(|p| *p != 0 && *p != own).unwrap_or(own)
}

fn is_frame_host(process_id: u32) -> bool {
    image_name(process_id).eq_ignore_ascii_case("applicationframehost")
}

/// Process ids belonging to this window's own app, excluding Windows' shared
/// `ApplicationFrameHost` frame process so Store apps do not collide with each other.
pub fn app_pids(hwnd: Hwnd) -> Vec<u32> {
    let own = pid(hwnd);
    if class(hwnd) != "ApplicationFrameWindow" {
        return (own != 0).then_some(own).into_iter().collect();
    }
    let mut out: Vec<u32> = children(hwnd).into_iter().map(pid).filter(|p| *p != 0 && *p != own).collect();
    out.sort_unstable();
    out.dedup();
    out
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
    if pids.is_empty() {
        return Vec::new();
    }
    top_level()
        .into_iter()
        .filter(|&hwnd| shown(hwnd) && (pids.contains(&pid(hwnd)) || pids.contains(&content_pid(hwnd))) && class(hwnd) != "deskcrew")
        .collect()
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

fn enabled(hwnd: Hwnd) -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled;
    unsafe { IsWindowEnabled(raw(hwnd)) != 0 }
}

/// `root` and the dialogs, menus and popups it opened, the highest first. Another main window of
/// the same app is not among them, unless `root` is disabled: then the app shows a dialog that
/// takes all its input, and one shown at startup has no owner.
fn own_windows(root: Hwnd) -> impl Iterator<Item = Hwnd> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GW_HWNDNEXT, GetTopWindow};
    let owners = app_pids(root);
    let blocked = !enabled(root);
    let top = unsafe { GetTopWindow(null_mut()) } as Hwnd;
    std::iter::successors((top != 0).then_some(top), |&at| Some(unsafe { GetWindow(raw(at), GW_HWNDNEXT) } as Hwnd).filter(|&next| next != 0))
        .filter(move |&hwnd| unsafe { IsWindowVisible(raw(hwnd)) } != 0 && (hwnd == root || owners.contains(&pid(hwnd)) || owners.contains(&content_pid(hwnd))) && class(hwnd) != "SysShadow")
        .filter(move |&hwnd| hwnd == root || blocked || belongs(hwnd, &[root]))
}

/// The app's own window at this point: a dialog or menu `root` opened over itself is where a real
/// click there would land.
pub fn own_window_at(root: Hwnd, x: i32, y: i32) -> Hwnd {
    own_windows(root).find(|&hwnd| inside(hwnd, x, y)).unwrap_or(root)
}

/// The window that takes `root`'s keys: `root`, or the dialog it opened over itself, which keeps
/// `root` from taking any input until it closes.
pub fn key_window(root: Hwnd) -> Hwnd {
    if enabled(root) {
        return root;
    }
    own_windows(root).find(|&hwnd| hwnd != root && enabled(hwnd)).unwrap_or(root)
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

/// Classic popup menus open now in these processes, newest first.
pub fn popup_menus(pids: &[u32]) -> Vec<Hwnd> {
    use windows_sys::Win32::UI::WindowsAndMessaging::FindWindowExW;
    let class: Vec<u16> = "#32768".encode_utf16().chain(Some(0)).collect();
    let mut found = Vec::new();
    let mut after = null_mut();
    unsafe {
        loop {
            after = FindWindowExW(null_mut(), after, class.as_ptr(), std::ptr::null());
            if after.is_null() {
                break;
            }
            if IsWindowVisible(after) != 0 && pids.contains(&pid(after as Hwnd)) {
                found.push(after as Hwnd);
            }
        }
    }
    found
}

/// The menu a popup menu window shows.
pub fn menu_of_popup(popup: Hwnd) -> isize {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SMTO_ABORTIFHUNG, SendMessageTimeoutW};
    const MN_GETHMENU: u32 = 0x01E1;
    let mut menu = 0;
    unsafe { SendMessageTimeoutW(raw(popup), MN_GETHMENU, 0, 0, SMTO_ABORTIFHUNG, 500, &mut menu) };
    menu as isize
}

fn menu_state(hwnd: Hwnd) -> Option<GUITHREADINFO> {
    unsafe {
        let thread = GetWindowThreadProcessId(raw(hwnd), null_mut());
        let mut info: GUITHREADINFO = zeroed();
        info.cbSize = size_of::<GUITHREADINFO>() as u32;
        (thread != 0 && GetGUIThreadInfo(thread, &mut info) != 0).then_some(info)
    }
}

/// The window an open popup menu reports its choice to, from its thread's menu state.
pub fn menu_owner(popup: Hwnd) -> Option<Hwnd> {
    menu_state(popup).filter(|info| !info.hwndMenuOwner.is_null()).map(|info| info.hwndMenuOwner as Hwnd)
}

/// True while this window's thread is still in a menu, which can outlast the menu showing.
pub fn in_menu(hwnd: Hwnd) -> bool {
    const GUI_INMENUMODE: u32 = 0x4;
    const GUI_POPUPMENUMODE: u32 = 0x10;
    menu_state(hwnd).is_some_and(|info| info.flags & (GUI_INMENUMODE | GUI_POPUPMENUMODE) != 0)
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

/// Browser processes deskcrew started with their cover check off.
static TAKES_INPUT_COVERED: std::sync::Mutex<Vec<u32>> = std::sync::Mutex::new(Vec::new());

pub fn note_takes_input_covered(pid: u32) {
    TAKES_INPUT_COVERED.lock().unwrap().push(pid);
}

/// True if this browser window would hold input sent to it now: no part of it shows, and it is
/// not one deskcrew started to take input while covered.
pub fn holds_input(hwnd: Hwnd) -> bool {
    !TAKES_INPUT_COVERED.lock().unwrap().contains(&pid(hwnd)) && !draws_covered_by_policy(hwnd) && fully_covered(hwnd)
}

/// True if this browser's WindowOcclusionEnabled policy is off, for the person or the machine
/// (`deskcrew browsers on`): it then keeps drawing covered windows and takes input there.
fn draws_covered_by_policy(hwnd: Hwnd) -> bool {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RegGetValueW};
    let vendor = match image_name(pid(hwnd)).to_lowercase().as_str() {
        "msedge" => "Microsoft\\Edge",
        "chrome" => "Google\\Chrome",
        "brave" => "BraveSoftware\\Brave",
        "vivaldi" => "Vivaldi",
        "opera" => "Opera Software\\Opera Stable",
        "chromium" => "Chromium",
        _ => return false,
    };
    let wide = |text: &str| text.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let key = wide(&format!("SOFTWARE\\Policies\\{vendor}"));
    let name = wide("WindowOcclusionEnabled");
    let read = |root| {
        let mut value: u32 = 1;
        let mut size = size_of::<u32>() as u32;
        let found = unsafe { RegGetValueW(root, key.as_ptr(), name.as_ptr(), RRF_RT_REG_DWORD, null_mut(), (&mut value as *mut u32).cast(), &mut size) } == 0;
        found.then_some(value)
    };
    // A machine policy wins over the person's.
    read(HKEY_LOCAL_MACHINE).or_else(|| read(HKEY_CURRENT_USER)) == Some(0)
}

/// How long Chromium takes to draw a covered window again once told to, and to take input there.
const REDRAW_WAIT: Duration = Duration::from_millis(300);

/// How long any other app takes to lay out and draw a window shown again.
const SHOWN_WAIT: Duration = Duration::from_millis(150);

/// How many agents are working in covered browser windows right now, and from when Chromium is
/// drawing them.
static KEPT_DRAWN: std::sync::Mutex<(usize, Option<Instant>)> = std::sync::Mutex::new((0, None));

/// A window of the kind Windows shows its task switcher thumbnails in, never put on screen.
fn thumbnail_stand_in() -> Option<Hwnd> {
    static STAND_IN: std::sync::OnceLock<Hwnd> = std::sync::OnceLock::new();
    let made = *STAND_IN.get_or_init(|| {
        let (tell, heard) = std::sync::mpsc::channel();
        // A window ends with the thread that made it, so it gets a thread of its own.
        thread::spawn(move || unsafe {
            use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
            use windows_sys::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, MSG, RegisterClassW, WNDCLASSW};
            let wide = |text: &str| text.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
            let module = GetModuleHandleW(null_mut());
            let class = wide("TaskListThumbnailWnd");
            let mut kind: WNDCLASSW = zeroed();
            kind.lpfnWndProc = Some(DefWindowProcW);
            kind.hInstance = module;
            kind.lpszClassName = class.as_ptr();
            RegisterClassW(&kind);
            let window = CreateWindowExW(WS_EX_TOOLWINDOW, class.as_ptr(), null_mut(), 0, 0, 0, 0, 0, null_mut(), null_mut(), module, null_mut());
            let _ = tell.send(window as Hwnd);
            let mut message: MSG = zeroed();
            while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
                DispatchMessageW(&message);
            }
        });
        heard.recv_timeout(Duration::from_secs(2)).unwrap_or(0)
    });
    (made != 0).then_some(made)
}

fn announce(stand_in: Hwnd, shown: bool) {
    use windows_sys::Win32::UI::{
        Accessibility::NotifyWinEvent,
        WindowsAndMessaging::{CHILDID_SELF, EVENT_OBJECT_HIDE, EVENT_OBJECT_SHOW, OBJID_WINDOW},
    };
    unsafe { NotifyWinEvent(if shown { EVENT_OBJECT_SHOW } else { EVENT_OBJECT_HIDE }, raw(stand_in), OBJID_WINDOW, CHILDID_SELF as i32) };
}

type ReadySignal = std::sync::Arc<(std::sync::Mutex<Option<bool>>, std::sync::Condvar)>;

/// A minimized window shown off screen for a while, and how to put it back.
struct OffScreen {
    hwnd: Hwnd,
    placement: windows_sys::Win32::UI::WindowsAndMessaging::WINDOWPLACEMENT,
    _lock: Option<ForegroundLock>,
    restore_minimized: bool,
    front: Hwnd,
    ready: ReadySignal,
}

/// Minimized windows shown off screen now, keyed with a generation token and agent refcount.
/// The last agent in a matching generation minimizes it again.
static OFF_SCREEN: std::sync::Mutex<std::collections::BTreeMap<Hwnd, (u64, usize, OffScreen)>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());
static NEXT_OFF_SCREEN_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

static WINDOW_LOCKS: std::sync::Mutex<std::collections::BTreeMap<Hwnd, std::sync::Weak<std::sync::Mutex<()>>>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

fn window_lock(hwnd: Hwnd) -> std::sync::Arc<std::sync::Mutex<()>> {
    let mut map = WINDOW_LOCKS.lock().unwrap();
    if let Some(lock) = map.get(&hwnd).and_then(std::sync::Weak::upgrade) {
        return lock;
    }
    if map.len() > 64 {
        map.retain(|_, v| v.strong_count() > 0);
    }
    let lock = std::sync::Arc::new(std::sync::Mutex::new(()));
    map.insert(hwnd, std::sync::Arc::downgrade(&lock));
    lock
}

enum ShareOutcome {
    Created(u64, ReadySignal),
    Existing(u64, ReadySignal),
    SelfActivated(Hwnd),
    Unavailable,
}

struct ReadyGuard(ReadySignal);

impl ReadyGuard {
    fn set(&self, ok: bool) {
        let mut locked = self.0.0.lock().unwrap();
        if locked.is_none() {
            *locked = Some(ok);
            self.0.1.notify_all();
        }
    }
}

impl Drop for ReadyGuard {
    fn drop(&mut self) {
        self.set(false);
    }
}

fn share_off_screen_inner(hwnd: Hwnd) -> ShareOutcome {
    {
        let mut shown = OFF_SCREEN.lock().unwrap();
        if let Some((epoch, count, off)) = shown.get_mut(&hwnd) {
            *count += 1;
            return ShareOutcome::Existing(*epoch, std::sync::Arc::clone(&off.ready));
        }
    }
    if !minimized(hwnd) {
        return ShareOutcome::Unavailable;
    }
    match show_off_screen(hwnd) {
        ShownOffScreen::Ok(off) => {
            let epoch = NEXT_OFF_SCREEN_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let ready = std::sync::Arc::clone(&off.ready);
            OFF_SCREEN.lock().unwrap().insert(hwnd, (epoch, 1, off));
            ShareOutcome::Created(epoch, ready)
        }
        ShownOffScreen::SelfActivated(front) => ShareOutcome::SelfActivated(front),
        ShownOffScreen::Failed => ShareOutcome::Unavailable,
    }
}

fn leave_off_screen_inner(hwnd: Hwnd, epoch: u64) -> Option<OffScreen> {
    let mut shown = OFF_SCREEN.lock().unwrap();
    let remove = match shown.get_mut(&hwnd) {
        Some((entry_epoch, count, _)) if *entry_epoch == epoch => {
            *count -= 1;
            *count == 0
        }
        _ => false,
    };
    remove.then(|| shown.remove(&hwnd).map(|(_, _, off)| off)).flatten()
}

fn leave_off_screen(hwnd: Hwnd, epoch: u64) {
    let lock = window_lock(hwnd);
    let _guard = lock.lock().unwrap();
    let dropped = leave_off_screen_inner(hwnd, epoch);
    drop(dropped);
}

/// While held, covered browser and Electron windows are drawn and take input. Dropping it lets
/// Chromium hide them again once no agent is working in one, and minimizes again one that was.
pub struct KeptDrawn(Option<Hwnd>, Option<(Hwnd, u64)>);

/// Turn a window's minimize and restore animations off or back on.
fn animations(hwnd: Hwnd, on: bool) {
    use windows_sys::Win32::Graphics::Dwm::{DWMWA_TRANSITIONS_FORCEDISABLED, DwmSetWindowAttribute};
    let off: i32 = if on { 0 } else { 1 };
    unsafe { DwmSetWindowAttribute(raw(hwnd), DWMWA_TRANSITIONS_FORCEDISABLED as u32, &off as *const i32 as *const _, size_of::<i32>() as u32) };
}

pub fn same_app(now: Hwnd, target: Hwnd) -> bool {
    if now == target {
        return true;
    }
    let pids = app_pids(target);
    pids.contains(&pid(now)) || pids.contains(&content_pid(now))
}

fn settle_behind(hwnd: Hwnd, front: Hwnd) {
    let now = foreground();
    if front != 0 && front != hwnd && now != front && same_app(now, hwnd) && !bring_forward(front) {
        stop_flashing(front);
    }
    stop_flashing(hwnd);
}

/// Make a window in the back fill its screen, short of the taskbar, without activating it:
/// Windows' own maximize always activates the window. True if it was already maximized.
pub fn fill_screen_behind(hwnd: Hwnd) -> Result<bool, String> {
    use windows_sys::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow};
    use windows_sys::Win32::UI::WindowsAndMessaging::{IsZoomed, SW_SHOWNOACTIVATE};
    let front = foreground();
    let lock = window_lock(hwnd);
    let guard = lock.lock().unwrap();
    let _fg_lock = (!front_thread(hwnd)).then(ForegroundLock::take);
    let was_min = was_minimized(hwnd);
    if was_min {
        FILLED.lock().unwrap().remove(&hwnd);
    }
    commit_shown_inner(hwnd);
    if unsafe { IsZoomed(raw(hwnd)) } != 0 {
        return Ok(true);
    }
    if unsafe { IsIconic(raw(hwnd)) } != 0 {
        FILLED.lock().unwrap().remove(&hwnd);
        show(hwnd, SW_SHOWNOACTIVATE)?;
        if unsafe { IsZoomed(raw(hwnd)) } != 0 {
            drop(_fg_lock);
            drop(guard);
            settle_behind(hwnd, front);
            return Ok(false);
        }
    }
    unsafe {
        let mut info: MONITORINFO = zeroed();
        info.cbSize = size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(MonitorFromWindow(raw(hwnd), MONITOR_DEFAULTTONEAREST), &mut info) != 0 {
            let work = info.rcWork;
            let mut before: RECT = zeroed();
            let has_before = GetWindowRect(raw(hwnd), &mut before) != 0;
            place(hwnd, work.left, work.top, work.right - work.left, work.bottom - work.top)?;
            if has_before {
                FILLED.lock().unwrap().entry(hwnd).or_insert(before);
            }
        }
    }
    drop(_fg_lock);
    drop(guard);
    settle_behind(hwnd, front);
    Ok(false)
}

/// Windows made to fill their screen in the back, and where each was before.
static FILLED: std::sync::Mutex<std::collections::BTreeMap<Hwnd, RECT>> = std::sync::Mutex::new(std::collections::BTreeMap::new());

fn restore_behind_inner(hwnd: Hwnd) -> Result<(), String> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{IsZoomed, SW_SHOWNOACTIVATE};
    let was_min = was_minimized(hwnd);
    commit_shown_inner(hwnd);
    if unsafe { IsIconic(raw(hwnd)) } != 0 {
        show(hwnd, SW_SHOWNOACTIVATE)?;
    }
    if unsafe { IsZoomed(raw(hwnd)) } != 0 {
        show(hwnd, SW_SHOWNOACTIVATE)?;
    }
    let before = FILLED.lock().unwrap().remove(&hwnd);
    if !was_min
        && let Some(before) = before
        && unsafe { IsIconic(raw(hwnd)) } == 0
        && unsafe { IsZoomed(raw(hwnd)) } == 0
    {
        place(hwnd, before.left, before.top, before.right - before.left, before.bottom - before.top)?;
    }
    Ok(())
}

/// Restore a window in the back without activating it: from minimized, maximized, or filling its
/// screen, back to where it was.
pub fn restore_behind(hwnd: Hwnd) -> Result<(), String> {
    let lock = window_lock(hwnd);
    let _guard = lock.lock().unwrap();
    restore_behind_inner(hwnd)
}

/// If `hwnd` was only shown off-screen temporarily, put it back at its real normal position so
/// `KeptDrawn::drop` won't re-minimize it after an explicit restore/snap/maximize.
/// Returns `true` if the off-screen window originally had `WPF_RESTORETOMAXIMIZED` set.
fn commit_shown_inner(hwnd: Hwnd) -> bool {
    let removed = OFF_SCREEN.lock().unwrap().remove(&hwnd);
    if let Some((_, _, mut off)) = removed {
        use windows_sys::Win32::UI::WindowsAndMessaging::{SW_SHOWNOACTIVATE, SetWindowPlacement, WPF_RESTORETOMAXIMIZED};
        let was_zoomed = (off.placement.flags & WPF_RESTORETOMAXIMIZED) != 0;
        off.restore_minimized = false;
        let _lock = off._lock.take();
        let ready = std::sync::Arc::clone(&off.ready);
        off.placement.flags = 0;
        off.placement.showCmd = SW_SHOWNOACTIVATE as u32;
        unsafe { SetWindowPlacement(raw(hwnd), &off.placement) };
        animations(hwnd, true);
        let mut locked = ready.0.lock().unwrap();
        if locked.is_none() {
            *locked = Some(true);
            ready.1.notify_all();
        }
        was_zoomed
    } else {
        false
    }
}

pub fn place_min(hwnd: Hwnd, theirs: bool) -> Result<(), String> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SW_MINIMIZE, SW_SHOWMINNOACTIVE};
    let lock = window_lock(hwnd);
    let _guard = lock.lock().unwrap();
    FILLED.lock().unwrap().remove(&hwnd);
    if !was_minimized(hwnd) {
        show(hwnd, if theirs { SW_MINIMIZE } else { SW_SHOWMINNOACTIVE })?;
    }
    Ok(())
}

pub fn place_max_front(hwnd: Hwnd) -> Result<(), String> {
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_MAXIMIZE;
    let lock = window_lock(hwnd);
    let _guard = lock.lock().unwrap();
    commit_shown_inner(hwnd);
    FILLED.lock().unwrap().remove(&hwnd);
    show(hwnd, SW_MAXIMIZE)
}

pub fn place_restore_front(hwnd: Hwnd) -> Result<(), String> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{IsZoomed, SW_RESTORE};
    let lock = window_lock(hwnd);
    let _guard = lock.lock().unwrap();
    if was_minimized(hwnd) || unsafe { IsZoomed(raw(hwnd)) != 0 } {
        commit_shown_inner(hwnd);
        FILLED.lock().unwrap().remove(&hwnd);
        show(hwnd, SW_RESTORE)
    } else {
        restore_behind_inner(hwnd)
    }
}

pub fn place_rect(hwnd: Hwnd, x: i32, y: i32, w: i32, h: i32) -> Result<(), String> {
    let lock = window_lock(hwnd);
    let _guard = lock.lock().unwrap();
    commit_shown_inner(hwnd);
    FILLED.lock().unwrap().remove(&hwnd);
    place(hwnd, x, y, w, h)
}

pub fn was_minimized(hwnd: Hwnd) -> bool {
    minimized(hwnd) || OFF_SCREEN.lock().unwrap().contains_key(&hwnd)
}

pub fn is_zoomed_or_filled(hwnd: Hwnd) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::IsZoomed;
    if was_minimized(hwnd) {
        return false;
    }
    FILLED.lock().unwrap().contains_key(&hwnd) || unsafe { IsZoomed(raw(hwnd)) != 0 }
}

fn snap_rect(work: RECT, right: bool) -> (i32, i32, i32, i32) {
    let half = (work.right - work.left) / 2;
    let (x, w) = if right { (work.left + half, work.right - (work.left + half)) } else { (work.left, half) };
    (x, work.top, w.max(1), (work.bottom - work.top).max(1))
}

fn move_monitor_rect(win: (i32, i32, i32, i32), src: RECT, dst: RECT) -> (i32, i32, i32, i32) {
    let (x, y, w, h) = win;
    let dst_w = (dst.right - dst.left).max(1);
    let dst_h = (dst.bottom - dst.top).max(1);
    let nw = w.max(1).min(dst_w);
    let nh = h.max(1).min(dst_h);
    let nx = (dst.left + (x - src.left)).clamp(dst.left, dst.left + dst_w - nw);
    let ny = (dst.top + (y - src.top)).clamp(dst.top, dst.top + dst_h - nh);
    (nx, ny, nw, nh)
}

/// Snap `hwnd` to the left or right half of its monitor's work area without taking focus.
pub fn snap_half(hwnd: Hwnd, right: bool) -> Result<(), String> {
    use windows_sys::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow};
    use windows_sys::Win32::UI::WindowsAndMessaging::{IsZoomed, SW_SHOWNOACTIVATE};
    let front = foreground();
    let lock = window_lock(hwnd);
    let guard = lock.lock().unwrap();
    let _fg_lock = (!front_thread(hwnd)).then(ForegroundLock::take);
    commit_shown_inner(hwnd);
    FILLED.lock().unwrap().remove(&hwnd);
    if unsafe { IsIconic(raw(hwnd)) } != 0 {
        show(hwnd, SW_SHOWNOACTIVATE)?;
    }
    if unsafe { IsZoomed(raw(hwnd)) } != 0 {
        show(hwnd, SW_SHOWNOACTIVATE)?;
    }
    unsafe {
        let mut info: MONITORINFO = zeroed();
        info.cbSize = size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(MonitorFromWindow(raw(hwnd), MONITOR_DEFAULTTONEAREST), &mut info) != 0 {
            let (x, y, w, h) = snap_rect(info.rcWork, right);
            place(hwnd, x, y, w, h)?;
        }
    }
    drop(_fg_lock);
    drop(guard);
    settle_behind(hwnd, front);
    Ok(())
}

/// Move `hwnd` to the next or previous monitor's work area without taking focus.
/// Returns `Ok(false)` if there is only one monitor.
pub fn move_monitor(hwnd: Hwnd, next: bool) -> Result<bool, String> {
    use windows_sys::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow};
    use windows_sys::Win32::UI::WindowsAndMessaging::{IsZoomed, SW_MAXIMIZE, SW_SHOWNOACTIVATE};
    unsafe extern "system" fn each(monitor: HMONITOR, _: HDC, _: *mut RECT, found: LPARAM) -> i32 {
        unsafe {
            let mut info: MONITORINFO = zeroed();
            info.cbSize = size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(monitor, &mut info) != 0 {
                (*(found as *mut Vec<(isize, RECT)>)).push((monitor as isize, info.rcWork));
            }
        }
        1
    }
    let mut monitors: Vec<(isize, RECT)> = Vec::new();
    unsafe { EnumDisplayMonitors(null_mut(), std::ptr::null(), Some(each), &mut monitors as *mut _ as LPARAM) };
    monitors.sort_by_key(|(_, r)| (r.left, r.top));
    if monitors.len() < 2 {
        return Ok(false);
    }
    let front = foreground();
    let theirs = front == hwnd;
    let lock = window_lock(hwnd);
    let guard = lock.lock().unwrap();
    let _fg_lock = (!front_thread(hwnd)).then(ForegroundLock::take);
    let was_min = was_minimized(hwnd);
    let restored_maximized = commit_shown_inner(hwnd);
    if was_min {
        FILLED.lock().unwrap().remove(&hwnd);
    }
    if unsafe { IsIconic(raw(hwnd)) } != 0 {
        show(hwnd, SW_SHOWNOACTIVATE)?;
    }
    let current = unsafe { MonitorFromWindow(raw(hwnd), MONITOR_DEFAULTTONEAREST) } as isize;
    let idx = monitors.iter().position(|(m, _)| *m == current).unwrap_or(0);
    let target = if next { (idx + 1) % monitors.len() } else { (idx + monitors.len() - 1) % monitors.len() };
    let (src_work, dst_work) = (monitors[idx].1, monitors[target].1);
    let os_zoomed = unsafe { IsZoomed(raw(hwnd)) } != 0;
    let zoomed = restored_maximized || os_zoomed;
    let filled = FILLED.lock().unwrap().remove(&hwnd);
    if zoomed || filled.is_some() {
        if os_zoomed {
            show(hwnd, SW_SHOWNOACTIVATE)?;
        }
        let normal = match filled {
            Some(r) => (r.left, r.top, r.right - r.left, r.bottom - r.top),
            None => outer_rect(hwnd),
        };
        let (nx, ny, nw, nh) = move_monitor_rect(normal, src_work, dst_work);
        if theirs && zoomed {
            place(hwnd, nx, ny, nw, nh)?;
            show(hwnd, SW_MAXIMIZE)?;
        } else {
            place(hwnd, dst_work.left, dst_work.top, dst_work.right - dst_work.left, dst_work.bottom - dst_work.top)?;
            FILLED.lock().unwrap().insert(hwnd, RECT { left: nx, top: ny, right: nx + nw, bottom: ny + nh });
        }
    } else {
        let (nx, ny, nw, nh) = move_monitor_rect(outer_rect(hwnd), src_work, dst_work);
        place(hwnd, nx, ny, nw, nh)?;
    }
    drop(_fg_lock);
    drop(guard);
    settle_behind(hwnd, front);
    Ok(true)
}

/// Minimize or restore `hwnd` (the Win+Down action), staying in the background unless `hwnd` is
/// already the person's foreground window.
pub fn minimize_or_restore(hwnd: Hwnd) -> Result<(), String> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{IsZoomed, SW_MINIMIZE, SW_SHOWMINNOACTIVE};
    let front = foreground();
    let theirs = front == hwnd;
    let lock = window_lock(hwnd);
    let guard = lock.lock().unwrap();
    let _fg_lock = (!theirs).then(ForegroundLock::take);
    if was_minimized(hwnd) {
        FILLED.lock().unwrap().remove(&hwnd);
        return Ok(());
    }
    if is_zoomed_or_filled(hwnd) {
        if theirs && unsafe { IsZoomed(raw(hwnd)) != 0 } {
            FILLED.lock().unwrap().remove(&hwnd);
            show(hwnd, SW_RESTORE)?;
        } else {
            restore_behind_inner(hwnd)?;
        }
    } else {
        FILLED.lock().unwrap().remove(&hwnd);
        show(hwnd, if theirs { SW_MINIMIZE } else { SW_SHOWMINNOACTIVE })?;
    }
    drop(_fg_lock);
    drop(guard);
    if !theirs {
        settle_behind(hwnd, front);
    } else {
        stop_flashing(hwnd);
    }
    Ok(())
}

/// A place for a window off every screen where Windows still counts it as on the screen it was on,
/// so an app that follows each screen's pixel density does not lay itself out again.
fn far_from_screens(normal: RECT, width: i32, height: i32) -> RECT {
    use windows_sys::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONEAREST, MonitorFromRect};
    const FAR: i32 = 31_000;
    let at = |left: i32, top: i32| RECT { left, top, right: left + width, bottom: top + height };
    let own = unsafe { MonitorFromRect(&normal, MONITOR_DEFAULTTONEAREST) };
    let tries = [at(-FAR, -FAR), at(normal.left, -FAR), at(-FAR, normal.top), at(normal.left, FAR - height), at(FAR - width, normal.top)];
    tries.into_iter().find(|rect| unsafe { MonitorFromRect(rect, MONITOR_DEFAULTTONEAREST) } == own).unwrap_or(tries[0])
}

/// Whether this window's thread is the one in front, whose input is the person's.
pub fn front_thread(hwnd: Hwnd) -> bool {
    let thread = |hwnd: HWND| unsafe { GetWindowThreadProcessId(hwnd, null_mut()) };
    thread(raw(hwnd)) == thread(unsafe { GetForegroundWindow() })
}

/// Whether `hwnd` (or its top-level owner window) is currently NOT the foreground window.
/// Unlike `!front_thread(hwnd)`, this stays `true` when two windows share a browser UI thread
/// (e.g. a Gemini PWA covered by AI Studio in the same Chrome/Edge process).
pub fn is_background(hwnd: Hwnd) -> bool {
    if hwnd == 0 {
        return false;
    }
    let root = unsafe { GetAncestor(raw(hwnd), GA_ROOT) } as Hwnd;
    let target = if root != 0 { root } else { hwnd };
    foreground() != target
}

pub fn is_winforms(hwnd: Hwnd) -> bool {
    class(hwnd).starts_with("WindowsForms10.")
}

/// Run `work` with the window moved off every screen, where nothing covers it, then put it back.
/// WinForms raises a click only when the window really shows at the spot of the release: covered,
/// it takes the press and drops the click. Its own coordinates do not change on the way.
pub fn uncovered<T>(hwnd: Hwnd, work: impl FnOnce() -> T) -> T {
    use windows_sys::Win32::UI::WindowsAndMessaging::SWP_ASYNCWINDOWPOS;
    let was_front = foreground();
    let input_before = crate::real::last_input_tick();
    let _lock = is_background(hwnd).then(ForegroundLock::take);
    let mut was: RECT = unsafe { zeroed() };
    if unsafe { GetWindowRect(raw(hwnd), &mut was) } == 0 {
        return work();
    }
    let (width, height) = (was.right - was.left, was.bottom - was.top);
    let away = far_from_screens(was, width, height);
    let (dx, dy) = (was.left - away.left, was.top - away.top);
    let before: Vec<Hwnd> = own_windows(hwnd).collect();
    let flags = SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE;
    unsafe { SetWindowPos(raw(hwnd), null_mut(), away.left, away.top, 0, 0, flags) };
    let result = work();
    // The way back does not wait on the app, which may still be running what the click started.
    // A dialog it opened meanwhile, centred on the window, comes back with it.
    let back = flags | SWP_ASYNCWINDOWPOS;
    unsafe { SetWindowPos(raw(hwnd), null_mut(), was.left, was.top, 0, 0, back) };
    for opened in own_windows(hwnd).filter(|w| !before.contains(w)) {
        let mut at: RECT = unsafe { zeroed() };
        if unsafe { GetWindowRect(raw(opened), &mut at) } != 0 && !on_screen(at.left + (at.right - at.left) / 2, at.top + (at.bottom - at.top) / 2) {
            unsafe { SetWindowPos(raw(opened), null_mut(), at.left + dx, at.top + dy, 0, 0, back) };
        }
    }
    if was_front != 0 && was_front != hwnd && !person_switching() && crate::real::last_input_tick() == input_before {
        let now = foreground();
        if now != was_front && same_app(now, hwnd) {
            if !bring_forward(was_front) {
                stop_flashing(was_front);
            }
            lower_behind(hwnd, was_front);
            if now != hwnd {
                lower_behind(now, was_front);
            }
        }
    }
    stop_flashing(hwnd);
    result
}

/// Show a minimized window, far off every screen and without activating it. For Chromium we
/// require `ForegroundLock::try_take()` because Chromium can self-activate on un-minimize; WPF does
/// not activate from `SW_SHOWNOACTIVATE`, so it still proceeds when Windows withheld the lock (for
/// instance in a non-foreground sandbox desktop or right after user input), and any unexpected
/// activation is reversed immediately by `show_off_screen` and `keep_drawn`.
enum ShownOffScreen {
    Ok(OffScreen),
    SelfActivated(Hwnd),
    Failed,
}

fn show_off_screen(hwnd: Hwnd) -> ShownOffScreen {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetWindowPlacement, HWND_BOTTOM, SW_SHOWNOACTIVATE, SetWindowPlacement, WINDOWPLACEMENT, WS_EX_TOPMOST};
    let lock = if is_wpf(hwnd) {
        ForegroundLock::try_take()
    } else {
        match ForegroundLock::try_take() {
            Some(lock) => Some(lock),
            None => return ShownOffScreen::Failed,
        }
    };
    let front = foreground();
    unsafe {
        let mut placement: WINDOWPLACEMENT = zeroed();
        placement.length = size_of::<WINDOWPLACEMENT>() as u32;
        if GetWindowPlacement(raw(hwnd), &mut placement) == 0 {
            return ShownOffScreen::Failed;
        }
        let normal = placement.rcNormalPosition;
        let (width, height) = ((normal.right - normal.left).max(1), (normal.bottom - normal.top).max(1));
        let mut away = placement;
        away.flags = 0;
        away.showCmd = SW_SHOWNOACTIVATE as u32;
        away.rcNormalPosition = far_from_screens(normal, width, height);
        animations(hwnd, false);
        let ex_style = GetWindowLongPtrW(raw(hwnd), GWL_EXSTYLE);
        if SetWindowPlacement(raw(hwnd), &away) == 0 {
            animations(hwnd, true);
            return ShownOffScreen::Failed;
        }
        if ex_style as u32 & WS_EX_TOPMOST == 0 {
            SetWindowPos(raw(hwnd), HWND_BOTTOM, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
        let ready = std::sync::Arc::new((std::sync::Mutex::new(None), std::sync::Condvar::new()));
        FILLED.lock().unwrap().remove(&hwnd);
        let shown = OffScreen { hwnd, placement, _lock: lock, restore_minimized: true, front, ready };
        // Should it come forward all the same, the person gets their window back at once.
        let now = foreground();
        if now != front && same_app(now, hwnd) {
            drop(shown);
            return ShownOffScreen::SelfActivated(front);
        }
        ShownOffScreen::Ok(shown)
    }
}

impl Drop for OffScreen {
    fn drop(&mut self) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{SW_SHOWMINNOACTIVE, SetWindowPlacement, WPF_RESTORETOMAXIMIZED};
        if self.restore_minimized {
            let mut back = self.placement;
            back.flags &= WPF_RESTORETOMAXIMIZED;
            back.showCmd = SW_SHOWMINNOACTIVE as u32;
            unsafe { SetWindowPlacement(raw(self.hwnd), &back) };
            animations(self.hwnd, true);
        }
        stop_flashing(self.hwnd);
    }
}

impl Drop for KeptDrawn {
    fn drop(&mut self) {
        // Minimized again first, while the foreground lock is still held.
        if let Some((hwnd, epoch)) = self.1 {
            leave_off_screen(hwnd, epoch);
        }
        let Some(stand_in) = self.0 else { return };
        let mut working = KEPT_DRAWN.lock().unwrap();
        working.0 -= 1;
        if working.0 == 0 {
            announce(stand_in, false);
        }
    }
}

/// Chromium stops drawing a browser or Electron window that other windows cover all over: it
/// leaves it out of accessibility and drops the clicks sent to it. While Windows shows its task
/// switcher thumbnails, Chromium draws every window it has, so deskcrew tells it a thumbnail view
/// opened for as long as an agent works in a covered window. Nothing shows on screen, nothing
/// moves, and the person's windows stay in front.
///
/// A minimized window is shown far off screen for that time, without being activated, and
/// minimized again afterwards: a browser, Electron or WPF one always, since they take no input
/// while minimized, and any other when the agent needs its picture (`pixels`). If Windows would
/// let it come forward over the person's window just then, it stays minimized.
pub fn keep_drawn(hwnd: Hwnd, pixels: bool) -> KeptDrawn {
    let chromium = is_chromium(hwnd);
    let off_screen = if minimized(hwnd) || OFF_SCREEN.lock().unwrap().contains_key(&hwnd) {
        if !(chromium || is_wpf(hwnd) || pixels) {
            return KeptDrawn(None, None);
        }
        let op_lock = window_lock(hwnd);
        let outcome = {
            let _guard = op_lock.lock().unwrap();
            share_off_screen_inner(hwnd)
        };
        let shown = match outcome {
            ShareOutcome::Unavailable => return KeptDrawn(None, None),
            ShareOutcome::SelfActivated(front) => {
                if front != 0 && !bring_forward(front) {
                    stop_flashing(front);
                }
                return KeptDrawn(None, None);
            }
            ShareOutcome::Created(epoch, ready) if !chromium => {
                let signal = ReadyGuard(std::sync::Arc::clone(&ready));
                thread::sleep(SHOWN_WAIT);
                let mut bring_back = 0;
                let keep = {
                    let _guard = op_lock.lock().unwrap();
                    let found = OFF_SCREEN
                        .lock()
                        .unwrap()
                        .get(&hwnd)
                        .filter(|(e, ..)| *e == epoch)
                        .map(|(_, _, off)| off.front);
                    if let Some(front) = found {
                        let now = foreground();
                        if now != front && same_app(now, hwnd) {
                            let removed = OFF_SCREEN.lock().unwrap().remove(&hwnd);
                            drop(removed);
                            bring_back = front;
                            false
                        } else {
                            signal.set(true);
                            true
                        }
                    } else {
                        signal.set(true);
                        true
                    }
                };
                if !keep {
                    if bring_back != 0 && !bring_forward(bring_back) {
                        stop_flashing(bring_back);
                    }
                    signal.set(false);
                    return KeptDrawn(None, None);
                }
                Some((hwnd, epoch))
            }
            ShareOutcome::Created(epoch, ready) => {
                ReadyGuard(ready).set(true);
                Some((hwnd, epoch))
            }
            ShareOutcome::Existing(epoch, ready) if !chromium => {
                let mut ok = ready.0.lock().unwrap();
                while ok.is_none() {
                    ok = ready.1.wait(ok).unwrap();
                }
                if !ok.unwrap_or(false) {
                    return KeptDrawn(None, None);
                }
                Some((hwnd, epoch))
            }
            ShareOutcome::Existing(epoch, _) => Some((hwnd, epoch)),
        };
        if !chromium {
            return KeptDrawn(None, shown);
        }
        shown
    } else if chromium && holds_input(hwnd) {
        None
    } else {
        return KeptDrawn(None, None);
    };
    let Some(stand_in) = thumbnail_stand_in() else { return KeptDrawn(None, off_screen) };
    let ready = {
        let mut working = KEPT_DRAWN.lock().unwrap();
        if working.0 == 0 {
            announce(stand_in, true);
            working.1 = Some(Instant::now() + REDRAW_WAIT);
        }
        working.0 += 1;
        working.1
    };
    if let Some(ready) = ready {
        thread::sleep(ready.saturating_duration_since(Instant::now()));
    }
    // A window just shown needs its own moment to draw, even when the view was already open.
    if off_screen.is_some() {
        thread::sleep(REDRAW_WAIT);
    }
    KeptDrawn(Some(stand_in), off_screen)
}

/// True if no part of this window shows: it is minimized, or other windows cover it all over.
/// A grid of points across it is checked, and any one that reaches it or a popup it owns counts.
pub fn fully_covered(hwnd: Hwnd) -> bool {
    const ACROSS: i32 = 14;
    if unsafe { IsIconic(raw(hwnd)) } != 0 {
        return true;
    }
    let (left, top, width, height) = rect(hwnd);
    if width < 1 || height < 1 {
        return true;
    }
    let inset = 6;
    let at = |from: i32, size: i32, step: i32| from + inset + (size - 2 * inset).max(1) * step / (ACROSS - 1);
    !(0..ACROSS).any(|i| (0..ACROSS).any(|j| owns_point(&[hwnd], at(left, width, i), at(top, height, j))))
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
    let mut owner = unsafe { GetWindow(raw(hwnd), GW_OWNER) } as Hwnd;
    if owner != 0 {
        for _ in 0..8 {
            if family.contains(&owner) {
                return true;
            }
            let next = unsafe { GetWindow(raw(owner), GW_OWNER) } as Hwnd;
            if next == 0 || next == owner {
                break;
            }
            owner = next;
        }
        return false;
    }
    let name = class(hwnd);
    name == "#32768" || name == "#32770" || name.contains("Popup") || name.contains("DropDown")
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
        let me = GetCurrentProcessId();
        let owners = app_pids(root as Hwnd);
        let is_theirs = |window: HWND| {
            let w = window as Hwnd;
            w == root as Hwnd || owners.contains(&pid(w)) || owners.contains(&content_pid(w))
        };
        let mut top = root;
        let mut at = GetWindow(root, GW_HWNDPREV);
        while !at.is_null() {
            let theirs = is_theirs(at);
            let popup = GetWindow(at, GW_OWNER) == root || (theirs && GetWindowLongPtrW(at, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW != 0);
            let shown = IsWindowVisible(at) != 0;
            if popup && shown {
                top = at;
            } else if shown && process(at) != me && !theirs {
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

fn process_identity(owner: u32) -> (u32, u64) {
    use windows_sys::Win32::{
        Foundation::FILETIME,
        System::Threading::{GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
    };
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

/// Which process owns this window, and when that process started. A handle can be given to a new
/// window after the old one closes, so a handle alone does not prove it is the same window.
pub fn identity(hwnd: Hwnd) -> (u32, u64) {
    process_identity(pid(hwnd))
}

/// For a Store app (`ApplicationFrameWindow`), the hosted child app's process identity when present.
/// Returns `None` for non-Store windows or when a minimized/suspended Store app temporarily has no
/// attached content child, so callers can detect frame reuse without mistaking minimize for close.
pub fn content_identity(hwnd: Hwnd) -> Option<(u32, u64)> {
    if class(hwnd) != "ApplicationFrameWindow" {
        return None;
    }
    let own = pid(hwnd);
    let content = content_pid(hwnd);
    (content != 0 && content != own).then(|| process_identity(content))
}

/// Whether the window in front is this one or a dialog or popup of its own. Being the same process
/// is not enough: two windows of one app share a process, and keys must not reach the other one.
pub fn front_is(root: Hwnd, surfaces: &[Hwnd]) -> bool {
    let front = foreground();
    // A dialog that holds all of the window's input counts, owned by it or not.
    if front == root || surfaces.contains(&front) || (!enabled(root) && key_window(root) == front) {
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

pub fn place(hwnd: Hwnd, x: i32, y: i32, w: i32, h: i32) -> Result<(), String> {
    unsafe {
        if IsWindow(raw(hwnd)) == 0 {
            return Err("that window is gone".into());
        }
        SetLastError(0);
        if SetWindowPos(raw(hwnd), null_mut(), x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE) != 0 {
            return Ok(());
        }
        Err(if GetLastError() == ERROR_ACCESS_DENIED {
            "blocked: the window runs as administrator".into()
        } else {
            "Windows could not move or resize that window".into()
        })
    }
}

pub fn show(hwnd: Hwnd, how: i32) -> Result<(), String> {
    unsafe {
        if IsWindow(raw(hwnd)) == 0 {
            return Err("that window is gone".into());
        }
        SetLastError(0);
        if ShowWindow(raw(hwnd), how) == 0 && GetLastError() == ERROR_ACCESS_DENIED {
            return Err("blocked: the window runs as administrator".into());
        }
        Ok(())
    }
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
        // Chromium/Electron/WebView2 routes keys through its Aura HWNDMessageHandler
        // (Chrome_WidgetWin_1), not through child accessibility/GPU windows.
        if let Some(input) = chromium_render_window(hwnd) {
            return input;
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
        children(hwnd).into_iter().find(|child| key_site(*child)).unwrap_or(hwnd)
    }
}

pub fn is_wpf(hwnd: Hwnd) -> bool {
    class(hwnd).starts_with("HwndWrapper")
}

/// The window a Store app or hosted XAML (Windows Terminal, for one) reads its keys from.
pub fn key_site(hwnd: Hwnd) -> bool {
    let name = class(hwnd);
    name == "Windows.UI.Core.CoreWindow" || name.contains("InputSite")
}

/// Windows that take posted keys and characters: every classic window, Chromium/WebView2/CEF
/// widgets, and the key/bridge windows (`CoreWindow`/`InputSite`) of WinUI/XAML/Store apps.
pub fn takes_posted_keys(hwnd: Hwnd) -> bool {
    !draws_itself(hwnd) || key_site(hwnd) || is_chromium_class(&class(hwnd))
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

/// Place `hwnd` immediately behind `after` in Z-order without activating or moving it.
pub fn lower_behind(hwnd: Hwnd, after: Hwnd) {
    if hwnd == 0 || after == 0 || hwnd == after {
        return;
    }
    unsafe {
        SetWindowPos(raw(hwnd), raw(after), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
    }
}

/// Child helper windows inside Chromium/WebView2 that never process posted mouse/key input:
/// `Intermediate D3D Window` is a disabled GPU swapchain surface and `Chrome_RenderWidgetHostHWND`
/// is Chromium's legacy accessibility/trackpad proxy. Real input goes to `Chrome_WidgetWin_1`.
fn is_chromium_non_input_child(name: &str) -> bool {
    name == "Intermediate D3D Window" || name == "Chrome_RenderWidgetHostHWND"
}

/// Window classes that run Chromium's `HWNDMessageHandler` and process posted mouse/key input.
pub fn is_chromium_input_class(name: &str) -> bool {
    name.starts_with("Chrome_WidgetWin_") || name.contains("CefBrowserWindow")
}

/// The innermost child window under a screen point, and that point in its client area. Windows
/// has already converted it to the app's own units, so a message can carry it as it is.
pub fn child_at(root: Hwnd, x: i32, y: i32) -> (Hwnd, i32, i32) {
    let mut at = root;
    loop {
        let mut point = POINT { x, y };
        unsafe {
            ScreenToClient(raw(at), &mut point);
            let child = ChildWindowFromPointEx(raw(at), point, CWP_SKIPINVISIBLE | CWP_SKIPDISABLED | CWP_SKIPTRANSPARENT) as Hwnd;
            if child == 0 || child == at {
                return (at, point.x, point.y);
            }
            let child_class = class(child);
            if is_chromium_non_input_child(&child_class) {
                return (at, point.x, point.y);
            }
            // Once we reach a Chromium HWNDMessageHandler (e.g. Chrome_WidgetWin_1), stop unless
            // descending into a nested Chromium input host (e.g. WebView2 Chrome_WidgetWin_0 -> _1).
            if is_chromium_input_class(&class(at)) && !is_chromium_input_class(&child_class) {
                return (at, point.x, point.y);
            }
            at = child;
        }
    }
}

/// Window classes belonging to Chromium, Electron, CEF and WebView2 hosts or render widgets.
pub fn is_chromium_class(name: &str) -> bool {
    name.starts_with("Chrome_") || name.contains("CefBrowser") || name.contains("WebView")
}

/// The Chromium/Electron/WebView2/CEF `HWNDMessageHandler` window that processes posted mouse and
/// keyboard messages (`Chrome_WidgetWin_1` first, then `Chrome_WidgetWin_0` / `CefBrowserWindow`,
/// falling back to `Chrome_RenderWidgetHostHWND` only when no `Chrome_WidgetWin_*` exists).
pub fn chromium_render_window(root: Hwnd) -> Option<Hwnd> {
    if root == 0 {
        return None;
    }
    let root_class = class(root);
    if root_class == "Chrome_WidgetWin_1" {
        return Some(root);
    }
    let kids = children(root);
    let mut widget0 = None;
    let mut legacy = None;
    for child in kids {
        let name = class(child);
        if name == "Chrome_WidgetWin_1" || name.contains("CefBrowserWindow") {
            return Some(child);
        }
        if widget0.is_none() && name.starts_with("Chrome_WidgetWin_") {
            widget0 = Some(child);
        }
        if legacy.is_none() && name == "Chrome_RenderWidgetHostHWND" {
            legacy = Some(child);
        }
    }
    widget0
        .or_else(|| is_chromium_input_class(&root_class).then_some(root))
        .or(legacy)
        .or_else(|| is_chromium_class(&root_class).then_some(root))
}

/// The Chromium/Electron/WebView2/CEF host window to wake with `WM_ACTIVATE`.
pub fn chromium_host(root: Hwnd) -> Option<Hwnd> {
    chromium_render_window(root)
}

/// Browsers, Electron apps, and embedded Chromium apps (WebView2, Tauri, CEF). They draw
/// themselves, but they read posted mouse and character messages, even while covered.
pub fn is_chromium(hwnd: Hwnd) -> bool {
    hwnd != 0 && (is_chromium_class(&class(hwnd)) || chromium_render_window(hwnd).is_some())
}

/// True when the specific screen point `(x, y)` inside `root` lands in a Chromium/Electron/WebView2/CEF
/// surface (distinguishing embedded WebView2 panels from native WinUI/WPF controls in the same window).
pub fn is_chromium_at(root: Hwnd, x: i32, y: i32) -> bool {
    root != 0 && (is_chromium_class(&class(root)) || is_chromium_class(&class(child_at(root, x, y).0)))
}

/// Apps that read the mouse button directly from posted mouse messages (wParam MK_* flags):
/// Chromium/Electron/WebView2/CEF surfaces, Qt (Krita's canvas draws and its buttons press from
/// them, in the back), and Firefox. Avoiding AttachThreadInput + held button state on these
/// prevents activation/titlebar flicker during drawing and prevents missed button releases.
pub fn reads_posted_mouse(hwnd: Hwnd) -> bool {
    is_chromium_class(&class(hwnd)) || is_qt(hwnd) || is_firefox(hwnd)
}

pub fn is_firefox(hwnd: Hwnd) -> bool {
    class(hwnd).starts_with("MozillaWindowClass")
}

pub fn is_qt(hwnd: Hwnd) -> bool {
    class(hwnd).starts_with("Qt")
}

/// Windows that read keys only while they hold keyboard focus in their own app: WPF, Qt, which
/// takes a shortcut only in the window it thinks is active, and Firefox.
pub fn keys_need_focus(hwnd: Hwnd) -> bool {
    is_wpf(hwnd) || is_qt(hwnd) || is_firefox(hwnd)
}

/// Windows whose pointer input only comes from a real mouse, pen or touch: posted mouse messages
/// do nothing there (WPF, WinUI and Store apps), so they need the pen or `--real`. WPF does take
/// them while it holds the mouse capture, but only once its window is active.
pub fn ignores_posted_mouse(hwnd: Hwnd) -> bool {
    draws_itself(hwnd) && !reads_posted_mouse(hwnd)
}

/// True if this point is on some monitor.
pub fn on_screen(x: i32, y: i32) -> bool {
    screens().iter().any(|&(left, top, w, h, _)| x >= left && y >= top && x < left + w && y < top + h)
}

/// Windows of Store and WinUI apps, and the XAML bridge windows hosted inside desktop apps such as
/// the new Notepad.
fn is_xaml_class(name: &str) -> bool {
    [
        "Xaml",
        "CoreWindow",
        "DesktopChildSiteBridge",
        "DesktopWindowContentBridge",
        "InputSite",
        "ApplicationFrame",
        "WinUIDesktop",
    ]
    .iter()
    .any(|part| name.contains(part))
}

fn is_xaml(hwnd: Hwnd) -> bool {
    is_xaml_class(&class(hwnd))
}

/// True if this window is a Store or WinUI app, or hosts WinUI / XAML Islands inside itself.
/// Store and WinUI apps can pull themselves forward a moment after an accessibility action runs.
pub fn is_store(hwnd: Hwnd) -> bool {
    if hwnd == 0 {
        return false;
    }
    if is_xaml(hwnd) {
        return true;
    }
    unsafe extern "system" fn find_xaml(child: HWND, found: LPARAM) -> BOOL {
        if is_xaml(child as Hwnd) {
            unsafe { *(found as *mut bool) = true };
            return 0;
        }
        1
    }
    let mut found = false;
    unsafe { EnumChildWindows(raw(hwnd), Some(find_xaml), &mut found as *mut bool as LPARAM) };
    found
}

/// Windows drawn by their own engine: their controls are not separate windows, and posted
/// messages do not reach them, Qt's pointer input aside.
pub fn draws_itself(hwnd: Hwnd) -> bool {
    let name = class(hwnd);
    is_xaml_class(&name) || ["Mozilla", "HwndWrapper", "Qt"].iter().any(|part| name.contains(part))
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
        let mut held = LOCKS.lock().unwrap();
        // Always refresh the OS lock: Windows drops it automatically if the user switches windows,
        // while keeping `held.1` true if an outer guard already holds the OS lock.
        let locked = unsafe { LockSetForegroundWindow(LSFW_LOCK) } != 0;
        held.1 = if held.0 == 0 { locked } else { held.1 || locked };
        held.0 += 1;
        ForegroundLock
    }

    /// The lock, if Windows gave it: it only does to a process that could take the foreground.
    pub fn try_take() -> Option<ForegroundLock> {
        let mut held = LOCKS.lock().unwrap();
        let locked = unsafe { LockSetForegroundWindow(LSFW_LOCK) } != 0;
        if !locked {
            if held.0 == 0 {
                held.1 = false;
            }
            return None;
        }
        held.1 = true;
        held.0 += 1;
        Some(ForegroundLock)
    }
}

/// Windows has one foreground lock, so agents working at once share it: how many hold it, and
/// whether Windows gave it. The last to let go unlocks it.
static LOCKS: std::sync::Mutex<(usize, bool)> = std::sync::Mutex::new((0, false));

impl Drop for ForegroundLock {
    fn drop(&mut self) {
        let mut held = LOCKS.lock().unwrap();
        held.0 -= 1;
        if held.0 == 0 && held.1 {
            unsafe { LockSetForegroundWindow(LSFW_UNLOCK) };
            held.1 = false;
        }
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
        let owners = app_pids(now);
        let owners = if owners.is_empty() { vec![pid(now)] } else { owners };
        for owner in owners {
            if !is_frame_host(owner) && !pulled.contains(&owner) {
                pulled.push(owner);
            }
        }
        drop(pulled);
        if !bring_forward(self.front) {
            stop_flashing(self.front);
        }
        let intruder = if outer != 0 { outer } else { now };
        self.keep_behind(intruder);
        stop_flashing(intruder);
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
pub fn person_switching() -> bool {
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
    let pids: Vec<u32> = pids.iter().copied().filter(|p| !is_frame_host(*p)).collect();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xaml_classes_include_store_winui_and_xaml_islands_bridges() {
        for name in [
            "ApplicationFrameWindow",
            "Windows.UI.Core.CoreWindow",
            "Windows.UI.Composition.DesktopWindowContentBridge",
            "Microsoft.UI.Content.DesktopChildSiteBridge",
            "Windows.UI.Input.InputSite.WindowClass",
            "InputSiteWindowClass",
            "WinUIDesktopWin32WindowClass",
            "DesktopWindowXamlSource",
        ] {
            assert!(is_xaml_class(name), "{name} should be recognized as XAML/WinUI");
        }
        for name in ["Notepad", "Edit", "RichEditD2DPT", "Chrome_WidgetWin_1", "WindowsForms10.Window.8.app.0.1", "HwndWrapper[App;;123]"] {
            assert!(!is_xaml_class(name), "{name} should not match is_xaml_class directly");
        }
    }

    #[test]
    fn snap_rect_splits_work_area_cleanly() {
        let work = RECT { left: 100, top: 40, right: 1101, bottom: 840 };
        assert_eq!(snap_rect(work, false), (100, 40, 500, 800));
        assert_eq!(snap_rect(work, true), (600, 40, 501, 800));
    }

    #[test]
    fn move_monitor_rect_preserves_offset_and_clamps_to_destination() {
        let src = RECT { left: -1920, top: 0, right: 0, bottom: 1040 };
        let dst = RECT { left: 0, top: 0, right: 1280, bottom: 720 };
        assert_eq!(move_monitor_rect((-1820, 100, 600, 400), src, dst), (100, 100, 600, 400));
        assert_eq!(move_monitor_rect((-320, 400, 1600, 900), src, dst), (0, 0, 1280, 720));
        let empty = RECT { left: 100, top: 200, right: 100, bottom: 150 };
        assert_eq!(move_monitor_rect((-1820, 100, 600, 400), src, empty), (100, 200, 1, 1));
    }

    #[test]
    fn chromium_classes_cover_browsers_electron_cef_and_webview2() {
        for name in [
            "Chrome_WidgetWin_1",
            "Chrome_WidgetWin_0",
            "Chrome_RenderWidgetHostHWND",
            "CefBrowserWindow",
            "WebView2Host",
        ] {
            assert!(is_chromium_class(name), "{name} should match is_chromium_class");
        }
        for name in ["Notepad", "HwndWrapper[App;;123]", "ApplicationFrameWindow", "MozillaWindowClass", "Qt620QWindowIcon"] {
            assert!(!is_chromium_class(name), "{name} should not match is_chromium_class");
        }
        assert!(is_chromium_input_class("Chrome_WidgetWin_1"));
        assert!(is_chromium_input_class("CefBrowserWindow"));
        assert!(!is_chromium_input_class("Chrome_RenderWidgetHostHWND"));
        assert!(!is_chromium_input_class("Intermediate D3D Window"));
        assert!(is_chromium_non_input_child("Intermediate D3D Window"));
        assert!(is_chromium_non_input_child("Chrome_RenderWidgetHostHWND"));
    }
}
