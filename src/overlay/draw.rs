// The pictures: a cursor with its name beside it, and the outline a press leaves on a control.
use crate::motion::{LEAN_MOST, Rgb};
use tiny_skia::{Color, FillRule, FilterQuality, GradientStop, LineJoin, Paint, Path, PathBuilder, Pixmap, PixmapPaint, Point, PremultipliedColorU8, RadialGradient, SpreadMode, Stroke, Transform};

/// The arrow, in pixels on a 100% screen with the tip at 0,0: a dart pointing up and left with
/// straight sides, a notch in its back and softly rounded corners, ringed in white. Each corner has
/// its rounding radius.
const TIP: ((f32, f32), f32) = ((0.0, 0.0), 0.45);
const RIGHT: ((f32, f32), f32) = ((11.52, 6.94), 0.4);
const NOTCH: ((f32, f32), f32) = ((5.68, 8.09), 0.6);
const BOTTOM: ((f32, f32), f32) = ((1.96, 13.19), 0.4);
/// The white ring around the arrow.
const RING: f32 = 1.4;
/// Where the tag's top left corner sits, from the tip.
const LABEL_AT: (f32, f32) = (10.66, 14.65);
/// The tag's height, the room either side of the name, and its corner radius.
const TAG_H: f32 = 19.2;
const TAG_PAD: f32 = 9.53;
const TAG_ROUND: f32 = 3.64;
/// While typing: the caret's width, and where the tag's bottom left corner sits from the caret's
/// top left, floating above it and just to its left.
#[cfg_attr(not(windows), allow(dead_code))]
const CARET_W: f32 = 2.6;
#[cfg_attr(not(windows), allow(dead_code))]
const CARET_TAG_AT: (f32, f32) = (-1.5, -3.95);
/// The tag over a caret is a little tighter than the one by the arrow: its height, side padding
/// and corner radius.
#[cfg_attr(not(windows), allow(dead_code))]
const CARET_TAG: Tag = Tag { h: 18.2, pad: 6.0, round: 3.5 };
const ARROW_TAG: Tag = Tag { h: TAG_H, pad: TAG_PAD, round: TAG_ROUND };

/// A name tag's height, side padding and corner radius, in pixels on a 100% screen.
struct Tag {
    h: f32,
    pad: f32,
    round: f32,
}
/// Text goes dark on a tag this light or lighter, and white on anything darker, with a touch of
/// the tag's colour in it.
const LIGHT_TAG: f32 = 150.0;
const DARK_TEXT: Rgb = (28, 28, 30);
const TEXT_TINT: f32 = 0.08;
/// Room around the picture for the ring and its anti-aliased edge.
const ROOM: f32 = 3.0;
const OUTLINE_FILL: f32 = 0.18;
const OUTLINE_LINE: f32 = 2.0;
const OUTLINE_ALPHA: f32 = 0.92;
const OUTLINE_ROUNDING: f32 = 7.0;
const OUTLINE_THIN: f32 = 0.9;
const RISE_MS: f32 = 110.0;
const TINT_MS: f32 = 140.0;
const GLOW_ARRIVE_MS: f32 = 35.0;
const GLOW_PEAK: f32 = 0.62;
/// When the press has finished moving and only the settled outline is left.
pub const PRESS_SETTLE_MS: f32 = 320.0;

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

fn unit(from: (f32, f32), to: (f32, f32)) -> (f32, f32) {
    let length = (to.0 - from.0).hypot(to.1 - from.1);
    ((to.0 - from.0) / length, (to.1 - from.1) / length)
}

/// The arrow's corners in order, with their radii.
fn outline() -> Vec<((f32, f32), f32)> {
    vec![TIP, RIGHT, NOTCH, BOTTOM]
}

/// The arrow, with every corner cut to a true circular arc. Trimming by a fixed length barely
/// rounds a sharp point, because how far a circle sits back depends on how sharp the point is.
fn arrow(x: f32, y: f32, s: f32) -> Option<Path> {
    let points = outline();
    let count = points.len();
    let to = |point: (f32, f32)| (x + point.0 * s, y + point.1 * s);
    let mut p = PathBuilder::new();
    for (i, &(at, radius)) in points.iter().enumerate() {
        if radius == 0.0 {
            let (px, py) = to(at);
            if i == 0 {
                p.move_to(px, py);
            } else {
                p.line_to(px, py);
            }
            continue;
        }
        let (before, after) = (points[(i + count - 1) % count].0, points[(i + 1) % count].0);
        let (back, on) = (unit(at, before), unit(at, after));
        let angle = (back.0 * on.0 + back.1 * on.1).clamp(-1.0, 1.0).acos();
        let reach = radius / (angle / 2.0).tan();
        let handle = reach - 4.0 / 3.0 * ((std::f32::consts::PI - angle) / 4.0).tan() * radius;
        let along = |direction: (f32, f32), distance: f32| to((at.0 + direction.0 * distance, at.1 + direction.1 * distance));
        let (enter, leave) = (along(back, reach), along(on, reach));
        let (pull_in, pull_out) = (along(back, handle), along(on, handle));
        if i == 0 {
            p.move_to(enter.0, enter.1);
        } else {
            p.line_to(enter.0, enter.1);
        }
        p.cubic_to(pull_in.0, pull_in.1, pull_out.0, pull_out.1, leave.0, leave.1);
    }
    p.close();
    p.finish()
}

/// Where the rounded tip really ends up, which is where a press lands.
fn tip() -> (f32, f32) {
    let (a, b) = (unit(TIP.0, BOTTOM.0), unit(TIP.0, RIGHT.0));
    let angle = (a.0 * b.0 + a.1 * b.1).acos();
    let middle = unit((0.0, 0.0), (a.0 + b.0, a.1 + b.1));
    let reach = TIP.1 * (1.0 / (angle / 2.0).sin() - 1.0);
    (TIP.0.0 + middle.0 * reach, TIP.0.1 + middle.1 * reach)
}

/// Text coverage from GDI, so any installed font and script draws: one byte per pixel.
pub struct Mask {
    pub w: i32,
    pub h: i32,
    pub alpha: Vec<u8>,
}

/// The text as a picture of one colour, its coverage from the mask.
fn ink(mask: &Mask, colour: Rgb) -> Option<Pixmap> {
    let mut text = Pixmap::new(mask.w.max(1) as u32, mask.h.max(1) as u32)?;
    for (pixel, &alpha) in text.pixels_mut().iter_mut().zip(&mask.alpha) {
        let part = |c: u8| ((c as u32 * alpha as u32 + 127) / 255) as u8;
        *pixel = PremultipliedColorU8::from_rgba(part(colour.0), part(colour.1), part(colour.2), alpha)?;
    }
    Some(text)
}

/// White text on a dark tag, dark text on a light one.
fn text_colour(tag: Rgb) -> Rgb {
    let light = 0.299 * tag.0 as f32 + 0.587 * tag.1 as f32 + 0.114 * tag.2 as f32;
    let tint = |c: u8| (255.0 - (255.0 - c as f32) * TEXT_TINT).round() as u8;
    if light >= LIGHT_TAG { DARK_TEXT } else { (tint(tag.0), tint(tag.1), tint(tag.2)) }
}


/// The cursor and its label, and where in the picture the tip sits.
#[cfg_attr(not(test), allow(dead_code))]
pub fn cursor(colour: Rgb, label: &Mask, s: f32) -> Option<(Pixmap, i32, i32)> {
    posed(colour, label, s, 0.0, 1.0)
}

/// The cursor with its arrow leaning `lean` degrees clockwise about the tip, and its tag drawn at
/// `squeeze` of its size toward its top left corner, the corner nearest the arrow.
pub fn posed(colour: Rgb, label: &Mask, s: f32, lean: f32, squeeze: f32) -> Option<(Pixmap, i32, i32)> {
    let room = (ROOM * s).ceil();
    // Room for the arrow at its furthest lean either way, so the picture's size and the tip's place
    // in it never change.
    let points = outline();
    let (mut left, mut top, mut right, mut bottom) = (0.0_f32, 0.0_f32, 0.0_f32, 0.0_f32);
    for step in -6..=6 {
        let (sin, cos) = (LEAN_MOST * step as f32 / 6.0).to_radians().sin_cos();
        for &((x, y), _) in &points {
            let (turned_x, turned_y) = (x * cos - y * sin, x * sin + y * cos);
            (left, top) = (left.min(turned_x), top.min(turned_y));
            (right, bottom) = (right.max(turned_x), bottom.max(turned_y));
        }
    }
    let (tip_x, tip_y) = ((room - left * s).ceil(), (room - top * s).ceil());
    let tag_h = (TAG_H * s).round();
    let pad = (TAG_PAD * s).round();
    let tag_w = label.w as f32 + pad * 2.0;
    let (tag_x, tag_y) = ((tip_x + LABEL_AT.0 * s).round(), (tip_y + LABEL_AT.1 * s).round());
    let width = (tag_x + tag_w).max(tip_x + right * s) + room;
    let height = (tag_y + tag_h).max(tip_y + bottom * s) + room;
    let mut canvas = Pixmap::new(width.ceil() as u32, height.ceil() as u32)?;

    let pointer = arrow(tip_x, tip_y, s)?;
    let turn = Transform::from_rotate_at(lean, tip_x, tip_y);
    let round = |width: f32| Stroke { width, line_join: LineJoin::Round, ..Stroke::default() };
    canvas.stroke_path(&pointer, &paint((255, 255, 255), 1.0), &round(RING * s * 2.0), turn, None);
    canvas.fill_path(&pointer, &paint(colour, 1.0), FillRule::Winding, turn, None);

    name_tag(&mut canvas, &ARROW_TAG, colour, label, s, (tag_x, tag_y), ((tag_x, tag_y), squeeze))?;
    let point = tip();
    Some((canvas, (tip_x + point.0 * s).round() as i32, (tip_y + point.1 * s).round() as i32))
}

/// The name tag with its top left at `at`, drawn at `squeeze` of its size toward `toward`.
fn name_tag(canvas: &mut Pixmap, style: &Tag, colour: Rgb, label: &Mask, s: f32, at: (f32, f32), (toward, squeeze): ((f32, f32), f32)) -> Option<()> {
    let (tag_h, pad) = ((style.h * s).round(), (style.pad * s).round());
    let tag_w = label.w as f32 + pad * 2.0;
    let shrink = Transform::from_row(squeeze, 0.0, 0.0, squeeze, toward.0 * (1.0 - squeeze), toward.1 * (1.0 - squeeze));
    let tag = rounded(at.0, at.1, tag_w, tag_h, style.round * s)?;
    canvas.fill_path(&tag, &paint(colour, 1.0), FillRule::Winding, shrink, None);
    let text = ink(label, text_colour(colour))?;
    let quality = if squeeze == 1.0 { FilterQuality::Nearest } else { FilterQuality::Bilinear };
    let place = shrink.pre_translate(at.0 + pad, (at.1 + (tag_h - label.h as f32) / 2.0).round());
    canvas.draw_pixmap(0, 0, text.as_ref(), &PixmapPaint { quality, ..PixmapPaint::default() }, place, None);
    Some(())
}

/// While typing: a caret of the agent's colour, `height` pixels tall, where the text goes in, with
/// the name tag floating above it. Gives where the caret's top left sits in the picture.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn caret(colour: Rgb, label: &Mask, s: f32, height: i32, squeeze: f32) -> Option<(Pixmap, i32, i32)> {
    let room = (ROOM * s).ceil();
    let (tag_h, pad) = ((CARET_TAG.h * s).round(), (CARET_TAG.pad * s).round());
    let tag_w = label.w as f32 + pad * 2.0;
    let bar_w = (CARET_W * s).round().max(2.0);
    let bar_x = (room + (-CARET_TAG_AT.0 * s).max(0.0)).round();
    let bar_y = (room + tag_h - CARET_TAG_AT.1 * s).round();
    let (tag_x, tag_y) = ((bar_x + CARET_TAG_AT.0 * s).round(), (bar_y + CARET_TAG_AT.1 * s - tag_h).round());
    let width = (tag_x + tag_w).max(bar_x + bar_w) + room;
    let mut canvas = Pixmap::new(width.ceil() as u32, (bar_y + height.max(1) as f32 + room).ceil() as u32)?;
    let bar = tiny_skia::Rect::from_xywh(bar_x, bar_y, bar_w, height.max(1) as f32)?;
    canvas.fill_rect(bar, &paint(colour, 1.0), Transform::identity(), None);
    name_tag(&mut canvas, &CARET_TAG, colour, label, s, (tag_x, tag_y), ((tag_x, tag_y + tag_h), squeeze))?;
    Some((canvas, bar_x as i32, bar_y as i32))
}

/// How far the outline's picture reaches past the control on each side.
pub fn outline_margin(s: f32) -> i32 {
    (OUTLINE_LINE * s).ceil() as i32
}

/// A control `ms` after being pressed: a glow bursts from its middle and spreads to the edges while
/// a thin outline fades in and thickens, leaving the outline and a light tint.
pub fn press(colour: Rgb, w: i32, h: i32, s: f32, ms: f32) -> Option<Pixmap> {
    let margin = outline_margin(s);
    let mut canvas = Pixmap::new((w + margin * 2) as u32, (h + margin * 2) as u32)?;
    let inset = OUTLINE_LINE * s / 2.0;
    let (width, height) = (canvas.width() as f32 - inset * 2.0, canvas.height() as f32 - inset * 2.0);
    let radius = (OUTLINE_ROUNDING * s).min(height * 0.25);
    let box_path = rounded(inset, inset, width, height, radius)?;
    let rise = (ms / RISE_MS).clamp(0.0, 1.0);
    let rise = rise * rise * (3.0 - 2.0 * rise);

    let tint = OUTLINE_FILL * (ms / TINT_MS).clamp(0.0, 1.0);
    canvas.fill_path(&box_path, &paint(colour, tint), FillRule::Winding, Transform::identity(), None);

    let spread = (ms / PRESS_SETTLE_MS).clamp(0.0, 1.0);
    if spread < 1.0 {
        let strength = GLOW_PEAK * (1.0 - spread).powf(1.4) * (ms / GLOW_ARRIVE_MS).clamp(0.0, 1.0);
        let outward = 1.0 - (1.0 - spread) * (1.0 - spread);
        let reach = width.hypot(height) / 2.0 * (0.2 + 0.95 * outward);
        let middle = Point::from_xy(canvas.width() as f32 / 2.0, canvas.height() as f32 / 2.0);
        let tone = |alpha: f32| Color::from_rgba8(colour.0, colour.1, colour.2, (alpha * 255.0) as u8);
        let stops = vec![GradientStop::new(0.0, tone(strength)), GradientStop::new(0.5, tone(strength * 0.55)), GradientStop::new(1.0, tone(0.0))];
        if let Some(shader) = RadialGradient::new(middle, middle, reach, stops, SpreadMode::Pad, Transform::identity()) {
            let glow = Paint { shader, anti_alias: true, ..Paint::default() };
            canvas.fill_path(&box_path, &glow, FillRule::Winding, Transform::identity(), None);
        }
    }

    let line = Stroke { width: (OUTLINE_THIN + (OUTLINE_LINE - OUTLINE_THIN) * rise) * s, ..Stroke::default() };
    canvas.stroke_path(&box_path, &paint(colour, OUTLINE_ALPHA * rise), &line, Transform::identity(), None);
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
        let canvas = press((240, 48, 110), 200, 100, 1.0, PRESS_SETTLE_MS).unwrap();
        let (_, _, _, edge) = pixel(&canvas, 103, 1);
        let (r, _, _, inside) = pixel(&canvas, 103, 53);
        assert!(edge >= 220, "edge alpha {edge}");
        assert!((40..=50).contains(&inside), "fill alpha {inside}");
        assert!(r <= inside);
    }

    #[test]
    fn a_press_starts_with_a_bright_middle_and_a_thin_faint_edge_then_settles() {
        let early = press((240, 48, 110), 200, 100, 1.0, 40.0).unwrap();
        let settled = press((240, 48, 110), 200, 100, 1.0, PRESS_SETTLE_MS).unwrap();
        assert!(pixel(&early, 102, 52).3 > pixel(&settled, 102, 52).3 * 2, "glow");
        assert!(pixel(&early, 102, 1).3 < pixel(&settled, 102, 1).3 / 2, "edge");
    }

    #[test]
    fn the_cursor_is_a_ringed_arrow_over_a_tag_of_its_colour() {
        let label = Mask { w: 30, h: 16, alpha: vec![255; 480] };
        let colour = (208, 111, 89);
        let (canvas, x, y) = cursor(colour, &label, 1.0).unwrap();
        let at = |dx: i32, dy: i32| pixel(&canvas, (x + dx) as u32, (y + dy) as u32);
        // The arrow's colour inside it, white around it, the tag's colour beside the name, the
        // name in near white, and nothing below the tag.
        assert_eq!(at(4, 5), (208, 111, 89, 255));
        let ringed = (0..10).flat_map(|dy| (-3..12).map(move |dx| (dx, dy))).any(|(dx, dy)| at(dx, dy) == (255, 255, 255, 255));
        assert!(ringed, "no white ring");
        assert_eq!(at(14, 20), (208, 111, 89, 255));
        let ink = text_colour(colour);
        assert_eq!(at(27, 23), (ink.0, ink.1, ink.2, 255));
        assert!(ink.0 > 245 && ink.1 > 235 && ink.2 > 235, "text {ink:?}");
        assert_eq!(pixel(&canvas, 1, canvas.height() - 2).3, 0, "no shadow");
        assert_eq!(text_colour((250, 204, 21)), DARK_TEXT);
    }

    #[test]
    fn the_arrow_leans_about_its_tip_and_the_tag_squeezes_toward_it() {
        let label = Mask { w: 30, h: 16, alpha: vec![255; 480] };
        let colour = (208, 111, 89);
        let (still, x, y) = posed(colour, &label, 2.0, 0.0, 1.0).unwrap();
        let (leaning, lx, ly) = posed(colour, &label, 2.0, -LEAN_MOST, 1.0).unwrap();
        let (squeezed, _, _) = posed(colour, &label, 2.0, 0.0, 0.9).unwrap();
        // Same size and the same tip, whatever the pose.
        assert_eq!((still.width(), still.height(), x, y), (leaning.width(), leaning.height(), lx, ly));
        assert_eq!((still.width(), still.height()), (squeezed.width(), squeezed.height()));
        // Leaning back swings the right point up, above where the arrow's top edge was.
        let coloured = |p: &Pixmap, dx: i32, dy: i32| p.pixel((x + dx) as u32, (y + dy) as u32).is_some_and(|c| (c.red(), c.green(), c.blue(), c.alpha()) == (208, 111, 89, 255));
        let above_edge = |p: &Pixmap| (2..26).flat_map(|dx| (-12..0).map(move |up| (dx, up))).filter(|&(dx, up)| coloured(p, dx, (dx as f32 * 0.6) as i32 + up - 2)).count();
        assert_eq!(above_edge(&still), 0);
        assert!(above_edge(&leaning) > 20, "{}", above_edge(&leaning));
        // The tag's far corner pulls in toward the arrow; its near corner stays put.
        let (right, bottom) = (still.width() as i32 - 9, still.height() as i32 - 16);
        assert_eq!(pixel(&still, right as u32, bottom as u32), (208, 111, 89, 255));
        assert_eq!(pixel(&squeezed, right as u32, bottom as u32).3, 0);
        assert!(coloured(&squeezed, 30, 36));
    }

    #[test]
    fn typing_shows_a_caret_with_the_tag_above_it() {
        let label = Mask { w: 30, h: 16, alpha: vec![255; 480] };
        let colour = (208, 111, 89);
        let (picture, x, y) = caret(colour, &label, 1.0, 24, 1.0).unwrap();
        let at = |dx: i32, dy: i32| pixel(&picture, (x + dx) as u32, (y + dy) as u32);
        // The caret runs down from its top left; the tag sits above it, starting just left of it.
        assert_eq!(at(0, 1), (208, 111, 89, 255));
        assert_eq!(at(0, 22), (208, 111, 89, 255));
        assert_eq!(at(4, 12).3, 0, "nothing beside the caret");
        assert_eq!(at(2, -8), (208, 111, 89, 255));
        assert_eq!(at(0, -2).3, 0, "a gap between tag and caret");
    }

    #[test]
    #[ignore = "writes big cursors to compare: set DESKCREW_BIG to a folder"]
    fn big() {
        let Some(folder) = std::env::var_os("DESKCREW_BIG") else { return };
        let scale: f32 = std::env::var("DESKCREW_BIG_SCALE").map_or(6.19, |v| v.parse().unwrap());
        for (name, colour) in [("Claude", (208, 111, 89)), ("Mia", (162, 61, 134))] {
            let label = crate::overlay::label_for_test(name, (11.0 * scale) as i32);
            let (picture, x, y) = cursor(colour, &label, scale).unwrap();
            let path = std::path::Path::new(&folder).join(format!("{name}-{x}-{y}.png"));
            let file = std::fs::File::create(path).unwrap();
            let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), picture.width(), picture.height());
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            // PNG wants straight alpha; the canvas holds it multiplied in.
            let straight: Vec<u8> = picture.pixels().iter().flat_map(|p| {
                let c = p.demultiply();
                [c.red(), c.green(), c.blue(), c.alpha()]
            }).collect();
            encoder.write_header().unwrap().write_image_data(&straight).unwrap();
        }
    }

    #[test]
    #[ignore = "writes frames of a move and some typing: set DESKCREW_FILM to a folder"]
    fn film() {
        use crate::motion::{Pose, along, bow, ease, travel_ms};
        use std::time::{Duration, Instant};
        let Some(folder) = std::env::var_os("DESKCREW_FILM") else { return };
        let scale = 1.5;
        let label = crate::overlay::label_for_test("Claude", (11.0 * scale) as i32);
        let (width, height) = (900u32, 420u32);
        let start = Instant::now();
        let mut pose = Pose::new();
        let mut frame = 0;
        let mut at = (720.0, 300.0);
        let mut shoot = |at: (f32, f32), ms: u64, pose: &mut Pose| {
            let now = start + Duration::from_millis(ms);
            pose.follow(now, at, scale);
            let (lean, squeeze) = pose.shown(now);
            let (picture, x, y) = posed((208, 111, 89), &label, scale, lean, squeeze).unwrap();
            let mut sheet = Pixmap::new(width, height).unwrap();
            sheet.fill(Color::from_rgba8(250, 249, 246, 255));
            sheet.draw_pixmap(at.0 as i32 - x, at.1 as i32 - y, picture.as_ref(), &Default::default(), Transform::identity(), None);
            let path = std::path::Path::new(&folder).join(format!("{frame:04}.png"));
            frame += 1;
            let file = std::fs::File::create(path).unwrap();
            let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.write_header().unwrap().write_image_data(sheet.data()).unwrap();
        };
        let mut ms = 0;
        let mut side = 1.0;
        for to in [(120.0, 120.0), (640.0, 90.0), (300.0, 330.0), (760.0, 260.0)] {
            let (from, travel) = (at, travel_ms(at, to) as u64);
            let middle = bow(from, to, side);
            side = -side;
            let begun = ms;
            while ms <= begun + travel {
                at = along(from, middle, to, ease((ms - begun) as f32 / travel as f32));
                shoot(at, ms, &mut pose);
                ms += 20;
            }
            for _ in 0..12 {
                shoot(at, ms, &mut pose);
                ms += 20;
            }
        }
        pose.type_keys(start + Duration::from_millis(ms), 5);
        for _ in 0..70 {
            shoot(at, ms, &mut pose);
            ms += 20;
        }
    }

    #[test]
    #[ignore = "writes a picture to look at: set DESKCREW_PREVIEW to a .png path"]
    fn preview() {
        let Some(path) = std::env::var_os("DESKCREW_PREVIEW") else { return };
        let (width, height) = (960u32, 590u32);
        let mut sheet = Pixmap::new(width, height).unwrap();
        sheet.fill(Color::from_rgba8(248, 248, 250, 255));
        let mut dark = PathBuilder::new();
        dark.push_rect(tiny_skia::Rect::from_xywh(480.0, 0.0, 480.0, 440.0).unwrap());
        dark.push_rect(tiny_skia::Rect::from_xywh(0.0, 440.0, 960.0, 150.0).unwrap());
        sheet.fill_path(&dark.finish().unwrap(), &paint((30, 30, 34), 1.0), FillRule::Winding, Transform::identity(), None);
        let spots = [("Claude", (208, 111, 89), 40, 30), ("Mia", (162, 61, 134), 250, 30), ("Codex", (45, 190, 96), 40, 170), ("Claude", (208, 111, 89), 520, 30), ("Mia", (162, 61, 134), 730, 30), ("Cline", (250, 204, 21), 520, 170)];
        for (name, colour, x, y) in spots {
            let (picture, _, _) = cursor(colour, &crate::overlay::label_for_test(name, 17), 1.5).unwrap();
            sheet.draw_pixmap(x, y, picture.as_ref(), &Default::default(), Transform::identity(), None);
        }
        for (x, colour) in [(250, (240, 48, 110)), (730, (217, 119, 87))] {
            let box_picture = press(colour, 130, 90, 1.5, PRESS_SETTLE_MS).unwrap();
            sheet.draw_pixmap(x, 200, box_picture.as_ref(), &Default::default(), Transform::identity(), None);
        }
        for (at, ms) in [15.0, 45.0, 80.0, 110.0, 150.0, 200.0, 260.0, 320.0].into_iter().enumerate() {
            let frame = press((240, 48, 110), 105, 105, 1.0, ms).unwrap();
            sheet.draw_pixmap(8 + at as i32 * 118, 460, frame.as_ref(), &Default::default(), Transform::identity(), None);
        }
        let file = std::fs::File::create(path).unwrap();
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header().unwrap().write_image_data(sheet.data()).unwrap();
    }
}
