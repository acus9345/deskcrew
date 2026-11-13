// Starting apps, and the clipboard. Both go through Windows itself, not through any window.
use crate::win::{self, Hwnd};
use std::{
    collections::HashSet,
    ptr::{copy_nonoverlapping, null, null_mut},
    thread,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        System::Com::CoTaskMemFree,
        UI::Shell::{BHID_EnumItems, IEnumShellItems, IShellItem, SHCreateItemFromParsingName, SIGDN_NORMALDISPLAY, SIGDN_PARENTRELATIVEPARSING},
    },
    core::{HSTRING, PWSTR},
};
use windows_sys::Win32::{
    Foundation::GlobalFree,
    System::{
        DataExchange::{
            CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData, GetClipboardSequenceNumber, IsClipboardFormatAvailable, OpenClipboard,
            RegisterClipboardFormatW, SetClipboardData,
        },
        Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock},
    },
    UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNOACTIVATE},
};

const CF_UNICODETEXT: u32 = 13;
const MAX_CLIPBOARD_CHARS: usize = 8000;
const WINDOW_TIMEOUT: Duration = Duration::from_secs(6);
const LAUNCH_WATCH: Duration = Duration::from_millis(20);
const LAUNCH_SETTLE: Duration = Duration::from_millis(2500);

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// A path, a URL or a program on PATH: Windows can open these directly.
fn shell_open(file: &str, args: &str) -> Result<(), String> {
    let (verb, file_w, args_w) = (wide("open"), wide(file), wide(args));
    let code = unsafe { ShellExecuteW(null_mut(), verb.as_ptr(), file_w.as_ptr(), args_w.as_ptr(), null(), SW_SHOWNOACTIVATE) };
    if code as isize > 32 { Ok(()) } else { Err(format!("Windows could not open '{file}'")) }
}

fn pwstr(value: PWSTR) -> String {
    let text = unsafe { value.to_string() }.unwrap_or_default();
    unsafe { CoTaskMemFree(Some(value.0 as *const _)) };
    text
}

/// Installed apps by display name, including Store apps that have no file to run.
fn start_menu() -> Vec<(String, String)> {
    let mut out = Vec::new();
    unsafe {
        let Ok(folder) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from("shell:AppsFolder"), None) else { return out };
        let Ok(items) = folder.BindToHandler::<_, IEnumShellItems>(None, &BHID_EnumItems) else { return out };
        loop {
            let mut slot = [None];
            let mut got = 0;
            if items.Next(&mut slot, Some(&mut got)).is_err() || got == 0 {
                return out;
            }
            let Some(item) = slot[0].take() else { return out };
            if let (Ok(name), Ok(id)) = (item.GetDisplayName(SIGDN_NORMALDISPLAY), item.GetDisplayName(SIGDN_PARENTRELATIVEPARSING)) {
                out.push((pwstr(name), pwstr(id)));
            }
        }
    }
}

fn start(app: &str, args: &str) -> Result<(), String> {
    let lower = app.to_lowercase();
    let is_path = app.contains('\\') || app.contains('/') || app.contains(':') || lower.ends_with(".exe");
    if is_path {
        return shell_open(app, args);
    }
    // Store apps run through their alias: a direct start misses the libraries they ship with.
    let alias = std::env::var_os("LOCALAPPDATA").map(|root| std::path::Path::new(&root).join("Microsoft\\WindowsApps").join(format!("{lower}.exe")));
    if let Some(alias) = alias.filter(|path| path.exists()) {
        return shell_open(&alias.to_string_lossy(), args);
    }
    let apps = start_menu();
    if let Some((_, id)) = apps.iter().find(|(name, _)| name.to_lowercase() == lower) {
        return shell_open("explorer.exe", &format!(r"shell:AppsFolder\{id}"));
    }
    if shell_open(app, args).is_ok() {
        return Ok(());
    }
    let close: Vec<&str> = apps.iter().filter(|(name, _)| name.to_lowercase().contains(&lower)).map(|(name, _)| name.as_str()).collect();
    match close.as_slice() {
        [] => Err(format!("no app named '{app}' in the Start Menu or on PATH")),
        [only] => {
            let found = apps.iter().find(|(name, _)| name == only);
            shell_open("explorer.exe", &format!(r"shell:AppsFolder\{}", found.map_or("", |(_, id)| id)))
        }
        many => Err(format!("'{app}' could be: {}", many.iter().take(8).copied().collect::<Vec<_>>().join(", "))),
    }
}

/// Start an app and return its first new window, if one appears in time. Unless `front` is set,
/// the person keeps their window: the new one goes behind it, however often it tries to come up.
/// Chromium browsers by name or path.
fn is_chromium_browser(app: &str) -> bool {
    let name = app.to_lowercase();
    let name = name.rsplit(['\\', '/']).next().unwrap_or(&name);
    ["msedge", "microsoft edge", "chrome", "chromium", "brave", "vivaldi", "opera"].iter().any(|browser| name.starts_with(browser))
}

/// A Chromium browser keeps every click and key while its window is covered all over, and runs
/// them only once part of it shows. Started with its cover check off, it takes them behind the
/// person's windows as it does in front.
fn taking_input_covered(args: &str) -> String {
    const SWITCH: &str = "--disable-features=CalculateNativeWinOcclusion";
    if args.contains("CalculateNativeWinOcclusion") {
        return args.to_string();
    }
    match args.find("--disable-features=") {
        Some(at) => format!("{}{SWITCH},{}", &args[..at], &args[at + "--disable-features=".len()..]),
        None if args.trim().is_empty() => SWITCH.to_string(),
        None => format!("{SWITCH} {args}"),
    }
}

pub fn launch(app: &str, args: &str, front: bool) -> Result<Option<Hwnd>, String> {
    let browser = is_chromium_browser(app);
    let args = if browser { taking_input_covered(args) } else { args.to_string() };
    let args = args.as_str();
    let before: HashSet<Hwnd> = win::top_level().into_iter().collect();
    let pids_before: HashSet<u32> = before.iter().map(|hwnd| win::pid(*hwnd)).collect();
    let person = win::FocusGuard::take();
    // Agents run under the app in front, and Windows lets that app's descendants take focus once:
    // the service would pass that on to the first app it opens. A lock stops it ever happening,
    // and Windows lifts the lock by itself the moment the person switches windows.
    let _lock = (!front).then(win::ForegroundLock::take);
    start(app, args)?;
    let started = Instant::now();
    let mut opened = None;
    while started.elapsed() < WINDOW_TIMEOUT {
        thread::sleep(LAUNCH_WATCH);
        if !front {
            person.hold_against(&before);
        }
        opened = win::list().into_iter().find(|w| !before.contains(&w.hwnd)).map(|w| w.hwnd);
        if opened.is_some() {
            break;
        }
    }
    let Some(hwnd) = opened else { return Ok(None) };
    // The new window is the agent's: its taskbar button is not left flashing for the person.
    win::note_worked_in(hwnd);
    // A browser already running opened this window in its old process, which kept its cover check.
    if browser && win::is_chromium(hwnd) && !pids_before.contains(&win::pid(hwnd)) {
        win::note_takes_input_covered(win::pid(hwnd));
    }
    if !front {
        // Apps keep reaching for the front for a while after their window appears.
        let appeared = Instant::now();
        while appeared.elapsed() < LAUNCH_SETTLE {
            // A Store app's own process shows up inside its frame a moment after the frame.
            win::note_worked_in(hwnd);
            person.hold_against(&before);
            person.keep_behind(hwnd);
            thread::sleep(LAUNCH_WATCH);
        }
    }
    Ok(Some(hwnd))
}

fn open_clipboard() -> Result<(), String> {
    for _ in 0..25 {
        if unsafe { OpenClipboard(null_mut()) } != 0 {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(20));
    }
    Err("another program is holding the clipboard".into())
}

pub const NO_CLIPBOARD_TEXT: &str = "(no text on the clipboard)";

pub fn clipboard_text() -> Result<String, String> {
    unsafe {
        if IsClipboardFormatAvailable(CF_UNICODETEXT) == 0 {
            return Ok(NO_CLIPBOARD_TEXT.into());
        }
        open_clipboard()?;
        let data = GetClipboardData(CF_UNICODETEXT);
        let mut text = String::new();
        if !data.is_null() {
            let locked = GlobalLock(data) as *const u16;
            if !locked.is_null() {
                let units = GlobalSize(data) / 2;
                let slice = std::slice::from_raw_parts(locked, units);
                let end = slice.iter().position(|u| *u == 0).unwrap_or(units);
                text = String::from_utf16_lossy(&slice[..end.min(MAX_CLIPBOARD_CHARS)]);
                GlobalUnlock(data);
            }
        }
        CloseClipboard();
        Ok(text)
    }
}

pub fn set_clipboard(text: &str) -> Result<(), String> {
    put_on_clipboard(&[(CF_UNICODETEXT, units_as_bytes(&wide(text)))])
}

fn units_as_bytes(units: &[u16]) -> Vec<u8> {
    units.iter().flat_map(|u| u.to_le_bytes()).collect()
}

/// Empty the clipboard and put these formats on it, opening it once so no one sees half of it.
fn put_on_clipboard(formats: &[(u32, Vec<u8>)]) -> Result<(), String> {
    unsafe {
        open_clipboard()?;
        EmptyClipboard();
        for (format, bytes) in formats {
            let memory = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1));
            if memory.is_null() {
                CloseClipboard();
                return Err("not enough memory for the clipboard".into());
            }
            let locked = GlobalLock(memory) as *mut u8;
            copy_nonoverlapping(bytes.as_ptr(), locked, bytes.len());
            GlobalUnlock(memory);
            if SetClipboardData(*format, memory).is_null() {
                GlobalFree(memory);
                CloseClipboard();
                return Err("Windows refused the clipboard text".into());
            }
        }
        CloseClipboard();
        Ok(())
    }
}

/// Clipboard formats that are a handle to something other than a block of memory (a bitmap, a
/// metafile, a palette, or a private handle), which cannot be copied as bytes. Windows makes the
/// bitmap ones again from the DIB that is kept.
fn handle_format(format: u32) -> bool {
    matches!(format, 2 | 3 | 9 | 14 | 0x80 | 0x82 | 0x83 | 0x8E) || (0x200..=0x3FF).contains(&format)
}

/// Clipboard text that clipboard history, cloud clipboard and clipboard managers leave out.
fn quiet_format() -> u32 {
    let name = wide("ExcludeClipboardContentFromMonitorProcessing");
    unsafe { RegisterClipboardFormatW(name.as_ptr()) }
}

/// Everything that was on the clipboard before an agent borrowed it to paste: text, pictures,
/// copied files, all of it.
pub struct SavedClipboard {
    formats: Vec<(u32, Vec<u8>)>,
    /// Which change of the clipboard the agent's own text was, once it is put there.
    lent: Option<u32>,
}

impl SavedClipboard {
    pub fn take() -> Result<SavedClipboard, String> {
        let mut formats = Vec::new();
        unsafe {
            open_clipboard()?;
            let mut format = 0;
            loop {
                format = EnumClipboardFormats(format);
                if format == 0 {
                    break;
                }
                if handle_format(format) {
                    continue;
                }
                let data = GetClipboardData(format);
                if data.is_null() {
                    continue;
                }
                let locked = GlobalLock(data) as *const u8;
                if locked.is_null() {
                    continue;
                }
                formats.push((format, std::slice::from_raw_parts(locked, GlobalSize(data)).to_vec()));
                GlobalUnlock(data);
            }
            CloseClipboard();
        }
        Ok(SavedClipboard { formats, lent: None })
    }

    /// Put the agent's text on the clipboard, kept out of the person's clipboard history.
    pub fn lend(&mut self, text: &str) -> Result<(), String> {
        put_on_clipboard(&[(CF_UNICODETEXT, units_as_bytes(&wide(text))), (quiet_format(), vec![0; 4])])?;
        self.lent = Some(unsafe { GetClipboardSequenceNumber() });
        Ok(())
    }

    /// Put the person's clipboard back, unless they copied something new in the meantime.
    pub fn put_back(self) -> Result<(), String> {
        if self.lent.is_some_and(|lent| unsafe { GetClipboardSequenceNumber() } != lent) {
            return Ok(());
        }
        // Putting it back is not a new copy, so history does not get it twice.
        let quiet = quiet_format();
        let mut formats = self.formats;
        if !formats.iter().any(|(format, _)| *format == quiet) {
            formats.push((quiet, vec![0; 4]));
        }
        put_on_clipboard(&formats)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browsers_get_their_cover_check_turned_off() {
        assert!(is_chromium_browser("msedge") && is_chromium_browser(r"C:\Program Files\Google\Chrome\Application\chrome.exe"));
        assert!(!is_chromium_browser("notepad"));
        assert_eq!(taking_input_covered(""), "--disable-features=CalculateNativeWinOcclusion");
        assert_eq!(taking_input_covered("--new-window x"), "--disable-features=CalculateNativeWinOcclusion --new-window x");
        assert_eq!(taking_input_covered("--disable-features=Foo x"), "--disable-features=CalculateNativeWinOcclusion,Foo x");
    }
}
