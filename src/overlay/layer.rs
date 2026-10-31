// A click-through window drawn from a picture. It never takes focus and is never hidden from
// capture: a window excluded from capture makes NVIDIA's recorder stop, so this one stays in.
use super::draw::Mask;
use std::{mem::zeroed, ptr::null_mut, sync::Once};
use tiny_skia::Pixmap;
use windows_sys::Win32::{
    Foundation::{HWND, POINT, SIZE},
    Graphics::Gdi::{
        BITMAPINFO, BLENDFUNCTION, CreateCompatibleDC, CreateDIBSection, CreateFontW, DIB_RGB_COLORS, DeleteDC,
        DeleteObject, GdiFlush, GetTextExtentPoint32W, HBITMAP, HDC, SelectObject, SetBkMode, SetTextColor, TextOutW,
    },
    System::LibraryLoader::GetModuleHandleW,
    UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, HWND_TOPMOST, RegisterClassW, SW_HIDE, SW_SHOWNOACTIVATE,
        SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetWindowPos, ShowWindow, ULW_ALPHA, UpdateLayeredWindow, WNDCLASSW,
        WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
    },
};

const CLASS: &[u16] = &[100, 101, 115, 107, 99, 114, 101, 119, 0]; // "deskcrew"
const FONT: &str = "Segoe UI Semibold";

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn register() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        let mut class: WNDCLASSW = zeroed();
        class.lpfnWndProc = Some(DefWindowProcW);
        class.hInstance = GetModuleHandleW(std::ptr::null());
        class.lpszClassName = CLASS.as_ptr();
        RegisterClassW(&class);
    });
}

/// A top-down 32-bit bitmap selected into its own memory context.
struct Surface {
    dc: HDC,
    bitmap: HBITMAP,
    bits: *mut u8,
    w: i32,
    h: i32,
}

impl Surface {
    fn new(w: i32, h: i32) -> Surface {
        unsafe {
            let dc = CreateCompatibleDC(null_mut());
            let mut info: BITMAPINFO = zeroed();
            info.bmiHeader.biSize = size_of_val(&info.bmiHeader) as u32;
            info.bmiHeader.biWidth = w;
            info.bmiHeader.biHeight = -h;
            info.bmiHeader.biPlanes = 1;
            info.bmiHeader.biBitCount = 32;
            let mut bits = null_mut();
            let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            SelectObject(dc, bitmap);
            Surface { dc, bitmap, bits: bits.cast(), w, h }
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            DeleteDC(self.dc);
            DeleteObject(self.bitmap);
        }
    }
}

/// Measure and draw a line of text white on black, and keep the coverage.
pub fn text_mask(text: &str, px: i32) -> Mask {
    let line: Vec<u16> = text.encode_utf16().collect();
    unsafe {
        let font = CreateFontW(-px, 0, 0, 0, 400, 0, 0, 0, 1, 0, 0, 4, 0, wide(FONT).as_ptr());
        let probe = CreateCompatibleDC(null_mut());
        let old = SelectObject(probe, font);
        let mut size: SIZE = zeroed();
        GetTextExtentPoint32W(probe, line.as_ptr(), line.len() as i32, &mut size);
        SelectObject(probe, old);
        DeleteDC(probe);

        let (w, h) = (size.cx.max(1), size.cy.max(1));
        let surface = Surface::new(w, h);
        let old = SelectObject(surface.dc, font);
        SetBkMode(surface.dc, 1);
        SetTextColor(surface.dc, 0x00FF_FFFF);
        TextOutW(surface.dc, 0, 0, line.as_ptr(), line.len() as i32);
        GdiFlush();
        SelectObject(surface.dc, old);
        DeleteObject(font);
        let pixels = std::slice::from_raw_parts(surface.bits, (w * h * 4) as usize);
        Mask { w, h, alpha: pixels.as_chunks::<4>().0.iter().map(|p| p[1]).collect() }
    }
}

pub struct Layer {
    hwnd: HWND,
    surface: Surface,
}

impl Layer {
    pub fn new(picture: &Pixmap) -> Layer {
        register();
        let (w, h) = (picture.width() as i32, picture.height() as i32);
        let surface = Surface::new(w, h);
        // The picture is premultiplied RGBA and the window wants premultiplied BGRA.
        let target = unsafe { std::slice::from_raw_parts_mut(surface.bits, (w * h * 4) as usize) };
        for (to, from) in target.as_chunks_mut::<4>().0.iter_mut().zip(picture.data().as_chunks::<4>().0) {
            *to = [from[2], from[1], from[0], from[3]];
        }
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                CLASS.as_ptr(),
                CLASS.as_ptr(),
                WS_POPUP,
                0,
                0,
                w,
                h,
                null_mut(),
                null_mut(),
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            )
        };
        Layer { hwnd, surface }
    }

    /// Put the window at `x,y` at this opacity. Cheap enough to call every frame.
    pub fn present(&self, x: i32, y: i32, alpha: u8) {
        let at = POINT { x, y };
        let origin = POINT { x: 0, y: 0 };
        let size = SIZE { cx: self.surface.w, cy: self.surface.h };
        let blend = BLENDFUNCTION { BlendOp: 0, BlendFlags: 0, SourceConstantAlpha: alpha, AlphaFormat: 1 };
        unsafe {
            UpdateLayeredWindow(self.hwnd, null_mut(), &at, &size, self.surface.dc, &origin, 0, &blend, ULW_ALPHA);
        }
    }

    pub fn show(&self) {
        unsafe { ShowWindow(self.hwnd, SW_SHOWNOACTIVATE) };
    }

    pub fn hide(&self) {
        unsafe { ShowWindow(self.hwnd, SW_HIDE) };
    }

    /// Back above everything, so cursors stay over the outlines they leave.
    pub fn raise(&self) {
        unsafe { SetWindowPos(self.hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE) };
    }
}

impl Drop for Layer {
    fn drop(&mut self) {
        unsafe { DestroyWindow(self.hwnd) };
    }
}
