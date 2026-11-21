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
#[cfg(any(windows, target_os = "linux"))]
const MAX_TEXT_LINES: usize = 30;
// Small text reads far better enlarged.
#[cfg(any(windows, target_os = "linux"))]
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

    #[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
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

/// One line of text read off a picture, and its box in that picture's pixels.
pub struct Text {
    pub text: String,
    pub rect: (i32, i32, i32, i32),
}

#[cfg(target_os = "macos")]
pub use crate::mac::ocr::read_text;

/// Encode a BGRA frame as a binary PPM (P6) image in memory for piping to tesseract.
#[cfg(target_os = "linux")]
fn to_ppm(frame: &Frame) -> Vec<u8> {
    let header = format!("P6\n{} {}\n255\n", frame.w, frame.h);
    let mut out = Vec::with_capacity(header.len() + (frame.w * frame.h * 3) as usize);
    out.extend_from_slice(header.as_bytes());
    for pixel in frame.pixels.as_chunks::<4>().0 {
        out.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
    }
    out
}

/// Group tesseract TSV word rows into lines with union bounding rectangles, scaling back by `by`.
#[cfg(target_os = "linux")]
fn parse_tesseract_tsv(tsv: &str, by: f32) -> Vec<Text> {
    use std::collections::BTreeMap;
    type LineKey = (i32, i32, i32, i32);
    type LineBox = (Vec<String>, i32, i32, i32, i32);
    // Key: (page_num, block_num, par_num, line_num) -> (words, left, top, right, bottom)
    let mut lines: BTreeMap<LineKey, LineBox> = BTreeMap::new();
    for (idx, row) in tsv.lines().enumerate() {
        if idx == 0 {
            continue;
        }
        let cols: Vec<&str> = row.split('\t').collect();
        if cols.len() < 12 {
            continue;
        }
        let level: i32 = match cols[0].parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        if level != 5 {
            continue;
        }
        let word = cols[11].trim();
        if word.is_empty() {
            continue;
        }
        let conf: f32 = cols[10].parse().unwrap_or(-1.0);
        if (0.0..20.0).contains(&conf) {
            continue;
        }
        let (Ok(page), Ok(block), Ok(par), Ok(line)) = (cols[1].parse(), cols[2].parse(), cols[3].parse(), cols[4].parse()) else { continue };
        let (Ok(left), Ok(top), Ok(w), Ok(h)) = (cols[6].parse::<i32>(), cols[7].parse::<i32>(), cols[8].parse::<i32>(), cols[9].parse::<i32>()) else { continue };
        if w <= 0 || h <= 0 {
            continue;
        }
        let entry = lines.entry((page, block, par, line)).or_insert_with(|| (Vec::new(), i32::MAX, i32::MAX, i32::MIN, i32::MIN));
        entry.0.push(word.to_string());
        entry.1 = entry.1.min(left);
        entry.2 = entry.2.min(top);
        entry.3 = entry.3.max(left + w);
        entry.4 = entry.4.max(top + h);
    }
    let back = |value: i32| (value as f32 / by).round() as i32;
    lines
        .into_values()
        .filter(|(words, left, _, right, _)| !words.is_empty() && right > left)
        .take(MAX_TEXT_LINES)
        .map(|(words, left, top, right, bottom)| Text {
            text: words.join(" "),
            rect: (back(left), back(top), back(right - left).max(1), back(bottom - top).max(1)),
        })
        .collect()
}

/// Read the text in a picture on Linux using `tesseract` if installed; returns an empty list if
/// `tesseract` is not present so `see` stays fast and quiet.
#[cfg(target_os = "linux")]
pub fn read_text(frame: &Frame) -> Result<Vec<Text>, String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    if frame.w == 0 || frame.h == 0 {
        return Ok(Vec::new());
    }
    let by = if frame.w.max(frame.h) < OCR_SMALL { 2 } else { 1 };
    let enlarged;
    let picture = if by > 1 {
        enlarged = frame.enlarge(by);
        &enlarged
    } else {
        frame
    };
    let mut child = match Command::new("tesseract")
        .args(["stdin", "stdout", "--psm", "6", "tsv"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("could not run tesseract: {e}")),
    };
    let ppm = to_ppm(picture);
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(&ppm);
    }
    let output = child.wait_with_output().map_err(|e| format!("tesseract failed: {e}"))?;
    if !output.status.success() {
        return Ok(Vec::new());
    }
    let tsv = String::from_utf8_lossy(&output.stdout);
    Ok(parse_tesseract_tsv(&tsv, by as f32))
}

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

    #[cfg(target_os = "linux")]
    #[test]
    fn ppm_and_tesseract_tsv_parse_lines() {
        let frame = Frame { w: 2, h: 1, pixels: vec![10, 20, 30, 255, 40, 50, 60, 255] };
        let ppm = to_ppm(&frame);
        assert_eq!(ppm, b"P6\n2 1\n255\n\x1e\x14\x0a\x3c\x32\x28");

        let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
                   5\t1\t1\t1\t1\t1\t20\t40\t60\t20\t95\tHello\n\
                   5\t1\t1\t1\t1\t2\t90\t40\t70\t20\t92\tWorld\n\
                   5\t1\t1\t1\t2\t1\t20\t80\t40\t20\t10\tNoise\n\
                   5\t1\t1\t1\t3\t1\t20\t120\t80\t20\t88\tSubmit\n";
        let lines = parse_tesseract_tsv(tsv, 2.0);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "Hello World");
        assert_eq!(lines[0].rect, (10, 20, 70, 10));
        assert_eq!(lines[1].text, "Submit");
        assert_eq!(lines[1].rect, (10, 60, 40, 10));
    }
}
