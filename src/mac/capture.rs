// Pictures of one window, taken from the window itself, so it can sit behind others. macOS asks
// the person once for screen recording permission.
use super::ffi::*;
use crate::shot::Frame;

/// The picture as blue-green-red-alpha pixels, drawn into memory laid out the way Frame wants.
pub fn frame_of(image: CGImageRef) -> Option<Frame> {
    unsafe {
        let (w, h) = (CGImageGetWidth(image), CGImageGetHeight(image));
        if w == 0 || h == 0 {
            return None;
        }
        let mut pixels = vec![0u8; w * h * 4];
        let space = CGColorSpaceCreateDeviceRGB();
        let context = CGBitmapContextCreate(pixels.as_mut_ptr().cast(), w, h, 8, w * 4, space, kCGImageAlphaPremultipliedFirst | kCGBitmapByteOrder32Little);
        CGColorSpaceRelease(space);
        if context.is_null() {
            return None;
        }
        CGContextDrawImage(context, CGRect::new(0.0, 0.0, w as f64, h as f64), image);
        CGContextRelease(context);
        Some(Frame { w: w as u32, h: h as u32, pixels })
    }
}

fn allowed() -> Result<(), String> {
    if unsafe { CGPreflightScreenCaptureAccess() } {
        return Ok(());
    }
    unsafe { CGRequestScreenCaptureAccess() };
    Err("pictures need screen recording permission: System Settings > Privacy & Security > Screen Recording, allow the app running your agent, then restart it".into())
}

use objc2::rc::autoreleasepool;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::msg_send;
use std::collections::HashMap;
use std::ffi::{c_char, c_void};
use std::sync::{Arc, Condvar, Mutex, Once};
use std::time::{Duration, Instant};

const SCK_TIMEOUT: Duration = Duration::from_secs(3);
const SCK_WINDOW_CACHE_TTL: Duration = Duration::from_secs(2);
const SCK_WINDOW_CACHE_MAX: usize = 64;
const BLOCK_HAS_COPY_DISPOSE: i32 = 1 << 25;

unsafe extern "C" {
    static _NSConcreteStackBlock: c_void;
    fn dlopen(filename: *const c_char, flag: i32) -> *mut c_void;
}

#[repr(C)]
struct BlockDescriptor {
    reserved: usize,
    size: usize,
    copy: unsafe extern "C" fn(*mut c_void, *const c_void),
    dispose: unsafe extern "C" fn(*mut c_void),
}

struct Slot<T> {
    ready: Mutex<Option<Option<T>>>,
    signal: Condvar,
}

impl<T> Slot<T> {
    fn new() -> Arc<Self> {
        Arc::new(Self { ready: Mutex::new(None), signal: Condvar::new() })
    }

    fn finish(&self, value: Option<T>) {
        let mut guard = self.ready.lock().unwrap();
        if guard.is_none() {
            *guard = Some(value);
            self.signal.notify_all();
        }
    }

    fn wait(&self, timeout: Duration) -> Option<T> {
        let guard = self.ready.lock().unwrap();
        let (mut guard, _) = self.signal.wait_timeout_while(guard, timeout, |v| v.is_none()).unwrap();
        guard.take().flatten()
    }
}

struct RetainedObj(*mut AnyObject);
unsafe impl Send for RetainedObj {}

impl Clone for RetainedObj {
    fn clone(&self) -> Self {
        if self.0.is_null() {
            return RetainedObj(std::ptr::null_mut());
        }
        let retained: *mut AnyObject = unsafe { msg_send![self.0, retain] };
        RetainedObj(retained)
    }
}

impl Drop for RetainedObj {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _: () = msg_send![self.0, release];
            }
        }
    }
}

/// Short-lived cache of `SCWindow` handles so rapid `see`/`shot` sequences reuse the last
/// `SCShareableContent` lookup without re-enumerating every desktop window each call.
static SCK_WINDOWS: Mutex<Option<HashMap<u32, (Instant, RetainedObj)>>> = Mutex::new(None);

fn cached_sck_window(id: u32) -> Option<RetainedObj> {
    let mut guard = SCK_WINDOWS.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.as_mut()?;
    if let Some((at, win)) = map.get(&id) {
        if at.elapsed() < SCK_WINDOW_CACHE_TTL {
            return Some(win.clone());
        }
        map.remove(&id);
    }
    None
}

fn cache_sck_window(id: u32, win: RetainedObj) {
    let mut guard = SCK_WINDOWS.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    if map.len() >= SCK_WINDOW_CACHE_MAX {
        map.retain(|_, (at, _)| at.elapsed() < SCK_WINDOW_CACHE_TTL);
        if map.len() >= SCK_WINDOW_CACHE_MAX {
            map.clear();
        }
    }
    map.insert(id, (Instant::now(), win));
}

fn evict_sck_window(id: u32) {
    let mut guard = SCK_WINDOWS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(map) = guard.as_mut() {
        map.remove(&id);
    }
}

#[repr(C)]
struct ContentBlock {
    isa: *const c_void,
    flags: i32,
    reserved: i32,
    invoke: unsafe extern "C" fn(*mut c_void, *mut AnyObject, *mut AnyObject),
    descriptor: *const BlockDescriptor,
    slot: *const Slot<RetainedObj>,
    window_id: u32,
}

unsafe extern "C" fn copy_content(dst: *mut c_void, src: *const c_void) {
    let _ = dst;
    unsafe {
        let block = &*(src as *const ContentBlock);
        Arc::increment_strong_count(block.slot);
    }
}

unsafe extern "C" fn dispose_content(src: *mut c_void) {
    unsafe {
        let block = &*(src as *const ContentBlock);
        drop(Arc::from_raw(block.slot));
    }
}

unsafe extern "C" fn invoke_content(raw_block: *mut c_void, content: *mut AnyObject, _err: *mut AnyObject) {
    unsafe {
        let block = &*(raw_block as *const ContentBlock);
        let slot = &*block.slot;
        let mut matched: Option<RetainedObj> = None;
        if !content.is_null() {
            let windows: *mut AnyObject = msg_send![content, windows];
            if !windows.is_null() {
                let count: usize = msg_send![windows, count];
                for at in 0..count {
                    let win: *mut AnyObject = msg_send![windows, objectAtIndex: at];
                    if !win.is_null() {
                        let wid: u32 = msg_send![win, windowID];
                        if wid == block.window_id {
                            let retained: *mut AnyObject = msg_send![win, retain];
                            matched = Some(RetainedObj(retained));
                            break;
                        }
                    }
                }
            }
        }
        slot.finish(matched);
    }
}

static CONTENT_DESCRIPTOR: BlockDescriptor = BlockDescriptor {
    reserved: 0,
    size: std::mem::size_of::<ContentBlock>(),
    copy: copy_content,
    dispose: dispose_content,
};

#[repr(C)]
struct ImageBlock {
    isa: *const c_void,
    flags: i32,
    reserved: i32,
    invoke: unsafe extern "C" fn(*mut c_void, CGImageRef, *mut AnyObject),
    descriptor: *const BlockDescriptor,
    slot: *const Slot<Owned>,
}

unsafe extern "C" fn copy_image(dst: *mut c_void, src: *const c_void) {
    let _ = dst;
    unsafe {
        let block = &*(src as *const ImageBlock);
        Arc::increment_strong_count(block.slot);
    }
}

unsafe extern "C" fn dispose_image(src: *mut c_void) {
    unsafe {
        let block = &*(src as *const ImageBlock);
        drop(Arc::from_raw(block.slot));
    }
}

unsafe extern "C" fn invoke_image(raw_block: *mut c_void, image: CGImageRef, _err: *mut AnyObject) {
    unsafe {
        let block = &*(raw_block as *const ImageBlock);
        let slot = &*block.slot;
        slot.finish(Owned::retain(image as CFTypeRef));
    }
}

static IMAGE_DESCRIPTOR: BlockDescriptor = BlockDescriptor {
    reserved: 0,
    size: std::mem::size_of::<ImageBlock>(),
    copy: copy_image,
    dispose: dispose_image,
};

fn load_screencapturekit() {
    static LOAD: Once = Once::new();
    LOAD.call_once(|| unsafe {
        dlopen(c"/System/Library/Frameworks/ScreenCaptureKit.framework/ScreenCaptureKit".as_ptr(), 1);
    });
}

fn fetch_sck_window(content_cls: &AnyClass, id: u32) -> Option<RetainedObj> {
    unsafe {
        let win_slot = Slot::<RetainedObj>::new();
        let win_raw = Arc::into_raw(Arc::clone(&win_slot));
        let mut win_block = ContentBlock {
            isa: &_NSConcreteStackBlock,
            flags: BLOCK_HAS_COPY_DISPOSE,
            reserved: 0,
            invoke: invoke_content,
            descriptor: &CONTENT_DESCRIPTOR,
            slot: win_raw,
            window_id: id,
        };
        let _: () = msg_send![
            content_cls,
            getShareableContentExcludingDesktopWindows: false,
            onScreenWindowsOnly: false,
            completionHandler: &mut win_block as *mut ContentBlock as *mut c_void
        ];
        drop(Arc::from_raw(win_raw));
        let found = win_slot.wait(SCK_TIMEOUT)?;
        cache_sck_window(id, found.clone());
        Some(found)
    }
}

fn capture_with_sck_window(filter_cls: &AnyClass, config_cls: &AnyClass, shot_cls: &AnyClass, target_win: &RetainedObj) -> Option<Frame> {
    unsafe {
        // Build desktop-independent SCContentFilter & full-resolution SCStreamConfiguration.
        let filter_alloc: *mut AnyObject = msg_send![filter_cls, alloc];
        let filter: *mut AnyObject = msg_send![filter_alloc, initWithDesktopIndependentWindow: target_win.0];
        if filter.is_null() {
            return None;
        }
        // Prefer SCContentFilter.contentRect (macOS 14+) over SCWindow.frame so the dimensions
        // match the exact captured content area without drop-shadow padding.
        let has_content_rect: bool = msg_send![filter, respondsToSelector: objc2::sel!(contentRect)];
        let content_rect: CGRect = if has_content_rect {
            msg_send![filter, contentRect]
        } else {
            msg_send![target_win.0, frame]
        };
        if !content_rect.size.width.is_finite()
            || !content_rect.size.height.is_finite()
            || content_rect.size.width < 1.0
            || content_rect.size.height < 1.0
        {
            let _: () = msg_send![filter, release];
            return None;
        }
        let config: *mut AnyObject = msg_send![config_cls, new];
        if config.is_null() {
            let _: () = msg_send![filter, release];
            return None;
        }
        let _: () = msg_send![config, setShowsCursor: false];
        // Exclude macOS window drop-shadows so screenshot coordinates match window.rect 1:1
        // (matching CGWindowListCreateImage with kCGWindowImageBoundsIgnoreFraming).
        let has_ignore_shadows: bool = msg_send![config, respondsToSelector: objc2::sel!(setIgnoreShadowsSingleWindow:)];
        if has_ignore_shadows {
            let _: () = msg_send![config, setIgnoreShadowsSingleWindow: true];
        } else {
            // Without shadow exclusion, desktop-independent capture includes drop-shadow margins
            // that shift click/OCR coordinates; fall back to CGWindowListCreateImage instead.
            let _: () = msg_send![config, release];
            let _: () = msg_send![filter, release];
            return None;
        }
        // SCContentFilter.pointPixelScale (macOS 14+) returns the exact backing scale for the
        // display hosting this window without touching AppKit's NSScreen off the main thread.
        let has_scale: bool = msg_send![filter, respondsToSelector: objc2::sel!(pointPixelScale)];
        let scale: f64 = if has_scale {
            let s: f32 = msg_send![filter, pointPixelScale];
            s as f64
        } else {
            2.0
        };
        let scale = if scale.is_finite() && scale >= 1.0 { scale } else { 1.0 };
        let px_w = ((content_rect.size.width * scale).round() as usize).max(1);
        let px_h = ((content_rect.size.height * scale).round() as usize).max(1);
        let _: () = msg_send![config, setWidth: px_w];
        let _: () = msg_send![config, setHeight: px_h];

        // Capture one CGImage directly from the compositor backing surface.
        let img_slot = Slot::<Owned>::new();
        let img_raw = Arc::into_raw(Arc::clone(&img_slot));
        let mut img_block = ImageBlock {
            isa: &_NSConcreteStackBlock,
            flags: BLOCK_HAS_COPY_DISPOSE,
            reserved: 0,
            invoke: invoke_image,
            descriptor: &IMAGE_DESCRIPTOR,
            slot: img_raw,
        };
        let _: () = msg_send![
            shot_cls,
            captureImageWithFilter: filter,
            configuration: config,
            completionHandler: &mut img_block as *mut ImageBlock as *mut c_void
        ];
        drop(Arc::from_raw(img_raw));
        let _: () = msg_send![config, release];
        let _: () = msg_send![filter, release];
        let image = img_slot.wait(SCK_TIMEOUT)?;
        frame_of(image.0 as CGImageRef)
    }
}

/// One-shot desktop-independent window capture via macOS 14+/15+ `ScreenCaptureKit`
/// (`SCScreenshotManager.captureImageWithFilter:configuration:completionHandler:`), avoiding the
/// deprecated `CGWindowListCreateImage` API on Sonoma and Sequoia. Reuses a short-TTL `SCWindow`
/// cache for fast repeated `see`/`shot` calls, refreshing once on stale handles.
fn capture_window_sck(id: u32, size_points: (i32, i32)) -> Option<Frame> {
    load_screencapturekit();
    let content_cls = AnyClass::get(c"SCShareableContent")?;
    let filter_cls = AnyClass::get(c"SCContentFilter")?;
    let config_cls = AnyClass::get(c"SCStreamConfiguration")?;
    let shot_cls = AnyClass::get(c"SCScreenshotManager")?;

    autoreleasepool(|_| {
        if let Some(cached) = cached_sck_window(id) {
            let (cached_w, cached_h) = unsafe {
                let r: CGRect = msg_send![cached.0, frame];
                (r.size.width.round() as i32, r.size.height.round() as i32)
            };
            if (cached_w - size_points.0).abs() <= 1
                && (cached_h - size_points.1).abs() <= 1
                && let Some(frame) = capture_with_sck_window(filter_cls, config_cls, shot_cls, &cached)
            {
                return Some(frame);
            }
            evict_sck_window(id);
        }
        let fresh = fetch_sck_window(content_cls, id)?;
        capture_with_sck_window(filter_cls, config_cls, shot_cls, &fresh)
    })
}

/// The window's own picture, and the points on screen per pixel of it (0.5 on a Retina screen).
pub fn window(id: u32, size_points: (i32, i32)) -> Result<(Frame, f32), String> {
    allowed()?;
    let width_points = size_points.0;
    if let Some(frame) = capture_window_sck(id, size_points) {
        let units = if frame.w > 0 { width_points as f32 / frame.w as f32 } else { 1.0 };
        return Ok((frame, units));
    }
    let null_rect = CGRect::new(f64::INFINITY, f64::INFINITY, 0.0, 0.0);
    let image = unsafe { CGWindowListCreateImage(null_rect, kCGWindowListOptionIncludingWindow, id, kCGWindowImageBoundsIgnoreFraming | kCGWindowImageBestResolution) };
    if image.is_null() {
        return Err("macOS would not draw that window".into());
    }
    let frame = frame_of(image);
    unsafe { CGImageRelease(image) };
    let frame = frame.ok_or("macOS would not draw that window")?;
    let units = if frame.w > 0 { width_points as f32 / frame.w as f32 } else { 1.0 };
    Ok((frame, units))
}

/// What is on screen in this rectangle of points, at full resolution.
pub fn screen(x: i32, y: i32, w: i32, h: i32) -> Result<(Frame, f32), String> {
    allowed()?;
    let rect = CGRect::new(x as f64, y as f64, w as f64, h as f64);
    let image = unsafe { CGWindowListCreateImage(rect, kCGWindowListOptionOnScreenOnly, kCGNullWindowID, kCGWindowImageBestResolution) };
    if image.is_null() {
        return Err("macOS would not copy the screen".into());
    }
    let frame = frame_of(image);
    unsafe { CGImageRelease(image) };
    let frame = frame.ok_or("macOS would not copy the screen")?;
    let units = w as f32 / frame.w.max(1) as f32;
    Ok((frame, units))
}
