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
