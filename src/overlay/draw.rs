// The pictures: a cursor with its name beside it, and the outline a press leaves on a control.
use crate::motion::Rgb;
use tiny_skia::{Color, FillRule, LineJoin, Paint, Path, PathBuilder, Pixmap, Stroke, Transform};

const OUTLINE_FILL: f32 = 0.16;

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

/// A wide arrow with its tip on the hot spot and a notch in its tail.
fn arrow(x: f32, y: f32, s: f32) -> Option<Path> {
    let mut p = PathBuilder::new();
    p.move_to(x, y);
    p.line_to(x + 27.0 * s, y + 10.5 * s);
    p.line_to(x + 14.5 * s, y + 14.5 * s);
    p.line_to(x + 10.5 * s, y + 27.0 * s);
    p.close();
    p.finish()
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
    let edge = 3.0 * s;
    let pad = (11.0 * s).round();
    let pill_h = (label.h as f32 + 9.0 * s).round();
    let pill_w = label.w as f32 + pad * 2.0;
    let (pill_x, pill_y) = (edge + 22.0 * s, edge + 27.0 * s);
    let mut canvas = Pixmap::new((pill_x + pill_w + edge).ceil() as u32, (pill_y + pill_h + edge).ceil() as u32)?;

    let pointer = arrow(edge, edge, s)?;
    canvas.fill_path(&pointer, &paint(colour, 1.0), FillRule::Winding, Transform::identity(), None);
    let rim = Stroke { width: 1.1 * s, line_join: LineJoin::Round, ..Stroke::default() };
    canvas.stroke_path(&pointer, &paint((17, 19, 24), 0.8), &rim, Transform::identity(), None);

    let pill = rounded(pill_x, pill_y, pill_w, pill_h, pill_h / 2.0)?;
    canvas.fill_path(&pill, &paint((0, 0, 0), 0.92), FillRule::Winding, Transform::identity(), None);
    stamp(&mut canvas, label, (pill_x + pad) as i32, (pill_y + (pill_h - label.h as f32) / 2.0) as i32);
    Some((canvas, edge as i32, edge as i32))
}

/// How far the outline's picture reaches past the control on each side.
pub fn outline_margin(s: f32) -> i32 {
    (3.0 * s).ceil() as i32
}

pub fn outline(colour: Rgb, w: i32, h: i32, s: f32) -> Option<Pixmap> {
    let margin = outline_margin(s);
    let mut canvas = Pixmap::new((w + margin * 2) as u32, (h + margin * 2) as u32)?;
    let inset = 1.5 * s;
    let (width, height) = (canvas.width() as f32 - inset * 2.0, canvas.height() as f32 - inset * 2.0);
    let radius = (9.0 * s).min(height / 2.0);
    let box_path = rounded(inset, inset, width, height, radius)?;
    canvas.fill_path(&box_path, &paint(colour, OUTLINE_FILL), FillRule::Winding, Transform::identity(), None);
    let line = Stroke { width: 2.4 * s, ..Stroke::default() };
    canvas.stroke_path(&box_path, &paint(colour, 1.0), &line, Transform::identity(), None);
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
        assert_eq!(edge, 255);
        assert!((35..=45).contains(&inside), "fill alpha {inside}");
        assert!(r <= inside);
    }

    #[test]
    fn the_cursor_is_drawn_in_its_colour_from_the_tip() {
        let label = Mask { w: 30, h: 16, alpha: vec![255; 480] };
        let (canvas, x, y) = cursor((255, 165, 31), &label, 1.0).unwrap();
        assert_eq!((x, y), (3, 3));
        let (r, g, b, a) = pixel(&canvas, 13, 11);
        assert_eq!((r, g, b, a), (255, 165, 31, 255));
    }
}
