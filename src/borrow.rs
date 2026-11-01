// Whether an agent may borrow the person's cursor. It is off until the person says otherwise, and
// the answer comes from a prompt on screen, so an agent cannot simply grant itself the cursor.
use std::{
    collections::HashSet,
    fs,
    path::PathBuf,
    ptr::null,
    sync::{
        Mutex,
        atomic::{AtomicI32, Ordering},
    },
    thread,
};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    Graphics::Gdi::{CreateFontW, DeleteObject},
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        HiDpi::GetDpiForSystem,
        WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, GetSystemMetrics, MSG, PostQuitMessage, RegisterClassW,
            SM_CXSCREEN, SM_CYSCREEN, SW_SHOWNOACTIVATE, SendMessageW, SetTimer, ShowWindow, WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_SETFONT, WM_TIMER,
            WNDCLASSW, WS_CAPTION, WS_CHILD, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
        },
    },
};

const ANSWER_MS: u32 = 60_000;
const FIRST_BUTTON: i32 = 100;
const EVERY_AGENT: &str = "*";
const OFF: &str = "the person has not let agents borrow the cursor. They can allow it by running: deskcrew cursor ask";

static ANSWER: AtomicI32 = AtomicI32::new(-1);

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

unsafe extern "system" fn answer(hwnd: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        match message {
            WM_COMMAND => {
                ANSWER.store((w & 0xFFFF) as i32 - FIRST_BUTTON, Ordering::SeqCst);
                DestroyWindow(hwnd);
                0
            }
            WM_TIMER | WM_CLOSE => {
                DestroyWindow(hwnd);
                0
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, message, w, l),
        }
    }
}

/// Ask on screen and wait. The window never takes focus, so nothing the person is typing can answer
/// it by accident, and no answer within a minute counts as no.
fn prompt(text: &str, buttons: &[&str]) -> Option<usize> {
    let (text, buttons) = (text.to_string(), buttons.iter().map(|b| b.to_string()).collect::<Vec<_>>());
    let shown = thread::spawn(move || unsafe {
        ANSWER.store(-1, Ordering::SeqCst);
        let module = GetModuleHandleW(null());
        let class = wide("deskcrew-ask");
        let mut kind: WNDCLASSW = std::mem::zeroed();
        kind.lpfnWndProc = Some(answer);
        kind.hInstance = module;
        kind.lpszClassName = class.as_ptr();
        kind.hbrBackground = 16 as _;
        RegisterClassW(&kind);
        let scale = GetDpiForSystem() as i32;
        let px = |value: i32| value * scale / 96;
        let (width, height) = (px(520), px(170));
        let (left, top) = ((GetSystemMetrics(SM_CXSCREEN) - width) / 2, (GetSystemMetrics(SM_CYSCREEN) - height) / 2);
        let title = wide("deskcrew");
        let window = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
            class.as_ptr(),
            title.as_ptr(),
            WS_POPUP | WS_CAPTION,
            left,
            top,
            width,
            height,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            module,
            null(),
        );
        let face = wide("Segoe UI");
        let font = CreateFontW(px(18), 0, 0, 0, 400, 0, 0, 0, 0, 0, 0, 5, 0, face.as_ptr());
        let child = |class: &str, label: &str, style: u32, x: i32, y: i32, w: i32, h: i32, id: i32| {
            let (class, label) = (wide(class), wide(label));
            let made = CreateWindowExW(0, class.as_ptr(), label.as_ptr(), WS_CHILD | WS_VISIBLE | style, x, y, w, h, window, id as usize as _, module, null());
            SendMessageW(made, WM_SETFONT, font as usize, 1);
        };
        child("STATIC", &text, 0, px(16), px(14), width - px(32), px(62), 0);
        let wide_each = (width - px(32) - px(8) * (buttons.len() as i32 - 1)) / buttons.len() as i32;
        for (at, label) in buttons.iter().enumerate() {
            child("BUTTON", label, 0, px(16) + at as i32 * (wide_each + px(8)), px(84), wide_each, px(34), FIRST_BUTTON + at as i32);
        }
        ShowWindow(window, SW_SHOWNOACTIVATE);
        SetTimer(window, 1, ANSWER_MS, None);
        let mut message: MSG = std::mem::zeroed();
        while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
            DispatchMessageW(&message);
        }
        DeleteObject(font);
        ANSWER.load(Ordering::SeqCst)
    });
    shown.join().ok().and_then(|picked| usize::try_from(picked).ok())
}

fn settings_file() -> PathBuf {
    let root = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    root.join("deskcrew").join("cursor.txt")
}

/// Whether agents may ask, and who may always borrow.
fn load() -> (bool, Vec<String>) {
    let text = fs::read_to_string(settings_file()).unwrap_or_default();
    let may_ask = text.lines().any(|line| line.trim() == "ask");
    let always = text.lines().filter_map(|line| line.trim().strip_prefix("always ")).map(str::to_string).collect();
    (may_ask, always)
}

fn save(may_ask: bool, always: &[String]) -> Result<(), String> {
    let path = settings_file();
    if let Some(folder) = path.parent() {
        fs::create_dir_all(folder).map_err(|e| e.to_string())?;
    }
    let mut lines: Vec<String> = always.iter().map(|name| format!("always {name}")).collect();
    if may_ask {
        lines.push("ask".into());
    }
    fs::write(path, lines.join("\n")).map_err(|e| e.to_string())
}

#[derive(Default)]
pub struct Permission {
    session: Mutex<HashSet<String>>,
    asking: Mutex<()>,
}

impl Permission {
    /// May this agent move the real cursor for one command? This is the only way in.
    pub fn allow(&self, who: &str) -> Result<(), String> {
        let _one_prompt_at_a_time = self.asking.lock().unwrap();
        let (may_ask, always) = load();
        let listed = |names: &[String]| names.iter().any(|name| name == who || name == EVERY_AGENT);
        let session: Vec<String> = self.session.lock().unwrap().iter().cloned().collect();
        if listed(&always) || listed(&session) {
            return Ok(());
        }
        if !may_ask {
            return Err(OFF.into());
        }
        let question = format!("{who} wants to borrow your cursor.\nIt will move your real mouse, then put it back.");
        match prompt(&question, &["Just this once", "This session", "Always", "No"]) {
            Some(0) => Ok(()),
            Some(1) => {
                self.session.lock().unwrap().insert(who.to_string());
                Ok(())
            }
            Some(2) => {
                let mut always = always;
                always.push(who.to_string());
                save(may_ask, &always)
            }
            _ => Err("the person said no to borrowing the cursor: find another way".into()),
        }
    }

    /// `off` always works, so anyone can take the cursor back. Anything that gives more away has to
    /// be confirmed on screen, or an agent could just run it.
    pub fn set(&self, word: Option<&str>) -> Result<String, String> {
        let (may_ask, always) = load();
        let question = match word {
            None => {
                let session: Vec<String> = self.session.lock().unwrap().iter().cloned().collect();
                let names = |list: &[String]| if list.is_empty() { "nobody".to_string() } else { list.join(", ") };
                return Ok(format!(
                    "cursor: agents {} ask; this session: {}; always: {}",
                    if may_ask { "may" } else { "may not" },
                    names(&session).replace(EVERY_AGENT, "every agent"),
                    names(&always).replace(EVERY_AGENT, "every agent")
                ));
            }
            Some("off") => {
                self.session.lock().unwrap().clear();
                save(false, &[])?;
                return Ok("off: no agent can borrow the cursor".into());
            }
            Some("ask") => "Let agents ask to borrow your cursor?\nYou will be asked each time.",
            Some("session") => "Let every agent borrow your cursor when needed?\nThis lasts until deskcrew stops.",
            Some("always") => "Let every agent always borrow your cursor when needed?\nThis lasts until you run: deskcrew cursor off",
            Some(_) => return Err("cursor [off|ask|session|always]".into()),
        };
        let _one_prompt_at_a_time = self.asking.lock().unwrap();
        if prompt(question, &["Allow", "No"]) != Some(0) {
            return Err("the person said no".into());
        }
        match word {
            Some("ask") => save(true, &always)?,
            Some("session") => {
                self.session.lock().unwrap().insert(EVERY_AGENT.to_string());
            }
            _ => save(may_ask, &[EVERY_AGENT.to_string()])?,
        }
        Ok("ok".into())
    }
}
