// Pictures of one window, taken from the window itself: it can sit behind others, and no overlay
// can ever land in the frame.
#[cfg(windows)]
use crate::win::{self, Hwnd};
use std::{fs, io::BufWriter, path::PathBuf};
#[cfg(windows)]
use std::{mem::zeroed, ptr::null_mut};
#[cfg(windows)]
use windows_sys::Win32::{
    Graphics::Gdi::{
        BITMAPINFO, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits,
        ReleaseDC, SRCCOPY, SelectObject,
    },
    Storage::Xps::PrintWindow,
};

#[cfg(windows)]
const PW_RENDERFULLCONTENT: u32 = 2;
pub const DEFAULT_EDGE: u32 = 1356;
// Blue, green, red, as the pixels are stored.
const MARK: [u8; 3] = [0, 0, 220];
const RULER: [u8; 3] = [200, 90, 0];
const DIGIT_W: i32 = 3;
const DIGIT_H: i32 = 5;
const DIGIT_SIZE: i32 = 2;
const DIGITS: [[u8; 5]; 10] = [
    [7, 5, 5, 5, 7],
    [2, 6, 2, 2, 7],
    [7, 1, 7, 4, 7],
    [7, 1, 7, 1, 7],
    [5, 5, 7, 1, 1],
    [7, 4, 7, 1, 7],
    [7, 4, 7, 5, 7],
    [7, 1, 1, 1, 1],
    [7, 5, 7, 5, 7],
    [7, 5, 7, 1, 7],
];

/// Blue-green-red-alpha pixels, top row first.
pub struct Frame {
    pub w: u32,
    pub h: u32,
    pub pixels: Vec<u8>,
}

/// How an image coordinate maps back to the screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frames {
    pub x: i32,
    pub y: i32,
    pub scale: f32,
}

impl Frames {
    pub fn to_screen(self, x: i32, y: i32) -> (i32, i32) {
        (self.x + (x as f32 * self.scale).round() as i32, self.y + (y as f32 * self.scale).round() as i32)
    }
}

#[cfg(windows)]
fn read_bitmap(dc: *mut core::ffi::c_void, bitmap: *mut core::ffi::c_void, w: i32, h: i32) -> Option<Vec<u8>> {
    unsafe {
        let mut info: BITMAPINFO = zeroed();
        info.bmiHeader.biSize = size_of_val(&info.bmiHeader) as u32;
        info.bmiHeader.biWidth = w;
        info.bmiHeader.biHeight = -h;
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        let lines = GetDIBits(dc, bitmap, 0, h as u32, pixels.as_mut_ptr().cast(), &mut info, DIB_RGB_COLORS);
        (lines == h).then_some(pixels)
    }
}

/// The window's own picture, cropped to the frame a person sees, and the screen pixels per pixel
/// of it: an older app draws itself in its own smaller units, even on a high-DPI screen.
#[cfg(windows)]
pub fn window(hwnd: Hwnd) -> Result<(Frame, f32), String> {
    if win::minimized(hwnd) {
        return Err("that window is minimized: place restore first".into());
    }
    let (ox, oy, ow, oh) = win::outer_rect(hwnd);
    let (x, y, w, h) = win::rect(hwnd);
    if ow <= 0 || oh <= 0 || w <= 0 || h <= 0 {
        return Err("that window has no size".into());
    }
    let full = unsafe {
        let screen = GetDC(null_mut());
        let dc = CreateCompatibleDC(screen);
        let bitmap = CreateCompatibleBitmap(screen, ow, oh);
        let old = SelectObject(dc, bitmap);
        let mut drew = PrintWindow(hwnd as _, dc, PW_RENDERFULLCONTENT) != 0;
        if !drew {
            drew = PrintWindow(hwnd as _, dc, 0) != 0;
        }
        SelectObject(dc, old);
        let pixels = if drew { read_bitmap(dc, bitmap, ow, oh) } else { None };
        DeleteObject(bitmap);
        DeleteDC(dc);
        ReleaseDC(null_mut(), screen);
        pixels
    };
    let full = full.ok_or("Windows would not draw that window")?;
    let scale = win::app_scale(hwnd);
    let units = |value: i32| (value as f32 / scale).round() as usize;
    let (left, top) = (units((x - ox).clamp(0, ow)), units((y - oy).clamp(0, oh)));
    let (w, h) = (units(w).min(ow as usize - left), units(h).min(oh as usize - top));
    let mut pixels = Vec::with_capacity(w * h * 4);
    for row in top..top + h {
        let start = (row * ow as usize + left) * 4;
        pixels.extend_from_slice(&full[start..start + w * 4]);
    }
    Ok((Frame { w: w as u32, h: h as u32, pixels }, scale))
}

/// What is on screen in this rectangle. The caller clears the overlay first.
#[cfg(windows)]
pub fn screen(x: i32, y: i32, w: i32, h: i32) -> Result<Frame, String> {
    let pixels = unsafe {
        let source = GetDC(null_mut());
        let dc = CreateCompatibleDC(source);
        let bitmap = CreateCompatibleBitmap(source, w, h);
        let old = SelectObject(dc, bitmap);
        let copied = BitBlt(dc, 0, 0, w, h, source, x, y, SRCCOPY) != 0;
        SelectObject(dc, old);
        let pixels = if copied { read_bitmap(dc, bitmap, w, h) } else { None };
        DeleteObject(bitmap);
        DeleteDC(dc);
        ReleaseDC(null_mut(), source);
        pixels
    };
    Ok(Frame { w: w as u32, h: h as u32, pixels: pixels.ok_or("Windows would not copy the screen")? })
}

impl Frame {
    /// One colour edge to edge: the app paints itself some way a window copy cannot see.
    pub fn is_blank(&self) -> bool {
        let first = &self.pixels[..3];
        self.pixels.as_chunks::<4>().0.iter().all(|p| &p[..3] == first)
    }

    pub fn crop(&self, x: u32, y: u32, w: u32, h: u32) -> Frame {
        let (x, y) = (x.min(self.w), y.min(self.h));
        let (w, h) = (w.min(self.w - x), h.min(self.h - y));
        let mut pixels = Vec::with_capacity((w * h * 4) as usize);
        for row in y..y + h {
            let start = ((row * self.w + x) * 4) as usize;
            pixels.extend_from_slice(&self.pixels[start..start + (w * 4) as usize]);
        }
        Frame { w, h, pixels }
    }

    /// Shrink by averaging, which keeps small text readable where skipping pixels would not.
    pub fn shrink(self, edge: u32) -> (Frame, f32) {
        let longest = self.w.max(self.h);
        if longest <= edge || edge == 0 {
            return (self, 1.0);
        }
        let scale = longest as f32 / edge as f32;
        let (w, h) = (((self.w as f32 / scale) as u32).max(1), ((self.h as f32 / scale) as u32).max(1));
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            let (top, bottom) = (y * self.h / h, ((y + 1) * self.h / h).max(y * self.h / h + 1).min(self.h));
            for x in 0..w {
                let (left, right) = (x * self.w / w, ((x + 1) * self.w / w).max(x * self.w / w + 1).min(self.w));
                let mut sum = [0u32; 3];
                for sy in top..bottom {
                    let row = (sy * self.w * 4) as usize;
                    for sx in left..right {
                        let at = row + (sx * 4) as usize;
                        for (channel, total) in sum.iter_mut().enumerate() {
                            *total += self.pixels[at + channel] as u32;
                        }
                    }
                }
                let count = (bottom - top) * (right - left);
                let at = ((y * w + x) * 4) as usize;
                for channel in 0..3 {
                    pixels[at + channel] = (sum[channel] / count) as u8;
                }
            }
        }
        (Frame { w, h, pixels }, scale)
    }

    fn dot(&mut self, x: i32, y: i32, colour: [u8; 3]) {
        if x >= 0 && y >= 0 && (x as u32) < self.w && (y as u32) < self.h {
            let at = ((y as u32 * self.w + x as u32) * 4) as usize;
            self.pixels[at..at + 3].copy_from_slice(&colour);
        }
    }

    fn block(&mut self, x: i32, y: i32, w: i32, h: i32, colour: [u8; 3]) {
        for dy in 0..h {
            for dx in 0..w {
                self.dot(x + dx, y + dy, colour);
            }
        }
    }

    /// A box around a control, numbered with its ref.
    pub fn mark(&mut self, x: i32, y: i32, w: i32, h: i32, number: u32) {
        for line in 0..2 {
            self.block(x, y + line, w, 1, MARK);
            self.block(x, y + h - 1 - line, w, 1, MARK);
            self.block(x + line, y, 1, h, MARK);
            self.block(x + w - 1 - line, y, 1, h, MARK);
        }
        self.number(x + 2, y + 2, number, MARK);
    }

    /// Digits on a white tag, big enough to survive being looked at by a model.
    fn number(&mut self, x: i32, y: i32, value: u32, colour: [u8; 3]) {
        let text = value.to_string();
        self.block(x, y, text.len() as i32 * (DIGIT_W + 1) * DIGIT_SIZE + 3, DIGIT_H * DIGIT_SIZE + 4, [255, 255, 255]);
        for (at, digit) in text.bytes().enumerate() {
            let rows = DIGITS[(digit - b'0') as usize];
            for (row, bits) in rows.iter().enumerate() {
                for column in 0..DIGIT_W {
                    if bits & (1 << (DIGIT_W - 1 - column)) != 0 {
                        let left = x + 2 + at as i32 * (DIGIT_W + 1) * DIGIT_SIZE + column * DIGIT_SIZE;
                        self.block(left, y + 2 + row as i32 * DIGIT_SIZE, DIGIT_SIZE, DIGIT_SIZE, colour);
                    }
                }
            }
        }
    }

    /// Lines every 100 image pixels, labelled in the x,y a click on this picture takes.
    pub fn rulers(&mut self) {
        for x in (100..self.w as i32).step_by(100) {
            for y in (0..self.h as i32).step_by(2) {
                self.dot(x, y, RULER);
            }
            self.number(x + 2, 1, x as u32, RULER);
        }
        for y in (100..self.h as i32).step_by(100) {
            for x in (0..self.w as i32).step_by(2) {
                self.dot(x, y, RULER);
            }
            self.number(1, y + 2, y as u32, RULER);
        }
    }

    /// A cheap fingerprint, to tell an unchanged picture without sending it again.
    pub fn hash(&self) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325_u64 ^ ((self.w as u64) << 32 | self.h as u64);
        for pixel in self.pixels.as_chunks::<4>().0 {
            hash ^= u32::from_le_bytes([pixel[0], pixel[1], pixel[2], 0]) as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
        }
        hash
    }

    /// The smallest box around what differs from another frame of the same size.
    pub fn changed_from(&self, other: &Frame) -> Option<(u32, u32, u32, u32)> {
        if (self.w, self.h) != (other.w, other.h) {
            return None;
        }
        let (mut left, mut top, mut right, mut bottom) = (self.w, self.h, 0, 0);
        for y in 0..self.h {
            let row = (y * self.w * 4) as usize;
            for x in 0..self.w {
                let at = row + (x * 4) as usize;
                if self.pixels[at..at + 3] != other.pixels[at..at + 3] {
                    left = left.min(x);
                    right = right.max(x + 1);
                    top = top.min(y);
                    bottom = bottom.max(y + 1);
                }
            }
        }
        (right > left).then(|| (left, top, right - left, bottom - top))
    }

    pub fn save(&self, path: &PathBuf) -> Result<(), String> {
        if let Some(folder) = path.parent() {
            fs::create_dir_all(folder).map_err(|e| e.to_string())?;
        }
        let file = fs::File::create(path).map_err(|e| e.to_string())?;
        let mut encoder = png::Encoder::new(BufWriter::new(file), self.w, self.h);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        let mut rgb = Vec::with_capacity((self.w * self.h * 3) as usize);
        for pixel in self.pixels.as_chunks::<4>().0 {
            rgb.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
        }
        writer.write_image_data(&rgb).map_err(|e| e.to_string())
    }
}

/// Where this agent's picture of this window goes. Each new one replaces the last.
pub fn path_for(agent: &str, window: u32) -> PathBuf {
    let slug: String = agent.chars().map(|c| if c.is_alphanumeric() { c } else { '-' }).collect();
    #[cfg(windows)]
    let root = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    #[cfg(not(windows))]
    let root = std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Library").join("Caches")).unwrap_or_else(std::env::temp_dir);
    root.join("deskcrew").join("shots").join(format!("{slug}-w{window}.png"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, value: u8) -> Frame {
        Frame { w, h, pixels: vec![value; (w * h * 4) as usize] }
    }

    #[test]
    fn shrinking_keeps_the_shape_and_reports_the_scale() {
        let (small, scale) = solid(2560, 1600, 200).shrink(1356);
        assert_eq!((small.w, small.h), (1356, 847));
        assert!((scale - 1.888).abs() < 0.01);
        assert_eq!(small.pixels[0], 200);
    }

    #[test]
    fn a_small_picture_is_left_alone() {
        let (same, scale) = solid(800, 600, 9).shrink(1356);
        assert_eq!((same.w, same.h, scale), (800, 600, 1.0));
    }

    #[test]
    fn image_points_map_back_to_the_screen() {
        let frames = Frames { x: 100, y: 50, scale: 2.0 };
        assert_eq!(frames.to_screen(10, 20), (120, 90));
    }

    #[test]
    fn a_changed_box_is_found_and_an_unchanged_frame_has_none() {
        let before = solid(10, 10, 0);
        let mut after = solid(10, 10, 0);
        assert_eq!(after.changed_from(&before), None);
        let at = ((3 * 10 + 4) * 4) as usize;
        after.pixels[at] = 255;
        assert_eq!(after.changed_from(&before), Some((4, 3, 1, 1)));
        assert_ne!(after.hash(), before.hash());
    }

    #[test]
    fn marks_and_rulers_draw_inside_the_picture() {
        let mut picture = solid(250, 150, 0);
        picture.mark(10, 10, 60, 40, 12);
        picture.mark(240, 140, 60, 40, 3);
        picture.rulers();
        assert!(!picture.is_blank());
        assert_eq!(picture.pixels.len(), 250 * 150 * 4);
    }

    #[test]
    fn a_blank_window_is_noticed() {
        assert!(solid(4, 4, 0).is_blank());
        let mut drawn = solid(4, 4, 0);
        drawn.pixels[20] = 9;
        assert!(!drawn.is_blank());
    }
}
