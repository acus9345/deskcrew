// Reading text off a picture with Apple's Vision framework, on the computer itself.
use super::ffi::*;
use crate::look::Text;
use crate::shot::Frame;
use objc2::rc::autoreleasepool;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use std::ffi::{CStr, c_char};
use std::ptr::null_mut;

const MAX_TEXT_LINES: usize = 30;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFDataCreate(alloc: CFAllocatorRef, bytes: *const u8, length: CFIndex) -> CFTypeRef;
}

/// A CGImage of the frame, which Vision reads.
pub fn image_of(frame: &Frame) -> Option<Owned> {
    unsafe {
        let data = Owned::new(CFDataCreate(std::ptr::null(), frame.pixels.as_ptr(), frame.pixels.len() as CFIndex))?;
        let provider = CGDataProviderCreateWithCFData(data.0);
        let space = CGColorSpaceCreateDeviceRGB();
        let image = CGImageCreate(
            frame.w as usize,
            frame.h as usize,
            8,
            32,
            frame.w as usize * 4,
            space,
            kCGImageAlphaPremultipliedFirst | kCGBitmapByteOrder32Little,
            provider,
            std::ptr::null(),
            false,
            0,
        );
        CGColorSpaceRelease(space);
        CGDataProviderRelease(provider);
        Owned::new(image)
    }
}

pub fn read_text(frame: &Frame) -> Result<Vec<Text>, String> {
    let mut opaque = Frame { w: frame.w, h: frame.h, pixels: frame.pixels.clone() };
    for pixel in opaque.pixels.as_chunks_mut::<4>().0 {
        pixel[3] = 255;
    }
    let image = image_of(&opaque).ok_or("could not hand the picture to Vision")?;
    autoreleasepool(|_| unsafe {
        let request: *mut AnyObject = msg_send![class!(VNRecognizeTextRequest), new];
        if request.is_null() {
            return Err("Vision is not available".to_string());
        }
        let _: () = msg_send![request, setRecognitionLevel: 0isize];
        let _: () = msg_send![request, setUsesLanguageCorrection: true];
        let options: *mut AnyObject = msg_send![class!(NSDictionary), dictionary];
        let handler: *mut AnyObject = msg_send![class!(VNImageRequestHandler), alloc];
        let handler: *mut AnyObject = msg_send![handler, initWithCGImage: image.0 as *mut AnyObject, options: options];
        let requests: *mut AnyObject = msg_send![class!(NSArray), arrayWithObject: request];
        let mut error: *mut AnyObject = null_mut();
        let done: bool = msg_send![handler, performRequests: requests, error: &mut error];
        let _: () = msg_send![handler, release];
        if !done {
            let _: () = msg_send![request, release];
            return Err("Vision could not read the picture".to_string());
        }
        let results: *mut AnyObject = msg_send![request, results];
        let count: usize = if results.is_null() { 0 } else { msg_send![results, count] };
        let mut out = Vec::new();
        for at in 0..count.min(MAX_TEXT_LINES) {
            let observation: *mut AnyObject = msg_send![results, objectAtIndex: at];
            let candidates: *mut AnyObject = msg_send![observation, topCandidates: 1usize];
            let best: *mut AnyObject = msg_send![candidates, firstObject];
            if best.is_null() {
                continue;
            }
            let string: *mut AnyObject = msg_send![best, string];
            let utf8: *const c_char = msg_send![string, UTF8String];
            if utf8.is_null() {
                continue;
            }
            let text = CStr::from_ptr(utf8).to_string_lossy().into_owned();
            // Vision measures from the bottom left, as a share of the picture.
            let bounds: CGRect = msg_send![observation, boundingBox];
            let (w, h) = (frame.w as f64, frame.h as f64);
            let rect = (
                (bounds.origin.x * w).round() as i32,
                ((1.0 - bounds.origin.y - bounds.size.height) * h).round() as i32,
                (bounds.size.width * w).round() as i32,
                (bounds.size.height * h).round() as i32,
            );
            if !text.trim().is_empty() {
                out.push(Text { text, rect });
            }
        }
        let _: () = msg_send![request, release];
        Ok(out)
    })
}
