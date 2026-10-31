// How a drawn cursor gets from one place to another, and what colour it is.
pub type Point = (f32, f32);

const MIN_TRAVEL_MS: f32 = 90.0;
const MAX_TRAVEL_MS: f32 = 320.0;
const MS_PER_PIXEL: f32 = 0.16;
const BOW: f32 = 0.08;

pub fn travel_ms(from: Point, to: Point) -> u32 {
    let distance = (to.0 - from.0).hypot(to.1 - from.1);
    (MIN_TRAVEL_MS + distance * MS_PER_PIXEL).min(MAX_TRAVEL_MS) as u32
}

/// Starts and stops gently, with no speed jump at either end.
pub fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
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

const PALETTE: [Rgb; 6] = [
    (255, 165, 31),
    (240, 48, 110),
    (21, 101, 240),
    (45, 190, 96),
    (138, 92, 246),
    (20, 184, 196),
];

/// A colour from the agent's name, so it keeps its colour between runs, skipping
/// colours already on screen so two agents never look alike.
pub fn colour_for(name: &str, taken: &[Rgb]) -> Rgb {
    let start = match name.to_lowercase().as_str() {
        "claude code" | "claude" => 0,
        "cursor" => 1,
        "fx" => 2,
        _ => name.bytes().fold(0x811C_9DC5_u32, |hash, byte| (hash ^ byte as u32).wrapping_mul(0x0100_0193)) as usize,
    };
    (0..PALETTE.len())
        .map(|step| PALETTE[(start + step) % PALETTE.len()])
        .find(|colour| !taken.contains(colour))
        .unwrap_or(PALETTE[start % PALETTE.len()])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_hops_are_quick_and_long_ones_are_capped() {
        assert_eq!(travel_ms((0.0, 0.0), (0.0, 0.0)), 90);
        assert_eq!(travel_ms((0.0, 0.0), (100.0, 0.0)), 106);
        assert_eq!(travel_ms((0.0, 0.0), (2560.0, 1600.0)), 320);
    }

    #[test]
    fn easing_starts_and_ends_still() {
        assert_eq!(ease(0.0), 0.0);
        assert_eq!(ease(1.0), 1.0);
        assert!((ease(0.5) - 0.5).abs() < 1e-6);
        assert!(ease(0.01) < 0.0001);
        assert!(ease(0.99) > 0.9999);
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
