// The real keyboard, and the real mouse for an agent the person has let borrow it. One agent at a
// time, it waits for a pause in the person's own typing, and it puts focus and the pointer back.
use crate::win::{self, Hwnd};
use std::{
    mem::zeroed,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::POINT,
    System::SystemInformation::GetTickCount,
    UI::{
        Input::KeyboardAndMouse::{
            GetLastInputInfo, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
            KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, LASTINPUTINFO, MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN,
            MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK,
            MOUSEEVENTF_WHEEL, MOUSEINPUT, SendInput, VIRTUAL_KEY, VK_BACK, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE,
            VK_F1, VK_HOME, VK_INSERT, VK_LEFT, VK_LWIN, VK_MENU, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE,
            VK_TAB, VK_UP, VkKeyScanW,
        },
        WindowsAndMessaging::{GetCursorPos, GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN},
    },
};

/// Canvases sample the pointer on their own clock, so a press has to last and arrive after the move.
const ARRIVE: Duration = Duration::from_millis(25);
const PRESS: Duration = Duration::from_millis(40);
const HOLD_LIMIT: Duration = Duration::from_secs(30);
const KEY_GAP: Duration = Duration::from_millis(6);
/// A window just brought forward drops the first press if it comes too soon.
const FOCUS_SETTLE: Duration = Duration::from_millis(350);
/// Input already sent still has to reach the app before focus can move away from it.
const DRAIN: Duration = Duration::from_millis(120);
const FOCUS_MOVED: &str = "stopped: focus moved away from that window, so nothing more was typed";
const QUIET_FOR: Duration = Duration::from_millis(350);
const QUIET_TIMEOUT: Duration = Duration::from_secs(3);
const STEP_PIXELS: f32 = 6.0;
const PIXELS_PER_SECOND: f32 = 1000.0;

/// Shortcuts that end a session or reach the shell: refused unless asked for with --force.
/// Marks our own mouse events, so the shield against hidden ones lets them through.
pub const OURS: usize = 0x6465_736B;

const RISKY: [&str; 8] = ["alt+f4", "win+l", "win+r", "win+d", "win+x", "alt+tab", "ctrl+shift+esc", "win+e"];

fn send(inputs: &[INPUT]) {
    unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), size_of::<INPUT>() as i32) };
}

fn mouse(flags: MOUSE_EVENT_FLAGS, dx: i32, dy: i32, data: u32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 { mi: MOUSEINPUT { dx, dy, mouseData: data, dwFlags: flags, time: 0, dwExtraInfo: OURS } },
    }
}

fn key(vk: VIRTUAL_KEY, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
    }
}

/// A real pointer move, which XAML and raw-input apps see and SetCursorPos does not reach.
pub fn move_to(x: i32, y: i32) {
    unsafe {
        let left = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let top = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let width = GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1) as i64;
        let height = GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1) as i64;
        let dx = ((x - left) as i64 * 65536 + width - 1) / width;
        let dy = ((y - top) as i64 * 65536 + height - 1) / height;
        send(&[mouse(MOUSEEVENTF_MOVE | MOUSEEVENTF_VIRTUALDESK | MOUSEEVENTF_ABSOLUTE, dx as i32, dy as i32, 0)]);
    }
}

fn press(right: bool, down: bool) {
    let flags = match (right, down) {
        (false, true) => MOUSEEVENTF_LEFTDOWN,
        (false, false) => MOUSEEVENTF_LEFTUP,
        (true, true) => MOUSEEVENTF_RIGHTDOWN,
        (true, false) => MOUSEEVENTF_RIGHTUP,
    };
    send(&[mouse(flags, 0, 0, 0)]);
}

pub fn click(x: i32, y: i32, right: bool, double: bool) {
    move_to(x, y);
    thread::sleep(ARRIVE);
    for _ in 0..if double { 2 } else { 1 } {
        press(right, true);
        thread::sleep(PRESS);
        press(right, false);
        thread::sleep(PRESS);
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Button {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

impl Button {
    pub fn named(word: &str) -> Option<Button> {
        match word {
            "left" => Some(Button::Left),
            "right" => Some(Button::Right),
            "middle" => Some(Button::Middle),
            "back" => Some(Button::Back),
            "forward" => Some(Button::Forward),
            _ => None,
        }
    }
}

fn press_button(button: Button, down: bool) {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP};
    let extra = if down { MOUSEEVENTF_XDOWN } else { MOUSEEVENTF_XUP };
    match button {
        Button::Left => press(false, down),
        Button::Right => press(true, down),
        Button::Middle => send(&[mouse(if down { MOUSEEVENTF_MIDDLEDOWN } else { MOUSEEVENTF_MIDDLEUP }, 0, 0, 0)]),
        Button::Back => send(&[mouse(extra, 0, 0, 1)]),
        Button::Forward => send(&[mouse(extra, 0, 0, 2)]),
    }
}

pub fn press_times(x: i32, y: i32, button: Button, times: u32) {
    move_to(x, y);
    thread::sleep(ARRIVE);
    for _ in 0..times {
        press_button(button, true);
        thread::sleep(PRESS);
        press_button(button, false);
        thread::sleep(PRESS);
    }
}

#[derive(Clone)]
enum Held {
    Keys(Vec<(VIRTUAL_KEY, bool)>),
    Mouse(Button),
}

static HELD: Mutex<Vec<Held>> = Mutex::new(Vec::new());
static HOLDS: AtomicU64 = AtomicU64::new(0);

fn hold(held: Held) {
    HELD.lock().unwrap().push(held);
    let this = HOLDS.fetch_add(1, Ordering::Relaxed) + 1;
    thread::spawn(move || {
        thread::sleep(HOLD_LIMIT);
        if HOLDS.load(Ordering::Relaxed) == this {
            release_all();
        }
    });
}

/// Keys stay down across commands, so they let go by themselves if the agent forgets.
pub fn hold_keys(chord: &Chord) {
    set_keys(chord, true);
    hold(Held::Keys(chord.keys.clone()));
}

pub fn hold_button(x: i32, y: i32, button: Button) {
    move_to(x, y);
    thread::sleep(ARRIVE);
    press_button(button, true);
    hold(Held::Mouse(button));
}

pub fn release_all() -> usize {
    let held: Vec<Held> = HELD.lock().unwrap().drain(..).collect();
    for item in held.iter().rev() {
        match item {
            Held::Keys(keys) => set_keys(&Chord { keys: keys.clone(), modified: false }, false),
            Held::Mouse(button) => press_button(*button, false),
        }
    }
    held.len()
}

pub fn wheel(x: i32, y: i32, notches: i32) {
    move_to(x, y);
    thread::sleep(ARRIVE);
    send(&[mouse(MOUSEEVENTF_WHEEL, 0, 0, (notches * 120) as u32)]);
}

/// Points every few pixels along a stroke, through a smooth curve if asked for.
pub fn trace(points: &[(i32, i32)], smooth: bool) -> Vec<(f32, f32)> {
    let at = |i: isize| {
        let p = points[i.clamp(0, points.len() as isize - 1) as usize];
        (p.0 as f32, p.1 as f32)
    };
    let mut out = vec![at(0)];
    for i in 0..points.len().saturating_sub(1) as isize {
        let (a, b, c, d) = (at(i - 1), at(i), at(i + 1), at(i + 2));
        let steps = (((c.0 - b.0).hypot(c.1 - b.1) / STEP_PIXELS).ceil() as usize).max(1);
        for step in 1..=steps {
            let t = step as f32 / steps as f32;
            out.push(if smooth {
                let curve = |p0: f32, p1: f32, p2: f32, p3: f32| {
                    0.5 * (2.0 * p1
                        + (p2 - p0) * t
                        + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t * t
                        + (3.0 * p1 - p0 - 3.0 * p2 + p3) * t * t * t)
                };
                (curve(a.0, b.0, c.0, d.0), curve(a.1, b.1, c.1, d.1))
            } else {
                (b.0 + (c.0 - b.0) * t, b.1 + (c.1 - b.1) * t)
            });
        }
    }
    out
}

/// Visit each point of a stroke on schedule, at a steady speed. False if stopped on the way.
pub fn pace(path: &[(f32, f32)], stop: &AtomicBool, mut visit: impl FnMut(usize, i32, i32)) -> bool {
    let started = Instant::now();
    for (at, point) in path.iter().enumerate() {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        let due = Duration::from_secs_f32(at as f32 * STEP_PIXELS / PIXELS_PER_SECOND);
        while let Some(left) = due.checked_sub(started.elapsed()) {
            if left > Duration::from_millis(2) {
                thread::sleep(left - Duration::from_millis(2));
            } else {
                std::hint::spin_loop();
            }
        }
        visit(at, point.0.round() as i32, point.1.round() as i32);
    }
    true
}

/// Press, follow the path, and always let go.
pub fn stroke(path: &[(f32, f32)], stop: &AtomicBool, mut moved: impl FnMut(i32, i32)) -> bool {
    let Some(first) = path.first() else { return true };
    move_to(first.0 as i32, first.1 as i32);
    thread::sleep(ARRIVE);
    press(false, true);
    thread::sleep(PRESS);
    let finished = pace(path, stop, |at, x, y| {
        move_to(x, y);
        if at % 3 == 0 {
            moved(x, y);
        }
    });
    thread::sleep(PRESS);
    press(false, false);
    finished
}

fn named_key(name: &str) -> Option<(VIRTUAL_KEY, bool)> {
    let lower = name.to_lowercase();
    let found = match lower.as_str() {
        "enter" | "return" => (VK_RETURN, false),
        "tab" => (VK_TAB, false),
        "esc" | "escape" => (VK_ESCAPE, false),
        "space" => (VK_SPACE, false),
        "backspace" => (VK_BACK, false),
        "delete" | "del" => (VK_DELETE, true),
        "insert" => (VK_INSERT, true),
        "home" => (VK_HOME, true),
        "end" => (VK_END, true),
        "pageup" => (VK_PRIOR, true),
        "pagedown" => (VK_NEXT, true),
        "left" => (VK_LEFT, true),
        "right" => (VK_RIGHT, true),
        "up" => (VK_UP, true),
        "down" => (VK_DOWN, true),
        "ctrl" | "control" => (VK_CONTROL, false),
        "shift" => (VK_SHIFT, false),
        "alt" => (VK_MENU, false),
        "win" | "meta" | "cmd" => (VK_LWIN, true),
        _ => {
            if let Some(number) = lower.strip_prefix('f').and_then(|n| n.parse::<u16>().ok())
                && (1..=24).contains(&number) {
                    return Some((VK_F1 + number - 1, false));
                }
            let mut chars = lower.chars();
            let (c, None) = (chars.next()?, chars.next()) else { return None };
            let scan = unsafe { VkKeyScanW(c as u16) };
            if scan == -1 {
                return None;
            }
            ((scan as u16) & 0xFF, false)
        }
    };
    Some(found)
}

#[derive(Debug)]
pub struct Chord {
    pub keys: Vec<(VIRTUAL_KEY, bool)>,
    pub modified: bool,
}

impl Chord {
    /// The modifier bits a mouse message carries, for a modified click with no keyboard.
    pub fn mouse_keys(&self) -> usize {
        const MK_SHIFT: usize = 0x04;
        const MK_CONTROL: usize = 0x08;
        self.keys.iter().fold(0, |bits, (vk, _)| match *vk {
            VK_SHIFT => bits | MK_SHIFT,
            VK_CONTROL => bits | MK_CONTROL,
            _ => bits,
        })
    }
}

pub fn chord(spec: &str) -> Result<Chord, String> {
    let mut keys = Vec::new();
    for part in spec.split('+').filter(|p| !p.is_empty()) {
        keys.push(named_key(part).ok_or_else(|| format!("unknown key '{part}'"))?);
    }
    if keys.is_empty() {
        return Err("no key given".into());
    }
    Ok(Chord { modified: keys.len() > 1, keys })
}

/// Extra modifiers do not make a shortcut safe, so ctrl+alt+f4 counts as alt+f4.
pub fn is_risky(spec: &str) -> bool {
    let name = |key: &str| match key.trim().to_lowercase().as_str() {
        "escape" => "esc".to_string(),
        "control" => "ctrl".to_string(),
        "cmd" | "meta" | "windows" | "super" => "win".to_string(),
        other => other.to_string(),
    };
    let pressed: Vec<String> = spec.split('+').map(name).collect();
    RISKY.iter().any(|risky| risky.split('+').all(|key| pressed.iter().any(|down| down == key)))
}

/// Real keys go to whatever has focus, so they are only sent while the target still has it.
pub fn check_front(root: Hwnd, pids: &[u32]) -> Result<(), String> {
    if win::front_is(root, pids) { Ok(()) } else { Err(FOCUS_MOVED.into()) }
}

pub fn press_chord(chord: &Chord) {
    hold_chord(chord, PRESS);
}

pub fn set_keys(chord: &Chord, down: bool) {
    let extended = |e: bool| if e { KEYEVENTF_EXTENDEDKEY } else { 0 };
    if down {
        for (vk, e) in &chord.keys {
            send(&[key(*vk, 0, extended(*e))]);
        }
    } else {
        for (vk, e) in chord.keys.iter().rev() {
            send(&[key(*vk, 0, extended(*e) | KEYEVENTF_KEYUP)]);
        }
    }
}

/// Keep the keys down for this long, then let go in reverse order.
pub fn hold_chord(chord: &Chord, hold: Duration) {
    set_keys(chord, true);
    thread::sleep(hold);
    set_keys(chord, false);
}

/// False if stopped on the way. Fails, having sent what it reports, if focus moves away.
pub fn type_text(text: &str, root: Hwnd, pids: &[u32], stop: &AtomicBool) -> Result<bool, String> {
    for (sent, unit) in text.encode_utf16().enumerate() {
        if stop.load(Ordering::Relaxed) {
            return Ok(false);
        }
        if !win::front_is(root, pids) {
            return Err(format!("{FOCUS_MOVED}: {sent} characters had been typed"));
        }
        if unit == 10 {
            send(&[key(VK_RETURN, 0, 0)]);
            send(&[key(VK_RETURN, 0, KEYEVENTF_KEYUP)]);
        } else {
            send(&[key(0, unit, KEYEVENTF_UNICODE)]);
            send(&[key(0, unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP)]);
        }
        thread::sleep(KEY_GAP);
    }
    Ok(true)
}

fn idle_for() -> Duration {
    unsafe {
        let mut info: LASTINPUTINFO = zeroed();
        info.cbSize = size_of::<LASTINPUTINFO>() as u32;
        GetLastInputInfo(&mut info);
        Duration::from_millis(GetTickCount().wrapping_sub(info.dwTime) as u64)
    }
}

/// Wait for a gap in the person's own input, so their click cannot land in the middle of ours.
pub fn wait_for_quiet() {
    let started = Instant::now();
    while idle_for() < QUIET_FOR && started.elapsed() < QUIET_TIMEOUT {
        thread::sleep(Duration::from_millis(20));
    }
}

/// Everything a front action borrows, handed back when it ends. The pointer is only ever
/// borrowed for --mouse.
pub struct Borrowed {
    pointer: Option<POINT>,
    guard: win::FocusGuard,
    root_pids: Vec<u32>,
}

pub fn borrow(root: Hwnd, pids: &[u32], mouse: bool) -> Result<Borrowed, String> {
    wait_for_quiet();
    let mut pointer = POINT { x: 0, y: 0 };
    unsafe { GetCursorPos(&mut pointer) };
    let pointer = mouse.then_some(pointer);
    let guard = win::FocusGuard::take();
    let already_front = win::foreground() == root;
    if !win::bring_forward(root) {
        return Err("Windows would not bring that window forward".into());
    }
    if !already_front {
        thread::sleep(FOCUS_SETTLE);
    }
    Ok(Borrowed { pointer, guard, root_pids: pids.to_vec() })
}

impl Drop for Borrowed {
    fn drop(&mut self) {
        thread::sleep(DRAIN);
        if let Some(pointer) = self.pointer {
            move_to(pointer.x, pointer.y);
        }
        self.guard.restore(&self.root_pids);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_names_parse_and_unknown_ones_are_named() {
        assert!(!chord("enter").unwrap().modified);
        assert!(chord("ctrl+shift+s").unwrap().modified);
        assert_eq!(chord("f5").unwrap().keys[0].0, VK_F1 + 4);
        assert!(chord("nonsense").unwrap_err().contains("nonsense"));
    }

    #[test]
    fn dangerous_shortcuts_are_recognised() {
        assert!(is_risky("Alt+F4"));
        assert!(is_risky("ctrl+shift+escape"));
        assert!(!is_risky("ctrl+s"));
        assert!(is_risky("shift+alt+f4"));
        assert!(is_risky("ctrl+win+l"));
        assert!(is_risky("cmd+r"));
        assert!(!is_risky("alt+f5"));
    }

    #[test]
    fn a_stroke_is_walked_in_small_steps_and_ends_on_its_last_point() {
        let path = trace(&[(0, 0), (60, 0)], false);
        assert_eq!(path.len(), 11);
        assert_eq!(path[1], (6.0, 0.0));
        assert_eq!(*path.last().unwrap(), (60.0, 0.0));
    }

    #[test]
    fn a_smooth_stroke_still_passes_through_its_points() {
        let path = trace(&[(0, 0), (60, 60), (120, 0)], true);
        assert!(path.iter().any(|p| (p.0 - 60.0).abs() < 0.01 && (p.1 - 60.0).abs() < 0.01));
        assert_eq!(*path.last().unwrap(), (120.0, 0.0));
    }
}
