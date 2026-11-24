// How a drawn cursor gets from one place to another, and what colour it is.
pub type Point = (f32, f32);

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

// How far apart stroke points are, and how fast a stroke is drawn on average, as on qlinter.org.
const STEP_PIXELS: f32 = 6.0;
pub const PIXELS_PER_SECOND: f32 = 504.0;
/// The handwriting rhythm: how much slower a stroke goes per degree of turn per pixel, how much
/// of each end eases in or out, and how much slower the very ends are.
const CURVE_SLOWS: f32 = 22.0;
const RAMP_SHARE: f32 = 0.1;
const RAMP_SLOWS: f32 = 1.4;

/// How fast every kind of typing goes: 600 words a minute, at five letters to a word.
pub const WORDS_PER_MINUTE: u64 = 600;
pub const KEY_EVERY: Duration = Duration::from_millis(60_000 / (WORDS_PER_MINUTE * 5));

/// How long a move takes, as on qlinter.org: twice deskcrew's old 160 ms + 0.4 ms a pixel, at most
/// 1120 ms.
const MIN_TRAVEL_MS: f32 = 320.0;
const MAX_TRAVEL_MS: f32 = 1120.0;
const MS_PER_PIXEL: f32 = 0.8;
/// How far the path's middle bends out, as a share of its length.
const BOW: f32 = 0.2;
/// The speed curve: a quick start, then a long gentle settle onto the target.
const EASE: [f32; 4] = [0.3, 0.0, 0.1, 1.0];
/// The arrow turns toward where it heads, as on qlinter.org: not at all below this speed, fully
/// from this speed, in pixels a second on a 100% screen, at most this far either way, and how
/// quickly it follows, in milliseconds.
const HEAD_FROM: f32 = 40.0;
const HEAD_FULL: f32 = 600.0;
pub const LEAN_MOST: f32 = 160.0;
const LEAN_MS: f32 = 110.0;
/// How quickly the arrow straightens up again: the same as it turns, as on qlinter.org.
const LEAN_BACK_MS: f32 = 110.0;
/// The lean is drawn in steps this fine, in degrees.
const LEAN_STEPS_PER_DEGREE: f32 = 4.0;
/// Where the resting arrow points, from the middle of its back to its tip: up and to the left.
const POINTING: (f32, f32) = (-6.7, -10.0);
/// Each key typed pulls the name tag in a little and lets it spring back: how far, how long the
/// pull takes, how long it holds, how long it takes back, how far apart pulses start, and the most
/// pulses one piece of typing shows.
const PULSE_DEPTH: f32 = 0.055;
const PULSE_IN_MS: f32 = 60.0;
const PULSE_HOLD_MS: f32 = 60.0;
const PULSE_OUT_MS: f32 = 160.0;
const PULSE_EVERY_MS: f32 = 200.0;
const PULSES_MOST: usize = 6;

pub fn travel_ms(from: Point, to: Point) -> u32 {
    let distance = (to.0 - from.0).hypot(to.1 - from.1);
    (MIN_TRAVEL_MS + distance * MS_PER_PIXEL).min(MAX_TRAVEL_MS) as u32
}

/// Starts and stops gently, with no speed jump at either end: gathers speed quickly, then takes
/// its time settling.
pub fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let [x1, y1, x2, y2] = EASE;
    let curve = |a: f32, b: f32, u: f32| 3.0 * (1.0 - u) * (1.0 - u) * u * a + 3.0 * (1.0 - u) * u * u * b + u * u * u;
    // Find where along the curve the time is t, then read its progress there.
    let (mut low, mut high) = (0.0_f32, 1.0_f32);
    for _ in 0..30 {
        let middle = (low + high) / 2.0;
        if curve(x1, x2, middle) < t { low = middle } else { high = middle }
    }
    curve(y1, y2, (low + high) / 2.0)
}

/// How the cursor holds itself: the arrow turns toward where it is heading, and the tag pulses as
/// keys are typed.
pub struct Pose {
    pub lean: f32,
    last: Option<(Instant, Point)>,
    typing: Option<(Instant, usize)>,
}

/// The lean, in degrees clockwise, that turns the arrow toward this velocity in pixels a second:
/// the turn from where it rests to the way it heads, at most LEAN_MOST, in full from HEAD_FULL
/// and in part below it, and none at all under HEAD_FROM.
fn lean_toward(velocity: (f32, f32)) -> f32 {
    let speed = velocity.0.hypot(velocity.1);
    if speed <= HEAD_FROM {
        return 0.0;
    }
    let cross = POINTING.0 * velocity.1 - POINTING.1 * velocity.0;
    let dot = POINTING.0 * velocity.0 + POINTING.1 * velocity.1;
    let turn = cross.atan2(dot).to_degrees();
    turn.clamp(-LEAN_MOST, LEAN_MOST) * (speed / HEAD_FULL).min(1.0)
}

impl Pose {
    pub fn new() -> Pose {
        Pose { lean: 0.0, last: None, typing: None }
    }

    /// Follow the cursor to where it is this frame. True while the pose is still changing.
    pub fn follow(&mut self, now: Instant, at: Point, scale: f32) -> bool {
        if let Some((then, was)) = self.last {
            let seconds = now.duration_since(then).as_secs_f32();
            if seconds > 0.0 {
                let per = seconds * scale.max(1.0);
                let wanted = lean_toward(((at.0 - was.0) / per, (at.1 - was.1) / per));
                let settling = wanted.abs() < self.lean.abs();
                let pace = if settling { LEAN_BACK_MS } else { LEAN_MS };
                self.lean += (wanted - self.lean) * (1.0 - (-seconds * 1000.0 / pace).exp());
                if wanted == 0.0 && self.lean.abs() < 0.05 {
                    self.lean = 0.0;
                }
            }
        }
        self.last = Some((now, at));
        self.lean != 0.0 || self.squeeze(now) < 1.0
    }

    /// Start the tag pulsing for this many keys.
    pub fn type_keys(&mut self, now: Instant, keys: usize) {
        self.typing = Some((now, keys.clamp(1, PULSES_MOST)));
    }

    /// One key typed: one pulse, starting now.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn key(&mut self, now: Instant) {
        // Keys faster than the pulse keep its rhythm instead of holding the tag squeezed.
        let pulsing = self.typing.is_some_and(|(started, _)| now.duration_since(started).as_secs_f32() * 1000.0 < PULSE_EVERY_MS);
        if !pulsing {
            self.typing = Some((now, 1));
        }
    }

    /// Stand straight and forget the last place, after the cursor jumped somewhere new.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn reset(&mut self) {
        self.lean = 0.0;
        self.last = None;
    }

    /// How big the tag is now, 1 at rest.
    pub fn squeeze(&self, now: Instant) -> f32 {
        let Some((started, keys)) = self.typing else { return 1.0 };
        let ms = now.duration_since(started).as_secs_f32() * 1000.0;
        let pull = (0..keys)
            .map(|key| ms - key as f32 * PULSE_EVERY_MS)
            .map(|at| {
                if at < 0.0 {
                    0.0
                } else if at < PULSE_IN_MS {
                    ease(at / PULSE_IN_MS)
                } else if at < PULSE_IN_MS + PULSE_HOLD_MS {
                    1.0
                } else {
                    1.0 - ease((at - PULSE_IN_MS - PULSE_HOLD_MS) / PULSE_OUT_MS)
                }
            })
            .fold(0.0, f32::max);
        1.0 - PULSE_DEPTH * pull
    }

    /// The pose rounded to what shows, so a frame that looks the same is not drawn again.
    pub fn look(&self, now: Instant) -> (i32, i32) {
        ((self.lean * LEAN_STEPS_PER_DEGREE).round() as i32, ((1.0 - self.squeeze(now)) * 400.0).round() as i32)
    }

    /// The pose as drawn: lean in degrees and the tag's size.
    pub fn shown(&self, now: Instant) -> (f32, f32) {
        let (lean, pull) = self.look(now);
        (lean as f32 / LEAN_STEPS_PER_DEGREE, 1.0 - pull as f32 / 400.0)
    }
}

/// The middle of a slightly curved path, so travel never looks ruled.
pub fn bow(from: Point, to: Point, side: f32) -> Point {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    // A mostly sideways move arcs up over what lies between, as on qlinter.org; a mostly upright
    // one bows to the side it is given.
    let side = if dx.abs() > dy.abs() { -dx.signum() } else { side };
    ((from.0 + to.0) / 2.0 - dy * BOW * side, (from.1 + to.1) / 2.0 + dx * BOW * side)
}

/// Where the cursor is at `t` along the curve from `from` to `to`.
pub fn along(from: Point, middle: Point, to: Point, t: f32) -> Point {
    let (p, q) = (1.0 - t, t);
    (
        p * p * from.0 + 2.0 * p * q * middle.0 + q * q * to.0,
        p * p * from.1 + 2.0 * p * q * middle.1 + q * q * to.1,
    )
}

pub type Rgb = (u8, u8, u8);

/// Every colour an agent can have, each unlike the rest. The first ones are the agents' own
/// brands; the rest are for agents without a colour of their own.
const PALETTE: [Rgb; 18] = [
    (217, 119, 87),  // Claude: its orange
    (24, 24, 27),    // Cursor: black
    (49, 134, 255),  // Gemini: its blue
    (16, 163, 127),  // Codex: ChatGPT's green
    (217, 70, 239),  // Copilot: its magenta
    (52, 232, 187),  // Windsurf: its teal
    (30, 48, 96),    // Antigravity: dark with a little blue
    (243, 78, 63),   // Amp: its coral
    (144, 70, 255),  // Kiro: its purple
    (82, 82, 91),    // Grok: charcoal
    (203, 213, 225), // OpenCode: light grey
    (132, 204, 22),
    (67, 56, 202),
    (56, 189, 248),
    (250, 204, 21),
    (161, 98, 60),
    (20, 184, 196),
    (236, 72, 153),
];

/// Each coding agent's own place in the palette, by the first word of its name, so
/// "Claude Code" and "Claude" match and a second "Claude 2" starts from the same colour.
const AGENT_COLOURS: [(&str, usize); 19] = [
    ("claude", 0),
    ("cursor", 1),
    ("gemini", 2),
    ("codex", 3),
    ("copilot", 4),
    ("github", 4),
    ("windsurf", 5),
    ("antigravity", 6),
    ("amp", 7),
    ("kiro", 8),
    ("grok", 9),
    ("opencode", 10),
    ("aider", 11),
    ("roo", 12),
    ("continue", 13),
    ("cline", 14),
    ("goose", 15),
    ("zed", 16),
    ("fx", 17),
];

/// A colour from the agent's name, so it keeps its colour between runs, skipping
/// colours already on screen so two agents never look alike.
pub fn colour_for(name: &str, taken: &[Rgb]) -> Rgb {
    let lower = name.to_lowercase();
    let first = lower.split_whitespace().next().unwrap_or("");
    let known = AGENT_COLOURS.iter().find(|(agent, _)| *agent == first).map(|(_, at)| *at);
    let start = known.unwrap_or_else(|| name.bytes().fold(0x811C_9DC5_u32, |hash, byte| (hash ^ byte as u32).wrapping_mul(0x0100_0193)) as usize);
    (0..PALETTE.len())
        .map(|step| PALETTE[(start + step) % PALETTE.len()])
        .find(|colour| !taken.contains(colour))
        .unwrap_or(PALETTE[start % PALETTE.len()])
}

/// Points every few pixels along a stroke, through a smooth curve if asked for.
pub fn trace(points: &[(i32, i32)], smooth: bool) -> Vec<(f32, f32)> {
    let at = |i: isize| {
        let p = points[i.clamp(0, points.len() as isize - 1) as usize];
        (p.0 as f32, p.1 as f32)
    };
    let mut out = vec![at(0)];
    for i in 0..points.len().saturating_sub(1) as isize {
        let (a, b, c, d) = (at(i - 1), at(i), at(i + 1), at(i + 2));
        let steps = (((c.0 - b.0).hypot(c.1 - b.1) / STEP_PIXELS).ceil() as usize).max(1);
        for step in 1..=steps {
            let t = step as f32 / steps as f32;
            out.push(if smooth {
                let curve = |p0: f32, p1: f32, p2: f32, p3: f32| {
                    0.5 * (2.0 * p1
                        + (p2 - p0) * t
                        + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t * t
                        + (3.0 * p1 - p0 - 3.0 * p2 + p3) * t * t * t)
                };
                (curve(a.0, b.0, c.0, d.0), curve(a.1, b.1, c.1, d.1))
            } else {
                (b.0 + (c.0 - b.0) * t, b.1 + (c.1 - b.1) * t)
            });
        }
    }
    out
}

/// When to reach each point of a stroke, in seconds from its start, with a handwriting rhythm as
/// on qlinter.org: on average `speed` pixels a second, slower where the stroke turns, and easing
/// in and out over its first and last tenth.
pub fn schedule(path: &[(f32, f32)], speed: f32) -> Vec<f32> {
    let count = path.len();
    if count < 2 {
        return vec![0.0; count];
    }
    let length = |a: (f32, f32), b: (f32, f32)| (b.0 - a.0).hypot(b.1 - a.1);
    let segments: Vec<f32> = (0..count).map(|i| if i == 0 { 0.0 } else { length(path[i - 1], path[i]) }).collect();
    let total: f32 = segments.iter().sum();
    if total <= 0.0 {
        return vec![0.0; count];
    }
    // How sharply the stroke turns at each point, per pixel, then smoothed over its neighbours.
    let bend: Vec<f32> = (0..count)
        .map(|i| {
            if i == 0 || i + 1 == count || segments[i] <= 0.0 || segments[i + 1] <= 0.0 {
                return 0.0;
            }
            let (a, b, c) = (path[i - 1], path[i], path[i + 1]);
            let (before, after) = ((b.1 - a.1).atan2(b.0 - a.0), (c.1 - b.1).atan2(c.0 - b.0));
            let mut turn = (after - before).abs();
            if turn > std::f32::consts::PI {
                turn = 2.0 * std::f32::consts::PI - turn;
            }
            turn / segments[i].max(1e-3)
        })
        .collect();
    let smooth = |i: usize| {
        let (low, high) = (i.saturating_sub(4), (i + 4).min(count - 1));
        bend[low..=high].iter().sum::<f32>() / (high - low + 1) as f32
    };
    let ramp = |x: f32| {
        let k = (x / RAMP_SHARE).clamp(0.0, 1.0);
        1.0 + RAMP_SLOWS * (1.0 - k * k * (3.0 - 2.0 * k))
    };
    let mut times = vec![0.0f32; count];
    let mut along = 0.0;
    for i in 1..count {
        along += segments[i];
        let share = along / total;
        times[i] = times[i - 1] + segments[i] * (1.0 + CURVE_SLOWS * smooth(i)) * ramp(share) * ramp(1.0 - share);
    }
    let scale = total / speed / times[count - 1].max(1e-6);
    times.iter().map(|t| t * scale).collect()
}

/// Visit each point of a stroke on its handwriting schedule. False if stopped on the way.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn pace(path: &[(f32, f32)], stop: &AtomicBool, visit: impl FnMut(usize, i32, i32)) -> bool {
    pace_at(path, PIXELS_PER_SECOND, stop, visit)
}

/// `pace` at another speed, in pixels a second.
pub fn pace_at(path: &[(f32, f32)], speed: f32, stop: &AtomicBool, mut visit: impl FnMut(usize, i32, i32)) -> bool {
    let started = Instant::now();
    let due_at = schedule(path, speed);
    for (at, point) in path.iter().enumerate() {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        let due = Duration::from_secs_f32(due_at[at]);
        while let Some(left) = due.checked_sub(started.elapsed()) {
            if left > Duration::from_millis(2) {
                thread::sleep(left - Duration::from_millis(2));
            } else {
                std::hint::spin_loop();
            }
        }
        visit(at, point.0.round() as i32, point.1.round() as i32);
    }
    true
}

#[derive(Debug, Clone, PartialEq)]
pub enum ShapeKind {
    Line { start: Point, end: Point },
    Rect { x: f32, y: f32, w: f32, h: f32, fill: bool },
    Circle { cx: f32, cy: f32, r: f32, fill: bool },
    Ellipse { cx: f32, cy: f32, rx: f32, ry: f32 },
    Arc { cx: f32, cy: f32, r: f32, start_deg: f32, end_deg: f32 },
    Bezier { p0: Point, p1: Point, p2: Point, p3: Point },
    Poly { points: Vec<Point>, close: bool },
}

impl ShapeKind {
    /// Apply --fill to a rect or circle and --close to a poly given outside the shape.
    pub fn with_flags(mut self, fill: bool, close: bool) -> ShapeKind {
        match &mut self {
            ShapeKind::Rect { fill: f, .. } | ShapeKind::Circle { fill: f, .. } => *f |= fill,
            ShapeKind::Poly { close: c, .. } => *c |= close,
            _ => {}
        }
        self
    }

    pub fn transform(&self, offset: Point, scale: f32) -> ShapeKind {
        let map = |p: Point| (offset.0 + p.0 * scale, offset.1 + p.1 * scale);
        match self {
            ShapeKind::Line { start, end } => ShapeKind::Line {
                start: map(*start),
                end: map(*end),
            },
            ShapeKind::Rect { x, y, w, h, fill } => ShapeKind::Rect {
                x: offset.0 + x * scale,
                y: offset.1 + y * scale,
                w: w * scale,
                h: h * scale,
                fill: *fill,
            },
            ShapeKind::Circle { cx, cy, r, fill } => ShapeKind::Circle {
                cx: offset.0 + cx * scale,
                cy: offset.1 + cy * scale,
                r: r * scale,
                fill: *fill,
            },
            ShapeKind::Ellipse { cx, cy, rx, ry } => ShapeKind::Ellipse {
                cx: offset.0 + cx * scale,
                cy: offset.1 + cy * scale,
                rx: rx * scale,
                ry: ry * scale,
            },
            ShapeKind::Arc { cx, cy, r, start_deg, end_deg } => ShapeKind::Arc {
                cx: offset.0 + cx * scale,
                cy: offset.1 + cy * scale,
                r: r * scale,
                start_deg: *start_deg,
                end_deg: *end_deg,
            },
            ShapeKind::Bezier { p0, p1, p2, p3 } => ShapeKind::Bezier {
                p0: map(*p0),
                p1: map(*p1),
                p2: map(*p2),
                p3: map(*p3),
            },
            ShapeKind::Poly { points, close } => ShapeKind::Poly {
                points: points.iter().copied().map(map).collect(),
                close: *close,
            },
        }
    }
}

/// The shape names `draw` understands.
pub const SHAPES: [&str; 7] = ["line", "rect", "circle", "ellipse", "arc", "bezier", "poly"];

pub fn is_shape(word: &str) -> bool {
    SHAPES.contains(&word.to_lowercase().as_str())
}

pub fn parse_point(s: &str) -> Result<Point, String> {
    let s = s.trim().trim_matches('\'').trim_matches('"').trim();
    let (x, y) = s.split_once(',').ok_or_else(|| format!("'{s}' is not x,y"))?;
    let x = x.trim().parse::<f32>().map_err(|_| format!("'{s}' is not x,y"))?;
    let y = y.trim().parse::<f32>().map_err(|_| format!("'{s}' is not x,y"))?;
    Ok((x, y))
}

pub fn parse_shape(script: &str) -> Result<Option<ShapeKind>, String> {
    let script = script.trim().trim_matches('\'').trim_matches('"').trim();
    let raw_tokens: Vec<&str> = script.split_whitespace().collect();
    if raw_tokens.is_empty() {
        return Ok(None);
    }
    let command = raw_tokens[0].to_lowercase();
    match command.as_str() {
        "line" => {
            if raw_tokens.len() < 3 {
                return Err("line requires start and end points: line <x1,y1> <x2,y2>".into());
            }
            let start = parse_point(raw_tokens[1])?;
            let end = parse_point(raw_tokens[2])?;
            Ok(Some(ShapeKind::Line { start, end }))
        }
        "rect" => {
            let mut fill = false;
            let mut args = Vec::new();
            for &token in &raw_tokens[1..] {
                if token == "--fill" {
                    fill = true;
                } else {
                    args.push(token);
                }
            }
            if args.len() == 2 {
                let (x, y) = parse_point(args[0])?;
                let (w, h) = parse_point(args[1])?;
                if w < 0.0 || h < 0.0 {
                    return Err("width and height must be non-negative".into());
                }
                Ok(Some(ShapeKind::Rect { x, y, w, h, fill }))
            } else if args.len() == 3 {
                let (x, y) = parse_point(args[0])?;
                let w = args[1].parse::<f32>().map_err(|_| format!("invalid width '{}'", args[1]))?;
                let h = args[2].parse::<f32>().map_err(|_| format!("invalid height '{}'", args[2]))?;
                if w < 0.0 || h < 0.0 {
                    return Err("width and height must be non-negative".into());
                }
                Ok(Some(ShapeKind::Rect { x, y, w, h, fill }))
            } else {
                Err("rect requires origin and size: rect <x,y> <w,h> [--fill]".into())
            }
        }
        "circle" => {
            let mut fill = false;
            let mut args = Vec::new();
            for &token in &raw_tokens[1..] {
                if token == "--fill" {
                    fill = true;
                } else {
                    args.push(token);
                }
            }
            if args.len() < 2 {
                return Err("circle requires centre and radius: circle <cx,cy> <r> [--fill]".into());
            }
            let (cx, cy) = parse_point(args[0])?;
            let r = args[1].parse::<f32>().map_err(|_| format!("invalid radius '{}'", args[1]))?;
            if r <= 0.0 {
                return Err("radius must be positive".into());
            }
            Ok(Some(ShapeKind::Circle { cx, cy, r, fill }))
        }
        "ellipse" => {
            if raw_tokens.len() < 3 {
                return Err("ellipse requires centre and radii: ellipse <cx,cy> <rx,ry>".into());
            }
            let (cx, cy) = parse_point(raw_tokens[1])?;
            let (rx, ry) = parse_point(raw_tokens[2])?;
            if rx <= 0.0 || ry <= 0.0 {
                return Err("radii must be positive".into());
            }
            Ok(Some(ShapeKind::Ellipse { cx, cy, rx, ry }))
        }
        "arc" => {
            if raw_tokens.len() < 5 {
                return Err("arc requires centre, radius, start and end angles: arc <cx,cy> <r> <start_deg> <end_deg>".into());
            }
            let (cx, cy) = parse_point(raw_tokens[1])?;
            let r = raw_tokens[2].parse::<f32>().map_err(|_| format!("invalid radius '{}'", raw_tokens[2]))?;
            if r <= 0.0 {
                return Err("radius must be positive".into());
            }
            let start_deg = raw_tokens[3].parse::<f32>().map_err(|_| format!("invalid start angle '{}'", raw_tokens[3]))?;
            let end_deg = raw_tokens[4].parse::<f32>().map_err(|_| format!("invalid end angle '{}'", raw_tokens[4]))?;
            Ok(Some(ShapeKind::Arc { cx, cy, r, start_deg, end_deg }))
        }
        "bezier" => {
            let points: Result<Vec<Point>, String> = raw_tokens[1..].iter().map(|&s| parse_point(s)).collect();
            let points = points?;
            if points.len() != 4 {
                return Err("bezier requires 4 control points: bezier <p0> <p1> <p2> <p3>".into());
            }
            Ok(Some(ShapeKind::Bezier {
                p0: points[0],
                p1: points[1],
                p2: points[2],
                p3: points[3],
            }))
        }
        "poly" => {
            let mut close = false;
            let mut pt_tokens = Vec::new();
            for &token in &raw_tokens[1..] {
                if token == "--close" {
                    close = true;
                } else {
                    pt_tokens.push(token);
                }
            }
            if pt_tokens.len() < 2 {
                return Err("poly requires at least 2 points: poly <p1> <p2> ... [--close]".into());
            }
            let points: Result<Vec<Point>, String> = pt_tokens.iter().map(|&s| parse_point(s)).collect();
            let points = points?;
            Ok(Some(ShapeKind::Poly { points, close }))
        }
        _ => Ok(None),
    }
}

fn generate_line(start: Point, end: Point) -> Vec<Point> {
    let d = (end.0 - start.0).hypot(end.1 - start.1);
    if d == 0.0 {
        return vec![start];
    }
    let steps = ((d / STEP_PIXELS).ceil() as usize).max(1);
    (0..=steps)
        .map(|i| {
            let t = i as f32 / steps as f32;
            (start.0 + (end.0 - start.0) * t, start.1 + (end.1 - start.1) * t)
        })
        .collect()
}

fn generate_rect(x: f32, y: f32, w: f32, h: f32, fill: bool) -> Vec<Vec<Point>> {
    let p0 = (x, y);
    let p1 = (x + w, y);
    let p2 = (x + w, y + h);
    let p3 = (x, y + h);

    let seg0 = generate_line(p0, p1);
    let seg1 = generate_line(p1, p2);
    let seg2 = generate_line(p2, p3);
    let seg3 = generate_line(p3, p0);

    let mut outline = seg0;
    if seg1.len() > 1 { outline.extend_from_slice(&seg1[1..]); }
    if seg2.len() > 1 { outline.extend_from_slice(&seg2[1..]); }
    if seg3.len() > 1 { outline.extend_from_slice(&seg3[1..]); }

    if !fill {
        return vec![outline];
    }

    let mut fill_stroke = Vec::new();
    let pitch = 4.0_f32;
    let rows = ((h / pitch).ceil() as usize).max(1);
    for row in 0..=rows {
        let y_curr = (y + row as f32 * pitch).min(y + h);
        let (row_start, row_end) = if row % 2 == 0 {
            ((x, y_curr), (x + w, y_curr))
        } else {
            ((x + w, y_curr), (x, y_curr))
        };
        let line = generate_line(row_start, row_end);
        if fill_stroke.is_empty() {
            fill_stroke.extend(line);
        } else {
            let conn = generate_line(*fill_stroke.last().unwrap(), row_start);
            if conn.len() > 1 {
                fill_stroke.extend_from_slice(&conn[1..]);
            }
            if line.len() > 1 {
                fill_stroke.extend_from_slice(&line[1..]);
            }
        }
    }
    vec![outline, fill_stroke]
}

fn generate_circle(cx: f32, cy: f32, r: f32, fill: bool) -> Vec<Vec<Point>> {
    let c = 2.0 * std::f32::consts::PI * r;
    let steps = ((c / STEP_PIXELS).ceil() as usize).max(12);
    let outline: Vec<Point> = (0..=steps)
        .map(|i| {
            let theta = 2.0 * std::f32::consts::PI * (i as f32 / steps as f32);
            (cx + r * theta.cos(), cy + r * theta.sin())
        })
        .collect();

    if !fill {
        return vec![outline];
    }

    let mut fill_stroke = Vec::new();
    let pitch = 4.0_f32;
    let rows = ((2.0 * r / pitch).ceil() as usize).max(1);
    for row in 0..=rows {
        let y_curr = (cy - r + row as f32 * pitch).min(cy + r);
        let dy = y_curr - cy;
        let dx_sq = r * r - dy * dy;
        if dx_sq >= 0.0 {
            let dx = dx_sq.sqrt();
            let (row_start, row_end) = if row % 2 == 0 {
                ((cx - dx, y_curr), (cx + dx, y_curr))
            } else {
                ((cx + dx, y_curr), (cx - dx, y_curr))
            };
            let line = generate_line(row_start, row_end);
            if fill_stroke.is_empty() {
                fill_stroke.extend(line);
            } else {
                let conn = generate_line(*fill_stroke.last().unwrap(), row_start);
                if conn.len() > 1 {
                    fill_stroke.extend_from_slice(&conn[1..]);
                }
                if line.len() > 1 {
                    fill_stroke.extend_from_slice(&line[1..]);
                }
            }
        }
    }
    vec![outline, fill_stroke]
}

fn generate_ellipse(cx: f32, cy: f32, rx: f32, ry: f32) -> Vec<Vec<Point>> {
    let h = ((rx - ry) / (rx + ry)).powi(2);
    let p = std::f32::consts::PI * (rx + ry) * (1.0 + 3.0 * h / (10.0 + (4.0 - 3.0 * h).max(0.0).sqrt()));
    let steps = ((p / STEP_PIXELS).ceil() as usize).max(12);
    let points: Vec<Point> = (0..=steps)
        .map(|i| {
            let theta = 2.0 * std::f32::consts::PI * (i as f32 / steps as f32);
            (cx + rx * theta.cos(), cy + ry * theta.sin())
        })
        .collect();
    vec![points]
}

fn generate_arc(cx: f32, cy: f32, r: f32, start_deg: f32, end_deg: f32) -> Vec<Vec<Point>> {
    let mut delta_deg = end_deg - start_deg;
    if delta_deg < 0.0 {
        delta_deg += 360.0;
    }
    if delta_deg == 0.0 {
        delta_deg = 360.0;
    }
    let arc_len = r * delta_deg.to_radians();
    let steps = ((arc_len / STEP_PIXELS).ceil() as usize).max(2);
    let points: Vec<Point> = (0..=steps)
        .map(|i| {
            let t = i as f32 / steps as f32;
            let deg = start_deg + delta_deg * t;
            let rad = deg.to_radians();
            (cx + r * rad.cos(), cy + r * rad.sin())
        })
        .collect();
    vec![points]
}

fn generate_bezier(p0: Point, p1: Point, p2: Point, p3: Point) -> Vec<Vec<Point>> {
    let d1 = (p1.0 - p0.0).hypot(p1.1 - p0.1);
    let d2 = (p2.0 - p1.0).hypot(p2.1 - p1.1);
    let d3 = (p3.0 - p2.0).hypot(p3.1 - p2.1);
    let chord = (p3.0 - p0.0).hypot(p3.1 - p0.1);
    let approx_len = (d1 + d2 + d3 + chord) * 0.5;
    let steps = ((approx_len / STEP_PIXELS).ceil() as usize).max(4);
    let points: Vec<Point> = (0..=steps)
        .map(|i| {
            let t = i as f32 / steps as f32;
            let u = 1.0 - t;
            let x = u * u * u * p0.0 + 3.0 * u * u * t * p1.0 + 3.0 * u * t * t * p2.0 + t * t * t * p3.0;
            let y = u * u * u * p0.1 + 3.0 * u * u * t * p1.1 + 3.0 * u * t * t * p2.1 + t * t * t * p3.1;
            (x, y)
        })
        .collect();
    vec![points]
}

fn generate_poly(points: &[Point], close: bool) -> Vec<Vec<Point>> {
    if points.is_empty() {
        return vec![];
    }
    if points.len() == 1 {
        return vec![points.to_vec()];
    }
    let mut stroke = Vec::new();
    let num_segs = if close { points.len() } else { points.len() - 1 };
    for i in 0..num_segs {
        let p_start = points[i];
        let p_end = points[(i + 1) % points.len()];
        let seg = generate_line(p_start, p_end);
        if i == 0 {
            stroke.extend(seg);
        } else if seg.len() > 1 {
            stroke.extend_from_slice(&seg[1..]);
        }
    }
    vec![stroke]
}

pub fn generate_shape(shape: &ShapeKind) -> Vec<Vec<Point>> {
    match shape {
        ShapeKind::Line { start, end } => vec![generate_line(*start, *end)],
        ShapeKind::Rect { x, y, w, h, fill } => generate_rect(*x, *y, *w, *h, *fill),
        ShapeKind::Circle { cx, cy, r, fill } => generate_circle(*cx, *cy, *r, *fill),
        ShapeKind::Ellipse { cx, cy, rx, ry } => generate_ellipse(*cx, *cy, *rx, *ry),
        ShapeKind::Arc { cx, cy, r, start_deg, end_deg } => generate_arc(*cx, *cy, *r, *start_deg, *end_deg),
        ShapeKind::Bezier { p0, p1, p2, p3 } => generate_bezier(*p0, *p1, *p2, *p3),
        ShapeKind::Poly { points, close } => generate_poly(points, *close),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_hops_are_quick_and_long_ones_are_capped() {
        assert_eq!(travel_ms((0.0, 0.0), (0.0, 0.0)), 320);
        assert_eq!(travel_ms((0.0, 0.0), (100.0, 0.0)), 400);
        assert_eq!(travel_ms((0.0, 0.0), (2560.0, 1600.0)), 1120);
    }

    #[test]
    fn easing_starts_and_ends_still() {
        assert!(ease(0.0).abs() < 1e-6);
        assert!((ease(1.0) - 1.0).abs() < 1e-6);
        assert!(ease(0.01) < 0.002);
        assert!(ease(0.99) > 0.999);
        // Most of the way is covered early, and the rest is a slow settle.
        assert!(ease(0.5) > 0.8);
        assert!((1..100).all(|i| ease(i as f32 / 100.0) >= ease((i - 1) as f32 / 100.0)));
    }

    #[test]
    fn the_arrow_turns_toward_where_it_heads_and_straightens_when_it_stops() {
        let mut pose = Pose::new();
        let start = Instant::now();
        let frame = |i: u64| start + Duration::from_millis(16 * i);
        // Rightward at 1500 pixels a second leans it clockwise; leftward leans it the other way.
        for i in 0..30 {
            pose.follow(frame(i), (i as f32 * 24.0, 0.0), 1.0);
        }
        assert!(pose.lean > 15.0 && pose.lean <= LEAN_MOST, "{}", pose.lean);
        for i in 30..60 {
            pose.follow(frame(i), ((60 - i) as f32 * 24.0, 0.0), 1.0);
        }
        assert!(pose.lean < -15.0, "{}", pose.lean);
        // Standing still, it settles back to upright and says so.
        let mut still = true;
        for i in 60..240 {
            still = pose.follow(frame(i), (0.0, 0.0), 1.0);
        }
        assert_eq!(pose.lean, 0.0);
        assert!(!still);
        // Down turns the tip toward down, counter-clockwise; up turns it clockwise until it points
        // straight up, about 34 degrees from where it rests.
        let mut down = Pose::new();
        for i in 0..30 {
            down.follow(frame(i), (0.0, i as f32 * 24.0), 1.0);
        }
        assert!(down.lean < -15.0, "{}", down.lean);
        let mut up = Pose::new();
        for i in 0..30 {
            up.follow(frame(i), (0.0, -(i as f32) * 24.0), 1.0);
        }
        assert!(up.lean > 25.0 && up.lean < 34.0, "{}", up.lean);
        // Along the way it already points it stays upright; slow moves do not turn it; the turn
        // grows with speed up to 600 pixels a second, and never passes LEAN_MOST.
        assert_eq!(lean_toward((-670.0, -1000.0)).round(), 0.0);
        assert_eq!(lean_toward((30.0, 0.0)), 0.0);
        assert!((lean_toward((300.0, 0.0)) - lean_toward((600.0, 0.0)) / 2.0).abs() < 0.01);
        assert!(lean_toward((670.0, 1000.0)).abs() <= LEAN_MOST);
    }

    #[test]
    fn each_key_pulls_the_tag_in_and_lets_it_back() {
        let mut pose = Pose::new();
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        assert_eq!(pose.squeeze(start), 1.0);
        pose.type_keys(start, 3);
        assert!((pose.squeeze(at(90)) - (1.0 - PULSE_DEPTH)).abs() < 1e-4);
        assert!(pose.squeeze(at(PULSE_EVERY_MS as u64 - 1)) > 0.99);
        assert!(pose.squeeze(at(PULSE_EVERY_MS as u64 + 90)) < 0.95);
        assert_eq!(pose.squeeze(at(5000)), 1.0);
        // Long typing shows a handful of pulses, not one per character.
        pose.type_keys(start, 500);
        assert_eq!(pose.squeeze(at(PULSES_MOST as u64 * PULSE_EVERY_MS as u64 + 400)), 1.0);
    }

    #[test]
    fn the_path_ends_exactly_on_target_and_bends_on_the_way() {
        let (from, to) = ((10.0, 20.0), (410.0, 20.0));
        let middle = bow(from, to, 1.0);
        assert_eq!(along(from, middle, to, 0.0), from);
        assert_eq!(along(from, middle, to, 1.0), to);
        // A sideways move arcs up whichever side it is given, going right or left.
        assert!(along(from, middle, to, 0.5).1 < 20.0);
        assert!(bow(from, to, -1.0).1 < 20.0);
        assert!(bow(to, from, 1.0).1 < 20.0);
        // An upright move bows to the side it is given.
        let (top, bottom) = ((50.0, 0.0), (50.0, 400.0));
        assert!(bow(top, bottom, 1.0).0 < 50.0 && bow(top, bottom, -1.0).0 > 50.0);
    }

    #[test]
    fn strokes_keep_a_handwriting_rhythm() {
        // A straight stroke takes its length over the average speed, and its ends go slower.
        let line: Vec<(f32, f32)> = (0..=100).map(|i| (i as f32 * 6.0, 0.0)).collect();
        let due = schedule(&line, PIXELS_PER_SECOND);
        assert_eq!(due[0], 0.0);
        assert!((due[100] - 600.0 / PIXELS_PER_SECOND).abs() < 1e-3, "{}", due[100]);
        let (start, middle) = (due[1] - due[0], due[51] - due[50]);
        assert!(start > middle * 2.0, "{start} vs {middle}");
        assert!(due.windows(2).all(|w| w[1] >= w[0]));
        // A sharp corner takes longer than the straight part before it.
        let mut corner: Vec<(f32, f32)> = (0..=50).map(|i| (i as f32 * 6.0, 0.0)).collect();
        corner.extend((1..=50).map(|i| (300.0, i as f32 * 6.0)));
        let due = schedule(&corner, PIXELS_PER_SECOND);
        assert!(due[51] - due[49] > (due[31] - due[29]) * 1.4);
    }

    #[test]
    fn a_name_keeps_its_colour() {
        assert_eq!(colour_for("Claude Code", &[]), colour_for("Claude Code", &[]));
    }

    #[test]
    fn well_known_agents_have_their_own_colours() {
        assert_eq!(colour_for("Claude Code", &[]), PALETTE[0]);
        assert_eq!(colour_for("cursor", &[]), PALETTE[1]);
        assert_eq!(colour_for("Gemini CLI", &[]), PALETTE[2]);
        assert_eq!(colour_for("Codex", &[]), (16, 163, 127));
        assert_eq!(colour_for("GitHub Copilot", &[]), colour_for("Copilot", &[]));
    }

    #[test]
    fn every_agent_differs_from_the_rest() {
        let names = ["Claude Code", "Cursor", "Gemini", "Codex", "Copilot", "Windsurf", "Antigravity", "Amp", "Kiro", "Grok", "OpenCode", "Aider", "Roo Code", "Continue", "Cline", "Goose", "Zed", "fx"];
        let colours: Vec<Rgb> = names.iter().map(|name| colour_for(name, &[])).collect();
        assert!(colours.iter().enumerate().all(|(at, colour)| !colours[..at].contains(colour)));
        assert!(PALETTE.iter().enumerate().all(|(at, colour)| !PALETTE[..at].contains(colour)));
    }

    #[test]
    fn a_second_copy_of_an_agent_gets_another_colour() {
        let first = colour_for("Claude 1", &[]);
        assert_eq!(first, PALETTE[0]);
        assert_ne!(colour_for("Claude 2", &[first]), first);
    }

    #[test]
    fn six_agents_on_screen_all_look_different() {
        let mut taken = Vec::new();
        for name in ["a", "b", "c", "d", "e", "f"] {
            let colour = colour_for(name, &taken);
            assert!(!taken.contains(&colour));
            taken.push(colour);
        }
    }

    #[test]
    fn test_parse_and_generate_line() {
        let parsed = parse_shape("line 10,20 100,140").unwrap().expect("shape expected");
        assert_eq!(parsed, ShapeKind::Line { start: (10.0, 20.0), end: (100.0, 140.0) });
        let strokes = generate_shape(&parsed);
        assert_eq!(strokes.len(), 1);
        let path = &strokes[0];
        assert_eq!(*path.first().unwrap(), (10.0, 20.0));
        assert_eq!(*path.last().unwrap(), (100.0, 140.0));
        // Collinearity check
        for pt in path {
            if (pt.0 - 10.0).abs() > 1e-4 {
                let slope = (pt.1 - 20.0) / (pt.0 - 10.0);
                assert!((slope - (120.0 / 90.0)).abs() < 1e-3);
            }
        }
        // Step size check
        for w in path.windows(2) {
            let step = (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1);
            assert!(step <= STEP_PIXELS + 0.1);
        }
    }

    #[test]
    fn test_parse_and_generate_rect_outline_and_fill() {
        let outline_parsed = parse_shape("rect 20,20 80,60").unwrap().expect("shape expected");
        assert_eq!(outline_parsed, ShapeKind::Rect { x: 20.0, y: 20.0, w: 80.0, h: 60.0, fill: false });
        let outline_strokes = generate_shape(&outline_parsed);
        assert_eq!(outline_strokes.len(), 1);
        assert_eq!(*outline_strokes[0].first().unwrap(), (20.0, 20.0));
        assert_eq!(*outline_strokes[0].last().unwrap(), (20.0, 20.0));

        let fill_parsed = parse_shape("rect 20,20 80,60 --fill").unwrap().expect("shape expected");
        assert_eq!(fill_parsed, ShapeKind::Rect { x: 20.0, y: 20.0, w: 80.0, h: 60.0, fill: true });
        let fill_strokes = generate_shape(&fill_parsed);
        assert_eq!(fill_strokes.len(), 2);
        // Both strokes are populated
        assert!(!fill_strokes[0].is_empty());
        assert!(!fill_strokes[1].is_empty());
    }

    #[test]
    fn test_parse_and_generate_circle_outline_and_fill() {
        let circle = parse_shape("circle 100,100 50").unwrap().expect("shape expected");
        assert_eq!(circle, ShapeKind::Circle { cx: 100.0, cy: 100.0, r: 50.0, fill: false });
        let strokes = generate_shape(&circle);
        assert_eq!(strokes.len(), 1);
        for pt in &strokes[0] {
            let dist = (pt.0 - 100.0).hypot(pt.1 - 100.0);
            assert!((dist - 50.0).abs() < 1.0, "Point {:?} distance {} from center", pt, dist);
        }

        let circle_fill = parse_shape("circle 100,100 50 --fill").unwrap().expect("shape expected");
        let fill_strokes = generate_shape(&circle_fill);
        assert_eq!(fill_strokes.len(), 2);
    }

    #[test]
    fn test_parse_and_generate_ellipse() {
        let ellipse = parse_shape("ellipse 200,150 80,40").unwrap().expect("shape expected");
        let strokes = generate_shape(&ellipse);
        assert_eq!(strokes.len(), 1);
        for pt in &strokes[0] {
            let eq = ((pt.0 - 200.0) / 80.0).powi(2) + ((pt.1 - 150.0) / 40.0).powi(2);
            assert!((eq - 1.0).abs() < 0.05, "Ellipse eq {}", eq);
        }
    }

    #[test]
    fn test_parse_and_generate_arc() {
        let arc = parse_shape("arc 100,100 50 270 90").unwrap().expect("shape expected");
        let strokes = generate_shape(&arc);
        assert_eq!(strokes.len(), 1);
        let path = &strokes[0];
        // Start is 270 deg (top): (100, 50)
        assert!((path[0].0 - 100.0).abs() < 1.0);
        assert!((path[0].1 - 50.0).abs() < 1.0);
        // End is 90 deg (bottom): (100, 150)
        let last = path.last().unwrap();
        assert!((last.0 - 100.0).abs() < 1.0);
        assert!((last.1 - 150.0).abs() < 1.0);
        // All points satisfy radius 50.0
        for pt in path {
            let dist = (pt.0 - 100.0).hypot(pt.1 - 100.0);
            assert!((dist - 50.0).abs() < 0.01, "Arc point radius error");
        }
        // Midpoint crosses 0 deg / right: x ≈ 150, y ≈ 100
        let mid = path[path.len() / 2];
        assert!((mid.0 - 150.0).abs() < 2.0);
        assert!((mid.1 - 100.0).abs() < 4.0);
    }

    #[test]
    fn test_parse_and_generate_bezier() {
        let bezier = parse_shape("bezier 0,0 20,80 80,80 100,0").unwrap().expect("shape expected");
        let strokes = generate_shape(&bezier);
        assert_eq!(strokes.len(), 1);
        let path = &strokes[0];
        assert_eq!(*path.first().unwrap(), (0.0, 0.0));
        assert_eq!(*path.last().unwrap(), (100.0, 0.0));
        // Convex hull property
        for pt in path {
            assert!(pt.0 >= -0.01 && pt.0 <= 100.01);
            assert!(pt.1 >= -0.01 && pt.1 <= 80.01);
        }
    }

    #[test]
    fn test_parse_and_generate_poly_open_and_closed() {
        let poly_open = parse_shape("poly 10,10 50,10 50,50").unwrap().expect("shape expected");
        let strokes_open = generate_shape(&poly_open);
        assert_eq!(strokes_open.len(), 1);
        assert_eq!(*strokes_open[0].first().unwrap(), (10.0, 10.0));
        assert_eq!(*strokes_open[0].last().unwrap(), (50.0, 50.0));

        let poly_closed = parse_shape("poly 10,10 50,10 50,50 --close").unwrap().expect("shape expected");
        let strokes_closed = generate_shape(&poly_closed);
        assert_eq!(strokes_closed.len(), 1);
        assert_eq!(*strokes_closed[0].first().unwrap(), (10.0, 10.0));
        assert_eq!(*strokes_closed[0].last().unwrap(), (10.0, 10.0));
    }

    #[test]
    fn test_shape_transform() {
        let line = ShapeKind::Line { start: (10.0, 20.0), end: (30.0, 40.0) };
        let transformed = line.transform((100.0, 200.0), 2.0);
        assert_eq!(transformed, ShapeKind::Line { start: (120.0, 240.0), end: (160.0, 280.0) });
    }

    #[test]
    fn test_legacy_coordinate_fallback() {
        let res = parse_shape("10,10 20,20; 30,30 40,40").unwrap();
        assert_eq!(res, None);
    }

    #[test]
    fn test_syntax_and_boundary_rejections() {
        assert!(parse_shape("circle 100,100 0").is_err());
        assert!(parse_shape("circle 100,100 -5").is_err());
        assert!(parse_shape("rect 10,10 -50,20").is_err());
        assert!(parse_shape("bezier 10,10 20,20").is_err());
        assert!(parse_shape("poly 10,10").is_err());
    }
}
