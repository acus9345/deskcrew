// Mouse and keyboard events posted straight to one app, so the person's own pointer never moves
// and their front app keeps its keys. `--real` sends keys through the system instead, while the
// app is briefly in front.
use super::ffi::*;
use std::{ptr::null, thread, time::Duration};

const CLICK_GAP: Duration = Duration::from_millis(30);
const KEY_GAP: Duration = Duration::from_millis(8);
const TYPE_PIECE: usize = 16;
// Stroke points are 6 px apart, so this draws at the same 1000 px/s as on Windows.
const STROKE_GAP: Duration = Duration::from_millis(6);

#[derive(Clone, Copy, PartialEq)]
pub enum Button {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

/// Where an event goes: one app in the background, or the system, to whatever is in front.
#[derive(Clone, Copy)]
pub enum To {
    App(i32),
    Front,
}

fn post(to: To, event: CGEventRef) -> Result<(), String> {
    if event.is_null() {
        return Err("macOS would not make that event".into());
    }
    unsafe {
        match to {
            To::App(pid) => CGEventPostToPid(pid, event),
            To::Front => CGEventPost(kCGHIDEventTap, event),
        }
        CFRelease(event as CFTypeRef);
    }
    Ok(())
}

fn aim(event: CGEventRef, window: u32) {
    if !event.is_null() {
        unsafe {
            CGEventSetIntegerValueField(event, kCGMouseEventWindowUnderMousePointer, window as i64);
            CGEventSetIntegerValueField(event, kCGMouseEventWindowUnderMousePointerThatCanHandleThisEvent, window as i64);
        }
    }
}

/// A click at a screen point, in points, aimed at one window of the app, with modifier keys held.
pub fn click(pid: i32, window: u32, x: f64, y: f64, button: Button, count: u32, flags: u64) -> Result<(), String> {
    let at = CGPoint { x, y };
    let (down, up, code, number) = match button {
        Button::Left => (kCGEventLeftMouseDown, kCGEventLeftMouseUp, kCGMouseButtonLeft, 0),
        Button::Right => (kCGEventRightMouseDown, kCGEventRightMouseUp, kCGMouseButtonRight, 1),
        Button::Middle => (kCGEventOtherMouseDown, kCGEventOtherMouseUp, kCGMouseButtonCenter, 2),
        Button::Back => (kCGEventOtherMouseDown, kCGEventOtherMouseUp, 3, 3),
        Button::Forward => (kCGEventOtherMouseDown, kCGEventOtherMouseUp, 4, 4),
    };
    for click in 1..=count {
        for kind in [down, up] {
            let event = unsafe { CGEventCreateMouseEvent(null(), kind, at, code) };
            if !event.is_null() {
                unsafe {
                    CGEventSetIntegerValueField(event, kCGMouseEventClickState, click as i64);
                    CGEventSetIntegerValueField(event, kCGMouseEventButtonNumber, number);
                    if flags != 0 {
                        CGEventSetFlags(event, flags);
                    }
                }
            }
            aim(event, window);
            post(To::App(pid), event)?;
            thread::sleep(CLICK_GAP);
        }
    }
    Ok(())
}

/// The pointer passing over a point, so hover effects and tooltips show.
pub fn hover(pid: i32, window: u32, x: f64, y: f64) -> Result<(), String> {
    let event = unsafe { CGEventCreateMouseEvent(null(), kCGEventMouseMoved, CGPoint { x, y }, kCGMouseButtonLeft) };
    aim(event, window);
    post(To::App(pid), event)
}

/// A press, a path of drags and a release, for drawing and dragging. `moved` hears each point.
pub fn stroke(pid: i32, window: u32, points: &[(f64, f64)], stop: &dyn Fn() -> bool, moved: &mut dyn FnMut(usize, f64, f64)) -> Result<bool, String> {
    let Some(&first) = points.first() else { return Ok(true) };
    let make = |kind: u32, (x, y): (f64, f64)| {
        let event = unsafe { CGEventCreateMouseEvent(null(), kind, CGPoint { x, y }, kCGMouseButtonLeft) };
        aim(event, window);
        event
    };
    post(To::App(pid), make(kCGEventLeftMouseDown, first))?;
    let mut last = first;
    let mut finished = true;
    for (at, point) in points.iter().enumerate().skip(1) {
        if stop() {
            finished = false;
            break;
        }
        thread::sleep(STROKE_GAP);
        post(To::App(pid), make(kCGEventLeftMouseDragged, *point))?;
        moved(at, point.0, point.1);
        last = *point;
    }
    post(To::App(pid), make(kCGEventLeftMouseUp, last))?;
    Ok(finished)
}

pub fn scroll(pid: i32, x: f64, y: f64, lines: i32) -> Result<(), String> {
    let event = unsafe { CGEventCreateScrollWheelEvent2(null(), kCGScrollEventUnitLine, 1, lines, 0, 0) };
    if !event.is_null() {
        unsafe { CGEventSetLocation(event, CGPoint { x, y }) };
    }
    post(To::App(pid), event)
}

/// Text as key events carrying the characters themselves, so any language types.
/// Returns how many characters went out before `stop` said to stop.
pub fn type_text(to: To, text: &str, stop: &dyn Fn() -> bool) -> Result<usize, String> {
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
            post(to, event)?;
        }
        sent += piece.len();
        thread::sleep(KEY_GAP);
    }
    Ok(String::from_utf16_lossy(&units[..sent]).chars().count())
}

/// A key and its modifiers, from text such as "cmd+s" or "enter". A modifier on its own,
/// such as "shift", has no key.
pub struct Chord {
    pub flags: u64,
    pub key: Option<u16>,
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

fn modifier(name: &str) -> Option<u64> {
    Some(match name {
        "cmd" | "command" | "win" | "meta" => kCGEventFlagMaskCommand,
        "ctrl" | "control" => kCGEventFlagMaskControl,
        "alt" | "option" | "opt" => kCGEventFlagMaskAlternate,
        "shift" => kCGEventFlagMaskShift,
        _ => return None,
    })
}

/// Modifier keys alone, as for `click --with ctrl+shift`.
pub fn modifiers(spec: &str) -> Result<u64, String> {
    spec.to_lowercase().split('+').map(str::trim).try_fold(0, |flags, part| modifier(part).map(|f| flags | f).ok_or(format!("unknown modifier '{part}'")))
}

pub fn chord(spec: &str) -> Result<Chord, String> {
    let lower = spec.to_lowercase();
    let parts: Vec<&str> = lower.split('+').map(str::trim).collect();
    let (last, rest) = parts.split_last().ok_or("no key given")?;
    let mut flags = 0;
    for part in rest {
        flags |= modifier(part).ok_or(format!("unknown modifier '{part}'"))?;
    }
    if let Some(alone) = modifier(last) {
        return Ok(Chord { flags: flags | alone, key: None });
    }
    let key = key_code(last).ok_or(format!("unknown key '{last}'"))?;
    Ok(Chord { flags, key: Some(key) })
}

/// Shortcuts that quit apps, close windows or force-quit: an agent needs --force for these.
pub fn is_risky(chord: &Chord) -> bool {
    let cmd = chord.flags & kCGEventFlagMaskCommand != 0;
    match chord.key {
        Some(12 | 13) => cmd,
        Some(53) => cmd && chord.flags & kCGEventFlagMaskAlternate != 0,
        _ => false,
    }
}

/// Press and let go, with `held` modifiers added, holding the key `hold` long.
pub fn press(to: To, chord: &Chord, held: u64, hold: Duration) -> Result<(), String> {
    let key = chord.key.ok_or("that is only a modifier: hold it with down, or add a key such as cmd+s")?;
    for down in [true, false] {
        let event = unsafe { CGEventCreateKeyboardEvent(null(), key, down) };
        if !event.is_null() {
            unsafe { CGEventSetFlags(event, chord.flags | held) };
        }
        post(to, event)?;
        thread::sleep(if down { hold.max(KEY_GAP) } else { KEY_GAP });
    }
    Ok(())
}

/// The key codes of the modifier keys, for showing a held key to the app.
fn modifier_codes(flags: u64) -> Vec<u16> {
    [(kCGEventFlagMaskCommand, 55), (kCGEventFlagMaskShift, 56), (kCGEventFlagMaskAlternate, 58), (kCGEventFlagMaskControl, 59)]
        .into_iter()
        .filter(|(flag, _)| flags & flag != 0)
        .map(|(_, code)| code)
        .collect()
}

/// Tell the app modifier keys went down or came back up.
pub fn modifiers_changed(pid: i32, flags: u64, down: bool) -> Result<(), String> {
    for code in modifier_codes(flags) {
        let event = unsafe { CGEventCreateKeyboardEvent(null(), code, down) };
        if !event.is_null() {
            unsafe { CGEventSetFlags(event, if down { flags } else { 0 }) };
        }
        post(To::App(pid), event)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords_read_like_the_windows_ones() {
        let save = chord("cmd+s").unwrap();
        assert_eq!((save.key, save.flags), (Some(1), kCGEventFlagMaskCommand));
        assert_eq!(chord("enter").unwrap().key, Some(36));
        assert_eq!(chord("m").unwrap().key, Some(46));
        assert_eq!(chord("shift").unwrap().key, None);
        assert!(chord("hyper+s").is_err());
        assert!(is_risky(&chord("cmd+q").unwrap()));
        assert!(!is_risky(&chord("cmd+c").unwrap()));
        assert_eq!(modifiers("ctrl+shift").unwrap(), kCGEventFlagMaskControl | kCGEventFlagMaskShift);
    }
}
