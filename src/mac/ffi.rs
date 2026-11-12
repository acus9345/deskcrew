// The C side of macOS that deskcrew needs: Core Foundation, Core Graphics, Core Text and the
// accessibility API. Only the calls used are declared.
#![allow(non_upper_case_globals, non_snake_case, dead_code)]
use objc2::encode::{Encode, Encoding};
use std::ffi::c_void;

pub type CFTypeRef = *const c_void;
pub type CFStringRef = *const c_void;
pub type CFArrayRef = *const c_void;
pub type CFDictionaryRef = *const c_void;
pub type CFAllocatorRef = *const c_void;
pub type AXUIElementRef = *const c_void;
pub type CGImageRef = *const c_void;
pub type CGEventRef = *mut c_void;
pub type CGContextRef = *mut c_void;
pub type CGColorSpaceRef = *const c_void;
pub type CFIndex = isize;
pub type AXError = i32;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CGPoint {
    pub x: f64,
    pub y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CGSize {
    pub width: f64,
    pub height: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CGRect {
    pub origin: CGPoint,
    pub size: CGSize,
}

unsafe impl Encode for CGPoint {
    const ENCODING: Encoding = Encoding::Struct("CGPoint", &[Encoding::Double, Encoding::Double]);
}
unsafe impl Encode for CGSize {
    const ENCODING: Encoding = Encoding::Struct("CGSize", &[Encoding::Double, Encoding::Double]);
}
unsafe impl Encode for CGRect {
    const ENCODING: Encoding = Encoding::Struct("CGRect", &[CGPoint::ENCODING, CGSize::ENCODING]);
}

impl CGRect {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect { origin: CGPoint { x, y }, size: CGSize { width: w, height: h } }
    }
}

#[repr(C)]
pub struct CFArrayCallBacks {
    _private: [usize; 5],
}
#[repr(C)]
pub struct CFDictionaryKeyCallBacks {
    _private: [usize; 6],
}
#[repr(C)]
pub struct CFDictionaryValueCallBacks {
    _private: [usize; 5],
}

pub const kCFStringEncodingUTF8: u32 = 0x0800_0100;
pub const kCFNumberSInt64Type: i32 = 4;
pub const kCFNumberDoubleType: i32 = 13;
pub const kAXValueCGPointType: i32 = 1;
pub const kAXValueCGSizeType: i32 = 2;
pub const kAXErrorSuccess: AXError = 0;

pub const kCGWindowListOptionOnScreenOnly: u32 = 1 << 0;
pub const kCGWindowListOptionIncludingWindow: u32 = 1 << 3;
pub const kCGWindowListExcludeDesktopElements: u32 = 1 << 4;
pub const kCGNullWindowID: u32 = 0;
pub const kCGWindowImageBoundsIgnoreFraming: u32 = 1 << 0;
pub const kCGWindowImageBestResolution: u32 = 1 << 3;
pub const kCGImageAlphaPremultipliedFirst: u32 = 2;
pub const kCGBitmapByteOrder32Little: u32 = 2 << 12;

pub const kCGEventLeftMouseDown: u32 = 1;
pub const kCGEventLeftMouseUp: u32 = 2;
pub const kCGEventRightMouseDown: u32 = 3;
pub const kCGEventRightMouseUp: u32 = 4;
pub const kCGEventMouseMoved: u32 = 5;
pub const kCGEventLeftMouseDragged: u32 = 6;
pub const kCGEventKeyDown: u32 = 10;
pub const kCGEventOtherMouseDown: u32 = 25;
pub const kCGEventOtherMouseUp: u32 = 26;
pub const kCGMouseButtonCenter: u32 = 2;
pub const kCGMouseEventButtonNumber: u32 = 3;
pub const kCGHIDEventTap: u32 = 0;
pub const kCGMouseButtonLeft: u32 = 0;
pub const kCGMouseButtonRight: u32 = 1;
pub const kCGMouseEventClickState: u32 = 1;
pub const kCGKeyboardEventKeycode: u32 = 9;
pub const kCGMouseEventWindowUnderMousePointer: u32 = 91;
pub const kCGMouseEventWindowUnderMousePointerThatCanHandleThisEvent: u32 = 92;
pub const kCGScrollEventUnitLine: u32 = 1;
pub const kCGSessionEventTap: u32 = 1;
pub const kCGHeadInsertEventTap: u32 = 0;
pub const kCGEventTapOptionListenOnly: u32 = 1;
pub const kCGEventFlagMaskShift: u64 = 0x0002_0000;
pub const kCGEventFlagMaskControl: u64 = 0x0004_0000;
pub const kCGEventFlagMaskAlternate: u64 = 0x0008_0000;
pub const kCGEventFlagMaskCommand: u64 = 0x0010_0000;

pub type CGEventTapCallBack = extern "C" fn(proxy: *mut c_void, kind: u32, event: CGEventRef, info: *mut c_void) -> CGEventRef;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    pub static kCFTypeArrayCallBacks: CFArrayCallBacks;
    pub static kCFTypeDictionaryKeyCallBacks: CFDictionaryKeyCallBacks;
    pub static kCFTypeDictionaryValueCallBacks: CFDictionaryValueCallBacks;
    pub static kCFBooleanTrue: CFTypeRef;
    pub static kCFBooleanFalse: CFTypeRef;
    pub static kCFRunLoopCommonModes: CFStringRef;
    pub fn CFRelease(cf: CFTypeRef);
    pub fn CFRetain(cf: CFTypeRef) -> CFTypeRef;
    pub fn CFHash(cf: CFTypeRef) -> usize;
    pub fn CFEqual(a: CFTypeRef, b: CFTypeRef) -> u8;
    pub fn CFGetTypeID(cf: CFTypeRef) -> usize;
    pub fn CFStringGetTypeID() -> usize;
    pub fn CFArrayGetTypeID() -> usize;
    pub fn CFBooleanGetTypeID() -> usize;
    pub fn CFNumberGetTypeID() -> usize;
    pub fn CFStringCreateWithBytes(alloc: CFAllocatorRef, bytes: *const u8, len: CFIndex, encoding: u32, external: u8) -> CFStringRef;
    pub fn CFStringGetLength(s: CFStringRef) -> CFIndex;
    pub fn CFStringGetBytes(
        s: CFStringRef,
        range: CFRange,
        encoding: u32,
        loss: u8,
        external: u8,
        buffer: *mut u8,
        max: CFIndex,
        used: *mut CFIndex,
    ) -> CFIndex;
    pub fn CFArrayCreate(alloc: CFAllocatorRef, values: *const CFTypeRef, count: CFIndex, callbacks: *const CFArrayCallBacks) -> CFArrayRef;
    pub fn CFArrayGetCount(array: CFArrayRef) -> CFIndex;
    pub fn CFArrayGetValueAtIndex(array: CFArrayRef, index: CFIndex) -> CFTypeRef;
    pub fn CFBooleanGetValue(b: CFTypeRef) -> u8;
    pub fn CFNumberGetValue(n: CFTypeRef, kind: i32, out: *mut c_void) -> u8;
    pub fn CFDictionaryGetValue(d: CFDictionaryRef, key: CFTypeRef) -> CFTypeRef;
    pub fn CFDictionaryCreate(
        alloc: CFAllocatorRef,
        keys: *const CFTypeRef,
        values: *const CFTypeRef,
        count: CFIndex,
        key_callbacks: *const CFDictionaryKeyCallBacks,
        value_callbacks: *const CFDictionaryValueCallBacks,
    ) -> CFDictionaryRef;
    pub fn CFDataGetBytePtr(data: CFTypeRef) -> *const u8;
    pub fn CFDataGetLength(data: CFTypeRef) -> CFIndex;
    pub fn CFMachPortCreateRunLoopSource(alloc: CFAllocatorRef, port: *mut c_void, order: CFIndex) -> *mut c_void;
    pub fn CFRunLoopGetMain() -> *mut c_void;
    pub fn CFRunLoopAddSource(run_loop: *mut c_void, source: *mut c_void, mode: CFStringRef);
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CFRange {
    pub location: CFIndex,
    pub length: CFIndex,
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    pub static kAXTrustedCheckOptionPrompt: CFStringRef;
    pub fn AXIsProcessTrusted() -> u8;
    pub fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> u8;
    pub fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    pub fn AXUIElementCopyAttributeValue(element: AXUIElementRef, attribute: CFStringRef, value: *mut CFTypeRef) -> AXError;
    pub fn AXUIElementCopyMultipleAttributeValues(element: AXUIElementRef, attributes: CFArrayRef, options: u32, values: *mut CFArrayRef) -> AXError;
    pub fn AXUIElementSetAttributeValue(element: AXUIElementRef, attribute: CFStringRef, value: CFTypeRef) -> AXError;
    pub fn AXUIElementIsAttributeSettable(element: AXUIElementRef, attribute: CFStringRef, settable: *mut u8) -> AXError;
    pub fn AXUIElementCopyActionNames(element: AXUIElementRef, names: *mut CFArrayRef) -> AXError;
    pub fn AXUIElementPerformAction(element: AXUIElementRef, action: CFStringRef) -> AXError;
    pub fn AXUIElementSetMessagingTimeout(element: AXUIElementRef, seconds: f32) -> AXError;
    pub fn AXUIElementGetTypeID() -> usize;
    pub fn AXValueGetValue(value: CFTypeRef, kind: i32, out: *mut c_void) -> u8;
    pub fn AXValueGetTypeID() -> usize;
    pub fn AXValueCreate(kind: i32, value: *const c_void) -> CFTypeRef;
    // Private, but stable for years and used widely: the window number behind an AX window.
    pub fn _AXUIElementGetWindow(element: AXUIElementRef, window: *mut u32) -> AXError;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    pub static kCGWindowNumber: CFStringRef;
    pub static kCGWindowOwnerPID: CFStringRef;
    pub static kCGWindowOwnerName: CFStringRef;
    pub static kCGWindowName: CFStringRef;
    pub static kCGWindowLayer: CFStringRef;
    pub static kCGWindowBounds: CFStringRef;
    pub fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> CFArrayRef;
    pub fn CGRectMakeWithDictionaryRepresentation(dict: CFDictionaryRef, rect: *mut CGRect) -> bool;
    pub fn CGWindowListCreateImage(bounds: CGRect, option: u32, window: u32, image_option: u32) -> CGImageRef;
    pub fn CGImageGetWidth(image: CGImageRef) -> usize;
    pub fn CGImageGetHeight(image: CGImageRef) -> usize;
    pub fn CGImageRelease(image: CGImageRef);
    pub fn CGImageCreate(
        width: usize,
        height: usize,
        bits_per_component: usize,
        bits_per_pixel: usize,
        bytes_per_row: usize,
        space: CGColorSpaceRef,
        bitmap_info: u32,
        provider: *const c_void,
        decode: *const f64,
        interpolate: bool,
        intent: i32,
    ) -> CGImageRef;
    pub fn CGDataProviderCreateWithCFData(data: CFTypeRef) -> *const c_void;
    pub fn CGDataProviderRelease(provider: *const c_void);
    pub fn CGColorSpaceCreateDeviceRGB() -> CGColorSpaceRef;
    pub fn CGColorSpaceRelease(space: CGColorSpaceRef);
    pub fn CGBitmapContextCreate(
        data: *mut c_void,
        width: usize,
        height: usize,
        bits_per_component: usize,
        bytes_per_row: usize,
        space: CGColorSpaceRef,
        bitmap_info: u32,
    ) -> CGContextRef;
    pub fn CGContextDrawImage(context: CGContextRef, rect: CGRect, image: CGImageRef);
    pub fn CGContextRelease(context: CGContextRef);
    pub fn CGContextSetTextPosition(context: CGContextRef, x: f64, y: f64);
    pub fn CGContextSetRGBFillColor(context: CGContextRef, r: f64, g: f64, b: f64, a: f64);
    pub fn CGPreflightScreenCaptureAccess() -> bool;
    pub fn CGRequestScreenCaptureAccess() -> bool;
    pub fn CGMainDisplayID() -> u32;
    pub fn CGDisplayBounds(display: u32) -> CGRect;
    pub fn CGEventCreateMouseEvent(source: *const c_void, kind: u32, at: CGPoint, button: u32) -> CGEventRef;
    pub fn CGEventCreateKeyboardEvent(source: *const c_void, key: u16, down: bool) -> CGEventRef;
    pub fn CGEventCreateScrollWheelEvent2(source: *const c_void, units: u32, count: u32, wheel1: i32, wheel2: i32, wheel3: i32) -> CGEventRef;
    pub fn CGEventKeyboardSetUnicodeString(event: CGEventRef, length: usize, text: *const u16);
    pub fn CGEventSetFlags(event: CGEventRef, flags: u64);
    pub fn CGEventGetFlags(event: CGEventRef) -> u64;
    pub fn CGEventSetLocation(event: CGEventRef, at: CGPoint);
    pub fn CGEventSetIntegerValueField(event: CGEventRef, field: u32, value: i64);
    pub fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
    pub fn CGEventPostToPid(pid: i32, event: CGEventRef);
    pub fn CGEventPost(tap: u32, event: CGEventRef);
    pub fn CGGetActiveDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
    pub fn CGEventTapCreate(tap: u32, place: u32, options: u32, mask: u64, callback: CGEventTapCallBack, info: *mut c_void) -> *mut c_void;
}

#[link(name = "CoreText", kind = "framework")]
unsafe extern "C" {
    pub static kCTFontAttributeName: CFStringRef;
    pub static kCTForegroundColorFromContextAttributeName: CFStringRef;
    pub fn CTFontCreateWithName(name: CFStringRef, size: f64, matrix: *const std::ffi::c_void) -> CFTypeRef;
    pub fn CTLineCreateWithAttributedString(text: CFTypeRef) -> CFTypeRef;
    pub fn CTLineGetTypographicBounds(line: CFTypeRef, ascent: *mut f64, descent: *mut f64, leading: *mut f64) -> f64;
    pub fn CTLineDraw(line: CFTypeRef, context: CGContextRef);
    pub fn CFAttributedStringCreate(alloc: CFAllocatorRef, text: CFStringRef, attributes: CFDictionaryRef) -> CFTypeRef;
}

#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {}
#[link(name = "Vision", kind = "framework")]
unsafe extern "C" {}

/// A Core Foundation object this code owns, released when dropped.
pub struct Owned(pub CFTypeRef);

// Core Foundation objects are safe to hand between threads; AX calls on them are too.
unsafe impl Send for Owned {}
unsafe impl Sync for Owned {}

impl Owned {
    /// Take ownership of what a Create or Copy call returned. Null gives None.
    pub fn new(cf: CFTypeRef) -> Option<Owned> {
        (!cf.is_null()).then_some(Owned(cf))
    }

    /// Keep an object owned by someone else.
    pub fn retain(cf: CFTypeRef) -> Option<Owned> {
        (!cf.is_null()).then(|| Owned(unsafe { CFRetain(cf) }))
    }
}

impl Clone for Owned {
    fn clone(&self) -> Owned {
        Owned(unsafe { CFRetain(self.0) })
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) };
    }
}

pub fn cf_string(text: &str) -> Owned {
    Owned(unsafe { CFStringCreateWithBytes(std::ptr::null(), text.as_ptr(), text.len() as CFIndex, kCFStringEncodingUTF8, 0) })
}

/// Text from a CFString, or nothing if it is something else.
pub fn string_of(cf: CFTypeRef) -> Option<String> {
    unsafe {
        if cf.is_null() || CFGetTypeID(cf) != CFStringGetTypeID() {
            return None;
        }
        let length = CFStringGetLength(cf);
        let range = CFRange { location: 0, length };
        let mut size = 0;
        CFStringGetBytes(cf, range, kCFStringEncodingUTF8, b'?', 0, std::ptr::null_mut(), 0, &mut size);
        let mut bytes = vec![0u8; size as usize];
        CFStringGetBytes(cf, range, kCFStringEncodingUTF8, b'?', 0, bytes.as_mut_ptr(), size, &mut size);
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }
}

pub fn bool_of(cf: CFTypeRef) -> Option<bool> {
    unsafe { (!cf.is_null() && CFGetTypeID(cf) == CFBooleanGetTypeID()).then(|| CFBooleanGetValue(cf) != 0) }
}

pub fn number_of(cf: CFTypeRef) -> Option<f64> {
    unsafe {
        if cf.is_null() || CFGetTypeID(cf) != CFNumberGetTypeID() {
            return None;
        }
        let mut value = 0.0f64;
        (CFNumberGetValue(cf, kCFNumberDoubleType, (&mut value as *mut f64).cast()) != 0).then_some(value)
    }
}

/// The items of a CFArray, still owned by the array.
pub fn items(array: CFTypeRef) -> Vec<CFTypeRef> {
    unsafe {
        if array.is_null() || CFGetTypeID(array) != CFArrayGetTypeID() {
            return Vec::new();
        }
        (0..CFArrayGetCount(array)).map(|i| CFArrayGetValueAtIndex(array, i)).collect()
    }
}

pub fn array_of(values: &[CFTypeRef]) -> Owned {
    Owned(unsafe { CFArrayCreate(std::ptr::null(), values.as_ptr(), values.len() as CFIndex, &kCFTypeArrayCallBacks) })
}

pub fn dictionary_of(keys: &[CFTypeRef], values: &[CFTypeRef]) -> Owned {
    Owned(unsafe {
        CFDictionaryCreate(std::ptr::null(), keys.as_ptr(), values.as_ptr(), keys.len() as CFIndex, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks)
    })
}
