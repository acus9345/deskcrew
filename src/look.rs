// Telling the agent when the control list misses part of a window, and reading what text it can
// from the picture there, so a screenshot is only needed for what is really just a drawing.
use crate::shot::Frame;

const GRID: i32 = 32;
// An area smaller than this share of the window, or this many pixels on a side, is just a gap.
const LEAST_SHARE: f32 = 0.12;
const LEAST_SIDE: i32 = 80;
// Above this share of the window a container is a canvas, not a control that covers its space.
const CONTAINER_SHARE: f32 = 0.4;
// Below this share of pixels away from the background colour, an area is plain.
const BUSY_SHARE: f32 = 0.01;
#[cfg(windows)]
const MAX_TEXT_LINES: usize = 30;
// Small text reads far better enlarged.
#[cfg(windows)]
const OCR_SMALL: u32 = 900;

/// The largest part of the window, in screen pixels, that no control on the list sits in.
/// `rects` holds each control's box and whether it is a container that may just be a canvas.
type Box4 = (i32, i32, i32, i32);

pub fn empty_area(window: Box4, rects: &[(Box4, bool)]) -> Option<Box4> {
    let (wx, wy, ww, wh) = window;
    if ww < LEAST_SIDE || wh < LEAST_SIDE {
        return None;
    }
    let (cw, ch) = (ww as f32 / GRID as f32, wh as f32 / GRID as f32);
    let mut covered = vec![[false; GRID as usize]; GRID as usize];
    let whole = (ww * wh) as f32;
    for &((x, y, w, h), container) in rects {
        if w <= 0 || h <= 0 || (container && (w * h) as f32 > whole * CONTAINER_SHARE) {
            continue;
        }
        let left = (((x - wx) as f32 / cw).floor() as i32).clamp(0, GRID);
        let right = (((x + w - wx) as f32 / cw).ceil() as i32).clamp(0, GRID);
        let top = (((y - wy) as f32 / ch).floor() as i32).clamp(0, GRID);
        let bottom = (((y + h - wy) as f32 / ch).ceil() as i32).clamp(0, GRID);
        for row in covered.iter_mut().take(bottom as usize).skip(top as usize) {
            for cell in row.iter_mut().take(right as usize).skip(left as usize) {
                *cell = true;
            }
        }
    }
    // The largest rectangle of free cells, by heights of free columns row after row.
    let mut heights = [0i32; GRID as usize];
    let mut best = (0, (0, 0, 0, 0));
    for (row, cells) in covered.iter().enumerate() {
        for (column, height) in heights.iter_mut().enumerate() {
            *height = if cells[column] { 0 } else { *height + 1 };
        }
        for start in 0..GRID as usize {
            let mut lowest = i32::MAX;
            for (end, height) in heights.iter().enumerate().skip(start) {
                lowest = lowest.min(*height);
                if lowest == 0 {
                    break;
                }
                let area = lowest * (end - start + 1) as i32;
                if area > best.0 {
                    best = (area, (start as i32, row as i32 + 1 - lowest, (end - start + 1) as i32, lowest));
                }
            }
        }
    }
    let (_, (column, row, columns, rows)) = best;
    let x = wx + (column as f32 * cw).round() as i32;
    let y = wy + (row as f32 * ch).round() as i32;
    let w = (columns as f32 * cw).round() as i32;
    let h = (rows as f32 * ch).round() as i32;
    let big = (w * h) as f32 >= whole * LEAST_SHARE && w >= LEAST_SIDE && h >= LEAST_SIDE;
    big.then_some((x, y, w, h))
}

impl Frame {
    /// Whether anything is drawn here at all, or it is one background colour.
    pub fn busy(&self) -> bool {
        let pixels = self.pixels.as_chunks::<4>().0;
        if pixels.is_empty() {
            return false;
        }
        let mut counts = std::collections::HashMap::new();
        for pixel in pixels.iter().step_by(7) {
            *counts.entry([pixel[0] >> 3, pixel[1] >> 3, pixel[2] >> 3]).or_insert(0usize) += 1;
        }
        let sampled: usize = counts.values().sum();
        let background = counts.values().copied().max().unwrap_or(0);
        (sampled - background) as f32 > sampled as f32 * BUSY_SHARE
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn enlarge(&self, by: u32) -> Frame {
        let (w, h) = (self.w * by, self.h * by);
        let mut pixels = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let at = (((y / by) * self.w + x / by) * 4) as usize;
                pixels.extend_from_slice(&self.pixels[at..at + 4]);
            }
        }
        Frame { w, h, pixels }
    }
}

/// One line of text Windows read off a picture, and its box in that picture's pixels.
pub struct Text {
    pub text: String,
    pub rect: (i32, i32, i32, i32),
}

#[cfg(target_os = "macos")]
pub use crate::mac::ocr::read_text;

/// Read the text in a picture with the OCR built into Windows, in the person's own languages.
#[cfg(windows)]
pub fn read_text(frame: &Frame) -> Result<Vec<Text>, String> {
    use windows::{
        Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap},
        Media::Ocr::OcrEngine,
        Security::Cryptography::CryptographicBuffer,
    };
    let fail = |e: windows::core::Error| format!("Windows could not read text: {}", e.message());
    let engine = OcrEngine::TryCreateFromUserProfileLanguages().map_err(fail)?;
    let most = OcrEngine::MaxImageDimension().map_err(fail)?;
    let by = if frame.w.max(frame.h) < OCR_SMALL && frame.w.max(frame.h) * 2 <= most { 2 } else { 1 };
    let enlarged;
    let mut picture = if by > 1 {
        enlarged = frame.enlarge(by);
        &enlarged
    } else {
        frame
    };
    let shrunk;
    let mut shrink_by = 1.0;
    if picture.w.max(picture.h) > most {
        let (small, scale) = picture.crop(0, 0, picture.w, picture.h).shrink(most);
        shrunk = small;
        picture = &shrunk;
        shrink_by = scale;
    }
    let mut pixels = picture.pixels.clone();
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel[3] = 255;
    }
    let buffer = CryptographicBuffer::CreateFromByteArray(&pixels).map_err(fail)?;
    let bitmap = SoftwareBitmap::CreateCopyFromBuffer(&buffer, BitmapPixelFormat::Bgra8, picture.w as i32, picture.h as i32).map_err(fail)?;
    let result = engine.RecognizeAsync(&bitmap).map_err(fail)?.join().map_err(fail)?;
    let back = |value: f32| (value * shrink_by / by as f32).round() as i32;
    let mut out = Vec::new();
    for line in result.Lines().map_err(fail)? {
        let text = line.Text().map_err(fail)?.to_string();
        let (mut left, mut top, mut right, mut bottom) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for word in line.Words().map_err(fail)? {
            let r = word.BoundingRect().map_err(fail)?;
            left = left.min(r.X);
            top = top.min(r.Y);
            right = right.max(r.X + r.Width);
            bottom = bottom.max(r.Y + r.Height);
        }
        if !text.trim().is_empty() && right > left {
            out.push(Text { text, rect: (back(left), back(top), back(right - left), back(bottom - top)) });
        }
        if out.len() == MAX_TEXT_LINES {
            break;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_full_of_controls_has_no_empty_area() {
        let rects: Vec<_> = (0..10).flat_map(|row| (0..10).map(move |col| ((col * 50, row * 40, 50, 40), false))).collect();
        assert_eq!(empty_area((0, 0, 500, 400), &rects), None);
    }

    #[test]
    fn a_canvas_pane_does_not_count_as_covering() {
        let rects = [((0, 0, 800, 40), false), ((0, 40, 800, 560), true)];
        let (x, y, w, h) = empty_area((0, 0, 800, 600), &rects).unwrap();
        assert!(y >= 40 && x == 0 && w == 800 && h >= 540, "{x},{y} {w}x{h}");
    }

    #[test]
    fn a_small_gap_is_ignored() {
        let rects = [((0, 0, 500, 360), false)];
        assert_eq!(empty_area((0, 0, 500, 400), &rects), None);
    }

    #[test]
    fn a_plain_area_is_not_busy_and_a_drawing_is() {
        let plain = Frame { w: 50, h: 50, pixels: vec![240; 50 * 50 * 4] };
        assert!(!plain.busy());
        let mut drawn = plain.crop(0, 0, 50, 50);
        for at in (0..drawn.pixels.len()).step_by(4 * 9) {
            drawn.pixels[at] = 0;
        }
        assert!(drawn.busy());
    }
}
