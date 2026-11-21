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
    pointer: POINT,
    /// Where the pen last was, so a pointer still there was left by the pen, not the person.
    last: Cell<Option<POINT>>,
}

/// How far a pointer may be from the pen's last spot and still count as left there by the pen.
const LEFT_BY_PEN: i32 = 3;

fn pointer() -> POINT {
    let mut at = POINT { x: 0, y: 0 };
    unsafe { GetCursorPos(&mut at) };
    at
}

// One thread uses a pen at a time, and real input is already one agent at a time.
unsafe impl Send for Pen {}

impl Pen {
    pub fn new() -> Option<Pen> {
        let device = unsafe { CreateSyntheticPointerDevice(PT_PEN, 1, POINTER_FEEDBACK_NONE) };
        (!device.is_null()).then(|| Pen { device, pointer: pointer(), last: Cell::new(None) })
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
        self.last.set(Some(pointer()));
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
        if let Some(last) = self.last.get() {
            let now = pointer();
            if (now.x - last.x).abs() <= LEFT_BY_PEN && (now.y - last.y).abs() <= LEFT_BY_PEN {
                unsafe { SetCursorPos(self.pointer.x, self.pointer.y) };
            }
        }
    }
}
