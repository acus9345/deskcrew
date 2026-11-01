// The pictures: a cursor with its name beside it, and the outline a press leaves on a control.
use crate::motion::Rgb;
use tiny_skia::{Color, FillRule, LineJoin, Paint, Path, PathBuilder, Pixmap, Stroke, Transform};

/// The arrow's corners and how much each is rounded, in pixels on a 100% screen. The tip is at 0,0.
const ARROW: [(f32, f32); 4] = [(0.0, 0.0), (41.0, 24.0), (20.0, 28.5), (6.5, 46.0)];
const ARROW_ROUNDING: [f32; 4] = [4.5, 4.5, 3.0, 4.5];
/// Where the rounded tip really ends up, which is where a press lands.
const TIP: (f32, f32) = (1.7, 1.5);
const ARROW_RIM: f32 = 1.7;
const LABEL_AT: (f32, f32) = (25.0, 38.0);
const LABEL_PAD: f32 = 12.0;
const LABEL_HEIGHT_EXTRA: f32 = 9.0;
const LABEL_EDGE_ALPHA: f32 = 0.16;
const SHADOW_ROOM: f32 = 12.0;
const SHADOW_DROP: f32 = 2.5;
const SHADOW_BLUR: f32 = 3.0;
const SHADOW_OPACITY: f32 = 0.34;
const OUTLINE_FILL: f32 = 0.18;
const OUTLINE_LINE: f32 = 2.0;
const OUTLINE_ALPHA: f32 = 0.92;
const OUTLINE_ROUNDING: f32 = 7.0;

fn paint(colour: Rgb, alpha: f32) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color(Color::from_rgba8(colour.0, colour.1, colour.2, (alpha * 255.0) as u8));
    paint.anti_alias = true;
    paint
}

fn rounded(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<Path> {
    let (right, bottom) = (x + w, y + h);
    let k = r * 0.45;
    let mut p = PathBuilder::new();
    p.move_to(x + r, y);
    p.line_to(right - r, y);
    p.cubic_to(right - k, y, right, y + k, right, y + r);
    p.line_to(right, bottom - r);
    p.cubic_to(right, bottom - k, right - k, bottom, right - r, bottom);
    p.line_to(x + r, bottom);
    p.cubic_to(x + k, bottom, x, bottom - k, x, bottom - r);
    p.line_to(x, y + r);
    p.cubic_to(x, y + k, x + k, y, x + r, y);
    p.close();
    p.finish()
}

/// The arrow, with every corner softened, so it reads as a shape and not a sliver.
fn arrow(x: f32, y: f32, s: f32) -> Option<Path> {
    let corner = |i: usize| (x + ARROW[i % 4].0 * s, y + ARROW[i % 4].1 * s);
    let toward = |from: (f32, f32), to: (f32, f32), distance: f32| {
        let length = (to.0 - from.0).hypot(to.1 - from.1);
        (from.0 + (to.0 - from.0) * distance / length, from.1 + (to.1 - from.1) * distance / length)
    };
    let mut p = PathBuilder::new();
    for i in 0..4 {
        let at = corner(i);
        let rounding = ARROW_ROUNDING[i] * s;
        let (enter, leave) = (toward(at, corner(i + 3), rounding), toward(at, corner(i + 1), rounding));
        if i == 0 {
            p.move_to(enter.0, enter.1);
        } else {
            p.line_to(enter.0, enter.1);
        }
        p.quad_to(at.0, at.1, leave.0, leave.1);
    }
    p.close();
    p.finish()
}

/// Spread the canvas's coverage out into a soft shadow, three box blurs being close to a true blur.
fn soften(canvas: &mut Pixmap, radius: usize, opacity: f32) {
    let (w, h) = (canvas.width() as usize, canvas.height() as usize);
    let mut alpha: Vec<u32> = canvas.data().chunks_exact(4).map(|p| p[3] as u32).collect();
    let mut next = vec![0u32; alpha.len()];
    let span = (radius * 2 + 1) as u32;
    for _ in 0..3 {
        for (lines, step, length) in [(h, w, w), (w, 1, h)] {
            let across = if step == 1 { w } else { 1 };
            for line in 0..lines {
                let start = line * if step == 1 { 1 } else { w };
                let along = |i: isize| start + (i.clamp(0, length as isize - 1) as usize) * if step == 1 { across } else { 1 };
                let mut total: u32 = (-(radius as isize)..=radius as isize).map(|i| alpha[along(i)]).sum();
                for i in 0..length as isize {
                    next[along(i)] = total / span;
                    total += alpha[along(i + radius as isize + 1)];
                    total -= alpha[along(i - radius as isize)];
                }
            }
            std::mem::swap(&mut alpha, &mut next);
        }
    }
    for (pixel, value) in canvas.data_mut().chunks_exact_mut(4).zip(alpha) {
        pixel[..3].fill(0);
        pixel[3] = (value as f32 * opacity) as u8;
    }
}

/// Text coverage from GDI, so any installed font and script draws: one byte per pixel.
pub struct Mask {
    pub w: i32,
    pub h: i32,
    pub alpha: Vec<u8>,
}

fn stamp(canvas: &mut Pixmap, mask: &Mask, left: i32, top: i32) {
    let width = canvas.width() as i32;
    let data = canvas.data_mut();
    for y in 0..mask.h {
        for x in 0..mask.w {
            let alpha = mask.alpha[(y * mask.w + x) as usize] as u32;
            if alpha == 0 {
                continue;
            }
            let at = (((top + y) * width + left + x) * 4) as usize;
            for channel in 0..4 {
                let under = data[at + channel] as u32;
                data[at + channel] = (alpha + under * (255 - alpha) / 255) as u8;
            }
        }
    }
}

/// The cursor and its label, and where in the picture the tip sits.
pub fn cursor(colour: Rgb, label: &Mask, s: f32) -> Option<(Pixmap, i32, i32)> {
    let room = (SHADOW_ROOM * s).ceil();
    let pad = (LABEL_PAD * s).round();
    let pill_h = (label.h as f32 + LABEL_HEIGHT_EXTRA * s).round();
    let pill_w = label.w as f32 + pad * 2.0;
    let (pill_x, pill_y) = (room + LABEL_AT.0 * s, room + LABEL_AT.1 * s);
    let width = (pill_x + pill_w).max(room + ARROW[1].0 * s) + room;
    let height = (pill_y + pill_h).max(room + ARROW[3].1 * s) + room;
    let mut canvas = Pixmap::new(width.ceil() as u32, height.ceil() as u32)?;

    let pointer = arrow(room, room, s)?;
    let pill = rounded(pill_x, pill_y, pill_w, pill_h, pill_h / 2.0)?;
    let black = paint((0, 0, 0), 1.0);
    let drop = Transform::from_translate(0.0, SHADOW_DROP * s);
    canvas.fill_path(&pointer, &black, FillRule::Winding, drop, None);
    canvas.fill_path(&pill, &black, FillRule::Winding, drop, None);
    soften(&mut canvas, (SHADOW_BLUR * s).round() as usize, SHADOW_OPACITY);

    canvas.fill_path(&pointer, &paint(colour, 1.0), FillRule::Winding, Transform::identity(), None);
    let dark = (colour.0 / 7, colour.1 / 7, colour.2 / 7);
    let rim = Stroke { width: ARROW_RIM * s, line_join: LineJoin::Round, ..Stroke::default() };
    canvas.stroke_path(&pointer, &paint(dark, 1.0), &rim, Transform::identity(), None);

    canvas.fill_path(&pill, &black, FillRule::Winding, Transform::identity(), None);
    // Black on a dark window would lose its edge, so it keeps a faint one.
    let edge = Stroke { width: s.max(1.0), ..Stroke::default() };
    canvas.stroke_path(&pill, &paint((255, 255, 255), LABEL_EDGE_ALPHA), &edge, Transform::identity(), None);
    stamp(&mut canvas, label, (pill_x + pad) as i32, (pill_y + (pill_h - label.h as f32) / 2.0) as i32);
    Some((canvas, (room + TIP.0 * s) as i32, (room + TIP.1 * s) as i32))
}

/// How far the outline's picture reaches past the control on each side.
pub fn outline_margin(s: f32) -> i32 {
    (OUTLINE_LINE * s).ceil() as i32
}

pub fn outline(colour: Rgb, w: i32, h: i32, s: f32) -> Option<Pixmap> {
    let margin = outline_margin(s);
    let mut canvas = Pixmap::new((w + margin * 2) as u32, (h + margin * 2) as u32)?;
    let inset = OUTLINE_LINE * s / 2.0;
    let (width, height) = (canvas.width() as f32 - inset * 2.0, canvas.height() as f32 - inset * 2.0);
    let radius = (OUTLINE_ROUNDING * s).min(height * 0.25);
    let box_path = rounded(inset, inset, width, height, radius)?;
    canvas.fill_path(&box_path, &paint(colour, OUTLINE_FILL), FillRule::Winding, Transform::identity(), None);
    let line = Stroke { width: OUTLINE_LINE * s, ..Stroke::default() };
    canvas.stroke_path(&box_path, &paint(colour, OUTLINE_ALPHA), &line, Transform::identity(), None);
    Some(canvas)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(canvas: &Pixmap, x: u32, y: u32) -> (u8, u8, u8, u8) {
        let p = canvas.pixel(x, y).unwrap();
        (p.red(), p.green(), p.blue(), p.alpha())
    }

    #[test]
    fn the_outline_is_solid_at_the_edge_and_see_through_inside() {
        let canvas = outline((240, 48, 110), 200, 100, 1.0).unwrap();
        let (_, _, _, edge) = pixel(&canvas, 103, 1);
        let (r, _, _, inside) = pixel(&canvas, 103, 53);
        assert!(edge >= 220, "edge alpha {edge}");
        assert!((40..=50).contains(&inside), "fill alpha {inside}");
        assert!(r <= inside);
    }

    #[test]
    fn the_cursor_is_drawn_in_its_colour_with_a_dark_rim_and_a_soft_shadow() {
        let label = Mask { w: 30, h: 16, alpha: vec![255; 480] };
        let (canvas, x, y) = cursor((255, 165, 31), &label, 1.0).unwrap();
        assert_eq!((x, y), (13, 13));
        assert_eq!(pixel(&canvas, 26, 30), (255, 165, 31, 255));
        let rim = (0..canvas.width()).map(|x| pixel(&canvas, x, 27)).find(|(_, _, _, a)| *a > 200);
        assert!(rim.is_some_and(|(r, _, _, _)| r < 90), "rim {rim:?}");
        let (_, _, _, shadow) = pixel(&canvas, 22, 62);
        assert!(shadow > 5 && shadow < 120, "shadow {shadow}");
    }

    #[test]
    #[ignore = "writes a picture to look at: set DESKCREW_PREVIEW to a .png path"]
    fn preview() {
        let Some(path) = std::env::var_os("DESKCREW_PREVIEW") else { return };
        let (width, height) = (960u32, 440u32);
        let mut sheet = Pixmap::new(width, height).unwrap();
        sheet.fill(Color::from_rgba8(248, 248, 250, 255));
        let mut dark = PathBuilder::new();
        dark.push_rect(tiny_skia::Rect::from_xywh(480.0, 0.0, 480.0, 440.0).unwrap());
        sheet.fill_path(&dark.finish().unwrap(), &paint((30, 30, 34), 1.0), FillRule::Winding, Transform::identity(), None);
        let spots = [("Claude Code", (255, 165, 31), 40, 30), ("Cursor", (240, 48, 110), 250, 30), ("fx", (21, 101, 240), 40, 170), ("Claude Code", (255, 165, 31), 520, 30), ("Cursor", (240, 48, 110), 730, 30), ("fx", (21, 101, 240), 520, 170)];
        for (name, colour, x, y) in spots {
            let (picture, _, _) = cursor(colour, &crate::overlay::label_for_test(name, 21), 1.5).unwrap();
            sheet.draw_pixmap(x, y, picture.as_ref(), &Default::default(), Transform::identity(), None);
        }
        for (x, colour) in [(250, (240, 48, 110)), (730, (255, 165, 31))] {
            let box_picture = outline(colour, 130, 90, 1.5).unwrap();
            sheet.draw_pixmap(x, 200, box_picture.as_ref(), &Default::default(), Transform::identity(), None);
        }
        let file = std::fs::File::create(path).unwrap();
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header().unwrap().write_image_data(sheet.data()).unwrap();
    }
}
