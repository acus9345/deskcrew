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

const PALETTE: [Rgb; 14] = [
    (255, 165, 31),
    (240, 48, 110),
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
