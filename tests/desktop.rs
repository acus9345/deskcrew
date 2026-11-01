// Drives real windows, so it only runs when asked:
//   cargo test --release --test desktop -- --ignored --nocapture
// Every window it touches is a throwaway it opened itself.
#![cfg(windows)]

use std::{
    env, fs,
    sync::atomic::{AtomicUsize, Ordering},
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, POINT},
    UI::{
        Input::KeyboardAndMouse::{KEYEVENTF_KEYUP, VK_CONTROL, VK_ESCAPE, VK_MENU, keybd_event},
        WindowsAndMessaging::{
            CallNextHookEx, DispatchMessageW, EnumWindows, GetClassNameW, GetCursorPos, GetForegroundWindow, GetMessageW, GetWindowDisplayAffinity, GetWindowRect,
            IsWindowVisible, MSG, MSLLHOOKSTRUCT, SetWindowsHookExW,
        },
    },
};

const EXE: &str = env!("CARGO_BIN_EXE_deskcrew");

/// Every synthetic mouse event on the machine: the proof that nobody took the person's cursor.
static SYNTHETIC: AtomicUsize = AtomicUsize::new(0);

unsafe extern "system" fn watch_mouse(code: i32, wparam: usize, lparam: isize) -> isize {
    unsafe {
        if code >= 0 && (*(lparam as *const MSLLHOOKSTRUCT)).flags & 1 != 0 {
            SYNTHETIC.fetch_add(1, Ordering::SeqCst);
        }
        CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
    }
}

fn watch_for_synthetic_mouse() {
    thread::spawn(|| unsafe {
        SetWindowsHookExW(14, Some(watch_mouse), std::ptr::null_mut(), 0);
        let mut message: MSG = std::mem::zeroed();
        while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
            DispatchMessageW(&message);
        }
    });
    thread::sleep(Duration::from_millis(200));
}

struct Report {
    passed: usize,
    failed: Vec<String>,
}

impl Report {
    fn check(&mut self, name: &str, ok: bool, detail: impl AsRef<str>) {
        let line: String = detail.as_ref().replace('\n', " | ").chars().take(150).collect();
        println!("{} {name} - {line}", if ok { "ok  " } else { "FAIL" });
        if ok {
            self.passed += 1;
        } else {
            self.failed.push(name.to_string());
        }
    }
}

fn dc(agent: &str, args: &[&str]) -> (bool, String) {
    let output = Command::new(EXE).args(["--agent", agent]).args(args).output().expect("deskcrew runs");
    let mut text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    text.push_str(String::from_utf8_lossy(&output.stderr).trim());
    (output.status.success(), text)
}

struct Form {
    child: Child,
    log: PathBuf,
}

impl Form {
    fn open(work: &Path, title: &str, x: i32, y: i32) -> Form {
        let log = work.join(format!("{title}.log"));
        let script = concat!(env!("CARGO_MANIFEST_DIR"), r"\tests\form.ps1");
        let child = Command::new("powershell.exe")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-STA", "-File", script, "-Title", title])
            .arg("-Log")
            .arg(&log)
            .args(["-X", &x.to_string(), "-Y", &y.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("form opens");
        let form = Form { child, log };
        let started = Instant::now();
        while !form.logged("ready") && started.elapsed() < Duration::from_secs(10) {
            thread::sleep(Duration::from_millis(50));
        }
        thread::sleep(Duration::from_millis(200));
        form
    }

    fn text(&self) -> String {
        fs::read_to_string(&self.log).unwrap_or_default()
    }

    fn logged(&self, wanted: &str) -> bool {
        self.text().lines().any(|line| line.contains(wanted))
    }

    fn wait_for(&self, wanted: &str) -> bool {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(3) {
            if self.logged(wanted) {
                return true;
            }
            thread::sleep(Duration::from_millis(30));
        }
        false
    }

    fn close(self) {}
}

impl Drop for Form {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn pointer_and_focus() -> ((i32, i32), usize) {
    let mut point = POINT { x: 0, y: 0 };
    unsafe {
        GetCursorPos(&mut point);
        ((point.x, point.y), GetForegroundWindow() as usize)
    }
}

unsafe extern "system" fn gather(hwnd: HWND, list: LPARAM) -> i32 {
    unsafe {
        let mut name = [0u16; 32];
        let got = GetClassNameW(hwnd, name.as_mut_ptr(), 32);
        if String::from_utf16_lossy(&name[..got as usize]) == "deskcrew" {
            (*(list as *mut Vec<usize>)).push(hwnd as usize);
        }
    }
    1
}

fn overlay_windows() -> Vec<usize> {
    let mut list: Vec<usize> = Vec::new();
    unsafe { EnumWindows(Some(gather), &mut list as *mut _ as LPARAM) };
    list
}

fn ref_of(reply: &str, text: &str) -> String {
    reply.lines().find(|l| l.contains(text)).and_then(|l| l.split_whitespace().next()).map(|r| r.trim_start_matches(['+', '~']).to_string()).unwrap_or_default()
}

struct Picture {
    pixels: Vec<u8>,
    line: usize,
    width: u32,
}

fn picture(path: &str) -> Picture {
    let decoder = png::Decoder::new(fs::File::open(path).expect("shot exists"));
    let mut reader = decoder.read_info().expect("png header");
    let mut pixels = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut pixels).expect("png body");
    Picture { pixels, line: info.line_size, width: info.width }
}

impl Picture {
    fn rgb(&self, x: u32, y: u32) -> (u8, u8, u8) {
        let at = (y as usize) * self.line + (x as usize) * 3;
        (self.pixels[at], self.pixels[at + 1], self.pixels[at + 2])
    }
}

fn press_stop_hotkey() {
    unsafe {
        keybd_event(VK_CONTROL as u8, 0, 0, 0);
        keybd_event(VK_MENU as u8, 0, 0, 0);
        keybd_event(VK_ESCAPE as u8, 0, 0, 0);
        keybd_event(VK_ESCAPE as u8, 0, KEYEVENTF_KEYUP, 0);
        keybd_event(VK_MENU as u8, 0, KEYEVENTF_KEYUP, 0);
        keybd_event(VK_CONTROL as u8, 0, KEYEVENTF_KEYUP, 0);
    }
}

#[test]
#[ignore = "drives real windows"]
fn everything_on_a_real_desktop() {
    let stamp = format!("{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() & 0xFFFFFF);
    let work = env::temp_dir().join(format!("deskcrew-e2e-{stamp}"));
    fs::create_dir_all(&work).unwrap();
    let mut r = Report { passed: 0, failed: Vec::new() };
    let _ = dc("t", &["quit"]);
    thread::sleep(Duration::from_millis(300));
    watch_for_synthetic_mouse();

    let title = format!("deskcrew-a-{stamp}");
    let form = Form::open(&work, &title, 200, 120);
    let cover = Form::open(&work, &format!("deskcrew-cover-{stamp}"), 200, 120);
    let a = "Claude Code";
    // Measured around one action at a time, so a person using the mouse meanwhile cannot fail it.
    let unmoved = |action: &[&str]| {
        let mut last = String::new();
        for _ in 0..6 {
            let before = pointer_and_focus();
            let injected = SYNTHETIC.load(Ordering::SeqCst);
            let reply = dc(a, action);
            thread::sleep(Duration::from_millis(60));
            let after = pointer_and_focus();
            let clean = SYNTHETIC.load(Ordering::SeqCst) == injected;
            if (after == before && clean) || !reply.0 {
                return (after == before && clean, reply.1);
            }
            last = format!("{before:?} became {after:?}, synthetic {}: {}", SYNTHETIC.load(Ordering::SeqCst) - injected, reply.1);
            thread::sleep(Duration::from_millis(700));
        }
        (false, last)
    };

    // Reading.
    let started = Instant::now();
    let (ok, listing) = dc(a, &["see", &title]);
    r.check("the first command starts the service and returns", ok && listing.contains("Alpha") && started.elapsed() < Duration::from_secs(5), format!("{} ms", started.elapsed().as_millis()));
    r.check("no title bar or scrollbar noise in a read", !listing.contains("Minimize") && !listing.contains("最小化") && !listing.contains("thumb"), "");
    let alpha = ref_of(&listing, "\"Alpha\"");
    let beta = ref_of(&listing, "\"Beta\"");
    let (_, again) = dc(a, &["see"]);
    r.check("reading again with nothing changed says so", again == "no change", &again);
    let (_, full) = dc(a, &["see", "-f"]);
    r.check("-f gives the whole list back", full.contains("Alpha"), format!("{} chars", full.len()));
    let (_, found) = dc(a, &["see", "--find", "wrap"]);
    r.check("--find narrows the list", found.lines().count() == 2 && found.contains("Wrap"), &found);

    // Background actions on a window that is covered by another.
    let (ok, reply) = dc(a, &["click", &alpha]);
    r.check("click reaches a covered window", ok && form.wait_for("click Alpha"), &reply);
    r.check("and the reply is only what changed", reply.lines().count() <= 3 && reply.contains("Pressed Alpha"), &reply);
    let (ok, reply) = dc(a, &["click", "Beta"]);
    r.check("a control can be clicked by name with no read at all", ok && form.wait_for("click Beta"), &reply);
    let (ok, reply) = dc(a, &["click", &alpha, &beta]);
    r.check("several refs run as one command", ok && reply.starts_with("ok 2 steps") && form.text().matches("click Alpha").count() == 2, &reply);
    let (ok, reply) = dc(a, &["do", "click Wrap; set Field 'hello there'; key tab"]);
    r.check("do runs mixed steps with one reply", ok && reply.starts_with("ok 3 steps") && form.wait_for("wrap True") && form.wait_for("field hello there"), &reply);
    let field = ref_of(&dc(a, &["see", "--find", "hello"]).1, "hello");
    let (_, text) = dc(a, &["get", &field]);
    r.check("get returns a control's text", text == "hello there", &text);
    let (ok, reply) = dc(a, &["select", "#Colour", "Green"]);
    r.check("select picks a dropdown entry", ok && form.wait_for("colour Green"), &reply);
    let row = ref_of(&dc(a, &["see", "--find", "Row 2"]).1, "Row 2");
    let (ok, reply) = dc(a, &["click", &row]);
    r.check("a list row can be selected", ok && form.wait_for("row Row 2"), &reply);
    let (ok, reply) = dc(a, &["scroll", "#Rows", "down", "2"]);
    r.check("a list scrolls", ok, &reply);
    let (ok, reply) = dc(a, &["click", "File", "Save"]);
    r.check("a menu opens and its item works", ok && form.wait_for("menu Save"), &reply);
    let (ok, reply) = dc(a, &["key", "ctrl+s"]);
    r.check("a shortcut without --real says how to do it", !ok && reply.contains("--real"), &reply);
    let (ok, reply) = dc(a, &["key", "alt+f4", "--real"]);
    r.check("a risky shortcut is refused", !ok && reply.contains("refused"), &reply);
    let (same, reply) = unmoved(&["click", "Alpha"]);
    r.check("background work leaves the pointer and focus alone", same, &reply);
    let (same, reply) = unmoved(&["click", "Classic"]);
    r.check("a classic menu lists its entries without opening or a mouse", same && reply.contains("Pick"), &reply);
    let (same, reply) = unmoved(&["click", "Pick"]);
    r.check("and its entry runs with no mouse at all", same && form.wait_for("menu Pick"), &reply);
    let (ok, reply) = dc(a, &["click", "Unavailable"]);
    r.check("a greyed menu entry is refused", !ok && reply.contains("disabled"), &reply);

    // Stable refs.
    let (_, reply) = dc(a, &["click", "Rebuild"]);
    r.check("a rebuilt control keeps its ref", form.wait_for("rebuilt") && !reply.contains(&format!("- {beta}")), &reply);
    let (ok, reply) = dc(a, &["click", &beta]);
    r.check("and that ref still works", ok && form.text().matches("click Beta").count() >= 3, &reply);
    let (_, reply) = dc(a, &["click", "Vanish"]);
    r.check("a control that disappears is reported", reply.contains(&format!("- {beta}")), &reply);
    let (ok, reply) = dc(a, &["click", &beta]);
    r.check("and its ref says it is gone", !ok && reply.contains("gone"), &reply);

    // Dialogs.
    let (ok, reply) = dc(a, &["click", "Dialog"]);
    r.check("a click that opens a modal dialog still returns", ok && form.wait_for("dialog open"), &reply);
    r.check("and the dialog is announced with its controls", reply.contains("+ window") && reply.contains("Are you sure"), &reply);
    let ok_button = reply.lines().find(|l| l.contains("button")).and_then(|l| l.split_whitespace().nth(1)).unwrap_or("").to_string();
    let (ok, reply) = dc(a, &["click", &ok_button]);
    r.check("the dialog can be answered", ok && form.wait_for("dialog closed"), &reply);

    // Pictures of a covered window.
    let (ok, shot) = dc(a, &["shot"]);
    let path = shot.split(".png").next().unwrap_or("").to_string() + ".png";
    r.check("a covered window can be pictured", ok && shot.contains("scale"), &shot);
    let (_, unchanged) = dc(a, &["shot"]);
    r.check("the same picture is not produced twice", unchanged == "unchanged", &unchanged);
    let pad_pixel = picture(&path).rgb(80, 300);
    r.check("the picture shows the window, not what covers it", pad_pixel == (250, 240, 0), format!("{pad_pixel:?}"));
    let (_, changed) = dc(a, &["do", "set Field 'tiny'"]);
    let (ok, part) = dc(a, &["shot", "--changed"]);
    r.check("--changed pictures only what changed", ok && part.contains("changed part"), format!("{changed} {part}"));

    // A canvas: nothing but mouse messages.
    let _ = dc(a, &["shot", "-f"]);
    let full_picture = picture(&path);
    let left_edge = (0..full_picture.width).find(|x| full_picture.rgb(*x, 300) == (250, 240, 0)).unwrap_or(0) as f32;
    let (ok, reply) = dc(a, &["click", "100,300"]);
    r.check("a canvas click goes by window message in the background", ok && reply.starts_with("ok message") && form.wait_for("down Left"), &reply);
    let down: Vec<f32> = form.text().lines().rev().find(|l| l.starts_with("down Left")).unwrap_or("").split(' ').nth(2).unwrap_or("0,0").split(',').map(|n| n.parse().unwrap_or(0.0)).collect();
    let expected = 100.0 - left_edge;
    r.check("and lands where the picture said", (down[0] - expected).abs() < 3.0 && down[1] > 0.0, format!("{down:?} expected x {expected}"));
    let (same, reply) = unmoved(&["click", "100,300"]);
    r.check("a message click moves nothing either", same, &reply);

    // Real input, which has to borrow the pointer and give it back.
    let (same, reply) = unmoved(&["click", "120,320", "--real"]);
    r.check("--real presses without the mouse", same && reply.starts_with("ok message"), &reply);
    let (ok, reply) = dc(a, &["click", "120,320", "--real", "--double"]);
    r.check("--real can double-click", ok && form.wait_for("double"), &reply);
    let (ok, reply) = dc(a, &["click", "4000,4000", "--real"]);
    r.check("a real click outside the window is refused", !ok && reply.contains("refused"), &reply);
    let before_outside = form.text().matches("down").count();
    let (ok, reply) = dc(a, &["draw", "60,250 4000,250"]);
    r.check("a stroke leaving the window is refused before it starts", !ok && reply.contains("refused") && form.text().matches("down").count() == before_outside, &reply);
    let lifted = form.text().matches("up Left").count();
    let (same, reply) = unmoved(&["drag", "60,250", "300,330"]);
    r.check("drag needs no mouse in a classic window", same && reply.starts_with("ok message") && form.text().matches("up Left").count() > lifted, &reply);
    let (ok, reply) = dc(a, &["drag", "60,250", "300,330", "--mouse"]);
    r.check("--mouse walks the real mouse", ok && reply.starts_with("ok mouse") && form.wait_for("moves="), &reply);
    let moves: u32 = form.text().lines().rev().find(|l| l.contains("moves=")).and_then(|l| l.split("moves=").nth(1)).and_then(|n| n.trim().parse().ok()).unwrap_or(0);
    r.check("through many points, not a jump", moves > 20, format!("{moves} moves"));
    let (ok, reply) = dc(a, &["draw", "60,250 200,300 300,250; 60,330 300,330", "--smooth"]);
    r.check("draw does several strokes", ok && form.text().matches("up Left").count() >= 4, &reply);
    let (ok, reply) = dc(a, &["type", "#Field", "typed", "--real"]);
    r.check("--real types with the real keyboard", ok && form.wait_for("typed"), &reply);
    let (ok, reply) = dc(a, &["key", "ctrl+a", "--real"]);
    r.check("--real can press a shortcut", ok, &reply);
    let before = pointer_and_focus();
    let (ok, reply) = dc(a, &["click", "120,320", "--mouse"]);
    thread::sleep(Duration::from_millis(100));
    r.check("--mouse puts the pointer and focus back", ok && pointer_and_focus() == before, &reply);

    // The overlay.
    let windows = overlay_windows();
    r.check("each agent has a drawn cursor", !windows.is_empty(), format!("{} layers", windows.len()));
    let excluded = windows.iter().filter(|h| {
        let mut affinity = 0;
        unsafe { GetWindowDisplayAffinity(**h as HWND, &mut affinity) != 0 && affinity != 0 }
    });
    r.check("nothing is hidden from capture, which stops NVIDIA recordings", excluded.count() == 0, "");
    let (ok, _) = dc(a, &["click", "Alpha"]);
    let mut rect = unsafe { std::mem::zeroed() };
    let visible = overlay_windows().iter().filter(|h| unsafe { IsWindowVisible(**h as HWND) != 0 }).any(|h| unsafe {
        GetWindowRect(*h as HWND, &mut rect);
        rect.left > 150 && rect.left < 700 && rect.top > 100 && rect.top < 500
    });
    r.check("the cursor is drawn on the control it pressed", ok && visible, format!("{} {}", rect.left, rect.top));

    // Agents side by side.
    let second = format!("deskcrew-b-{stamp}");
    let third = format!("deskcrew-c-{stamp}");
    let form_b = Form::open(&work, &second, 860, 120);
    let form_c = Form::open(&work, &third, 860, 560);
    let script = |n: usize| (0..n).map(|_| "click Alpha").collect::<Vec<_>>().join("; ");
    let started = Instant::now();
    let solo = script(6);
    let (ok, _) = dc("Cursor", &["do", &solo, "-w", &second]);
    let one_agent = started.elapsed();
    r.check("one agent alone, for comparison", ok, format!("{} ms", one_agent.as_millis()));
    let started = Instant::now();
    thread::scope(|scope| {
        for (agent, window) in [("Cursor", second.clone()), ("fx", third.clone()), ("Claude Code", title.clone())] {
            let solo = &solo;
            scope.spawn(move || dc(agent, &["do", solo, "-w", &window]));
        }
    });
    let together = started.elapsed();
    r.check("three agents in three windows run at once", together < one_agent * 2, format!("{} ms vs {} ms alone", together.as_millis(), one_agent.as_millis()));
    r.check("and nothing crossed between them", form_b.text().matches("click Alpha").count() == 12 && form_c.text().matches("click Alpha").count() == 6, "12 and 6 presses");
    let before = form_b.text().matches("click Beta").count();
    let started = Instant::now();
    thread::scope(|scope| {
        for agent in ["one", "two"] {
            let (solo, second) = (&solo, &second);
            scope.spawn(move || dc(agent, &["do", &solo.replace("Alpha", "Beta"), "-w", second]));
        }
    });
    r.check("two agents in one window take turns", form_b.text().matches("click Beta").count() == before + 12 && started.elapsed() > one_agent, format!("{} ms", started.elapsed().as_millis()));
    let (_, status) = dc(a, &["status"]);
    r.check("each agent is tracked by name", status.contains("Cursor") && status.contains("fx"), &status);

    // The emergency stop.
    press_stop_hotkey();
    thread::sleep(Duration::from_millis(200));
    let (ok, reply) = dc(a, &["click", "Alpha"]);
    r.check("Ctrl+Alt+Esc stops every agent", !ok && reply.contains("stopped by the person"), &reply);
    let (ok, reply) = dc(a, &["see", "-f"]);
    r.check("reading still works while stopped", ok && reply.contains("Alpha"), format!("{} chars", reply.len()));
    let _ = dc(a, &["resume"]);
    let (ok, reply) = dc(a, &["click", "Alpha"]);
    r.check("resume lets work go on", ok, &reply);

    // A control that can only be clicked by the real mouse is refused, not clicked with it.
    let _ = dc("Cursor", &["see", "-w", &second]);
    let _ = dc("Cursor", &["shot", "-f"]);
    let (ok, reply) = dc("Cursor", &["click", "100,300", "--right"]);
    r.check("a right click opens a classic popup", ok && reply.contains("Popped"), &reply);
    let synthetic = SYNTHETIC.load(Ordering::SeqCst);
    let (ok, reply) = dc("Cursor", &["click", "Popped"]);
    thread::sleep(Duration::from_millis(100));
    r.check("the shield refuses a control that would take the mouse", !ok && reply.contains("--mouse"), &reply);
    r.check("and the cursor was not touched", SYNTHETIC.load(Ordering::SeqCst) == synthetic, format!("{}", SYNTHETIC.load(Ordering::SeqCst) - synthetic));

    // Windows closing, and the service.
    form_c.close();
    thread::sleep(Duration::from_millis(400));
    let (ok, reply) = dc("fx", &["click", "Alpha"]);
    r.check("a closed window is reported plainly", !ok && reply.contains("closed"), &reply);
    let (ok, reply) = dc(a, &["see", "no-such-window"]);
    r.check("an unknown window is named", !ok && reply.contains("no-such-window"), &reply);
    let (ok, reply) = dc(a, &["launch", "no-such-app-xyz"]);
    r.check("launching an unknown app is refused", !ok && reply.contains("no app named"), &reply);
    let (ok, _) = dc(a, &["clip", "set", "deskcrew clipboard"]);
    let saved = (ok, dc(a, &["clip"]).1);
    r.check("the clipboard can be set and read", saved.1 == "deskcrew clipboard", &saved.1);
    let mut stdout = Command::new(EXE).args(["--agent", a, "status"]).stdout(Stdio::piped()).spawn().unwrap();
    let mut piped = String::new();
    stdout.stdout.take().unwrap().read_to_string(&mut piped).unwrap();
    let _ = stdout.wait();
    r.check("output arrives on a pipe and the pipe closes", piped.contains("running"), &piped);

    form.close();
    cover.close();
    form_b.close();
    let _ = dc(a, &["quit"]);
    println!("{} passed, {} failed", r.passed, r.failed.len());
    assert!(r.failed.is_empty(), "failed: {:?}", r.failed);
}
