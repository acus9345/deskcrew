// Mouse and keyboard events posted straight to one app, so the person's own pointer never moves
// and their front app keeps its keys.
use super::ffi::*;
use std::{ptr::null, thread, time::Duration};

const CLICK_GAP: Duration = Duration::from_millis(30);
const KEY_GAP: Duration = Duration::from_millis(8);
const TYPE_PIECE: usize = 16;

#[derive(Clone, Copy, PartialEq)]
pub enum Button {
    Left,
    Right,
}

fn post(pid: i32, event: CGEventRef) -> Result<(), String> {
    if event.is_null() {
        return Err("macOS would not make that event".into());
    }
    unsafe {
        CGEventPostToPid(pid, event);
        CFRelease(event as CFTypeRef);
    }
    Ok(())
}

/// A click at a screen point, in points, aimed at one window of the app.
pub fn click(pid: i32, window: u32, x: f64, y: f64, button: Button, count: u32) -> Result<(), String> {
    let at = CGPoint { x, y };
    let (down, up, code) = match button {
        Button::Left => (kCGEventLeftMouseDown, kCGEventLeftMouseUp, kCGMouseButtonLeft),
        Button::Right => (kCGEventRightMouseDown, kCGEventRightMouseUp, kCGMouseButtonRight),
    };
    for click in 1..=count {
        for kind in [down, up] {
            let event = unsafe { CGEventCreateMouseEvent(null(), kind, at, code) };
            if !event.is_null() {
                unsafe {
                    CGEventSetIntegerValueField(event, kCGMouseEventClickState, click as i64);
                    CGEventSetIntegerValueField(event, kCGMouseEventWindowUnderMousePointer, window as i64);
                    CGEventSetIntegerValueField(event, kCGMouseEventWindowUnderMousePointerThatCanHandleThisEvent, window as i64);
                }
            }
            post(pid, event)?;
            thread::sleep(CLICK_GAP);
        }
    }
    Ok(())
}

/// A press, a path of drags and a release, for drawing and dragging.
pub fn stroke(pid: i32, window: u32, points: &[(f64, f64)]) -> Result<(), String> {
    let Some((&first, rest)) = points.split_first() else { return Ok(()) };
    let make = |kind: u32, (x, y): (f64, f64)| {
        let event = unsafe { CGEventCreateMouseEvent(null(), kind, CGPoint { x, y }, kCGMouseButtonLeft) };
        if !event.is_null() {
            unsafe {
                CGEventSetIntegerValueField(event, kCGMouseEventWindowUnderMousePointer, window as i64);
                CGEventSetIntegerValueField(event, kCGMouseEventWindowUnderMousePointerThatCanHandleThisEvent, window as i64);
            }
        }
        event
    };
    post(pid, make(kCGEventLeftMouseDown, first))?;
    for point in rest {
        thread::sleep(KEY_GAP);
        post(pid, make(kCGEventLeftMouseDragged, *point))?;
    }
    post(pid, make(kCGEventLeftMouseUp, *rest.last().unwrap_or(&first)))
}

pub fn scroll(pid: i32, x: f64, y: f64, lines: i32) -> Result<(), String> {
    let event = unsafe { CGEventCreateScrollWheelEvent2(null(), kCGScrollEventUnitLine, 1, lines, 0, 0) };
    if !event.is_null() {
        unsafe { CGEventSetLocation(event, CGPoint { x, y }) };
    }
    post(pid, event)
}

/// Text as key events carrying the characters themselves, so any language types.
pub fn type_text(pid: i32, text: &str, stop: &dyn Fn() -> bool) -> Result<usize, String> {
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut sent = 0;
    for piece in units.chunks(TYPE_PIECE) {
        if stop() {
            break;
        }
        for down in [true, false] {
            let event = unsafe { CGEventCreateKeyboardEvent(null(), 0, down) };
            if !event.is_null() {
                unsafe { CGEventKeyboardSetUnicodeString(event, piece.len(), piece.as_ptr()) };
            }
            post(pid, event)?;
        }
        sent += piece.len();
        thread::sleep(KEY_GAP);
    }
    Ok(String::from_utf16_lossy(&units[..sent]).chars().count())
}

/// A key and its modifiers, from text such as "cmd+s" or "enter".
pub struct Chord {
    pub flags: u64,
    pub key: u16,
}

fn key_code(name: &str) -> Option<u16> {
    const LETTERS: &str = "asdfhgzxcv_bqweryt123465=97-80]ou[ip_lj'k;\\,/nm.";
    let code = match name {
        "enter" | "return" => 36,
        "tab" => 48,
        "space" => 49,
        "backspace" | "delete" => 51,
        "esc" | "escape" => 53,
        "del" | "forwarddelete" => 117,
        "home" => 115,
        "end" => 119,
        "pageup" => 116,
        "pagedown" => 121,
        "left" => 123,
        "right" => 124,
        "down" => 125,
        "up" => 126,
        "`" => 50,
        "f1" => 122,
        "f2" => 120,
        "f3" => 99,
        "f4" => 118,
        "f5" => 96,
        "f6" => 97,
        "f7" => 98,
        "f8" => 100,
        "f9" => 101,
        "f10" => 109,
        "f11" => 103,
        "f12" => 111,
        _ => {
            let mut chars = name.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else { return None };
            if c == '_' {
                return None;
            }
            return LETTERS.chars().position(|known| known == c).map(|at| at as u16);
        }
    };
    Some(code)
}

pub fn chord(spec: &str) -> Result<Chord, String> {
    let lower = spec.to_lowercase();
    let parts: Vec<&str> = lower.split('+').map(str::trim).collect();
    let (key, modifiers) = parts.split_last().ok_or("key needs a key")?;
    let mut flags = 0;
    for modifier in modifiers {
        flags |= match *modifier {
            "cmd" | "command" | "win" | "meta" => kCGEventFlagMaskCommand,
            "ctrl" | "control" => kCGEventFlagMaskControl,
            "alt" | "option" | "opt" => kCGEventFlagMaskAlternate,
            "shift" => kCGEventFlagMaskShift,
            other => return Err(format!("unknown modifier '{other}'")),
        };
    }
    let key = key_code(key).ok_or(format!("unknown key '{key}'"))?;
    Ok(Chord { flags, key })
}

/// Shortcuts that quit apps, log out, lock or force-quit: an agent needs --force for these.
pub fn is_risky(chord: &Chord) -> bool {
    let cmd = chord.flags & kCGEventFlagMaskCommand != 0;
    let q = chord.key == 12;
    let w = chord.key == 13;
    (cmd && (q || w)) || (cmd && chord.flags & kCGEventFlagMaskAlternate != 0 && chord.key == 53)
}

pub fn press(pid: i32, chord: &Chord) -> Result<(), String> {
    for down in [true, false] {
        let event = unsafe { CGEventCreateKeyboardEvent(null(), chord.key, down) };
        if !event.is_null() {
            unsafe { CGEventSetFlags(event, chord.flags) };
        }
        post(pid, event)?;
        thread::sleep(KEY_GAP);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords_read_like_the_windows_ones() {
        let save = chord("cmd+s").unwrap();
        assert_eq!((save.key, save.flags), (1, kCGEventFlagMaskCommand));
        assert_eq!(chord("enter").unwrap().key, 36);
        assert_eq!(chord("m").unwrap().key, 46);
        assert!(chord("hyper+s").is_err());
        assert!(is_risky(&chord("cmd+q").unwrap()));
        assert!(!is_risky(&chord("cmd+c").unwrap()));
    }
}
