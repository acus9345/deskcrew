// The drawn cursors. One thread owns every window and steps them once per display frame while
// anything is moving, then sleeps on its message queue, so an idle overlay costs no CPU.
mod draw;
mod layer;

use crate::motion::{self, Point, Rgb};
use draw::{cursor, outline, outline_margin};
use layer::{Layer, text_mask};
use std::{
    mem::zeroed,
    ptr::null_mut,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Graphics::Dwm::DwmFlush,
    System::Threading::{GetCurrentThreadId, INFINITE},
    UI::{
        HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForSystem, SetProcessDpiAwarenessContext},
        Input::KeyboardAndMouse::{MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, RegisterHotKey, VK_ESCAPE},
        WindowsAndMessaging::{
            DispatchMessageW, GetSystemMetrics, MSG, MsgWaitForMultipleObjects, PM_NOREMOVE, PM_REMOVE, PeekMessageW,
            PostThreadMessageW, QS_ALLINPUT, SM_CXSCREEN, SM_CYSCREEN, WM_APP, WM_HOTKEY,
        },
    },
};

/// An action fires this long after asking to move, even if drawing has fallen behind.
const ARRIVAL_TIMEOUT: Duration = Duration::from_millis(900);
const FLASH_IN_MS: f32 = 80.0;
const FLASH_HOLD_MS: f32 = 900.0;
const FLASH_OUT_MS: f32 = 220.0;
const REST_AFTER: Duration = Duration::from_secs(6);
const REST_FADE_MS: f32 = 160.0;
const ARRIVE_FROM: f32 = 90.0;

#[derive(Clone, Copy)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

enum Command {
    Move { agent: String, to: Point, arrived: Sender<()> },
    Jump { agent: String, to: Point },
    Press { agent: String, target: Rect },
    Aside { on: bool, cleared: Sender<()> },
    Quit,
}

pub fn screen_size() -> (i32, i32) {
    make_aware();
    unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) }
}

/// Work in real screen pixels, so accessibility bounds and the overlay agree.
pub fn make_aware() {
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

pub struct Overlay {
    commands: Sender<Command>,
    thread: u32,
    worker: JoinHandle<Stats>,
}

impl Overlay {
    /// `stop` goes up when the person presses Ctrl+Alt+Esc.
    pub fn start(stop: Arc<AtomicBool>) -> Overlay {
        let (commands, inbox) = mpsc::channel();
        let (ready, thread) = mpsc::channel();
        let worker = thread::spawn(move || run(inbox, ready, stop));
        Overlay { commands, thread: thread.recv().unwrap_or(0), worker }
    }

    fn send(&self, command: Command) {
        let _ = self.commands.send(command);
        unsafe { PostThreadMessageW(self.thread, WM_APP, 0, 0) };
    }

    /// Glide there and return once the cursor has arrived.
    pub fn move_to(&self, agent: &str, x: i32, y: i32) {
        let (arrived, wait) = mpsc::channel();
        self.send(Command::Move { agent: agent.to_string(), to: (x as f32, y as f32), arrived });
        let _ = wait.recv_timeout(ARRIVAL_TIMEOUT);
    }

    /// Follow a path the real pointer is walking, with no glide of its own.
    pub fn jump(&self, agent: &str, x: i32, y: i32) {
        self.send(Command::Jump { agent: agent.to_string(), to: (x as f32, y as f32) });
    }

    /// Clear every cursor off the screen for a screen copy, or bring them back.
    pub fn aside(&self, on: bool) {
        let (cleared, wait) = mpsc::channel();
        self.send(Command::Aside { on, cleared });
        let _ = wait.recv_timeout(ARRIVAL_TIMEOUT);
    }

    /// Light up the control this agent just pressed.
    pub fn press(&self, agent: &str, target: Rect) {
        self.send(Command::Press { agent: agent.to_string(), target });
    }

    pub fn stop(self) -> Stats {
        self.send(Command::Quit);
        self.worker.join().unwrap_or_default()
    }
}

/// How smooth the motion really was.
#[derive(Default)]
pub struct Stats {
    frames: u32,
    moving_ms: f32,
    longest_gap_ms: f32,
    flights: u32,
}

impl Stats {
    pub fn summary(&self) -> String {
        let rate = self.frames as f32 * 1000.0 / self.moving_ms.max(1.0);
        format!(
            "{} moves, {rate:.0} frames per second while moving, longest gap {:.1} ms",
            self.flights, self.longest_gap_ms
        )
    }
}

struct Flight {
    from: Point,
    middle: Point,
    to: Point,
    started: Instant,
    ms: f32,
    arrived: Sender<()>,
}

struct Agent {
    name: String,
    colour: Rgb,
    layer: Layer,
    tip: (i32, i32),
    at: Point,
    side: f32,
    flight: Option<Flight>,
    last_active: Instant,
    alpha: u8,
    visible: bool,
    aside: bool,
}

impl Agent {
    fn new(name: &str, to: Point, taken: &[Rgb], scale: f32) -> Option<Agent> {
        let colour = motion::colour_for(name, taken);
        let label = text_mask(name, (14.0 * scale) as i32);
        let (picture, x, y) = cursor(colour, &label, scale)?;
        Some(Agent {
            name: name.to_string(),
            colour,
            layer: Layer::new(&picture),
            tip: (x, y),
            at: (to.0 + ARRIVE_FROM * scale, to.1 + ARRIVE_FROM * scale),
            side: if taken.len().is_multiple_of(2) { 1.0 } else { -1.0 },
            flight: None,
            last_active: Instant::now(),
            alpha: 255,
            visible: false,
            aside: false,
        })
    }

    fn draw(&self) {
        let alpha = if self.aside { 0 } else { self.alpha };
        self.layer.present(self.at.0.round() as i32 - self.tip.0, self.at.1.round() as i32 - self.tip.1, alpha);
    }

    fn jump(&mut self, to: Point) {
        self.last_active = Instant::now();
        self.alpha = 255;
        self.at = to;
        self.draw();
        if !self.visible {
            self.layer.show();
            self.visible = true;
        }
    }

    fn fly(&mut self, to: Point, arrived: Sender<()>) {
        self.last_active = Instant::now();
        self.alpha = 255;
        self.draw();
        if !self.visible {
            self.layer.show();
            self.visible = true;
        }
        self.side = -self.side;
        let from = self.at;
        let ms = motion::travel_ms(from, to) as f32;
        self.flight = Some(Flight { from, middle: motion::bow(from, to, self.side), to, started: Instant::now(), ms, arrived });
    }

    /// One frame of travel. True while the cursor is still in the air.
    fn step(&mut self, now: Instant) -> bool {
        let Some(flight) = &self.flight else { return false };
        let t = now.duration_since(flight.started).as_secs_f32() * 1000.0 / flight.ms;
        self.at = motion::along(flight.from, flight.middle, flight.to, motion::ease(t));
        self.last_active = now;
        self.draw();
        if t < 1.0 {
            return true;
        }
        let _ = flight.arrived.send(());
        self.flight = None;
        false
    }

    /// Fade away once nothing has happened for a while. True while fading.
    fn rest(&mut self, now: Instant) -> bool {
        if !self.visible || self.flight.is_some() {
            return false;
        }
        let idle = now.duration_since(self.last_active);
        if idle < REST_AFTER {
            return false;
        }
        let faded = (idle - REST_AFTER).as_secs_f32() * 1000.0 / REST_FADE_MS;
        if faded >= 1.0 {
            self.layer.hide();
            self.visible = false;
            return false;
        }
        self.alpha = (255.0 * (1.0 - faded)) as u8;
        self.draw();
        true
    }
}

struct Flash {
    layer: Layer,
    x: i32,
    y: i32,
    started: Instant,
    aside: bool,
}

impl Flash {
    fn new(colour: Rgb, target: Rect, scale: f32) -> Option<Flash> {
        let margin = outline_margin(scale);
        let layer = Layer::new(&outline(colour, target.w, target.h, scale)?);
        let flash = Flash { layer, x: target.x - margin, y: target.y - margin, started: Instant::now(), aside: false };
        flash.layer.present(flash.x, flash.y, 0);
        flash.layer.show();
        Some(flash)
    }

    /// Draw this frame. False once it has faded out.
    fn step(&self, now: Instant) -> bool {
        let ms = now.duration_since(self.started).as_secs_f32() * 1000.0;
        let opacity = if ms < FLASH_IN_MS {
            ms / FLASH_IN_MS
        } else if ms < FLASH_HOLD_MS {
            1.0
        } else {
            1.0 - (ms - FLASH_HOLD_MS) / FLASH_OUT_MS
        };
        let alpha = if self.aside { 0.0 } else { opacity.clamp(0.0, 1.0) * 255.0 };
        self.layer.present(self.x, self.y, alpha as u8);
        opacity > 0.0
    }
}

fn run(inbox: Receiver<Command>, ready: Sender<u32>, stop: Arc<AtomicBool>) -> Stats {
    let mut stats = Stats::default();
    let mut agents: Vec<Agent> = Vec::new();
    let mut flashes: Vec<Flash> = Vec::new();
    let mut last_frame: Option<Instant> = None;
    let mut aside = false;
    unsafe {
        make_aware();
        // Windows only gives a thread a message queue once it asks for a message.
        let mut message: MSG = zeroed();
        PeekMessageW(&mut message, null_mut(), 0, 0, PM_NOREMOVE);
        let _ = ready.send(GetCurrentThreadId());
        RegisterHotKey(null_mut(), 1, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, VK_ESCAPE as u32);
    }
    let scale = (unsafe { GetDpiForSystem() } as f32 / 96.0).max(1.0);

    loop {
        unsafe {
            let mut message: MSG = zeroed();
            while PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) != 0 {
                if message.message == WM_HOTKEY {
                    stop.store(true, Ordering::Relaxed);
                }
                DispatchMessageW(&message);
            }
        }
        for command in inbox.try_iter() {
            match command {
                Command::Quit => return stats,
                Command::Move { agent, to, arrived } => {
                    if !agents.iter().any(|a| a.name == agent) {
                        let taken: Vec<Rgb> = agents.iter().map(|a| a.colour).collect();
                        agents.extend(Agent::new(&agent, to, &taken, scale));
                    }
                    if let Some(found) = agents.iter_mut().find(|a| a.name == agent) {
                        found.aside = aside;
                        found.fly(to, arrived);
                        stats.flights += 1;
                    }
                }
                Command::Jump { agent, to } => {
                    if !agents.iter().any(|a| a.name == agent) {
                        let taken: Vec<Rgb> = agents.iter().map(|a| a.colour).collect();
                        agents.extend(Agent::new(&agent, to, &taken, scale));
                    }
                    if let Some(found) = agents.iter_mut().find(|a| a.name == agent) {
                        found.jump(to);
                    }
                }
                Command::Press { agent, target } => {
                    if let Some(found) = agents.iter_mut().find(|a| a.name == agent) {
                        found.last_active = Instant::now();
                        flashes.extend(Flash::new(found.colour, target, scale));
                        found.layer.raise();
                    }
                }
                Command::Aside { on, cleared } => {
                    aside = on;
                    for agent in &mut agents {
                        agent.aside = on;
                        if agent.visible {
                            agent.draw();
                        }
                    }
                    for flash in &mut flashes {
                        flash.aside = on;
                        flash.step(Instant::now());
                    }
                    if on {
                        // The second flush covers a composition already under way at the first.
                        unsafe {
                            DwmFlush();
                            DwmFlush();
                        }
                    }
                    let _ = cleared.send(());
                }
            }
        }

        let now = Instant::now();
        let mut flying = false;
        let mut fading = false;
        for agent in &mut agents {
            flying |= agent.step(now);
            fading |= agent.rest(now);
        }
        flashes.retain(|flash| flash.step(now));

        if flying {
            if let Some(last) = last_frame {
                let gap = now.duration_since(last).as_secs_f32() * 1000.0;
                stats.frames += 1;
                stats.moving_ms += gap;
                stats.longest_gap_ms = stats.longest_gap_ms.max(gap);
            }
            last_frame = Some(now);
        } else {
            last_frame = None;
        }

        if flying || fading || !flashes.is_empty() {
            unsafe { DwmFlush() };
            continue;
        }
        let wake = agents
            .iter()
            .filter(|a| a.visible)
            .map(|a| (REST_AFTER.saturating_sub(now.duration_since(a.last_active))).as_millis() as u32 + 1)
            .min()
            .unwrap_or(INFINITE);
        unsafe { MsgWaitForMultipleObjects(0, std::ptr::null(), 0, wake, QS_ALLINPUT) };
    }
}
