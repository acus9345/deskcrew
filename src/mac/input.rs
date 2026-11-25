// Mouse and keyboard events posted straight to one app, so the person's own pointer never moves
// and their front app keeps its keys. `--real` sends keys through the system instead, while the
// app is briefly in front.
use super::ffi::*;
use std::{ptr::null, thread, time::Duration};

const CLICK_GAP: Duration = Duration::from_millis(30);
const KEY_GAP: Duration = Duration::from_millis(8);
const TYPE_PIECE: usize = 16;

#[derive(Clone, Copy, PartialEq)]
pub enum Button {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

impl Button {
    pub fn named(word: &str) -> Option<Button> {
        match word.to_lowercase().as_str() {
            "left" => Some(Button::Left),
            "right" => Some(Button::Right),
            "middle" => Some(Button::Middle),
            "back" => Some(Button::Back),
            "forward" => Some(Button::Forward),
            _ => None,
        }
    }
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
pub fn click(pid: i32, window: u32, at: (f64, f64), button: Button, count: u32, flags: u64, hold: Duration) -> Result<(), String> {
    let at = CGPoint { x: at.0, y: at.1 };
    let (down, up, code, number) = match button {
        Button::Left => (kCGEventLeftMouseDown, kCGEventLeftMouseUp, kCGMouseButtonLeft, 0),
        Button::Right => (kCGEventRightMouseDown, kCGEventRightMouseUp, kCGMouseButtonRight, 1),
        Button::Middle => (kCGEventOtherMouseDown, kCGEventOtherMouseUp, kCGMouseButtonCenter, 2),
        Button::Back => (kCGEventOtherMouseDown, kCGEventOtherMouseUp, 3, 3),
        Button::Forward => (kCGEventOtherMouseDown, kCGEventOtherMouseUp, 4, 4),
    };
    for click in 1..=count {
        for (idx, kind) in [down, up].into_iter().enumerate() {
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
            thread::sleep(if idx == 0 { hold.max(CLICK_GAP) } else { CLICK_GAP });
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
pub fn stroke(
    pid: i32,
    window: u32,
    points: &[(f64, f64)],
    button: Button,
    flags: u64,
    stop: &dyn Fn() -> bool,
    moved: &mut dyn FnMut(usize, f64, f64),
) -> Result<bool, String> {
    let Some(&first) = points.first() else { return Ok(true) };
    let (down, up, drag, code, number) = match button {
        Button::Left => (kCGEventLeftMouseDown, kCGEventLeftMouseUp, kCGEventLeftMouseDragged, kCGMouseButtonLeft, 0),
        Button::Right => (kCGEventRightMouseDown, kCGEventRightMouseUp, kCGEventRightMouseDragged, kCGMouseButtonRight, 1),
        Button::Middle => (kCGEventOtherMouseDown, kCGEventOtherMouseUp, kCGEventOtherMouseDragged, kCGMouseButtonCenter, 2),
        Button::Back => (kCGEventOtherMouseDown, kCGEventOtherMouseUp, kCGEventOtherMouseDragged, 3, 3),
        Button::Forward => (kCGEventOtherMouseDown, kCGEventOtherMouseUp, kCGEventOtherMouseDragged, 4, 4),
    };
    let make = |kind: u32, (x, y): (f64, f64)| {
        let event = unsafe { CGEventCreateMouseEvent(null(), kind, CGPoint { x, y }, code) };
        if !event.is_null() {
            unsafe {
                CGEventSetIntegerValueField(event, kCGMouseEventButtonNumber, number);
                if flags != 0 {
                    CGEventSetFlags(event, flags);
                }
            }
        }
        aim(event, window);
        event
    };
    post(To::App(pid), make(down, first))?;
    let mut last = first;
    let mut finished = true;
    // The handwriting rhythm the Windows and Linux strokes keep too.
    let path: Vec<(f32, f32)> = points.iter().map(|p| (p.0 as f32, p.1 as f32)).collect();
    let due = crate::motion::schedule(&path, crate::motion::PIXELS_PER_SECOND);
    let started = std::time::Instant::now();
    let mut drag_err = None;
    for (at, point) in points.iter().enumerate().skip(1) {
        if stop() {
            finished = false;
            break;
        }
        let secs = due[at];
        if let Ok(wait_for) = Duration::try_from_secs_f32(secs)
            && let Some(wait) = wait_for.checked_sub(started.elapsed())
        {
            thread::sleep(wait);
        }
        if let Err(err) = post(To::App(pid), make(drag, *point)) {
            drag_err = Some(err);
            break;
        }
        moved(at, point.0, point.1);
        last = *point;
    }
    let up_res = post(To::App(pid), make(up, last));
    if let Some(err) = drag_err {
        return Err(err);
    }
    up_res?;
    Ok(finished)
}

pub fn scroll(pid: i32, window: u32, x: f64, y: f64, lines: i32, sideways: bool) -> Result<(), String> {
    let (v, h) = if sideways { (0, lines) } else { (lines, 0) };
    let event = unsafe { CGEventCreateScrollWheelEvent2(null(), kCGScrollEventUnitLine, 2, v, h, 0) };
    if !event.is_null() {
        unsafe { CGEventSetLocation(event, CGPoint { x, y }) };
    }
    aim(event, window);
    post(To::App(pid), event)
}

/// Text as key events carrying the characters themselves, so any language types.
/// Returns how many characters went out before `stop` said to stop.
pub fn type_text(to: To, text: &str, stop: &dyn Fn() -> bool) -> Result<usize, String> {
    let normalized = crate::motion::normalize_newlines(text);
    let units: Vec<u16> = normalized.encode_utf16().collect();
    let mut sent = 0;
    for piece in pieces(&normalized) {
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
        thread::sleep(crate::motion::KEY_EVERY * piece.len() as u32);
    }
    Ok(String::from_utf16_lossy(&units[..sent]).chars().count())
}

/// The text in runs of at most TYPE_PIECE UTF-16 units, never cutting a character in two.
fn pieces(text: &str) -> Vec<Vec<u16>> {
    let mut out: Vec<Vec<u16>> = Vec::new();
    let mut buffer = [0u16; 2];
    for c in text.chars() {
        let units = c.encode_utf16(&mut buffer);
        match out.last_mut() {
            Some(piece) if piece.len() + units.len() <= TYPE_PIECE => piece.extend_from_slice(units),
            _ => out.push(units.to_vec()),
        }
    }
    out
}

/// A key and its modifiers, from text such as "cmd+s" or "enter". A modifier on its own,
/// such as "shift", has no key.
pub struct Chord {
    pub flags: u64,
    pub key: Option<u16>,
}

const LETTERS: &str = "asdfhgzxcv_bqweryt123465=97-80]ou[ip_lj'k;\\,/nm.";

/// The ASCII character corresponding to a macOS virtual key code, if it is a printable key.
pub fn key_char(code: u16) -> Option<char> {
    if code == 50 {
        return Some('`');
    }
    LETTERS.chars().nth(code as usize).filter(|&c| c != '_')
}

fn key_code(name: &str) -> Option<u16> {
    let code = match name {
        "enter" | "return" => 36,
        "tab" => 48,
        "space" | "spacebar" => 49,
        "backspace" | "back" | "delete" => 51,
        "esc" | "escape" => 53,
        "del" | "forwarddelete" => 117,
        "insert" | "ins" => 114,
        "home" => 115,
        "end" => 119,
        "pageup" | "pgup" => 116,
        "pagedown" | "pgdn" => 121,
        "left" => 123,
        "right" => 124,
        "down" => 125,
        "up" => 126,
        "plus" | "equal" | "equals" => 24,
        "minus" => 27,
        "quote" => 39,
        "semicolon" => 41,
        "backslash" => 42,
        "comma" => 43,
        "slash" => 44,
        "period" => 47,
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
        "f13" => 105,
        "f14" => 107,
        "f15" => 113,
        "f16" => 106,
        "f17" => 64,
        "f18" => 79,
        "f19" => 80,
        "f20" => 90,
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
        "cmd" | "command" | "win" | "meta" | "super" => kCGEventFlagMaskCommand,
        "ctrl" | "control" => kCGEventFlagMaskControl,
        "alt" | "option" | "opt" => kCGEventFlagMaskAlternate,
        "shift" => kCGEventFlagMaskShift,
        _ => return None,
    })
}

/// Modifier keys alone, as for `click --with ctrl+shift`.
pub fn modifiers(spec: &str) -> Result<u64, String> {
    if spec.trim().is_empty() {
        return Err("no modifier given".into());
    }
    spec.to_lowercase().split('+').map(str::trim).try_fold(0, |flags, part| {
        if part.is_empty() {
            return Err(format!("empty modifier in '{spec}'"));
        }
        modifier(part).map(|f| flags | f).ok_or(format!("unknown modifier '{part}'"))
    })
}

pub fn chord(spec: &str) -> Result<Chord, String> {
    let lower = spec.to_lowercase();
    if lower.trim().is_empty() {
        return Err("no key given".into());
    }
    let parts: Vec<&str> = lower.split('+').map(str::trim).collect();
    if parts.iter().any(|p| p.is_empty()) {
        return Err(format!("empty key in '{spec}': use 'plus' for '+'"));
    }
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
        assert_eq!(chord("super+s").unwrap().flags, kCGEventFlagMaskCommand);
        assert_eq!(chord("ctrl + pgup").unwrap().key, Some(116));
        assert_eq!(chord("pgdn").unwrap().key, Some(121));
        assert_eq!(chord("spacebar").unwrap().key, Some(49));
        assert_eq!(chord("back").unwrap().key, Some(51));
        assert_eq!(chord("delete").unwrap().key, Some(51));
        assert_eq!(chord("del").unwrap().key, Some(117));
        assert_eq!(chord("forwarddelete").unwrap().key, Some(117));
        assert_eq!(chord("insert").unwrap().key, Some(114));
        assert_eq!(chord("ins").unwrap().key, Some(114));
        assert_eq!(chord("plus").unwrap().key, Some(24));
        assert_eq!(chord("equal").unwrap().key, Some(24));
        assert_eq!(chord("equals").unwrap().key, Some(24));
        assert_eq!(chord("minus").unwrap().key, Some(27));
        assert_eq!(chord("quote").unwrap().key, Some(39));
        assert_eq!(chord("semicolon").unwrap().key, Some(41));
        assert_eq!(chord("backslash").unwrap().key, Some(42));
        assert_eq!(chord("comma").unwrap().key, Some(43));
        assert_eq!(chord("slash").unwrap().key, Some(44));
        assert_eq!(chord("period").unwrap().key, Some(47));
        assert_eq!(chord("f20").unwrap().key, Some(90));
        assert!(chord("hyper+s").is_err());
        assert!(chord("").is_err());
        assert!(chord("cmd++").is_err());
        assert!(is_risky(&chord("cmd+q").unwrap()));
        assert!(!is_risky(&chord("cmd+c").unwrap()));
        assert_eq!(modifiers("ctrl+shift").unwrap(), kCGEventFlagMaskControl | kCGEventFlagMaskShift);
        assert!(modifiers("").is_err());
        assert!(modifiers("ctrl+").is_err());
    }

    #[test]
    fn typing_never_cuts_a_character_in_two() {
        let text = format!("{}\u{1F600}b", "a".repeat(TYPE_PIECE - 1));
        let pieces = pieces(&text);
        assert!(pieces.iter().all(|piece| piece.len() <= TYPE_PIECE && String::from_utf16(piece).is_ok()), "{pieces:?}");
        assert_eq!(pieces.concat(), text.encode_utf16().collect::<Vec<_>>());
    }
}
