// A second pointer of our own. Windows treats a synthetic pen as separate hardware, so modern apps
// can be pressed and drawn on while the person keeps their mouse. Classic apps get the pen turned
// into mouse events, which would move the pointer, so they get window messages instead.
use crate::real;
use std::{mem::zeroed, sync::atomic::AtomicBool, thread, time::Duration};
use windows_sys::Win32::{
    Foundation::POINT,
    UI::{
        Controls::{CreateSyntheticPointerDevice, DestroySyntheticPointerDevice, HSYNTHETICPOINTERDEVICE, POINTER_FEEDBACK_NONE, POINTER_TYPE_INFO},
        Input::Pointer::{
            InjectSyntheticPointerInput, POINTER_FLAG_DOWN, POINTER_FLAG_FIRSTBUTTON, POINTER_FLAG_INCONTACT, POINTER_FLAG_INRANGE,
            POINTER_FLAG_SECONDBUTTON, POINTER_FLAG_UP, POINTER_FLAG_UPDATE, POINTER_FLAGS,
        },
        WindowsAndMessaging::{PEN_MASK_PRESSURE, PT_PEN},
    },
};

const PRESSURE: u32 = 512;
const SETTLE: Duration = Duration::from_millis(30);
const PRESS: Duration = Duration::from_millis(40);
const PEN_FLAG_BARREL: u32 = 1;

pub struct Pen(HSYNTHETICPOINTERDEVICE);

// One thread uses a pen at a time, and real input is already one agent at a time.
unsafe impl Send for Pen {}

impl Pen {
    pub fn new() -> Option<Pen> {
        let device = unsafe { CreateSyntheticPointerDevice(PT_PEN, 1, POINTER_FEEDBACK_NONE) };
        (!device.is_null()).then_some(Pen(device))
    }

    fn send(&self, x: i32, y: i32, flags: POINTER_FLAGS, pressure: u32) -> bool {
        unsafe {
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
            InjectSyntheticPointerInput(self.0, &input, 1) != 0
        }
    }

    fn hover(&self, x: i32, y: i32) -> bool {
        self.send(x, y, POINTER_FLAG_INRANGE | POINTER_FLAG_UPDATE, 0)
    }

    fn press(&self, x: i32, y: i32, button: POINTER_FLAGS) -> bool {
        self.send(x, y, POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT | button | POINTER_FLAG_DOWN, PRESSURE)
    }

    fn slide(&self, x: i32, y: i32) -> bool {
        self.send(x, y, POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT | POINTER_FLAG_FIRSTBUTTON | POINTER_FLAG_UPDATE, PRESSURE)
    }

    fn lift(&self, x: i32, y: i32) -> bool {
        let up = self.send(x, y, POINTER_FLAG_INRANGE | POINTER_FLAG_UP, 0);
        thread::sleep(SETTLE);
        self.send(x, y, POINTER_FLAG_UPDATE, 0) && up
    }

    /// One press, or two for a double click. A right click is the pen's barrel button.
    pub fn tap(&self, x: i32, y: i32, right: bool, double: bool) -> bool {
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
    pub fn stroke(&self, path: &[(f32, f32)], stop: &AtomicBool, mut moved: impl FnMut(i32, i32)) -> Result<bool, String> {
        let Some(first) = path.first() else { return Ok(true) };
        let (x, y) = (first.0.round() as i32, first.1.round() as i32);
        if !self.hover(x, y) {
            return Err("Windows would not take pen input".into());
        }
        thread::sleep(SETTLE);
        self.press(x, y, POINTER_FLAG_FIRSTBUTTON);
        thread::sleep(PRESS);
        let mut last = (x, y);
        let finished = real::pace(path, stop, |at, x, y| {
            last = (x, y);
            self.slide(x, y);
            if at % 3 == 0 {
                moved(x, y);
            }
        });
        thread::sleep(SETTLE);
        self.lift(last.0, last.1);
        Ok(finished)
    }
}

impl Drop for Pen {
    fn drop(&mut self) {
        unsafe { DestroySyntheticPointerDevice(self.0) };
    }
}
