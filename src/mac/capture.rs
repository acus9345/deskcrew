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

/// The window's own picture, and the points on screen per pixel of it (0.5 on a Retina screen).
pub fn window(id: u32, width_points: i32) -> Result<(Frame, f32), String> {
    allowed()?;
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
