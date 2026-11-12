// Each agent's drawn cursor on X11: a small window that clicks pass through, kept just above the
// window the agent works in. With a compositor it is see-through at the edges; without one, as on
// a bare Xvfb, the window is cut to the cursor's outline. The pictures and the motion are the same
// as on Windows and the Mac.
#[path = "../overlay/draw.rs"]
pub mod draw;

use crate::motion::{self, Rgb};
use draw::Mask;
use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::shape::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{
    ClipOrdering, ColormapAlloc, ConfigureWindowAux, ConnectionExt as _, CreateGCAux, CreateWindowAux, ImageFormat, Rectangle, StackMode, VisualClass,
    Window as XWindow, WindowClass,
};
use x11rb::rust_connection::RustConnection;

const FRAME: Duration = Duration::from_millis(16);
const RESTACK_EVERY: Duration = Duration::from_millis(100);
const PRESS_LINGER_MS: f32 = 450.0;
const LABEL_PX: f32 = 11.0;
/// Without a compositor a window has no see-through pixels, so it is cut to the parts at least
/// this opaque.
const CUT_ALPHA: u8 = 110;
/// A compositor redraws the screen on its own time, so a hidden cursor needs a frame to leave it.
const COMPOSITOR_SETTLE: Duration = Duration::from_millis(40);

/// Bold sans fonts that ship with most distributions, Arial's metric twins first.
const FONTS: [&str; 10] = [
    "/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf",
    "/usr/share/fonts/truetype/liberation2/LiberationSans-Bold.ttf",
    "/usr/share/fonts/liberation-sans/LiberationSans-Bold.ttf",
    "/usr/share/fonts/liberation-sans-fonts/LiberationSans-Bold.ttf",
    "/usr/share/fonts/TTF/LiberationSans-Bold.ttf",
    "/usr/share/fonts/truetype/croscore/Arimo-Bold.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
    "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans-Bold.ttf",
    "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf",
    "/usr/share/fonts/dejavu/DejaVuSans-Bold.ttf",
];

pub enum Command {
    Move { agent: String, x: i32, y: i32, instant: bool, done: Option<Sender<()>> },
    Over { agent: String, window: u32 },
    Press { agent: String, target: (i32, i32, i32, i32) },
    Type { agent: String, keys: usize },
    Aside { on: bool, done: Sender<()> },
}

#[derive(Clone)]
pub struct Overlay {
    commands: Sender<Command>,
}

static SHARED: OnceLock<Overlay> = OnceLock::new();

impl Overlay {
    /// Start drawing on the display, once. Screen copies hide the cursors from then on.
    pub fn start() -> Overlay {
        SHARED
            .get_or_init(|| {
                let (commands, inbox) = mpsc::channel();
                thread::spawn(move || run(inbox));
                Overlay { commands }
            })
            .clone()
    }

    /// The running overlay, if the service started one.
    pub fn running() -> Option<Overlay> {
        SHARED.get().cloned()
    }

    /// Glide there and return once the cursor has arrived.
    pub fn move_to(&self, agent: &str, x: i32, y: i32) {
        let (done, arrived) = mpsc::channel();
        let _ = self.commands.send(Command::Move { agent: agent.into(), x, y, instant: false, done: Some(done) });
        let _ = arrived.recv_timeout(Duration::from_secs(2));
    }

    /// Be there at once, for `--fast` and for following a stroke as it is drawn.
    pub fn jump(&self, agent: &str, x: i32, y: i32) {
        let _ = self.commands.send(Command::Move { agent: agent.into(), x, y, instant: true, done: None });
    }

    pub fn over(&self, agent: &str, window: u32) {
        let _ = self.commands.send(Command::Over { agent: agent.into(), window });
    }

    pub fn press(&self, agent: &str, target: (i32, i32, i32, i32)) {
        let _ = self.commands.send(Command::Press { agent: agent.into(), target });
    }

    /// Pulse this agent's name tag as it types this many keys.
    pub fn typing(&self, agent: &str, keys: usize) {
        let _ = self.commands.send(Command::Type { agent: agent.into(), keys });
    }

    /// Hide every cursor while the screen is copied, or bring them back. Returns once done.
    pub fn aside(&self, on: bool) {
        let (done, finished) = mpsc::channel();
        let _ = self.commands.send(Command::Aside { on, done });
        let _ = finished.recv_timeout(Duration::from_millis(500));
    }
}

/// The name in the tag's font, as coverage. Without a font the tag is drawn without a name.
pub fn text_mask(text: &str, px: f32) -> Mask {
    use ab_glyph::{Font, FontVec, PxScale, ScaleFont};
    static FONT: OnceLock<Option<FontVec>> = OnceLock::new();
    let font = FONT.get_or_init(|| FONTS.iter().find_map(|path| std::fs::read(path).ok().and_then(|bytes| FontVec::try_from_vec(bytes).ok())));
    let Some(font) = font else { return Mask { w: 1, h: 1, alpha: vec![0] } };
    let scaled = font.as_scaled(PxScale::from(px));
    let (ascent, descent) = (scaled.ascent(), scaled.descent());
    let mut glyphs = Vec::new();
    let mut pen = 0.0;
    let mut last = None;
    for c in text.chars() {
        let id = scaled.glyph_id(c);
        if let Some(before) = last {
            pen += scaled.kern(before, id);
        }
        glyphs.push(id.with_scale_and_position(scaled.scale(), ab_glyph::point(pen, ascent)));
        pen += scaled.h_advance(id);
        last = Some(id);
    }
    let (w, h) = ((pen.ceil() as i32 + 1).max(1), ((ascent - descent).ceil() as i32 + 1).max(1));
    let mut alpha = vec![0u8; (w * h) as usize];
    for glyph in glyphs {
        let Some(outline) = font.outline_glyph(glyph) else { continue };
        let bounds = outline.px_bounds();
        outline.draw(|gx, gy, coverage| {
            let (x, y) = (bounds.min.x as i32 + gx as i32, bounds.min.y as i32 + gy as i32);
            if x >= 0 && y >= 0 && x < w && y < h {
                let at = (y * w + x) as usize;
                alpha[at] = alpha[at].max((coverage * 255.0).round() as u8);
            }
        });
    }
    Mask { w, h, alpha }
}

#[cfg(test)]
pub fn label_for_test(text: &str, px: i32) -> Mask {
    text_mask(text, px as f32)
}

/// How a cursor window looks on this display.
#[derive(Clone, Copy)]
struct Look {
    root: XWindow,
    depth: u8,
    visual: u32,
    colormap: u32,
    /// Cut windows to their outline: there is no compositor to blend their edges.
    cut: bool,
}

/// A click-through window showing a picture.
struct Layer {
    window: XWindow,
    gc: u32,
    w: u16,
    h: u16,
    at: (i32, i32),
    shown: bool,
}

impl Layer {
    fn new(conn: &RustConnection, look: &Look) -> Option<Layer> {
        let window = conn.generate_id().ok()?;
        let aux = CreateWindowAux::new().override_redirect(1).background_pixel(0).border_pixel(0).colormap(look.colormap);
        conn.create_window(look.depth, window, look.root, 0, 0, 1, 1, 0, WindowClass::INPUT_OUTPUT, look.visual, &aux).ok()?;
        // An empty input shape lets every click through to what is underneath.
        conn.shape_rectangles(shape::SO::SET, shape::SK::INPUT, ClipOrdering::UNSORTED, window, 0, 0, &[]).ok()?;
        let gc = conn.generate_id().ok()?;
        conn.create_gc(gc, window, &CreateGCAux::new()).ok()?;
        Some(Layer { window, gc, w: 1, h: 1, at: (i32::MIN, i32::MIN), shown: false })
    }

    /// Show a picture with its top left at x,y.
    fn show(&mut self, conn: &RustConnection, look: &Look, picture: &tiny_skia::Pixmap, x: i32, y: i32, visible: bool) {
        let (w, h) = (picture.width() as u16, picture.height() as u16);
        if (w, h) != (self.w, self.h) || (x, y) != self.at {
            let aux = ConfigureWindowAux::new().x(x).y(y).width(w as u32).height(h as u32);
            let _ = conn.configure_window(self.window, &aux);
            (self.w, self.h, self.at) = (w, h, (x, y));
        }
        // The picture is premultiplied RGBA; X wants blue, green, red, alpha. A window with no
        // alpha channel shows its colours opaque, so edges are unmultiplied there.
        let mut bytes = Vec::with_capacity(picture.data().len());
        for pixel in picture.pixels() {
            if look.depth == 32 && !look.cut {
                bytes.extend_from_slice(&[pixel.blue(), pixel.green(), pixel.red(), pixel.alpha()]);
            } else {
                let c = pixel.demultiply();
                bytes.extend_from_slice(&[c.blue(), c.green(), c.red(), 255]);
            }
        }
        // Big pictures go in bands that fit one request.
        let row = w as usize * 4;
        let most = conn.maximum_request_bytes().saturating_sub(64).max(row);
        let band = (most / row).max(1);
        for (index, chunk) in bytes.chunks(band * row).enumerate() {
            let rows = (chunk.len() / row) as u16;
            let _ = conn.put_image(ImageFormat::Z_PIXMAP, self.window, self.gc, w, rows, 0, (index * band) as i16, 0, look.depth, chunk);
        }
        if look.cut {
            let _ = conn.shape_rectangles(shape::SO::SET, shape::SK::BOUNDING, ClipOrdering::UNSORTED, self.window, 0, 0, &outline(picture));
        }
        if visible && !self.shown {
            let _ = conn.map_window(self.window);
            self.shown = true;
        }
    }

    fn place(&mut self, conn: &RustConnection, x: i32, y: i32) {
        if (x, y) != self.at {
            let _ = conn.configure_window(self.window, &ConfigureWindowAux::new().x(x).y(y));
            self.at = (x, y);
        }
    }

    /// Just above the agent's window, or above everything if it has none.
    fn stack(&self, conn: &RustConnection, above: Option<XWindow>) {
        let aux = match above {
            Some(sibling) => ConfigureWindowAux::new().sibling(sibling).stack_mode(StackMode::ABOVE),
            None => ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
        };
        let _ = conn.configure_window(self.window, &aux);
    }

    fn hide(&mut self, conn: &RustConnection) {
        if self.shown {
            let _ = conn.unmap_window(self.window);
            self.shown = false;
        }
    }

    fn unhide(&mut self, conn: &RustConnection) {
        if !self.shown {
            let _ = conn.map_window(self.window);
            self.shown = true;
        }
    }

    fn destroy(self, conn: &RustConnection) {
        let _ = conn.free_gc(self.gc);
        let _ = conn.destroy_window(self.window);
    }
}

/// The picture's opaque parts as runs of rectangles, row by row.
fn outline(picture: &tiny_skia::Pixmap) -> Vec<Rectangle> {
    let w = picture.width() as usize;
    let mut runs = Vec::new();
    for (y, row) in picture.pixels().chunks(w).enumerate() {
        let mut start = None;
        for (x, pixel) in row.iter().enumerate().chain(std::iter::once((w, &tiny_skia::PremultipliedColorU8::TRANSPARENT))) {
            match (pixel.alpha() >= CUT_ALPHA, start) {
                (true, None) => start = Some(x),
                (false, Some(from)) => {
                    runs.push(Rectangle { x: from as i16, y: y as i16, width: (x - from) as u16, height: 1 });
                    start = None;
                }
                _ => {}
            }
        }
    }
    runs
}

struct Glide {
    from: motion::Point,
    middle: motion::Point,
    to: motion::Point,
    started: Instant,
    ms: u32,
    done: Option<Sender<()>>,
}

struct Agent {
    colour: Rgb,
    label: Mask,
    pose: motion::Pose,
    look: (i32, i32),
    layer: Layer,
    tip: (i32, i32),
    at: motion::Point,
    glide: Option<Glide>,
    above: Option<XWindow>,
    side: f32,
}

struct Press {
    colour: Rgb,
    target: (i32, i32, i32, i32),
    layer: Layer,
    started: Instant,
    above: Option<XWindow>,
}

/// The window the window manager put around this one, which is what sits among the screen's
/// windows: a cursor can only be stacked against that.
fn frame_of(conn: &RustConnection, root: XWindow, window: XWindow) -> XWindow {
    let mut at = window;
    for _ in 0..8 {
        let Some(tree) = conn.query_tree(at).ok().and_then(|c| c.reply().ok()) else { return at };
        if tree.parent == root || tree.parent == x11rb::NONE {
            return at;
        }
        at = tree.parent;
    }
    at
}

fn look_of(conn: &RustConnection, screen: usize) -> Option<Look> {
    let setup = conn.setup().roots.get(screen)?;
    let root = setup.root;
    let compositor = conn
        .intern_atom(false, format!("_NET_WM_CM_S{screen}").as_bytes())
        .ok()
        .and_then(|c| c.reply().ok())
        .and_then(|atom| conn.get_selection_owner(atom.atom).ok())
        .and_then(|c| c.reply().ok())
        .is_some_and(|owner| owner.owner != x11rb::NONE);
    // A 32 bit visual lets a compositor blend the cursor's soft edges with what is under it.
    let argb = setup
        .allowed_depths
        .iter()
        .filter(|d| d.depth == 32)
        .flat_map(|d| d.visuals.iter())
        .find(|v| v.class == VisualClass::TRUE_COLOR)
        .map(|v| v.visual_id);
    match argb {
        Some(visual) if compositor => {
            let colormap = conn.generate_id().ok()?;
            conn.create_colormap(ColormapAlloc::NONE, colormap, root, visual).ok()?;
            Some(Look { root, depth: 32, visual, colormap, cut: false })
        }
        _ => Some(Look { root, depth: setup.root_depth, visual: setup.root_visual, colormap: setup.default_colormap, cut: true }),
    }
}

fn run(inbox: Receiver<Command>) {
    let Ok((conn, screen)) = x11rb::connect(None) else { return };
    let Some(look) = look_of(&conn, screen) else { return };
    let scale = 1.0;
    let mut agents: HashMap<String, Agent> = HashMap::new();
    let mut presses: Vec<Press> = Vec::new();
    let mut aside = false;
    let mut restacked = Instant::now();
    loop {
        let Ok(first) = inbox.recv_timeout(FRAME) else {
            if matches!(inbox.try_recv(), Err(mpsc::TryRecvError::Disconnected)) {
                return;
            }
            tick(&conn, &look, scale, &mut agents, &mut presses, aside, &mut restacked);
            continue;
        };
        for command in std::iter::once(first).chain(inbox.try_iter()) {
            match command {
                Command::Move { agent, x, y, instant, done } => {
                    let taken: Vec<Rgb> = agents.values().map(|a| a.colour).collect();
                    let side = if agents.len().is_multiple_of(2) { 1.0 } else { -1.0 };
                    if !agents.contains_key(&agent) {
                        let colour = motion::colour_for(&agent, &taken);
                        let label = text_mask(&agent, LABEL_PX * scale);
                        let Some(mut layer) = Layer::new(&conn, &look) else { continue };
                        let Some((picture, tx, ty)) = draw::posed(colour, &label, scale, 0.0, 1.0) else { continue };
                        // A new cursor arrives from a little way off, as on Windows.
                        let start = (x as f32 + 90.0, y as f32 + 90.0);
                        layer.show(&conn, &look, &picture, start.0 as i32 - tx, start.1 as i32 - ty, !aside);
                        agents.insert(
                            agent.clone(),
                            Agent { colour, label, pose: motion::Pose::new(), look: (0, 0), layer, tip: (tx, ty), at: start, glide: None, above: None, side },
                        );
                    }
                    let entry = agents.get_mut(&agent).expect("just added");
                    let to = (x as f32, y as f32);
                    if instant {
                        entry.glide = None;
                        entry.at = to;
                        entry.layer.place(&conn, x - entry.tip.0, y - entry.tip.1);
                        if let Some(done) = done {
                            let _ = done.send(());
                        }
                        continue;
                    }
                    let ms = motion::travel_ms(entry.at, to);
                    entry.glide = Some(Glide { from: entry.at, middle: motion::bow(entry.at, to, entry.side), to, started: Instant::now(), ms, done });
                    entry.side = -entry.side;
                    if !aside {
                        entry.layer.stack(&conn, entry.above);
                    }
                }
                Command::Over { agent, window } => {
                    let frame = frame_of(&conn, look.root, window);
                    if let Some(entry) = agents.get_mut(&agent) {
                        entry.above = Some(frame);
                        if !aside {
                            entry.layer.stack(&conn, entry.above);
                        }
                    }
                }
                Command::Press { agent, target } => {
                    if let Some(entry) = agents.get(&agent)
                        && let Some(layer) = Layer::new(&conn, &look)
                    {
                        presses.push(Press { colour: entry.colour, target, layer, started: Instant::now(), above: entry.above });
                    }
                }
                Command::Type { agent, keys } => {
                    if let Some(entry) = agents.get_mut(&agent) {
                        entry.pose.type_keys(Instant::now(), keys);
                    }
                }
                Command::Aside { on, done } => {
                    aside = on;
                    for agent in agents.values_mut() {
                        if on { agent.layer.hide(&conn) } else { agent.layer.unhide(&conn) }
                    }
                    for press in &mut presses {
                        if on { press.layer.hide(&conn) } else { press.layer.unhide(&conn) }
                    }
                    // Once a round trip returns, the server has done everything asked before it.
                    let _ = conn.get_input_focus().ok().and_then(|c| c.reply().ok());
                    if on && !look.cut {
                        thread::sleep(COMPOSITOR_SETTLE);
                    }
                    let _ = done.send(());
                }
            }
        }
        tick(&conn, &look, scale, &mut agents, &mut presses, aside, &mut restacked);
    }
}

/// One frame: glides, poses, press effects and keeping the stack.
fn tick(conn: &RustConnection, look: &Look, scale: f32, agents: &mut HashMap<String, Agent>, presses: &mut Vec<Press>, aside: bool, restacked: &mut Instant) {
    let now = Instant::now();
    for agent in agents.values_mut() {
        if let Some(glide) = &mut agent.glide {
            let t = (glide.started.elapsed().as_millis() as f32 / glide.ms.max(1) as f32).min(1.0);
            agent.at = motion::along(glide.from, glide.middle, glide.to, motion::ease(t));
            if t >= 1.0 {
                if let Some(done) = glide.done.take() {
                    let _ = done.send(());
                }
                agent.glide = None;
            }
        }
        agent.pose.follow(now, agent.at, 1.0);
        let (left, top) = (agent.at.0.round() as i32 - agent.tip.0, agent.at.1.round() as i32 - agent.tip.1);
        let pose = agent.pose.look(now);
        if pose != agent.look {
            agent.look = pose;
            let (lean, squeeze) = agent.pose.shown(now);
            if let Some((picture, _, _)) = draw::posed(agent.colour, &agent.label, scale, lean, squeeze) {
                agent.layer.show(conn, look, &picture, left, top, !aside);
            }
        } else {
            agent.layer.place(conn, left, top);
        }
    }
    let mut index = 0;
    while index < presses.len() {
        let press = &mut presses[index];
        let ms = press.started.elapsed().as_millis() as f32;
        if ms > draw::PRESS_SETTLE_MS + PRESS_LINGER_MS {
            presses.remove(index).layer.destroy(conn);
            continue;
        }
        let margin = draw::outline_margin(scale);
        let (x, y, w, h) = press.target;
        // The press animates until it settles, then stays as it is until it goes.
        if ms <= draw::PRESS_SETTLE_MS + 2.0 * FRAME.as_millis() as f32
            && let Some(picture) = draw::press(press.colour, w, h, scale, ms.min(draw::PRESS_SETTLE_MS))
        {
            press.layer.show(conn, look, &picture, x - margin, y - margin, !aside);
            if !aside {
                press.layer.stack(conn, press.above);
            }
        }
        index += 1;
    }
    if *restacked + RESTACK_EVERY <= now && !aside {
        *restacked = now;
        for agent in agents.values() {
            agent.layer.stack(conn, agent.above);
        }
    }
    let _ = conn.flush();
}
