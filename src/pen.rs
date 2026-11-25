// A second pointer of our own, for apps that take no posted mouse messages (WPF, WinUI, Store
// apps). Windows moves the person's pointer along with a synthetic pen, so the pen puts it back
// where it was when it is done, unless the person has moved it since.
use crate::{motion, real::Button};
use std::{cell::Cell, mem::zeroed, sync::atomic::AtomicBool, thread, time::Duration};
use windows_sys::Win32::{
    Foundation::POINT,
    UI::{
        Controls::{CreateSyntheticPointerDevice, DestroySyntheticPointerDevice, HSYNTHETICPOINTERDEVICE, POINTER_FEEDBACK_NONE, POINTER_TYPE_INFO},
        Input::Pointer::{
            InjectSyntheticPointerInput, POINTER_FLAG_DOWN, POINTER_FLAG_FIRSTBUTTON, POINTER_FLAG_INCONTACT, POINTER_FLAG_INRANGE,
            POINTER_FLAG_SECONDBUTTON, POINTER_FLAG_UP, POINTER_FLAG_UPDATE, POINTER_FLAGS,
        },
        WindowsAndMessaging::{GetCursorPos, PEN_MASK_PRESSURE, PT_PEN, SetCursorPos},
    },
};

const PRESSURE: u32 = 512;
/// The pen moves four times as fast as a drawn cursor glides: the window is in front for as long
/// as the stroke lasts, and Paint and WPF drew clean curves at up to 6000 pixels a second.
const SPEED: f32 = 4000.0;
const SETTLE: Duration = Duration::from_millis(30);
const PRESS: Duration = Duration::from_millis(40);
const PEN_FLAG_BARREL: u32 = 1;

pub struct Pen {
    device: HSYNTHETICPOINTERDEVICE,
    /// Where the person's pointer was before the pen moved it.
    pointer: Cell<Option<POINT>>,
    /// Where the pen last aimed, where it aimed just before that, and what GetCursorPos last saw.
    sent: Cell<Option<POINT>>,
    prev_sent: Cell<Option<POINT>>,
    observed: Cell<Option<POINT>>,
}

/// How far a pointer may be from the pen's last spot and still count as left there by the pen.
const LEFT_BY_PEN: i32 = 4;

fn near(a: POINT, b: POINT) -> bool {
    (a.x as i64 - b.x as i64).abs() <= LEFT_BY_PEN as i64 && (a.y as i64 - b.y as i64).abs() <= LEFT_BY_PEN as i64
}

fn should_restore_cursor(now: POINT, sent: Option<POINT>, prev_sent: Option<POINT>, observed: Option<POINT>) -> bool {
    sent.is_some_and(|p| near(now, p)) || prev_sent.is_some_and(|p| near(now, p)) || observed.is_some_and(|p| near(now, p))
}

fn pointer() -> Option<POINT> {
    let mut at = POINT { x: 0, y: 0 };
    unsafe { GetCursorPos(&mut at) != 0 }.then_some(at)
}

// One thread uses a pen at a time, and real input is already one agent at a time.
unsafe impl Send for Pen {}

impl Pen {
    pub fn new() -> Option<Pen> {
        let device = unsafe { CreateSyntheticPointerDevice(PT_PEN, 1, POINTER_FEEDBACK_NONE) };
        (!device.is_null()).then(|| Pen {
            device,
            pointer: Cell::new(pointer()),
            sent: Cell::new(None),
            prev_sent: Cell::new(None),
            observed: Cell::new(None),
        })
    }

    fn send(&self, x: i32, y: i32, flags: POINTER_FLAGS, pressure: u32) -> bool {
        let ok = unsafe {
            let mut input: POINTER_TYPE_INFO = zeroed();
            input.r#type = PT_PEN;
            let pen = &mut input.Anonymous.penInfo;
            pen.pointerInfo.pointerType = PT_PEN;
            pen.pointerInfo.pointerId = 1;
            pen.pointerInfo.pointerFlags = flags;
            pen.pointerInfo.ptPixelLocation = POINT { x, y };
            pen.pointerInfo.ptPixelLocationRaw = POINT { x, y };
            pen.penMask = PEN_MASK_PRESSURE;
            if flags & POINTER_FLAG_SECONDBUTTON != 0 {
                pen.penFlags = PEN_FLAG_BARREL;
            }
            pen.pressure = pressure;
            InjectSyntheticPointerInput(self.device, &input, 1) != 0
        };
        if ok {
            self.prev_sent.set(self.sent.get());
            self.sent.set(Some(POINT { x, y }));
            if let Some(obs) = pointer()
                && !self.pointer.get().is_some_and(|orig| near(obs, orig))
            {
                self.observed.set(Some(obs));
            }
        }
        ok
    }

    fn hover(&self, x: i32, y: i32) -> bool {
        self.send(x, y, POINTER_FLAG_INRANGE | POINTER_FLAG_UPDATE, 0)
    }

    fn press(&self, x: i32, y: i32, button: POINTER_FLAGS) -> bool {
        self.send(x, y, POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT | button | POINTER_FLAG_DOWN, PRESSURE)
    }

    fn slide(&self, x: i32, y: i32, button: POINTER_FLAGS) -> bool {
        self.send(x, y, POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT | button | POINTER_FLAG_UPDATE, PRESSURE)
    }

    fn lift(&self, x: i32, y: i32) -> bool {
        let up = self.send(x, y, POINTER_FLAG_INRANGE | POINTER_FLAG_UP, 0);
        thread::sleep(SETTLE);
        self.send(x, y, POINTER_FLAG_UPDATE, 0) && up
    }

    /// One press, or two for a double click. A right click is the pen's barrel button.
    pub fn tap(&self, x: i32, y: i32, right: bool, double: bool) -> bool {
        crate::real::wait_for_quiet();
        if self.sent.get().is_none() {
            self.pointer.set(pointer());
        }
        if !self.hover(x, y) {
            return false;
        }
        thread::sleep(SETTLE);
        let button = if right { POINTER_FLAG_SECONDBUTTON } else { POINTER_FLAG_FIRSTBUTTON };
        for _ in 0..if double { 2 } else { 1 } {
            self.press(x, y, button);
            thread::sleep(PRESS);
            self.send(x, y, POINTER_FLAG_INRANGE | POINTER_FLAG_UP, 0);
            thread::sleep(PRESS);
        }
        self.send(x, y, POINTER_FLAG_UPDATE, 0);
        true
    }

    /// Press, follow the path at a steady speed, and always lift. False if stopped on the way.
    pub fn stroke(&self, path: &[(f32, f32)], button: Button, stop: &AtomicBool, moved: impl FnMut(i32, i32)) -> Result<bool, String> {
        self.put_down(path, button)?;
        Ok(self.draw_on(path, button, stop, moved))
    }

    /// Touch down at the start of the path. Only this needs the window on top: Windows keeps
    /// sending this pen to the window it pressed until it lifts.
    pub fn put_down(&self, path: &[(f32, f32)], button: Button) -> Result<(), String> {
        let Some(first) = path.first() else { return Ok(()) };
        crate::real::wait_for_quiet();
        if self.sent.get().is_none() {
            self.pointer.set(pointer());
        }
        let (x, y) = (first.0.round() as i32, first.1.round() as i32);
        if !self.hover(x, y) {
            return Err("Windows would not take pen input".into());
        }
        thread::sleep(SETTLE);
        self.press(x, y, pen_button(button));
        thread::sleep(PRESS);
        Ok(())
    }

    /// Follow the rest of a path already pressed down, and always lift. False if stopped on the way.
    pub fn draw_on(&self, path: &[(f32, f32)], button: Button, stop: &AtomicBool, mut moved: impl FnMut(i32, i32)) -> bool {
        let Some(first) = path.first() else { return true };
        let mut last = (first.0.round() as i32, first.1.round() as i32);
        let finished = motion::pace_at(path, SPEED, stop, |at, x, y| {
            last = (x, y);
            self.slide(x, y, pen_button(button));
            if at % 3 == 0 {
                moved(x, y);
            }
        });
        thread::sleep(SETTLE);
        self.lift(last.0, last.1);
        finished
    }
}

/// The barrel button is the pen's right click.
fn pen_button(button: Button) -> POINTER_FLAGS {
    if button == Button::Right { POINTER_FLAG_SECONDBUTTON } else { POINTER_FLAG_FIRSTBUTTON }
}

impl Drop for Pen {
    fn drop(&mut self) {
        unsafe { DestroySyntheticPointerDevice(self.device) };
        // Give the pointer back, unless the person has taken it somewhere else meanwhile.
        if self.sent.get().is_some()
            && let Some(orig) = self.pointer.get()
        {
            // Let any in-flight synthetic pointer event drain so it cannot move the cursor after SetCursorPos.
            thread::sleep(SETTLE);
            if let Some(now) = pointer()
                && should_restore_cursor(now, self.sent.get(), self.prev_sent.get(), self.observed.get())
            {
                unsafe { SetCursorPos(orig.x, orig.y) };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(x: i32, y: i32) -> POINT {
        POINT { x, y }
    }

    #[test]
    fn restores_cursor_when_async_win32k_lagged_observed_point() {
        // Simulates InjectSyntheticPointerInput queued (300, 400), GetCursorPos immediately after
        // still saw previous point (120, 120), then by Pen::drop the OS cursor arrived at (300, 400).
        assert!(should_restore_cursor(
            pt(300, 400),
            Some(pt(300, 400)),
            Some(pt(120, 120)),
            Some(pt(120, 120)),
        ));
    }

    #[test]
    fn restores_cursor_for_prev_sent_observed_and_boundary_tolerance() {
        assert!(should_restore_cursor(pt(120, 120), Some(pt(300, 400)), Some(pt(120, 120)), None));
        assert!(should_restore_cursor(pt(500, 500), Some(pt(520, 520)), None, Some(pt(500, 500))));
        assert!(should_restore_cursor(
            pt(100 + LEFT_BY_PEN, 200 - LEFT_BY_PEN),
            Some(pt(100, 200)),
            None,
            None,
        ));
        assert!(!should_restore_cursor(
            pt(100 + LEFT_BY_PEN + 1, 200),
            Some(pt(100, 200)),
            None,
            None,
        ));
    }

    #[test]
    fn skips_restore_when_user_moved_cursor_or_pen_was_unused() {
        assert!(!should_restore_cursor(
            pt(900, 900),
            Some(pt(300, 400)),
            Some(pt(280, 390)),
            Some(pt(280, 390)),
        ));
        // Y-axis outside tolerance on negative coordinates must not restore.
        assert!(!should_restore_cursor(
            pt(-100, -200 - LEFT_BY_PEN - 1),
            Some(pt(-100, -200)),
            None,
            None,
        ));
        assert!(!should_restore_cursor(pt(300, 400), None, None, None));
        // Extreme coordinate difference does not overflow i32 subtraction.
        assert!(!should_restore_cursor(pt(i32::MAX, i32::MIN), Some(pt(i32::MIN, i32::MAX)), None, None));
    }
}
