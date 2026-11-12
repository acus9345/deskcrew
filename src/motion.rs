// How a drawn cursor gets from one place to another, and what colour it is.
pub type Point = (f32, f32);

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

// How far apart stroke points are, and how fast a stroke is drawn.
const STEP_PIXELS: f32 = 6.0;
#[cfg_attr(not(windows), allow(dead_code))]
const PIXELS_PER_SECOND: f32 = 1000.0;

/// How fast every kind of typing goes: 600 words a minute, at five letters to a word.
pub const WORDS_PER_MINUTE: u64 = 600;
pub const KEY_EVERY: Duration = Duration::from_millis(60_000 / (WORDS_PER_MINUTE * 5));

const MIN_TRAVEL_MS: f32 = 160.0;
const MAX_TRAVEL_MS: f32 = 560.0;
const MS_PER_PIXEL: f32 = 0.4;
/// How far the path's middle bends out, as a share of its length.
const BOW: f32 = 0.2;
/// The speed curve: a quick start, then a long gentle settle onto the target.
const EASE: [f32; 4] = [0.3, 0.0, 0.1, 1.0];
/// How far the arrow leans per pixel a second of sideways speed on a 100% screen, the most it
/// leans either way, and how quickly it follows the speed, in milliseconds.
const LEAN_PER_SPEED: f32 = 0.035;
pub const LEAN_MOST: f32 = 32.0;
const LEAN_MS: f32 = 130.0;
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

/// How the cursor holds itself: the arrow leans into sideways travel, and the tag pulses as keys
/// are typed.
pub struct Pose {
    pub lean: f32,
    last: Option<(Instant, Point)>,
    typing: Option<(Instant, usize)>,
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
                let speed = (at.0 - was.0) / seconds / scale.max(1.0);
                let wanted = (speed * LEAN_PER_SPEED).clamp(-LEAN_MOST, LEAN_MOST);
                self.lean += (wanted - self.lean) * (1.0 - (-seconds * 1000.0 / LEAN_MS).exp());
                if wanted == 0.0 && self.lean.abs() < 0.15 {
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
        ((self.lean * 2.0).round() as i32, ((1.0 - self.squeeze(now)) * 400.0).round() as i32)
    }

    /// The pose as drawn: lean in degrees and the tag's size.
    pub fn shown(&self, now: Instant) -> (f32, f32) {
        let (lean, pull) = self.look(now);
        (lean as f32 / 2.0, 1.0 - pull as f32 / 400.0)
    }
}

/// The middle of a slightly curved path, so travel never looks ruled.
pub fn bow(from: Point, to: Point, side: f32) -> Point {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
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

const PALETTE: [Rgb; 14] = [
    (208, 111, 89),
    (162, 61, 134),
    (21, 101, 240),
    (45, 190, 96),
    (138, 92, 246),
    (20, 184, 196),
    (132, 204, 22),
    (217, 70, 239),
    (220, 38, 38),
    (56, 189, 248),
    (250, 204, 21),
    (161, 98, 60),
    (148, 163, 184),
    (67, 56, 202),
];

/// Each coding agent's own place in the palette, by the first word of its name, so
/// "Claude Code" and "Claude" match and a second "Claude 2" starts from the same colour.
const AGENT_COLOURS: [(&str, usize); 16] = [
    ("claude", 0),
    ("cursor", 1),
    ("gemini", 2),
    ("fx", 2),
    ("codex", 3),
    ("copilot", 4),
    ("github", 4),
    ("windsurf", 5),
    ("aider", 6),
    ("amp", 7),
    ("roo", 8),
    ("continue", 9),
    ("cline", 10),
    ("goose", 11),
    ("opencode", 12),
    ("kiro", 13),
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

/// Visit each point of a stroke on schedule, at a steady speed. False if stopped on the way.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn pace(path: &[(f32, f32)], stop: &AtomicBool, mut visit: impl FnMut(usize, i32, i32)) -> bool {
    let started = Instant::now();
    for (at, point) in path.iter().enumerate() {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        let due = Duration::from_secs_f32(at as f32 * STEP_PIXELS / PIXELS_PER_SECOND);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_hops_are_quick_and_long_ones_are_capped() {
        assert_eq!(travel_ms((0.0, 0.0), (0.0, 0.0)), 160);
        assert_eq!(travel_ms((0.0, 0.0), (100.0, 0.0)), 200);
        assert_eq!(travel_ms((0.0, 0.0), (2560.0, 1600.0)), 560);
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
    fn the_arrow_leans_into_sideways_travel_and_straightens_when_it_stops() {
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
        for i in 60..120 {
            still = pose.follow(frame(i), (0.0, 0.0), 1.0);
        }
        assert_eq!(pose.lean, 0.0);
        assert!(!still);
        // Straight down does not lean at all.
        let mut down = Pose::new();
        for i in 0..30 {
            down.follow(frame(i), (0.0, i as f32 * 24.0), 1.0);
        }
        assert_eq!(down.lean, 0.0);
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
        assert!(along(from, middle, to, 0.5).1 > 20.0);
        assert!(bow(from, to, -1.0).1 < 20.0);
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
        assert_eq!(colour_for("GitHub Copilot", &[]), colour_for("Copilot", &[]));
    }

    #[test]
    fn every_agent_differs_from_the_rest() {
        let names = ["Claude Code", "Cursor", "Gemini", "Codex", "Copilot", "Windsurf", "Aider", "Amp", "Roo Code", "Continue", "Cline", "Goose", "OpenCode", "Kiro"];
        let colours: Vec<Rgb> = names.iter().map(|name| colour_for(name, &[])).collect();
        assert!(colours.iter().enumerate().all(|(at, colour)| !colours[..at].contains(colour)));
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
}
