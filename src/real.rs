// The real keyboard, and the real mouse for an agent the person has let borrow it. One agent at a
// time, it waits for a pause in the person's own typing, and it puts focus and the pointer back.
pub use crate::motion::{pace, trace};
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
/// A window just brought forward drops the first press if it comes too soon.
const FOCUS_SETTLE: Duration = Duration::from_millis(350);
/// The pen needs far less: a pen press reaches a window as soon as it is on top, and strokes
/// arrived whole with no wait at all in WPF; these leave a margin.
const PEN_SETTLE: Duration = Duration::from_millis(50);
const PEN_DRAIN: Duration = Duration::from_millis(30);
/// Input already sent still has to reach the app before focus can move away from it.
const DRAIN: Duration = Duration::from_millis(120);
const FOCUS_MOVED: &str = "stopped: focus moved away from that window, so nothing more was typed";
const QUIET_FOR: Duration = Duration::from_millis(350);
const QUIET_TIMEOUT: Duration = Duration::from_secs(3);

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

pub type MouseButton = Button;

/// Shift, Ctrl and Alt held through a drag or a stroke.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers(u8);

impl Modifiers {
    pub const NONE: Modifiers = Modifiers(0);
    const SHIFT: u8 = 1;
    const CONTROL: u8 = 2;
    const ALT: u8 = 4;

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    fn has(self, bit: u8) -> bool {
        self.0 & bit != 0
    }

    pub fn virtual_keys(self) -> Vec<VIRTUAL_KEY> {
        [(Self::SHIFT, VK_SHIFT), (Self::CONTROL, VK_CONTROL), (Self::ALT, VK_MENU)].into_iter().filter(|(bit, _)| self.has(*bit)).map(|(_, vk)| vk).collect()
    }

    /// The MK_ bits a mouse message carries. Alt has none: it shows only in the keyboard state.
    pub fn mouse_keys(self) -> usize {
        const MK_SHIFT: usize = 0x4;
        const MK_CONTROL: usize = 0x8;
        (if self.has(Self::SHIFT) { MK_SHIFT } else { 0 }) | (if self.has(Self::CONTROL) { MK_CONTROL } else { 0 })
    }

    /// "shift", "ctrl+alt" and so on.
    pub fn parse(spec: &str) -> Result<Modifiers, String> {
        let mut bits = 0;
        for part in spec.split('+').map(str::trim).filter(|p| !p.is_empty()) {
            bits |= match part.to_lowercase().as_str() {
                "shift" => Self::SHIFT,
                "ctrl" | "control" => Self::CONTROL,
                "alt" => Self::ALT,
                other => return Err(format!("unknown key '{other}' in --with: use shift, ctrl or alt, joined with +")),
            };
        }
        Ok(Modifiers(bits))
    }
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

/// What is held down, and which agent holds it.
static HELD: Mutex<Vec<(String, Held)>> = Mutex::new(Vec::new());
static HOLDS: AtomicU64 = AtomicU64::new(0);

/// The real keyboard and mouse are shared, so only one agent holds anything at a time.
fn may_hold(who: &str) -> Result<(), String> {
    match HELD.lock().unwrap().iter().find(|(holder, _)| holder != who) {
        Some((holder, _)) => Err(format!("{holder} is holding a key or button: wait for it to let go")),
        None => Ok(()),
    }
}

fn hold(who: &str, held: Held) {
    HELD.lock().unwrap().push((who.to_string(), held));
    let this = HOLDS.fetch_add(1, Ordering::Relaxed) + 1;
    let who = who.to_string();
    thread::spawn(move || {
        thread::sleep(HOLD_LIMIT);
        if HOLDS.load(Ordering::Relaxed) == this {
            release_of(&who);
        }
    });
}

/// Keys stay down across commands, so they let go by themselves if the agent forgets.
pub fn hold_keys(who: &str, chord: &Chord) -> Result<(), String> {
    may_hold(who)?;
    set_keys(chord, true);
    hold(who, Held::Keys(chord.keys.clone()));
    Ok(())
}

pub fn hold_button(who: &str, x: i32, y: i32, button: Button) -> Result<(), String> {
    may_hold(who)?;
    move_to(x, y);
    thread::sleep(ARRIVE);
    press_button(button, true);
    hold(who, Held::Mouse(button));
    Ok(())
}

/// Let go of what one agent holds.
pub fn release_of(who: &str) -> usize {
    release(|holder| holder == who)
}

/// Let go of everything every agent holds.
pub fn release_all() -> usize {
    release(|_| true)
}

fn release(chosen: impl Fn(&str) -> bool) -> usize {
    let held: Vec<Held> = {
        let mut all = HELD.lock().unwrap();
        let (gone, kept): (Vec<_>, Vec<_>) = all.drain(..).partition(|(holder, _)| chosen(holder));
        *all = kept;
        gone.into_iter().map(|(_, held)| held).collect()
    };
    for item in held.iter().rev() {
        match item {
            Held::Keys(keys) => set_keys(&Chord { keys: keys.clone(), modified: false }, false),
            Held::Mouse(button) => press_button(*button, false),
        }
    }
    held.len()
}

pub fn wheel(x: i32, y: i32, notches: i32, sideways: bool) {
    const MOUSEEVENTF_HWHEEL: windows_sys::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS = 0x1000;
    move_to(x, y);
    thread::sleep(ARRIVE);
    send(&[mouse(if sideways { MOUSEEVENTF_HWHEEL } else { MOUSEEVENTF_WHEEL }, 0, 0, (notches * 120) as u32)]);
}

/// Press, follow the path, and always let go of the button and any keys held for it.
pub fn stroke(path: &[(f32, f32)], button: Button, modifiers: Modifiers, stop: &AtomicBool, mut moved: impl FnMut(i32, i32)) -> bool {
    let Some(first) = path.first() else { return true };
    let keys = modifiers.virtual_keys();
    move_to(first.0 as i32, first.1 as i32);
    thread::sleep(ARRIVE);
    for vk in &keys {
        send(&[key(*vk, 0, 0)]);
    }
    press_button(button, true);
    thread::sleep(PRESS);
    let finished = pace(path, stop, |at, x, y| {
        move_to(x, y);
        if at % 3 == 0 {
            moved(x, y);
        }
    });
    thread::sleep(PRESS);
    press_button(button, false);
    for vk in keys.iter().rev() {
        send(&[key(*vk, 0, KEYEVENTF_KEYUP)]);
    }
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
        "win" | "meta" | "cmd" | "windows" | "super" => (VK_LWIN, true),
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
    /// Its Shift, Ctrl and Alt, as virtual keys.
    pub fn modifiers(&self) -> Vec<u16> {
        self.keys.iter().map(|(vk, _)| *vk).filter(|vk| matches!(*vk, VK_SHIFT | VK_CONTROL | VK_MENU)).collect()
    }

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

/// Extra modifiers do not make a shortcut safe, so ctrl+alt+f4 counts as alt+f4. Keys are
/// compared once parsed, so alt+f04 is alt+f4 too.
#[cfg(test)]
pub fn is_risky(spec: &str) -> bool {
    chord(spec).is_ok_and(|parsed| is_risky_chord(&parsed))
}

/// Whether pressing this chord, with any keys agents already hold down, makes a risky shortcut.
pub fn is_risky_chord(pressed: &Chord) -> bool {
    let mut down: Vec<VIRTUAL_KEY> = pressed.keys.iter().map(|(vk, _)| *vk).collect();
    for (_, held) in HELD.lock().unwrap().iter() {
        if let Held::Keys(keys) = held {
            down.extend(keys.iter().map(|(vk, _)| *vk));
        }
    }
    RISKY.iter().any(|risky| chord(risky).is_ok_and(|risky| risky.keys.iter().all(|(vk, _)| down.contains(vk))))
}

/// Real keys go to whatever has focus, so they are only sent while the target still has it.
pub fn check_front(root: Hwnd, surfaces: &[Hwnd]) -> Result<(), String> {
    if win::front_is(root, surfaces) { Ok(()) } else { Err(FOCUS_MOVED.into()) }
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
pub fn type_text(text: &str, root: Hwnd, surfaces: &[Hwnd], stop: &AtomicBool) -> Result<bool, String> {
    for (sent, unit) in text.encode_utf16().enumerate() {
        if stop.load(Ordering::Relaxed) {
            return Ok(false);
        }
        if !win::front_is(root, surfaces) {
            return Err(format!("{FOCUS_MOVED}: {sent} characters had been typed"));
        }
        if unit == 10 {
            send(&[key(VK_RETURN, 0, 0)]);
            send(&[key(VK_RETURN, 0, KEYEVENTF_KEYUP)]);
        } else {
            send(&[key(0, unit, KEYEVENTF_UNICODE)]);
            send(&[key(0, unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP)]);
        }
        thread::sleep(crate::motion::KEY_EVERY);
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
    drain: Duration,
    guard: win::FocusGuard,
    root_pids: Vec<u32>,
}

/// What focus is borrowed for: the keyboard, the person's own mouse, or deskcrew's pen.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Loan {
    Keys,
    Mouse,
    Pen,
}

pub fn borrow(root: Hwnd, pids: &[u32], loan: Loan) -> Result<Borrowed, String> {
    let mouse = loan == Loan::Mouse;
    wait_for_quiet();
    let mut pointer = POINT { x: 0, y: 0 };
    unsafe { GetCursorPos(&mut pointer) };
    let pointer = mouse.then_some(pointer);
    let guard = win::FocusGuard::take();
    // A dialog the window opened over itself takes its input, and is what comes forward: the
    // window itself is disabled until it closes, and keys would go nowhere if it came forward.
    let front = win::key_window(root);
    let already_front = win::foreground() == front;
    if !already_front && !win::bring_forward(front) {
        return Err("Windows would not bring that window forward".into());
    }
    if !already_front {
        thread::sleep(if loan == Loan::Pen { PEN_SETTLE } else { FOCUS_SETTLE });
    }
    let drain = if loan == Loan::Pen { PEN_DRAIN } else { DRAIN };
    Ok(Borrowed { pointer, drain, guard, root_pids: pids.to_vec() })
}

impl Drop for Borrowed {
    fn drop(&mut self) {
        thread::sleep(self.drain);
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
        assert!(is_risky("alt+f04"));
        assert!(is_risky("super+r"));
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
