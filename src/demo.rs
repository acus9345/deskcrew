// Three pretend agents roaming the screen. Nothing underneath is touched.
use crate::overlay::{Overlay, Rect, screen_size};
use std::sync::{Arc, atomic::AtomicBool};
use std::thread;
use std::time::{Duration, Instant};

const AGENTS: [&str; 3] = ["Claude Code", "Cursor", "fx"];

/// Enough randomness to stop the agents looking synchronised.
struct Dice(u64);

impl Dice {
    fn next(&mut self, below: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % below as u64) as usize
    }
}

pub fn run(seconds: u64, linger: u64) {
    let overlay = Overlay::start(Arc::new(AtomicBool::new(false)));
    let (width, height) = screen_size();
    let (cell_w, cell_h) = (width / 3, height / 4);
    let (button_w, button_h) = (width / 10, height / 14);
    let until = Instant::now() + Duration::from_secs(seconds);

    thread::scope(|scope| {
        for (column, name) in AGENTS.iter().enumerate() {
            let overlay = &overlay;
            scope.spawn(move || {
                let mut dice = Dice(0x9E37_79B9_7F4A_7C15 ^ (column as u64 + 1).wrapping_mul(0xBF58_476D_1CE4_E5B9));
                while Instant::now() < until {
                    let cell = (dice.next(3) as i32, dice.next(3) as i32);
                    // Anywhere across the screen, so moves run sideways as well as up and down.
                    let x = dice.next(3) as i32 * cell_w + cell.0 * (cell_w - button_w) / 2;
                    let y = height / 8 + cell.1 * cell_h;
                    let target = Rect { x, y, w: button_w, h: button_h };
                    overlay.move_to(name, x + button_w / 2, y + button_h / 2);
                    overlay.press(name, target);
                    // Now and then it types something there.
                    if dice.next(3) == 0 {
                        thread::sleep(Duration::from_millis(400));
                        overlay.typing(name, 3 + dice.next(4));
                        thread::sleep(Duration::from_millis(900));
                    }
                    thread::sleep(Duration::from_millis(420 + dice.next(600) as u64));
                }
            });
        }
    });

    thread::sleep(Duration::from_secs(linger));
    let stats = overlay.stop();
    println!("{}", stats.summary());
}

/// One agent clicks a spot in the middle of the screen, waits, then types, a few times over, so the
/// name tag's pulse can be watched on its own.
pub fn typing() {
    let overlay = Overlay::start(Arc::new(AtomicBool::new(false)));
    let (width, height) = screen_size();
    let spots = [(width / 2, height / 2), (width / 2 + width / 6, height / 2 - height / 10), (width / 2 - width / 6, height / 2 + height / 10)];
    for (x, y) in spots {
        overlay.move_to("Claude", x, y);
        overlay.press("Claude", Rect { x: x - 60, y: y - 16, w: 120, h: 32 });
        thread::sleep(Duration::from_millis(900));
        overlay.typing("Claude", 6);
        thread::sleep(Duration::from_millis(2200));
    }
    let stats = overlay.stop();
    println!("{}", stats.summary());
}
