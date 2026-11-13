// The windows on screen and starting apps.
use super::{ax, ffi::*};
use std::{collections::HashSet, process::Command, thread, time::Duration};

const LAUNCH_WAIT: Duration = Duration::from_secs(10);
const LAUNCH_STEP: Duration = Duration::from_millis(150);

#[derive(Clone)]
pub struct Window {
    pub id: u32,
    pub pid: i32,
    pub app: String,
    pub title: String,
    pub rect: ax::Bounds,
}

/// Ordinary app windows on screen, front first. Menus, the Dock and overlays sit on other layers.
pub fn list() -> Vec<Window> {
    let mut out = Vec::new();
    unsafe {
        let Some(info) = Owned::new(CGWindowListCopyWindowInfo(kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements, kCGNullWindowID)) else {
            return out;
        };
        let me = std::process::id() as i32;
        for entry in items(info.0) {
            let number = |key: CFStringRef| number_of(CFDictionaryGetValue(entry, key));
            if number(kCGWindowLayer) != Some(0.0) {
                continue;
            }
            let (Some(id), Some(pid)) = (number(kCGWindowNumber), number(kCGWindowOwnerPID)) else { continue };
            if pid as i32 == me {
                continue;
            }
            let mut bounds = CGRect::default();
            let dict = CFDictionaryGetValue(entry, kCGWindowBounds);
            if dict.is_null() || !CGRectMakeWithDictionaryRepresentation(dict, &mut bounds) || bounds.size.width < 2.0 || bounds.size.height < 2.0 {
                continue;
            }
            out.push(Window {
                id: id as u32,
                pid: pid as i32,
                app: string_of(CFDictionaryGetValue(entry, kCGWindowOwnerName)).unwrap_or_default(),
                // Empty without screen recording permission: filled from accessibility below.
                title: string_of(CFDictionaryGetValue(entry, kCGWindowName)).unwrap_or_default(),
                rect: (bounds.origin.x as i32, bounds.origin.y as i32, bounds.size.width as i32, bounds.size.height as i32),
            });
        }
    }
    let untitled: HashSet<i32> = out.iter().filter(|w| w.title.is_empty()).map(|w| w.pid).collect();
    for pid in untitled {
        for (window, number) in ax::windows_of(pid) {
            let title = ax::text(&window, "AXTitle").unwrap_or_default();
            if let Some(listed) = out.iter_mut().find(|w| w.id == number) {
                listed.title = title;
            }
        }
    }
    out
}

pub fn find(id: u32) -> Option<Window> {
    list().into_iter().find(|w| w.id == id)
}

/// The accessibility element for a window number.
pub fn element(window: &Window) -> Option<Owned> {
    ax::windows_of(window.pid).into_iter().find(|(_, number)| *number == window.id).map(|(element, _)| element)
}

/// Open an app by name or path, without bringing it to the front, and wait for a new window.
pub fn launch(app: &str, args: &[String], front: bool) -> Result<Option<Window>, String> {
    let before: HashSet<u32> = list().iter().map(|w| w.id).collect();
    let mut open = Command::new("/usr/bin/open");
    if !front {
        open.arg("-g");
    }
    if app.contains('/') {
        open.arg(app);
    } else {
        open.args(["-a", app]);
    }
    if !args.is_empty() {
        open.arg("--args").args(args);
    }
    let done = open.output().map_err(|e| e.to_string())?;
    if !done.status.success() {
        return Err(format!("no app named '{app}': {}", String::from_utf8_lossy(&done.stderr).trim()));
    }
    let started = std::time::Instant::now();
    while started.elapsed() < LAUNCH_WAIT {
        if let Some(new) = list().into_iter().find(|w| !before.contains(&w.id)) {
            return Ok(Some(new));
        }
        thread::sleep(LAUNCH_STEP);
    }
    Ok(None)
}

/// The main display's height in points, to turn top-left coordinates into AppKit's bottom-left ones.
pub fn main_height() -> f64 {
    unsafe { CGDisplayBounds(CGMainDisplayID()).size.height }
}

/// The app in front, by process id.
pub fn front_pid() -> Option<i32> {
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    unsafe {
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let app: *mut AnyObject = msg_send![workspace, frontmostApplication];
        if app.is_null() {
            return None;
        }
        let pid: i32 = msg_send![app, processIdentifier];
        Some(pid)
    }
}

/// Bring an app to the front. True once it is there.
pub fn activate(pid: i32) -> bool {
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    let asked = unsafe {
        let app: *mut AnyObject = msg_send![class!(NSRunningApplication), runningApplicationWithProcessIdentifier: pid];
        // NSApplicationActivateIgnoringOtherApps, so it comes forward even from the background.
        !app.is_null() && {
            let done: bool = msg_send![app, activateWithOptions: 2usize];
            done
        }
    };
    if !asked {
        return false;
    }
    for _ in 0..20 {
        if front_pid() == Some(pid) {
            return true;
        }
        thread::sleep(Duration::from_millis(25));
    }
    false
}

/// The usable part of the main display, without the menu bar and Dock, in top-left points.
pub fn work_area() -> ax::Bounds {
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    unsafe {
        let screen: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
        if screen.is_null() {
            let all = CGDisplayBounds(CGMainDisplayID());
            return (0, 0, all.size.width as i32, all.size.height as i32);
        }
        let area: CGRect = msg_send![screen, visibleFrame];
        let top = main_height() - area.origin.y - area.size.height;
        (area.origin.x as i32, top as i32, area.size.width as i32, area.size.height as i32)
    }
}

/// Each display: where it is in points, and whether it is the main one.
pub fn screens() -> Vec<(ax::Bounds, bool)> {
    let mut ids = [0u32; 16];
    let mut count = 0u32;
    unsafe { CGGetActiveDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count) };
    let main = unsafe { CGMainDisplayID() };
    ids[..count as usize]
        .iter()
        .map(|id| {
            let r = unsafe { CGDisplayBounds(*id) };
            ((r.origin.x as i32, r.origin.y as i32, r.size.width as i32, r.size.height as i32), *id == main)
        })
        .collect()
}

const PLAIN_TEXT: &std::ffi::CStr = c"public.utf8-plain-text";

fn pasteboard() -> *mut objc2::runtime::AnyObject {
    use objc2::{class, msg_send};
    unsafe { msg_send![class!(NSPasteboard), generalPasteboard] }
}

fn ns_string(text: &str) -> *mut objc2::runtime::AnyObject {
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    let owned = std::ffi::CString::new(text.replace('\0', "")).unwrap_or_default();
    unsafe {
        let made: *mut AnyObject = msg_send![class!(NSString), stringWithUTF8String: owned.as_ptr()];
        made
    }
}

/// The clipboard's text, or None if it holds no text.
pub fn clipboard() -> Option<String> {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    objc2::rc::autoreleasepool(|_| unsafe {
        let kind = ns_string(PLAIN_TEXT.to_str().unwrap_or_default());
        let text: *mut AnyObject = msg_send![pasteboard(), stringForType: kind];
        if text.is_null() {
            return None;
        }
        let utf8: *const std::ffi::c_char = msg_send![text, UTF8String];
        (!utf8.is_null()).then(|| std::ffi::CStr::from_ptr(utf8).to_string_lossy().into_owned())
    })
}

/// Everything on the clipboard before an agent borrowed it to paste: every item and every kind of
/// data on each, so pictures and copied files come back as well as text.
pub struct SavedClipboard {
    items: Vec<Vec<(String, Vec<u8>)>>,
    /// The clipboard's change count once the agent's own text is on it.
    lent: Option<isize>,
}

/// Kinds that ask clipboard managers and Universal Clipboard to leave an entry alone (nspasteboard.org).
const LEAVE_ALONE: [&str; 2] = ["org.nspasteboard.TransientType", "org.nspasteboard.ConcealedType"];

fn mark_left_alone(board: *mut objc2::runtime::AnyObject) {
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    unsafe {
        for kind in LEAVE_ALONE {
            let empty: *mut AnyObject = msg_send![class!(NSData), data];
            let _: bool = msg_send![board, setData: empty, forType: ns_string(kind)];
        }
    }
}

impl SavedClipboard {
    pub fn take() -> SavedClipboard {
        use objc2::msg_send;
        use objc2::runtime::AnyObject;
        use std::ffi::{CStr, c_char, c_void};
        let items = objc2::rc::autoreleasepool(|_| unsafe {
            let mut items = Vec::new();
            let list: *mut AnyObject = msg_send![pasteboard(), pasteboardItems];
            if list.is_null() {
                return items;
            }
            let count: usize = msg_send![list, count];
            for at in 0..count {
                let item: *mut AnyObject = msg_send![list, objectAtIndex: at];
                let kinds: *mut AnyObject = msg_send![item, types];
                let kinds_count: usize = if kinds.is_null() { 0 } else { msg_send![kinds, count] };
                let mut kept = Vec::new();
                for k in 0..kinds_count {
                    let kind: *mut AnyObject = msg_send![kinds, objectAtIndex: k];
                    let name: *const c_char = msg_send![kind, UTF8String];
                    let data: *mut AnyObject = msg_send![item, dataForType: kind];
                    if name.is_null() || data.is_null() {
                        continue;
                    }
                    let length: usize = msg_send![data, length];
                    let bytes: *const c_void = msg_send![data, bytes];
                    let copy = if length == 0 || bytes.is_null() { Vec::new() } else { std::slice::from_raw_parts(bytes as *const u8, length).to_vec() };
                    kept.push((CStr::from_ptr(name).to_string_lossy().into_owned(), copy));
                }
                if !kept.is_empty() {
                    items.push(kept);
                }
            }
            items
        });
        SavedClipboard { items, lent: None }
    }

    /// Put the agent's text on the clipboard, marked so clipboard managers leave it out.
    pub fn lend(&mut self, text: &str) -> Result<(), String> {
        use objc2::msg_send;
        objc2::rc::autoreleasepool(|_| unsafe {
            let board = pasteboard();
            let _: isize = msg_send![board, clearContents];
            let kind = ns_string(PLAIN_TEXT.to_str().unwrap_or_default());
            let done: bool = msg_send![board, setString: ns_string(text), forType: kind];
            if !done {
                return Err("macOS refused the clipboard text".into());
            }
            mark_left_alone(board);
            self.lent = Some(msg_send![board, changeCount]);
            Ok(())
        })
    }

    /// Put the person's clipboard back, unless they copied something new in the meantime.
    pub fn put_back(self) -> Result<(), String> {
        use objc2::runtime::AnyObject;
        use objc2::{class, msg_send};
        use std::ffi::c_void;
        objc2::rc::autoreleasepool(|_| unsafe {
            let board = pasteboard();
            if let Some(lent) = self.lent {
                let now: isize = msg_send![board, changeCount];
                if now != lent {
                    return Ok(());
                }
            }
            let _: isize = msg_send![board, clearContents];
            if self.items.is_empty() {
                return Ok(());
            }
            let list: *mut AnyObject = msg_send![class!(NSMutableArray), array];
            for kept in &self.items {
                let item: *mut AnyObject = msg_send![class!(NSPasteboardItem), new];
                for (kind, bytes) in kept {
                    let data: *mut AnyObject = msg_send![class!(NSData), dataWithBytes: bytes.as_ptr() as *const c_void, length: bytes.len()];
                    let _: bool = msg_send![item, setData: data, forType: ns_string(kind)];
                }
                let _: () = msg_send![list, addObject: item];
                let _: () = msg_send![item, release];
            }
            let done: bool = msg_send![board, writeObjects: list];
            if done { Ok(()) } else { Err("macOS would not take the clipboard back".into()) }
        })
    }
}

/// Replace the clipboard with text, or empty it with None.
pub fn set_clipboard(text: Option<&str>) -> Result<(), String> {
    use objc2::msg_send;
    objc2::rc::autoreleasepool(|_| unsafe {
        let board = pasteboard();
        let _: isize = msg_send![board, clearContents];
        let Some(text) = text else { return Ok(()) };
        let kind = ns_string(PLAIN_TEXT.to_str().unwrap_or_default());
        let done: bool = msg_send![board, setString: ns_string(text), forType: kind];
        if done { Ok(()) } else { Err("macOS refused the clipboard text".into()) }
    })
}
