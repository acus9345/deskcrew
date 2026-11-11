//! A throwaway X11 window for testing deskcrew on Linux where no GUI toolkit is installed.
//! It draws itself with plain X11 and answers accessibility (AT-SPI) requests itself, so it needs
//! only an X display and a D-Bus daemon for the accessibility bus (AT_SPI_BUS_ADDRESS).
//! It prints what reaches it, like scripts/linux-form.py does for GTK.
//!
//!     cargo build --release --example linux_form
//!     AT_SPI_BUS_ADDRESS=... DISPLAY=:42 target/release/examples/linux_form

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("linux_form is a Linux test window");
}

#[cfg(target_os = "linux")]
fn main() {
    form::run();
}

#[cfg(target_os = "linux")]
mod form {
    use std::sync::{Arc, Mutex};
    use x11rb::{
        connection::Connection,
        protocol::{
            Event,
            xproto::{self, AtomEnum, ConnectionExt as _, CreateGCAux, CreateWindowAux, EventMask, PropMode, Rectangle, WindowClass},
        },
        rust_connection::RustConnection,
        wrapper::ConnectionExt as _,
    };
    use zbus::{interface, zvariant::OwnedObjectPath};

    const TITLE: &str = "Deskcrew Form";
    const SIZE: (u16, u16) = (420, 260);
    const ROOT: &str = "/org/a11y/atspi/accessible/root";

    // Where each part sits in the window.
    const NAME_BOX: (i32, i32, i32, i32) = (140, 20, 240, 26);
    const GIFT_BOX: (i32, i32, i32, i32) = (140, 64, 18, 18);
    const SUBMIT_BOX: (i32, i32, i32, i32) = (140, 100, 100, 30);
    const RESULT_BOX: (i32, i32, i32, i32) = (140, 150, 260, 20);

    // AtspiStateType bits.
    const EDITABLE: u32 = 7;
    const CHECKED: u32 = 4;
    const ENABLED: u32 = 8;
    const FOCUSABLE: u32 = 11;
    const FOCUSED: u32 = 12;
    const SENSITIVE: u32 = 24;
    const SHOWING: u32 = 25;
    const SINGLE_LINE: u32 = 26;
    const VISIBLE: u32 = 30;
    const ACTIVE: u32 = 1;

    #[derive(Default)]
    struct Model {
        name: String,
        gift: bool,
        result: String,
        name_focused: bool,
        select_all: bool,
    }

    struct Shared {
        model: Mutex<Model>,
        conn: RustConnection,
        window: u32,
        gc: u32,
        font: bool,
    }

    impl Shared {
        fn origin(&self) -> (i32, i32) {
            self.conn
                .translate_coordinates(self.window, self.conn.setup().roots[0].root, 0, 0)
                .ok()
                .and_then(|c| c.reply().ok())
                .map_or((0, 0), |r| (r.dst_x as i32, r.dst_y as i32))
        }

        fn submit(&self) {
            let mut m = self.model.lock().unwrap();
            m.result = format!("Ordered for {}, gift wrap {}", m.name, if m.gift { "yes" } else { "no" });
            println!("{}", m.result);
        }

        fn draw(&self) {
            let m = self.model.lock().unwrap();
            let c = &self.conn;
            let w = self.window;
            let rect = |r: (i32, i32, i32, i32)| Rectangle { x: r.0 as i16, y: r.1 as i16, width: r.2 as u16, height: r.3 as u16 };
            let _ = c.clear_area(false, w, 0, 0, SIZE.0, SIZE.1);
            let _ = c.poly_rectangle(w, self.gc, &[rect(NAME_BOX), rect(GIFT_BOX), rect(SUBMIT_BOX)]);
            if m.gift {
                let (x, y, s, _) = GIFT_BOX;
                let _ = c.poly_fill_rectangle(w, self.gc, &[rect((x + 4, y + 4, s - 7, s - 7))]);
            }
            if self.font {
                let text = |x: i32, y: i32, s: &str| {
                    let bytes: Vec<u8> = s.chars().map(|ch| if ch.is_ascii() { ch as u8 } else { b'?' }).take(250).collect();
                    let _ = c.image_text8(w, self.gc, x as i16, y as i16, &bytes);
                };
                text(20, 38, "Name");
                text(NAME_BOX.0 + 6, 38, &format!("{}{}", m.name, if m.name_focused { "|" } else { "" }));
                text(GIFT_BOX.0 + 26, 78, "Gift wrap");
                text(SUBMIT_BOX.0 + 26, 120, "Submit");
                text(RESULT_BOX.0, RESULT_BOX.1 + 14, if m.result.is_empty() { "No order yet" } else { &m.result });
            }
            let _ = c.flush();
        }
    }

    #[derive(Clone, Copy, PartialEq)]
    enum Part {
        Root,
        Frame,
        Name,
        Gift,
        Submit,
        Result,
    }

    const PARTS: [Part; 4] = [Part::Name, Part::Gift, Part::Submit, Part::Result];

    impl Part {
        fn path(self) -> &'static str {
            match self {
                Part::Root => ROOT,
                Part::Frame => "/deskcrew/form/frame",
                Part::Name => "/deskcrew/form/name",
                Part::Gift => "/deskcrew/form/gift",
                Part::Submit => "/deskcrew/form/submit",
                Part::Result => "/deskcrew/form/result",
            }
        }

        fn rect(self) -> (i32, i32, i32, i32) {
            match self {
                Part::Root | Part::Frame => (0, 0, SIZE.0 as i32, SIZE.1 as i32),
                Part::Name => NAME_BOX,
                Part::Gift => (GIFT_BOX.0, GIFT_BOX.1, 120, GIFT_BOX.3),
                Part::Submit => SUBMIT_BOX,
                Part::Result => RESULT_BOX,
            }
        }
    }

    fn reference(conn: &zbus::Connection, part: Part) -> (String, OwnedObjectPath) {
        let bus = conn.unique_name().map(|n| n.to_string()).unwrap_or_default();
        (bus, OwnedObjectPath::try_from(part.path()).unwrap())
    }

    struct Accessible(Part, Arc<Shared>);

    #[interface(name = "org.a11y.atspi.Accessible")]
    impl Accessible {
        #[zbus(property)]
        fn name(&self) -> String {
            let m = self.1.model.lock().unwrap();
            match self.0 {
                Part::Root => "linux_form".into(),
                Part::Frame => TITLE.into(),
                Part::Name => "Name".into(),
                Part::Gift => "Gift wrap".into(),
                Part::Submit => "Submit".into(),
                Part::Result => if m.result.is_empty() { "No order yet".into() } else { m.result.clone() },
            }
        }

        #[zbus(property)]
        fn description(&self) -> String {
            String::new()
        }

        #[zbus(property)]
        fn child_count(&self) -> i32 {
            match self.0 {
                Part::Root => 1,
                Part::Frame => PARTS.len() as i32,
                _ => 0,
            }
        }

        #[zbus(property)]
        fn parent(&self, #[zbus(connection)] conn: &zbus::Connection) -> (String, OwnedObjectPath) {
            match self.0 {
                Part::Root => (String::new(), OwnedObjectPath::try_from("/org/a11y/atspi/null").unwrap()),
                Part::Frame => reference(conn, Part::Root),
                _ => reference(conn, Part::Frame),
            }
        }

        fn get_children(&self, #[zbus(connection)] conn: &zbus::Connection) -> Vec<(String, OwnedObjectPath)> {
            match self.0 {
                Part::Root => vec![reference(conn, Part::Frame)],
                Part::Frame => PARTS.iter().map(|p| reference(conn, *p)).collect(),
                _ => Vec::new(),
            }
        }

        fn get_index_in_parent(&self) -> i32 {
            PARTS.iter().position(|p| *p == self.0).map_or(0, |at| at as i32)
        }

        fn get_role_name(&self) -> String {
            match self.0 {
                Part::Root => "application",
                Part::Frame => "frame",
                Part::Name => "text",
                Part::Gift => "check box",
                Part::Submit => "push button",
                Part::Result => "label",
            }
            .into()
        }

        fn get_state(&self) -> Vec<u32> {
            let m = self.1.model.lock().unwrap();
            let mut bits: u64 = 0;
            let mut on = |bit: u32| bits |= 1u64 << bit;
            for bit in [ENABLED, SENSITIVE, SHOWING, VISIBLE] {
                on(bit);
            }
            match self.0 {
                Part::Frame => on(ACTIVE),
                Part::Name => {
                    on(EDITABLE);
                    on(FOCUSABLE);
                    on(SINGLE_LINE);
                    if m.name_focused {
                        on(FOCUSED);
                    }
                }
                Part::Gift if m.gift => on(CHECKED),
                _ => {}
            }
            vec![bits as u32, (bits >> 32) as u32]
        }

        fn get_interfaces(&self) -> Vec<String> {
            let mut out = vec!["org.a11y.atspi.Accessible".to_string(), "org.a11y.atspi.Component".to_string()];
            match self.0 {
                Part::Name => out.extend(["org.a11y.atspi.Text".to_string(), "org.a11y.atspi.EditableText".to_string()]),
                Part::Gift | Part::Submit => out.push("org.a11y.atspi.Action".to_string()),
                Part::Root => out.push("org.a11y.atspi.Application".to_string()),
                _ => {}
            }
            out
        }
    }

    struct Component(Part, Arc<Shared>);

    #[interface(name = "org.a11y.atspi.Component")]
    impl Component {
        fn get_extents(&self, _coord_type: u32) -> (i32, i32, i32, i32) {
            let (ox, oy) = self.1.origin();
            let (x, y, w, h) = self.0.rect();
            (ox + x, oy + y, w, h)
        }

        fn grab_focus(&self) -> bool {
            if self.0 != Part::Name {
                return false;
            }
            self.1.model.lock().unwrap().name_focused = true;
            self.1.draw();
            true
        }
    }

    struct Text(Arc<Shared>);

    #[interface(name = "org.a11y.atspi.Text")]
    impl Text {
        fn get_text(&self, start: i32, end: i32) -> String {
            let text = self.0.model.lock().unwrap().name.clone();
            let count = text.chars().count() as i32;
            let end = if end < 0 || end > count { count } else { end };
            text.chars().skip(start.max(0) as usize).take((end - start.max(0)).max(0) as usize).collect()
        }

        #[zbus(property)]
        fn character_count(&self) -> i32 {
            self.0.model.lock().unwrap().name.chars().count() as i32
        }

        #[zbus(property)]
        fn caret_offset(&self) -> i32 {
            self.0.model.lock().unwrap().name.chars().count() as i32
        }

        fn set_caret_offset(&self, _offset: i32) -> bool {
            true
        }
    }

    struct EditableText(Arc<Shared>);

    #[interface(name = "org.a11y.atspi.EditableText")]
    impl EditableText {
        fn set_text_contents(&self, text: String) -> bool {
            self.0.model.lock().unwrap().name = text;
            self.0.draw();
            true
        }

        fn insert_text(&self, position: i32, text: String, _length: i32) -> bool {
            {
                let mut m = self.0.model.lock().unwrap();
                let mut chars: Vec<char> = m.name.chars().collect();
                let at = (position.max(0) as usize).min(chars.len());
                chars.splice(at..at, text.chars());
                m.name = chars.into_iter().collect();
            }
            self.0.draw();
            true
        }
    }

    struct Action(Part, Arc<Shared>);

    #[interface(name = "org.a11y.atspi.Action")]
    impl Action {
        fn get_actions(&self) -> Vec<(String, String, String)> {
            let name = if self.0 == Part::Gift { "toggle" } else { "click" };
            vec![(name.into(), String::new(), String::new())]
        }

        #[zbus(property)]
        fn n_actions(&self) -> i32 {
            1
        }

        fn do_action(&self, index: i32) -> bool {
            if index != 0 {
                return false;
            }
            match self.0 {
                Part::Gift => {
                    let mut m = self.1.model.lock().unwrap();
                    m.gift = !m.gift;
                }
                Part::Submit => self.1.submit(),
                _ => return false,
            }
            self.1.draw();
            true
        }
    }

    fn inside(r: (i32, i32, i32, i32), x: i32, y: i32) -> bool {
        x >= r.0 && y >= r.1 && x < r.0 + r.2 && y < r.1 + r.3
    }

    pub fn run() {
        let (conn, screen) = x11rb::connect(None).expect("no X display: set DISPLAY");
        let root = conn.setup().roots[screen].clone();
        let window = conn.generate_id().unwrap();
        let events = EventMask::EXPOSURE | EventMask::KEY_PRESS | EventMask::BUTTON_PRESS | EventMask::FOCUS_CHANGE;
        conn.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            window,
            root.root,
            60,
            60,
            SIZE.0,
            SIZE.1,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new().background_pixel(root.white_pixel).event_mask(events),
        )
        .unwrap();
        let utf8 = conn.intern_atom(false, b"UTF8_STRING").unwrap().reply().unwrap().atom;
        let net_name = conn.intern_atom(false, b"_NET_WM_NAME").unwrap().reply().unwrap().atom;
        let net_pid = conn.intern_atom(false, b"_NET_WM_PID").unwrap().reply().unwrap().atom;
        conn.change_property8(PropMode::REPLACE, window, AtomEnum::WM_NAME, AtomEnum::STRING, TITLE.as_bytes()).unwrap();
        conn.change_property8(PropMode::REPLACE, window, net_name, utf8, TITLE.as_bytes()).unwrap();
        conn.change_property8(PropMode::REPLACE, window, AtomEnum::WM_CLASS, AtomEnum::STRING, b"linux_form\0LinuxForm\0").unwrap();
        conn.change_property32(PropMode::REPLACE, window, net_pid, AtomEnum::CARDINAL, &[std::process::id()]).unwrap();
        let font = conn.generate_id().unwrap();
        let has_font = conn.open_font(font, b"fixed").ok().and_then(|c| c.check().ok()).is_some();
        let gc = conn.generate_id().unwrap();
        let mut aux = CreateGCAux::new().foreground(root.black_pixel).background(root.white_pixel);
        if has_font {
            aux = aux.font(font);
        }
        conn.create_gc(gc, window, &aux).unwrap();
        conn.map_window(window).unwrap();
        conn.flush().unwrap();

        let setup = conn.setup();
        let (min, max) = (setup.min_keycode, setup.max_keycode);
        let map = conn.get_keyboard_mapping(min, max - min + 1).unwrap().reply().unwrap();
        let per = map.keysyms_per_keycode as usize;
        let syms = map.keysyms;

        let shared = Arc::new(Shared { model: Mutex::new(Model::default()), conn, window, gc, font: has_font });

        let address = std::env::var("AT_SPI_BUS_ADDRESS").expect("set AT_SPI_BUS_ADDRESS to an accessibility bus");
        let mut builder = zbus::blocking::connection::Builder::address(address.as_str()).expect("bad AT_SPI_BUS_ADDRESS");
        builder = builder.serve_at(Part::Root.path(), Accessible(Part::Root, shared.clone())).unwrap();
        builder = builder.serve_at(Part::Frame.path(), Accessible(Part::Frame, shared.clone())).unwrap();
        builder = builder.serve_at(Part::Frame.path(), Component(Part::Frame, shared.clone())).unwrap();
        for part in PARTS {
            builder = builder.serve_at(part.path(), Accessible(part, shared.clone())).unwrap();
            builder = builder.serve_at(part.path(), Component(part, shared.clone())).unwrap();
        }
        builder = builder.serve_at(Part::Name.path(), Text(shared.clone())).unwrap();
        builder = builder.serve_at(Part::Name.path(), EditableText(shared.clone())).unwrap();
        builder = builder.serve_at(Part::Gift.path(), Action(Part::Gift, shared.clone())).unwrap();
        builder = builder.serve_at(Part::Submit.path(), Action(Part::Submit, shared.clone())).unwrap();
        let _bus = builder.build().expect("could not join the accessibility bus");

        shared.draw();
        println!("ready");

        loop {
            let Ok(event) = shared.conn.wait_for_event() else { break };
            match event {
                Event::Expose(_) => shared.draw(),
                Event::ButtonPress(press) => {
                    let (x, y) = (press.event_x as i32, press.event_y as i32);
                    {
                        let mut m = shared.model.lock().unwrap();
                        m.name_focused = inside(NAME_BOX, x, y);
                        if inside(Part::Gift.rect(), x, y) {
                            m.gift = !m.gift;
                        }
                    }
                    if inside(SUBMIT_BOX, x, y) {
                        shared.submit();
                    }
                    shared.draw();
                }
                Event::KeyPress(key) => {
                    let row = (key.detail - min) as usize * per;
                    let shift = u16::from(key.state) & u16::from(xproto::KeyButMask::SHIFT) != 0;
                    let control = u16::from(key.state) & u16::from(xproto::KeyButMask::CONTROL) != 0;
                    let sym = syms.get(row + usize::from(shift)).copied().filter(|s| *s != 0).or_else(|| syms.get(row).copied()).unwrap_or(0);
                    let mut m = shared.model.lock().unwrap();
                    if !m.name_focused {
                        continue;
                    }
                    if control && (sym == 0x61 || sym == 0x41) {
                        m.select_all = true;
                    } else if sym == 0xff08 {
                        m.name.pop();
                    } else if let Some(ch) = char::from_u32(sym).filter(|c| (' '..='~').contains(c)).filter(|_| !control) {
                        if m.select_all {
                            m.name.clear();
                            m.select_all = false;
                        }
                        m.name.push(ch);
                    }
                    drop(m);
                    shared.draw();
                }
                _ => {}
            }
        }
    }
}
