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
        DataExchange::{CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard, SetClipboardData},
        Memory::{GHND, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock},
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
pub fn launch(app: &str, args: &str, front: bool) -> Result<Option<Hwnd>, String> {
    let before: HashSet<Hwnd> = win::top_level().into_iter().collect();
    let person = win::FocusGuard::take();
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
    if !front {
        // Apps keep reaching for the front for a while after their window appears.
        let appeared = Instant::now();
        while appeared.elapsed() < LAUNCH_SETTLE {
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

pub fn clipboard_text() -> Result<String, String> {
    unsafe {
        if IsClipboardFormatAvailable(CF_UNICODETEXT) == 0 {
            return Ok("(no text on the clipboard)".into());
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
    let units = wide(text);
    unsafe {
        open_clipboard()?;
        let memory = GlobalAlloc(GHND, units.len() * 2);
        if memory.is_null() {
            CloseClipboard();
            return Err("not enough memory for the clipboard".into());
        }
        let locked = GlobalLock(memory) as *mut u16;
        copy_nonoverlapping(units.as_ptr(), locked, units.len());
        GlobalUnlock(memory);
        EmptyClipboard();
        if SetClipboardData(CF_UNICODETEXT, memory).is_null() {
            GlobalFree(memory);
            CloseClipboard();
            return Err("Windows refused the clipboard text".into());
        }
        CloseClipboard();
        Ok(())
    }
}
