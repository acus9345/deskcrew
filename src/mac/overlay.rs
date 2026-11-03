// Each agent's drawn cursor on a Mac: a borderless window that clicks pass through, kept just above
// the window the agent works in, so the person's other windows can cover it. AppKit only runs on the
// main thread, so the service hands it commands and the main thread draws them.
#[path = "../overlay/draw.rs"]
pub mod draw;

use crate::mac::ffi::*;
use crate::motion::{self, Rgb};
use draw::Mask;
use objc2::rc::autoreleasepool;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use std::collections::HashMap;
use std::ptr::null;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

const FRAME: Duration = Duration::from_millis(16);
const RESTACK_EVERY: Duration = Duration::from_millis(100);
const LABEL_PX: f64 = 12.0;
const PRESS_LINGER_MS: f32 = 450.0;
const IMAGE_ALPHA_LAST: u32 = 1;
const UI_FONT_SYSTEM: u32 = 2;
const BEHAVIOUR: usize = 1 | 16 | 64 | 256;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

pub enum Command {
    Move { agent: String, x: i32, y: i32, instant: bool, done: Option<Sender<()>> },
    Over { agent: String, window: u32 },
    Press { agent: String, target: Rect },
    Aside(bool),
}

#[derive(Clone)]
pub struct Overlay {
    commands: Sender<Command>,
}

impl Overlay {
    pub fn new() -> (Overlay, Receiver<Command>) {
        let (commands, inbox) = mpsc::channel();
        (Overlay { commands }, inbox)
    }

    /// Glide there and return once the cursor has arrived.
    pub fn move_to(&self, agent: &str, x: i32, y: i32) {
        let (done, arrived) = mpsc::channel();
        let _ = self.commands.send(Command::Move { agent: agent.into(), x, y, instant: false, done: Some(done) });
        let _ = arrived.recv_timeout(Duration::from_secs(2));
    }

    /// Be there at once, for `--fast` and for following a stroke as it is drawn.
    pub fn jump(&self, agent: &str, x: i32, y: i32) {
        let _ = self.commands.send(Command::Move { agent: agent.into(), x, y, instant: true, done: None });
    }

    pub fn over(&self, agent: &str, window: u32) {
        let _ = self.commands.send(Command::Over { agent: agent.into(), window });
    }

    pub fn press(&self, agent: &str, target: Rect) {
        let _ = self.commands.send(Command::Press { agent: agent.into(), target });
    }

    /// Hide every cursor while the screen is copied.
    pub fn aside(&self, on: bool) {
        let _ = self.commands.send(Command::Aside(on));
        std::thread::sleep(FRAME * 2);
    }
}

/// White text on nothing, as coverage, in the system font.
pub fn text_mask(text: &str, px: f64) -> Mask {
    unsafe {
        let font = Owned::new(CTFontCreateUIFontForLanguage(UI_FONT_SYSTEM, px, null()));
        let Some(font) = font else { return Mask { w: 1, h: 1, alpha: vec![0] } };
        let attributes = dictionary_of(&[kCTFontAttributeName, kCTForegroundColorFromContextAttributeName], &[font.0, kCFBooleanTrue]);
        let string = cf_string(text);
        let Some(attributed) = Owned::new(CFAttributedStringCreate(null(), string.0, attributes.0)) else { return Mask { w: 1, h: 1, alpha: vec![0] } };
        let Some(line) = Owned::new(CTLineCreateWithAttributedString(attributed.0)) else { return Mask { w: 1, h: 1, alpha: vec![0] } };
        let (mut ascent, mut descent, mut leading) = (0.0, 0.0, 0.0);
        let width = CTLineGetTypographicBounds(line.0, &mut ascent, &mut descent, &mut leading);
        let (w, h) = ((width.ceil() as usize + 2).max(1), ((ascent + descent).ceil() as usize + 2).max(1));
        let mut pixels = vec![0u8; w * h * 4];
        let space = CGColorSpaceCreateDeviceRGB();
        let context = CGBitmapContextCreate(pixels.as_mut_ptr().cast(), w, h, 8, w * 4, space, IMAGE_ALPHA_LAST);
        CGColorSpaceRelease(space);
        if !context.is_null() {
            CGContextSetRGBFillColor(context, 1.0, 1.0, 1.0, 1.0);
            CGContextSetTextPosition(context, 1.0, descent + 1.0);
            CTLineDraw(line.0, context);
            CGContextRelease(context);
        }
        Mask { w: w as i32, h: h as i32, alpha: pixels.as_chunks::<4>().0.iter().map(|p| p[3]).collect() }
    }
}

#[cfg(test)]
pub fn label_for_test(text: &str, px: i32) -> Mask {
    text_mask(text, px as f64)
}

/// A see-through window showing a picture. Lives on the main thread only.
struct Layer {
    window: *mut AnyObject,
    w: f64,
    h: f64,
}

fn image_of(picture: &tiny_skia::Pixmap) -> Option<Owned> {
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFDataCreate(alloc: CFAllocatorRef, bytes: *const u8, length: CFIndex) -> CFTypeRef;
    }
    unsafe {
        let data = Owned::new(CFDataCreate(null(), picture.data().as_ptr(), picture.data().len() as CFIndex))?;
        let provider = CGDataProviderCreateWithCFData(data.0);
        let space = CGColorSpaceCreateDeviceRGB();
        let (w, h) = (picture.width() as usize, picture.height() as usize);
        let image = CGImageCreate(w, h, 8, 32, w * 4, space, IMAGE_ALPHA_LAST, provider, null(), false, 0);
        CGColorSpaceRelease(space);
        CGDataProviderRelease(provider);
        Owned::new(image)
    }
}

impl Layer {
    unsafe fn new() -> Layer {
        unsafe {
            let window: *mut AnyObject = msg_send![class!(NSWindow), alloc];
            let rect = CGRect::new(0.0, 0.0, 1.0, 1.0);
            let window: *mut AnyObject = msg_send![window, initWithContentRect: rect, styleMask: 0usize, backing: 2usize, defer: false];
            let clear: *mut AnyObject = msg_send![class!(NSColor), clearColor];
            let _: () = msg_send![window, setOpaque: false];
            let _: () = msg_send![window, setBackgroundColor: clear];
            let _: () = msg_send![window, setHasShadow: false];
            let _: () = msg_send![window, setIgnoresMouseEvents: true];
            let _: () = msg_send![window, setReleasedWhenClosed: false];
            let _: () = msg_send![window, setLevel: 0isize];
            let _: () = msg_send![window, setCollectionBehavior: BEHAVIOUR];
            let view: *mut AnyObject = msg_send![window, contentView];
            let _: () = msg_send![view, setWantsLayer: true];
            Layer { window, w: 1.0, h: 1.0 }
        }
    }

    /// Show a picture drawn at `scale` pixels per point, with its top left at x,y in points.
    unsafe fn show(&mut self, picture: &tiny_skia::Pixmap, scale: f64, x: f64, y: f64, screen_h: f64) {
        let Some(image) = image_of(picture) else { return };
        self.w = picture.width() as f64 / scale;
        self.h = picture.height() as f64 / scale;
        unsafe {
            let frame = CGRect::new(x, screen_h - y - self.h, self.w, self.h);
            let _: () = msg_send![self.window, setFrame: frame, display: false];
            let view: *mut AnyObject = msg_send![self.window, contentView];
            let layer: *mut AnyObject = msg_send![view, layer];
            let _: () = msg_send![layer, setContentsScale: scale];
            let _: () = msg_send![layer, setContents: image.0 as *mut AnyObject];
        }
    }

    unsafe fn place(&self, x: f64, y: f64, screen_h: f64) {
        let origin = CGPoint { x, y: screen_h - y - self.h };
        unsafe {
            let _: () = msg_send![self.window, setFrameOrigin: origin];
        }
    }

    /// Just above the agent's window, or in front if it has none.
    unsafe fn stack(&self, above: u32) {
        unsafe {
            let _: () = msg_send![self.window, orderWindow: 1isize, relativeTo: above as isize];
        }
    }

    unsafe fn hide(&self) {
        unsafe {
            let _: () = msg_send![self.window, orderOut: null::<AnyObject>()];
        }
    }
}

impl Drop for Layer {
    fn drop(&mut self) {
        unsafe {
            let _: () = msg_send![self.window, close];
            let _: () = msg_send![self.window, release];
        }
    }
}

struct Glide {
    from: motion::Point,
    middle: motion::Point,
    to: motion::Point,
    started: Instant,
    ms: u32,
    done: Option<Sender<()>>,
}

struct Agent {
    colour: Rgb,
    layer: Layer,
    tip: (f64, f64),
    at: motion::Point,
    glide: Option<Glide>,
    above: u32,
    side: f32,
}

struct Press {
    colour: Rgb,
    target: Rect,
    layer: Layer,
    started: Instant,
    above: u32,
}

/// Runs on the main thread for as long as the service lives, drawing whatever the service asks.
pub fn run(inbox: Receiver<Command>, stop: &dyn Fn() -> bool) {
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        // An accessory app has no Dock icon and never becomes the front app.
        let _: bool = msg_send![app, setActivationPolicy: 1isize];
        let _: () = msg_send![app, finishLaunching];
        let screen: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
        let scale: f64 = if screen.is_null() { 2.0 } else { msg_send![screen, backingScaleFactor] };
        let screen_h = crate::mac::apps::main_height();
        let mut agents: HashMap<String, Agent> = HashMap::new();
        let mut presses: Vec<Press> = Vec::new();
        let mut aside = false;
        let mut restacked = Instant::now();
        while !stop() {
            autoreleasepool(|_| {
                let until: *mut AnyObject = msg_send![class!(NSDate), dateWithTimeIntervalSinceNow: FRAME.as_secs_f64()];
                let mode: *mut AnyObject = msg_send![class!(NSString), stringWithUTF8String: c"kCFRunLoopDefaultMode".as_ptr()];
                loop {
                    let event: *mut AnyObject = msg_send![app, nextEventMatchingMask: usize::MAX, untilDate: until, inMode: mode, dequeue: true];
                    if event.is_null() {
                        break;
                    }
                    let _: () = msg_send![app, sendEvent: event];
                }
            });
            while let Ok(command) = inbox.try_recv() {
                match command {
                    Command::Move { agent, x, y, instant, done } => {
                        let taken: Vec<Rgb> = agents.values().map(|a| a.colour).collect();
                        let side = if agents.len().is_multiple_of(2) { 1.0 } else { -1.0 };
                        let entry = agents.entry(agent.clone()).or_insert_with(|| {
                            let colour = motion::colour_for(&agent, &taken);
                            let mut layer = Layer::new();
                            let mask = text_mask(&agent, LABEL_PX * scale);
                            let mut tip = (0.0, 0.0);
                            if let Some((picture, tx, ty)) = draw::cursor(colour, &mask, scale as f32) {
                                tip = (tx as f64 / scale, ty as f64 / scale);
                                layer.show(&picture, scale, x as f64 - tip.0, y as f64 - tip.1, screen_h);
                            }
                            Agent { colour, layer, tip, at: (x as f32, y as f32), glide: None, above: 0, side }
                        });
                        let to = (x as f32, y as f32);
                        if instant {
                            entry.glide = None;
                            entry.at = to;
                            entry.layer.place(x as f64 - entry.tip.0, y as f64 - entry.tip.1, screen_h);
                            if let Some(done) = done {
                                let _ = done.send(());
                            }
                            continue;
                        }
                        let ms = motion::travel_ms(entry.at, to);
                        entry.glide = Some(Glide { from: entry.at, middle: motion::bow(entry.at, to, entry.side), to, started: Instant::now(), ms, done });
                        entry.side = -entry.side;
                        if !aside {
                            entry.layer.stack(entry.above);
                        }
                    }
                    Command::Over { agent, window } => {
                        if let Some(entry) = agents.get_mut(&agent) {
                            entry.above = window;
                            if !aside {
                                entry.layer.stack(window);
                            }
                        }
                    }
                    Command::Press { agent, target } => {
                        if let Some(entry) = agents.get(&agent) {
                            presses.push(Press { colour: entry.colour, target, layer: Layer::new(), started: Instant::now(), above: entry.above });
                        }
                    }
                    Command::Aside(on) => {
                        aside = on;
                        for agent in agents.values() {
                            if on { agent.layer.hide() } else { agent.layer.stack(agent.above) }
                        }
                    }
                }
            }
            for agent in agents.values_mut() {
                let Some(glide) = &mut agent.glide else { continue };
                let t = (glide.started.elapsed().as_millis() as f32 / glide.ms.max(1) as f32).min(1.0);
                agent.at = motion::along(glide.from, glide.middle, glide.to, motion::ease(t));
                agent.layer.place(agent.at.0 as f64 - agent.tip.0, agent.at.1 as f64 - agent.tip.1, screen_h);
                if t >= 1.0 {
                    if let Some(done) = glide.done.take() {
                        let _ = done.send(());
                    }
                    agent.glide = None;
                }
            }
            presses.retain_mut(|press| {
                let ms = press.started.elapsed().as_millis() as f32;
                if ms > draw::PRESS_SETTLE_MS + PRESS_LINGER_MS {
                    return false;
                }
                let margin = draw::outline_margin(scale as f32) as f64 / scale;
                let (w, h) = ((press.target.w as f64 * scale) as i32, (press.target.h as f64 * scale) as i32);
                if let Some(picture) = draw::press(press.colour, w, h, scale as f32, ms.min(draw::PRESS_SETTLE_MS)) {
                    press.layer.show(&picture, scale, press.target.x as f64 - margin, press.target.y as f64 - margin, screen_h);
                    if !aside {
                        press.layer.stack(press.above);
                    }
                }
                true
            });
            if restacked.elapsed() >= RESTACK_EVERY && !aside {
                restacked = Instant::now();
                for agent in agents.values() {
                    agent.layer.stack(agent.above);
                }
            }
        }
    }
}
