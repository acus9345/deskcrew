// Whether an agent may borrow the person's cursor. It is off until the person says otherwise,
// the answer comes from a prompt on screen, and "this session" / "always" grants are tied to
// the calling agent's actual process and executable path rather than its self-reported name.
use std::{
    collections::HashSet,
    fs,
    path::PathBuf,
    ptr::null,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicI32, AtomicU64, Ordering},
    },
    thread,
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, FILETIME, GetLastError, HWND, LPARAM, LRESULT, WPARAM},
    Graphics::Gdi::{CreateFontW, DeleteObject},
    System::{
        LibraryLoader::GetModuleHandleW,
        Threading::{GetProcessTimes, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW},
    },
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
/// How recent the person's own input must be for a button press on the prompt to count.
const PERSON_CLICK_MS: u32 = 1_000;
const FIRST_BUTTON: i32 = 100;
const EVERY_AGENT: &str = "*";
const PROGRAM_PREFIX: &str = "always program\t";
const PROGRAM_SPACE_PREFIX: &str = "always program ";
const LEGACY_MARKED_PREFIX: &str = "always? ";
const LEGACY_PREFIX: &str = "always ";
const MAX_CHAIN: usize = 24;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const OFF: &str = "the person has not let agents borrow the cursor. They can allow it by running: deskcrew cursor ask";

/// Shells skipped when finding the program behind a shell command, unless no non-root parent sits above them.
const SHELLS: [&str; 15] = ["cmd", "powershell", "pwsh", "bash", "sh", "zsh", "fish", "dash", "ksh", "csh", "tcsh", "nu", "elvish", "wsl", "wslhost"];
/// Desktop, terminal, and system host processes we never attribute an agent's grant to.
const ROOTS: [&str; 23] = [
    "explorer",
    "windowsterminal",
    "wt",
    "openconsole",
    "conhost",
    "mintty",
    "alacritty",
    "wezterm-gui",
    "wezterm",
    "tabby",
    "hyper",
    "sihost",
    "svchost",
    "services",
    "winlogon",
    "csrss",
    "smss",
    "wininit",
    "taskhostw",
    "runtimebroker",
    "dllhost",
    "applicationframehost",
    "wslservice",
];

static ANSWER: AtomicI32 = AtomicI32::new(-1);
static BUTTON_COUNT: AtomicI32 = AtomicI32::new(0);

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

unsafe extern "system" fn answer(hwnd: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        match message {
            // Only the person's own click on an actual prompt button answers: a message any program can post does not.
            WM_COMMAND => {
                let code = (w >> 16) & 0xFFFF;
                let id = (w & 0xFFFF) as i32;
                let idx = id - FIRST_BUTTON;
                let count = BUTTON_COUNT.load(Ordering::SeqCst);
                if code == 0 && idx >= 0 && idx < count && clicked_by_person(hwnd, id) {
                    ANSWER.store(idx, Ordering::SeqCst);
                    DestroyWindow(hwnd);
                }
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

/// Whether the real pointer is on the clicked button and the person used the mouse or keyboard
/// just now: a button press another program posts moves neither.
fn clicked_by_person(hwnd: HWND, button_id: i32) -> bool {
    use windows_sys::Win32::{
        Foundation::{POINT, RECT},
        System::SystemInformation::GetTickCount,
        UI::{
            Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO},
            WindowsAndMessaging::{GetCursorPos, GetDlgItem, GetWindowRect},
        },
    };
    unsafe {
        let button = GetDlgItem(hwnd, button_id);
        if button.is_null() {
            return false;
        }
        let (mut at, mut rect): (POINT, RECT) = (std::mem::zeroed(), std::mem::zeroed());
        let mut last = LASTINPUTINFO { cbSize: size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
        if GetCursorPos(&mut at) == 0 || GetWindowRect(button, &mut rect) == 0 || GetLastInputInfo(&mut last) == 0 {
            return false;
        }
        let inside = at.x >= rect.left && at.x < rect.right && at.y >= rect.top && at.y < rect.bottom;
        inside && GetTickCount().wrapping_sub(last.dwTime) < PERSON_CLICK_MS
    }
}

/// Ask on screen and wait. The window never takes focus, so nothing the person is typing can answer
/// it by accident, and no answer within a minute counts as no.
fn prompt(text: &str, buttons: &[&str]) -> Option<usize> {
    let (text, buttons) = (text.to_string(), buttons.iter().map(|b| b.to_string()).collect::<Vec<_>>());
    let shown = thread::spawn(move || unsafe {
        const SS_NOPREFIX: u32 = 0x0080;
        ANSWER.store(-1, Ordering::SeqCst);
        BUTTON_COUNT.store(buttons.len() as i32, Ordering::SeqCst);
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
        let (width, height) = (px(580), px(200));
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
        if window.is_null() {
            DeleteObject(font);
            return -1;
        }
        let child = |class: &str, label: &str, style: u32, x: i32, y: i32, w: i32, h: i32, id: i32| {
            let (class, label) = (wide(class), wide(label));
            let made = CreateWindowExW(0, class.as_ptr(), label.as_ptr(), WS_CHILD | WS_VISIBLE | style, x, y, w, h, window, id as usize as _, module, null());
            SendMessageW(made, WM_SETFONT, font as usize, 1);
        };
        child("STATIC", &text, SS_NOPREFIX, px(16), px(14), width - px(32), px(84), 0);
        let wide_each = (width - px(32) - px(8) * (buttons.len() as i32 - 1)) / buttons.len() as i32;
        for (at, label) in buttons.iter().enumerate() {
            child("BUTTON", label, 0, px(16) + at as i32 * (wide_each + px(8)), px(110), wide_each, px(34), FIRST_BUTTON + at as i32);
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

/// Verified identity of the calling agent program, derived from the pipe client's PID and process tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caller {
    pub exe_key: String,
    pub exe: String,
    pub pid: u32,
    pub created: u64,
    pub program: String,
    pub known_name: Option<&'static str>,
}

impl Caller {
    fn from_node(node: &ProcNode, known_name: Option<&'static str>) -> Self {
        let program = file_name(&node.exe).to_string();
        Self { exe_key: node.exe_key.clone(), exe: node.exe.clone(), pid: node.pid, created: node.created, program, known_name }
    }
}

/// Resolves the caller from `GetNamedPipeClientProcessId` once per request when cursor permission is checked.
pub struct CallerLookup {
    client_pid: Option<u32>,
    resolved: OnceLock<Result<Caller, String>>,
}

impl CallerLookup {
    pub fn new(client_pid: Option<u32>) -> Self {
        Self { client_pid, resolved: OnceLock::new() }
    }

    pub fn resolve(&self) -> Result<&Caller, String> {
        self.resolved.get_or_init(|| resolve_caller(self.client_pid)).as_ref().map_err(Clone::clone)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProcNode {
    pid: u32,
    exe: String,
    exe_key: String,
    created: u64,
}

fn strip_verbatim(path: &str) -> String {
    if let Some(unc) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else if let Some(rest) = path.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        path.to_string()
    }
}

fn normalize_exe(raw: &str, canonicalize: bool) -> Option<(String, String)> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let cleaned = strip_verbatim(trimmed);
    let exe = if canonicalize {
        fs::canonicalize(&cleaned).map(|p| strip_verbatim(&p.to_string_lossy())).unwrap_or(cleaned)
    } else {
        cleaned
    };
    let exe = exe.replace('/', "\\");
    if exe.is_empty() || exe.chars().any(|c| matches!(c, '\0' | '\t' | '\r' | '\n')) {
        return None;
    }
    let exe_key = exe.to_lowercase();
    Some((exe, exe_key))
}

fn file_name(exe: &str) -> &str {
    exe.rsplit('\\').next().unwrap_or(exe)
}

fn exe_stem(exe_key: &str) -> &str {
    let name = file_name(exe_key);
    name.strip_suffix(".exe").unwrap_or(name)
}

fn is_deskcrew_binary(exe_key: &str, self_exe_key: &str) -> bool {
    if exe_key == self_exe_key {
        return true;
    }
    let Some((dir, file)) = exe_key.rsplit_once('\\') else { return false };
    let Some((self_dir, _)) = self_exe_key.rsplit_once('\\') else { return false };
    dir == self_dir
        && file
            .strip_prefix("deskcrew-old-")
            .and_then(|rest| rest.strip_suffix(".exe"))
            .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

fn process_info(pid: u32) -> Option<ProcNode> {
    if pid <= 4 {
        return None;
    }
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut buffer = vec![0u16; 520];
        let path = loop {
            let mut size = buffer.len() as u32;
            if QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buffer.as_mut_ptr(), &mut size) != 0 {
                break Some(String::from_utf16_lossy(&buffer[..size as usize]));
            }
            if GetLastError() == ERROR_INSUFFICIENT_BUFFER && buffer.len() < 32_768 {
                buffer.resize(buffer.len() * 2, 0);
            } else {
                break None;
            }
        };
        let mut creation = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let mut exit = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let mut kernel = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let mut user = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let times_ok = GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) != 0;
        CloseHandle(handle);
        let path = path?;
        if !times_ok {
            return None;
        }
        let created = ((creation.dwHighDateTime as u64) << 32) | (creation.dwLowDateTime as u64);
        let (exe, exe_key) = normalize_exe(&path, true)?;
        Some(ProcNode { pid, exe, exe_key, created })
    }
}

fn resolve_caller(client_pid: Option<u32>) -> Result<Caller, String> {
    resolve_caller_with(client_pid, std::process::id())
}

fn resolve_caller_with(client_pid: Option<u32>, service_pid: u32) -> Result<Caller, String> {
    let client_pid = client_pid.ok_or_else(|| "could not identify the calling process".to_string())?;
    let first = process_info(client_pid).ok_or_else(|| "could not identify the calling process".to_string())?;
    let parents = crate::client::processes();
    // Confirm the client did not exit and have its PID reused across the snapshot.
    let second = process_info(client_pid).ok_or_else(|| "could not identify the calling process".to_string())?;
    if first.created != second.created || first.exe_key != second.exe_key {
        return Err("could not identify the calling process".into());
    }

    let mut chain = vec![first];
    let mut seen = HashSet::from([client_pid]);
    for _ in 0..MAX_CHAIN {
        let cur = chain.last().unwrap();
        let Some(&(parent_pid, _)) = parents.get(&cur.pid) else { break };
        if parent_pid <= 4 || !seen.insert(parent_pid) {
            break;
        }
        let Some(parent) = process_info(parent_pid) else { break };
        // A parent created after its child is a reused PID, not the true parent.
        if parent.created > cur.created {
            break;
        }
        chain.push(parent);
    }

    let self_exe_key = std::env::current_exe()
        .ok()
        .and_then(|path| normalize_exe(&path.to_string_lossy(), true))
        .map(|(_, key)| key)
        .unwrap_or_default();
    pick_caller_from_chain(&chain, &self_exe_key, service_pid)
}

fn is_same_app_helper(node: &ProcNode, ancestor: &ProcNode) -> bool {
    let Some((dir, _)) = ancestor.exe_key.rsplit_once('\\') else {
        return false;
    };
    dir.matches('\\').count() >= 2 && node.exe_key.starts_with(dir) && node.exe_key.as_bytes().get(dir.len()) == Some(&b'\\')
}

fn pick_caller_from_chain(chain: &[ProcNode], self_exe_key: &str, service_pid: u32) -> Result<Caller, String> {
    if self_exe_key.is_empty() || chain.is_empty() {
        return Err("could not identify the calling program".into());
    }
    if chain[0].pid == service_pid {
        return Err("could not identify the calling program".into());
    }
    let start_idx = if is_deskcrew_binary(&chain[0].exe_key, self_exe_key) {
        let idx = chain
            .iter()
            .position(|node| !is_deskcrew_binary(&node.exe_key, self_exe_key))
            .ok_or_else(|| "could not identify the calling program".to_string())?;
        if chain[..idx].iter().any(|node| node.pid == service_pid) {
            return Err("could not identify the calling program".into());
        }
        idx
    } else {
        0
    };
    let end_idx = chain.len().min(start_idx + crate::client::MOST_ANCESTORS);
    let window = &chain[start_idx..end_idx];

    let mut last_shell: Option<&ProcNode> = None;
    for (idx, node) in window.iter().enumerate() {
        let stem = exe_stem(&node.exe_key);
        if is_deskcrew_binary(&node.exe_key, self_exe_key) {
            if node.pid == service_pid {
                return Err("could not identify the calling program".into());
            }
            continue;
        }
        if ROOTS.contains(&stem) {
            break;
        }
        if let Some(known) = crate::client::agent_program(stem) {
            return Ok(Caller::from_node(node, Some(known)));
        }
        if SHELLS.contains(&stem) {
            last_shell = Some(node);
            continue;
        }
        // A bundled helper inside a known agent's own install folder (such as Antigravity's language
        // server) attributes to that agent; any other binary (including tools inside an IDE's
        // terminal) stops right here so it cannot inherit the IDE's grant.
        for anc in &window[idx + 1..] {
            let anc_stem = exe_stem(&anc.exe_key);
            if anc.pid == service_pid || is_deskcrew_binary(&anc.exe_key, self_exe_key) || ROOTS.contains(&anc_stem) {
                break;
            }
            if let Some(known) = crate::client::agent_program(anc_stem)
                && is_same_app_helper(node, anc)
            {
                return Ok(Caller::from_node(anc, Some(known)));
            }
        }
        return Ok(Caller::from_node(node, None));
    }

    last_shell
        .map(|shell| Caller::from_node(shell, None))
        .ok_or_else(|| "could not identify the calling program".into())
}

fn settings_file() -> PathBuf {
    crate::shot::home().join("cursor.txt")
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProgramGrant {
    exe_key: String,
    exe: String,
    label: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Settings {
    may_ask: bool,
    every: bool,
    programs: Vec<ProgramGrant>,
    legacy: Vec<String>,
}

fn is_hidden_format(c: char) -> bool {
    matches!(c, '\u{061C}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}')
}

fn clean_display(text: &str, max_chars: usize) -> String {
    let flat: String = text
        .chars()
        .filter(|&c| !is_hidden_format(c))
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    flat.chars().take(max_chars).collect::<String>().trim().to_string()
}

fn parse_settings(text: &str) -> Settings {
    let mut settings = Settings::default();
    let mut add_program = |raw_exe: &str, raw_label: &str| {
        let Some((exe, exe_key)) = normalize_exe(raw_exe, false) else { return };
        if settings.programs.iter().any(|p| p.exe_key == exe_key) {
            return;
        }
        let label = clean_display(raw_label, 64);
        let label = if label.is_empty() { file_name(&exe).to_string() } else { label };
        settings.programs.push(ProgramGrant { exe_key, exe, label });
    };
    let mut add_legacy = |raw_name: &str| {
        let name = clean_display(raw_name, 64);
        if !name.is_empty() && !settings.legacy.iter().any(|existing| existing == &name) {
            settings.legacy.push(name);
        }
    };

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == "ask" {
            settings.may_ask = true;
        } else if trimmed == "always *" {
            settings.every = true;
        } else if let Some(rest) = trimmed.strip_prefix(PROGRAM_PREFIX) {
            let (exe, label) = rest.split_once('\t').unwrap_or((rest, ""));
            add_program(exe, label);
        } else if let Some(rest) = trimmed.strip_prefix(PROGRAM_SPACE_PREFIX) {
            add_program(rest, "");
        } else if let Some(name) = trimmed.strip_prefix(LEGACY_MARKED_PREFIX) {
            add_legacy(name);
        } else if let Some(name) = trimmed.strip_prefix(LEGACY_PREFIX) {
            add_legacy(name);
        }
    }
    settings
}

fn format_settings(settings: &Settings) -> String {
    let mut lines = Vec::new();
    if settings.every {
        lines.push(format!("always {EVERY_AGENT}"));
    }
    for prog in &settings.programs {
        let label = clean_display(&prog.label, 64);
        let label = if label.is_empty() { file_name(&prog.exe).to_string() } else { label };
        lines.push(format!("{PROGRAM_PREFIX}{}\t{label}", prog.exe));
    }
    for name in &settings.legacy {
        let cleaned = clean_display(name, 64);
        if !cleaned.is_empty() {
            lines.push(format!("{LEGACY_MARKED_PREFIX}{cleaned}"));
        }
    }
    if settings.may_ask {
        lines.push("ask".into());
    }
    lines.join("\n")
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SessionGrant {
    pid: u32,
    created: u64,
    exe_key: String,
    label: String,
}

#[derive(Default)]
struct SessionState {
    every: bool,
    grants: Vec<SessionGrant>,
}

impl SessionState {
    fn clear(&mut self) {
        self.every = false;
        self.grants.clear();
    }

    fn allows(&self, caller: &Caller) -> bool {
        self.every
            || self
                .grants
                .iter()
                .any(|g| g.pid == caller.pid && g.created == caller.created && g.exe_key == caller.exe_key)
    }

    fn insert(&mut self, grant: SessionGrant) {
        if !self.grants.iter().any(|g| g.pid == grant.pid && g.created == grant.created && g.exe_key == grant.exe_key) {
            self.grants.push(grant);
        }
    }
}

fn short_exe_display(exe: &str) -> String {
    let clean = clean_display(exe, 260);
    if clean.chars().count() <= 42 {
        return clean;
    }
    let prefix = if clean.as_bytes().get(1) == Some(&b':') && clean.as_bytes().get(2) == Some(&b'\\') {
        format!("{}\\...\\", &clean[..2])
    } else if clean.starts_with(r"\\") {
        r"\\...\".to_string()
    } else {
        r"...\".to_string()
    };
    let parts: Vec<&str> = clean.split('\\').filter(|p| !p.is_empty()).collect();
    let mut tail = String::new();
    for part in parts.iter().rev() {
        let candidate = if tail.is_empty() { (*part).to_string() } else { format!("{part}\\{tail}") };
        if !tail.is_empty() && prefix.chars().count() + candidate.chars().count() > 42 {
            break;
        }
        tail = candidate;
    }
    let out = format!("{prefix}{tail}");
    if out.chars().count() > 42 {
        let last = parts.last().copied().unwrap_or(&clean);
        let room = 42usize.saturating_sub(prefix.chars().count());
        format!("{prefix}{}", last.chars().take(room).collect::<String>())
    } else {
        out
    }
}

fn prompt_question(who: &str, caller: &Caller) -> String {
    let name = clean_display(who, 32);
    let name = if name.is_empty() { "your agent".to_string() } else { name };
    let path = short_exe_display(&caller.exe);
    let shown = if path.is_empty() { name } else { format!("{name} ({path})") };
    format!("{shown} wants to borrow your cursor.\nIt will move your real mouse, then put it back.")
}

fn grant_label(who: &str, caller: &Caller) -> String {
    let cleaned = clean_display(who, 64);
    if caller.known_name == Some(cleaned.as_str()) {
        cleaned
    } else if let Some(known) = caller.known_name {
        if cleaned.is_empty() { known.to_string() } else { format!("{known} / {cleaned}") }
    } else if cleaned.is_empty() || cleaned == "your agent" {
        clean_display(&caller.program, 64)
    } else {
        cleaned
    }
}

fn legacy_matches_caller(legacy_name: &str, who: &str, caller: &Caller) -> bool {
    if caller.known_name == Some(legacy_name) {
        return true;
    }
    if legacy_name != who {
        return false;
    }
    match caller.known_name {
        None => !is_known_agent_display_name(who),
        // Never let one recognised agent clear another recognised agent's legacy entry by spoofing --agent.
        Some(known) => legacy_name == known || !is_known_agent_display_name(who),
    }
}

fn is_known_agent_display_name(name: &str) -> bool {
    matches!(
        name,
        "Claude Code"
            | "Claude"
            | "Cursor"
            | "Codex"
            | "Gemini"
            | "Copilot"
            | "Windsurf"
            | "OpenCode"
            | "Aider"
            | "Goose"
            | "Amp"
            | "Kiro"
            | "Antigravity"
            | "Grok"
            | "Zed"
            | "Cline"
    )
}

#[derive(Default)]
pub struct Permission {
    session: Mutex<SessionState>,
    asking: Mutex<()>,
    file_lock: Mutex<()>,
    off_generation: AtomicU64,
    path: Option<PathBuf>,
}

impl Permission {
    #[cfg(test)]
    fn with_path(path: PathBuf) -> Self {
        Self { path: Some(path), ..Self::default() }
    }

    fn settings_path(&self) -> PathBuf {
        self.path.clone().unwrap_or_else(settings_file)
    }

    /// Whether agents may ask, and who may always borrow. A setting saved before 0.1.24 is still read
    /// from deskcrew's old folder until it is saved again.
    fn load(&self) -> Settings {
        let text = match &self.path {
            Some(path) => fs::read_to_string(path).unwrap_or_default(),
            None => fs::read_to_string(settings_file())
                .or_else(|_| crate::shot::old_home().map(|old| fs::read_to_string(old.join("cursor.txt"))).unwrap_or_else(|| Err(std::io::ErrorKind::NotFound.into())))
                .unwrap_or_default(),
        };
        parse_settings(&text)
    }

    fn save(&self, settings: &Settings) -> Result<(), String> {
        let path = self.settings_path();
        if let Some(folder) = path.parent() {
            fs::create_dir_all(folder).map_err(|e| e.to_string())?;
        }
        fs::write(path, format_settings(settings)).map_err(|e| e.to_string())
    }

    /// May this agent move the real cursor for one command? This is the only way in.
    pub fn allow(&self, who: &str, caller: Result<&Caller, String>) -> Result<(), String> {
        self.allow_with(who, caller, prompt)
    }

    fn allow_with(&self, who: &str, caller: Result<&Caller, String>, ask: impl FnOnce(&str, &[&str]) -> Option<usize>) -> Result<(), String> {
        let caller = caller?;
        let can_always = !SHELLS.contains(&exe_stem(&caller.exe_key));
        let _one_prompt_at_a_time = self.asking.lock().unwrap();
        let off_gen = self.off_generation.load(Ordering::SeqCst);
        let may_ask = {
            let _file = self.file_lock.lock().unwrap();
            let settings = self.load();
            let allowed_session = self.session.lock().unwrap().allows(caller);
            let allowed_always = settings.every || (can_always && settings.programs.iter().any(|p| p.exe_key == caller.exe_key));
            if allowed_session || allowed_always {
                return Ok(());
            }
            settings.may_ask
        };
        if !may_ask {
            return Err(OFF.into());
        }
        let question = prompt_question(who, caller);
        let buttons: &[&str] = if can_always {
            &["Just this once", "This session", "Always", "No"]
        } else {
            &["Just this once", "This session", "No"]
        };
        match ask(&question, buttons) {
            Some(0) => {
                if self.off_generation.load(Ordering::SeqCst) != off_gen {
                    return Err("cursor permission was turned off".into());
                }
                Ok(())
            }
            Some(1) => {
                let mut session = self.session.lock().unwrap();
                if self.off_generation.load(Ordering::SeqCst) != off_gen {
                    return Err("cursor permission was turned off".into());
                }
                session.insert(SessionGrant {
                    pid: caller.pid,
                    created: caller.created,
                    exe_key: caller.exe_key.clone(),
                    label: grant_label(who, caller),
                });
                Ok(())
            }
            Some(2) if can_always => {
                let _file = self.file_lock.lock().unwrap();
                if self.off_generation.load(Ordering::SeqCst) != off_gen {
                    return Err("cursor permission was turned off".into());
                }
                let mut settings = self.load();
                let label = grant_label(who, caller);
                if let Some(existing) = settings.programs.iter_mut().find(|p| p.exe_key == caller.exe_key) {
                    existing.exe = caller.exe.clone();
                    existing.label = label;
                } else {
                    settings.programs.push(ProgramGrant {
                        exe_key: caller.exe_key.clone(),
                        exe: caller.exe.clone(),
                        label,
                    });
                }
                settings.legacy.retain(|name| !legacy_matches_caller(name, who, caller));
                self.save(&settings)
            }
            _ => Err("the person said no to borrowing the cursor: find another way".into()),
        }
    }

    /// `off` always works, so anyone can take the cursor back. Anything that gives more away has to
    /// be confirmed on screen, or an agent could just run it.
    pub fn set(&self, word: Option<&str>) -> Result<String, String> {
        self.set_with(word, prompt)
    }

    fn set_with(&self, word: Option<&str>, ask: impl FnOnce(&str, &[&str]) -> Option<usize>) -> Result<String, String> {
        let question = match word {
            None => {
                let _file = self.file_lock.lock().unwrap();
                let settings = self.load();
                let session = self.session.lock().unwrap();
                let session_desc = if session.every {
                    "every agent".to_string()
                } else if session.grants.is_empty() {
                    "nobody".to_string()
                } else {
                    session.grants.iter().map(|g| format!("{} (pid {})", g.label, g.pid)).collect::<Vec<_>>().join(", ")
                };
                let mut always_parts = Vec::new();
                if settings.every {
                    always_parts.push("every agent".to_string());
                }
                for prog in &settings.programs {
                    if prog.label.eq_ignore_ascii_case(file_name(&prog.exe)) {
                        always_parts.push(prog.exe.clone());
                    } else {
                        always_parts.push(format!("{} ({})", prog.label, prog.exe));
                    }
                }
                for name in &settings.legacy {
                    always_parts.push(format!("{name} (needs re-confirming)"));
                }
                let always_desc = if always_parts.is_empty() { "nobody".to_string() } else { always_parts.join(", ") };
                return Ok(format!(
                    "cursor: agents {} ask; this session: {}; always: {}",
                    if settings.may_ask { "may" } else { "may not" },
                    session_desc,
                    always_desc
                ));
            }
            Some("off") => {
                let _file = self.file_lock.lock().unwrap();
                let mut session = self.session.lock().unwrap();
                self.off_generation.fetch_add(1, Ordering::SeqCst);
                session.clear();
                self.save(&Settings::default())?;
                return Ok("off: no agent can borrow the cursor".into());
            }
            Some("ask") => "Let agents ask to borrow your cursor?\nYou will be asked each time.",
            Some("session") => "Let every agent borrow your cursor when needed?\nThis lasts until deskcrew stops.",
            Some("always") => "Let every agent always borrow your cursor when needed?\nThis lasts until you run: deskcrew cursor off",
            Some(_) => return Err("cursor [off|ask|session|always]".into()),
        };
        let _one_prompt_at_a_time = self.asking.lock().unwrap();
        let off_gen = self.off_generation.load(Ordering::SeqCst);
        if ask(question, &["Allow", "No"]) != Some(0) {
            return Err("the person said no".into());
        }
        match word {
            Some("ask") => {
                let _file = self.file_lock.lock().unwrap();
                if self.off_generation.load(Ordering::SeqCst) != off_gen {
                    return Err("cursor permission was turned off".into());
                }
                let mut settings = self.load();
                settings.may_ask = true;
                self.save(&settings)?;
            }
            Some("session") => {
                let mut session = self.session.lock().unwrap();
                if self.off_generation.load(Ordering::SeqCst) != off_gen {
                    return Err("cursor permission was turned off".into());
                }
                session.every = true;
            }
            _ => {
                let _file = self.file_lock.lock().unwrap();
                if self.off_generation.load(Ordering::SeqCst) != off_gen {
                    return Err("cursor permission was turned off".into());
                }
                let mut settings = self.load();
                settings.every = true;
                settings.programs.clear();
                settings.legacy.clear();
                self.save(&settings)?;
            }
        }
        Ok("ok".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn caller(exe: &str, pid: u32, created: u64, known_name: Option<&'static str>) -> Caller {
        let (exe, exe_key) = normalize_exe(exe, false).unwrap();
        Caller {
            program: file_name(&exe).to_string(),
            exe,
            exe_key,
            pid,
            created,
            known_name,
        }
    }

    fn node(pid: u32, exe: &str, created: u64) -> ProcNode {
        let (exe, exe_key) = normalize_exe(exe, false).unwrap();
        ProcNode { pid, exe, exe_key, created }
    }

    fn temp_cursor_path(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("deskcrew-borrow-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir.join("cursor.txt")
    }

    #[test]
    fn spoofed_agent_name_cannot_reuse_session_or_always_grant() {
        let path = temp_cursor_path("spoof");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "ask\n").unwrap();
        let perm = Permission::with_path(path.clone());

        let claude = caller(r"C:\Users\User\.local\bin\claude.exe", 1001, 500, Some("Claude Code"));
        let codex = caller(r"C:\Users\User\.cargo\bin\codex.exe", 1002, 600, Some("Codex"));
        let fake_claude_path = caller(r"C:\Temp\claude.exe", 1003, 700, Some("Claude Code"));

        // Grant "This session" to Claude Code.
        let prompts = AtomicUsize::new(0);
        perm.allow_with("Claude Code", Ok(&claude), |_, _| {
            prompts.fetch_add(1, Ordering::SeqCst);
            Some(1)
        })
        .unwrap();
        assert_eq!(prompts.load(Ordering::SeqCst), 1);

        // Same Claude process works without prompting.
        perm.allow_with("Claude Code", Ok(&claude), |_, _| panic!("should not prompt")).unwrap();

        // Codex claiming --agent "Claude Code" must still be prompted (and refused on No).
        let err = perm
            .allow_with("Claude Code", Ok(&codex), |question, _| {
                assert!(question.contains(r"Claude Code (C:\Users\User\.cargo\bin\codex.exe)"), "prompt was: {question}");
                Some(3)
            })
            .unwrap_err();
        assert!(err.contains("said no"));

        // A different claude.exe process (new session, or reused PID with new creation time) does not inherit "This session".
        let claude_other_pid = caller(r"C:\Users\User\.local\bin\claude.exe", 1004, 800, Some("Claude Code"));
        let claude_reused_pid = caller(r"C:\Users\User\.local\bin\claude.exe", 1001, 999, Some("Claude Code"));
        assert!(perm.allow_with("Claude Code", Ok(&claude_other_pid), |_, _| Some(3)).is_err());
        assert!(perm.allow_with("Claude Code", Ok(&claude_reused_pid), |_, _| Some(3)).is_err());

        // Grant "Always" to Claude Code (new process).
        perm.allow_with("Claude Code", Ok(&claude_other_pid), |_, _| Some(2)).unwrap();

        // Another legitimate claude.exe process at the same path gets "Always".
        perm.allow_with("Claude Code", Ok(&claude_reused_pid), |_, _| panic!("should not prompt")).unwrap();

        // Codex claiming --agent "Claude Code" and an imposter C:\Temp\claude.exe are still blocked/prompted.
        assert!(perm.allow_with("Claude Code", Ok(&codex), |_, _| Some(3)).is_err());
        assert!(perm.allow_with("Claude Code", Ok(&fake_claude_path), |_, _| Some(3)).is_err());

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn old_always_name_lines_remain_readable_and_require_one_reconfirmation() {
        let path = temp_cursor_path("legacy");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "always Claude Code\nalways Cursor\nask\n").unwrap();
        let perm = Permission::with_path(path.clone());

        let status = perm.set(None).unwrap();
        assert!(status.contains("Claude Code (needs re-confirming)"), "{status}");
        assert!(status.contains("Cursor (needs re-confirming)"), "{status}");

        let claude = caller(r"C:\Users\User\.local\bin\claude.exe", 2001, 100, Some("Claude Code"));
        let codex = caller(r"C:\Users\User\.cargo\bin\codex.exe", 2002, 200, Some("Codex"));
        let evil = caller(r"C:\Temp\evil.exe", 2003, 300, None);

        // Spoofing Claude Code does not match old "always Claude Code" and does not clear it,
        // even if an unrecognized binary gets "Always" clicked while claiming "Claude Code".
        assert!(perm.allow_with("Claude Code", Ok(&codex), |_, _| Some(3)).is_err());
        perm.allow_with("Claude Code", Ok(&evil), |_, _| Some(2)).unwrap();
        assert!(fs::read_to_string(&path).unwrap().contains("always? Claude Code"));

        // Real Claude Code also has to confirm once, and confirming upgrades its entry while keeping Cursor marked.
        let asked = AtomicUsize::new(0);
        perm.allow_with("Claude Code", Ok(&claude), |question, _| {
            asked.fetch_add(1, Ordering::SeqCst);
            assert!(question.starts_with(r"Claude Code (C:\Users\User\.local\bin\claude.exe) wants to borrow your cursor"));
            Some(2)
        })
        .unwrap();
        assert_eq!(asked.load(Ordering::SeqCst), 1);

        // Next call from Claude Code succeeds without prompting.
        perm.allow_with("Claude Code", Ok(&claude), |_, _| panic!("should not prompt again")).unwrap();

        let saved = fs::read_to_string(&path).unwrap();
        assert!(saved.contains("always program\tC:\\Users\\User\\.local\\bin\\claude.exe\tClaude Code"), "{saved}");
        assert!(saved.contains("always? Cursor"), "{saved}");
        assert!(!saved.contains("always? Claude Code"), "{saved}");

        let status = perm.set(None).unwrap();
        assert!(!status.contains("Claude Code (needs re-confirming)"), "{status}");
        assert!(status.contains("Cursor (needs re-confirming)"), "{status}");

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn unidentifiable_caller_fails_closed_even_when_every_agent_is_allowed() {
        let path = temp_cursor_path("failclosed");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "always *\nask\n").unwrap();
        let perm = Permission::with_path(path.clone());
        let err = perm.allow_with("Claude Code", Err("could not identify the calling process".into()), |_, _| panic!("must not prompt")).unwrap_err();
        assert!(err.contains("could not identify"));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn cursor_off_during_prompt_prevents_stale_grant() {
        let path = temp_cursor_path("offrace");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "ask\n").unwrap();
        let perm = Permission::with_path(path.clone());
        let claude = caller(r"C:\Users\User\.local\bin\claude.exe", 3001, 100, Some("Claude Code"));

        let err = perm
            .allow_with("Claude Code", Ok(&claude), |_, _| {
                perm.set(Some("off")).unwrap();
                Some(2)
            })
            .unwrap_err();
        assert!(err.contains("turned off"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn prompt_sanitizes_newlines_bidi_and_always_shows_real_path() {
        let codex = caller(r"C:\tools\codex.exe", 10, 10, Some("Codex"));
        let q = prompt_question("Claude Code\n\u{202E}Allow?", &codex);
        assert!(q.starts_with(r"Claude Code Allow? (C:\tools\codex.exe) wants to borrow your cursor."));
        assert!(!q.contains('\u{202E}'));
        assert_eq!(q.lines().count(), 2);

        let fake_claude = caller(r"C:\Temp\claude.exe", 11, 10, Some("Claude Code"));
        let q2 = prompt_question("Claude Code", &fake_claude);
        assert_eq!(q2, "Claude Code (C:\\Temp\\claude.exe) wants to borrow your cursor.\nIt will move your real mouse, then put it back.");
    }

    #[test]
    fn shells_cannot_hold_or_gain_always_grants() {
        let path = temp_cursor_path("shell");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "always program\tC:\\Program Files\\PowerShell\\7\\pwsh.exe\tpwsh.exe\nask\n").unwrap();
        let perm = Permission::with_path(path.clone());
        let pwsh = caller(r"C:\Program Files\PowerShell\7\pwsh.exe", 4001, 100, None);

        // Even a pre-existing always program line for a shell is ignored; only Just this once / This session / No are offered.
        let err = perm
            .allow_with("your agent", Ok(&pwsh), |_, buttons| {
                assert_eq!(buttons, &["Just this once", "This session", "No"]);
                Some(2)
            })
            .unwrap_err();
        assert!(err.contains("said no"));

        // "This session" still works for that specific shell process.
        perm.allow_with("your agent", Ok(&pwsh), |_, _| Some(1)).unwrap();
        perm.allow_with("your agent", Ok(&pwsh), |_, _| panic!("session should allow")).unwrap();
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn ancestor_selection_picks_nearest_agent_or_non_shell_caller() {
        let self_exe = r"c:\users\user\.cargo\bin\deskcrew.exe";
        let service_pid = 9999;

        // Normal agent in shell: claude.exe -> bash.exe -> deskcrew.exe
        let chain = vec![
            node(3, r"C:\Users\User\.cargo\bin\deskcrew.exe", 30),
            node(2, r"C:\Program Files\Git\usr\bin\bash.exe", 20),
            node(1, r"C:\Users\User\.local\bin\claude.exe", 10),
        ];
        let picked = pick_caller_from_chain(&chain, self_exe, service_pid).unwrap();
        assert_eq!(picked.pid, 1);
        assert_eq!(picked.known_name, Some("Claude Code"));

        // MCP: Antigravity -> language_server (same app folder) -> deskcrew mcp -> deskcrew
        let chain = vec![
            node(4, r"C:\Users\User\.cargo\bin\deskcrew.exe", 40),
            node(3, r"C:\Users\User\.cargo\bin\deskcrew.exe", 30),
            node(2, r"C:\Users\User\AppData\Local\Programs\antigravity\resources\bin\language_server.exe", 20),
            node(1, r"C:\Users\User\AppData\Local\Programs\antigravity\Antigravity.exe", 10),
        ];
        let picked = pick_caller_from_chain(&chain, self_exe, service_pid).unwrap();
        assert_eq!(picked.pid, 1);
        assert_eq!(picked.known_name, Some("Antigravity"));

        // Arbitrary binary in an IDE terminal cannot inherit the IDE's grant.
        let chain = vec![
            node(4, r"C:\Users\User\.cargo\bin\deskcrew.exe", 40),
            node(3, r"C:\Temp\evil.exe", 30),
            node(2, r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe", 20),
            node(1, r"C:\Users\User\AppData\Local\Programs\cursor\Cursor.exe", 10),
        ];
        let picked = pick_caller_from_chain(&chain, self_exe, service_pid).unwrap();
        assert_eq!(picked.pid, 3);
        assert_eq!(picked.exe_key, r"c:\temp\evil.exe");

        // Old MCP copy left during `deskcrew setup` + non-listed host (VS Code):
        let chain = vec![
            node(3, r"C:\Users\User\.cargo\bin\deskcrew.exe", 30),
            node(2, r"C:\Users\User\.cargo\bin\deskcrew-old-1.exe", 20),
            node(1, r"C:\Users\User\AppData\Local\Programs\Microsoft VS Code\Code.exe", 10),
        ];
        let picked = pick_caller_from_chain(&chain, self_exe, service_pid).unwrap();
        assert_eq!(picked.pid, 1);
        assert_eq!(picked.known_name, None);

        // Codex inside Cursor's terminal picks Codex (nearest), not Cursor.
        let chain = vec![
            node(4, r"C:\Users\User\.cargo\bin\deskcrew.exe", 40),
            node(3, r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe", 30),
            node(2, r"C:\tools\codex.exe", 20),
            node(1, r"C:\Users\User\AppData\Local\Programs\cursor\Cursor.exe", 10),
        ];
        let picked = pick_caller_from_chain(&chain, self_exe, service_pid).unwrap();
        assert_eq!(picked.pid, 2);
        assert_eq!(picked.known_name, Some("Codex"));

        // `deskcrew run` skips the outer deskcrew client and resolves the actual caller.
        let chain = vec![
            node(4, r"C:\Users\User\.cargo\bin\deskcrew.exe", 40),
            node(3, r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe", 30),
            node(2, r"C:\Users\User\.cargo\bin\deskcrew.exe", 20),
            node(1, r"C:\Temp\evil.exe", 10),
        ];
        let picked = pick_caller_from_chain(&chain, self_exe, service_pid).unwrap();
        assert_eq!(picked.pid, 1);
        assert_eq!(picked.exe_key, r"c:\temp\evil.exe");

        // `deskcrew launch` child passing through the service process itself fails closed.
        let chain = vec![
            node(4, r"C:\Users\User\.cargo\bin\deskcrew.exe", 40),
            node(3, r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe", 30),
            node(service_pid, r"C:\Users\User\.cargo\bin\deskcrew.exe", 20),
            node(1, r"C:\Users\User\.local\bin\claude.exe", 10),
        ];
        assert!(pick_caller_from_chain(&chain, self_exe, service_pid).is_err());

        // Direct pipe connection by another program is not skipped.
        let chain = vec![node(2, r"C:\Temp\evil.exe", 20), node(1, r"C:\Windows\explorer.exe", 10)];
        let picked = pick_caller_from_chain(&chain, self_exe, service_pid).unwrap();
        assert_eq!(picked.pid, 2);
        assert_eq!(picked.exe_key, r"c:\temp\evil.exe");

        // Imposter copy of deskcrew.exe elsewhere is not skipped to explorer.exe.
        let chain = vec![node(2, r"C:\Temp\deskcrew.exe", 20), node(1, r"C:\Windows\explorer.exe", 10)];
        let picked = pick_caller_from_chain(&chain, self_exe, service_pid).unwrap();
        assert_eq!(picked.pid, 2);
        assert_eq!(picked.exe_key, r"c:\temp\deskcrew.exe");

        // Interactive user shell inside WindowsTerminal stops at the shell, not WindowsTerminal/explorer.
        let chain = vec![
            node(4, r"C:\Users\User\.cargo\bin\deskcrew.exe", 40),
            node(3, r"C:\Windows\System32\cmd.exe", 30),
            node(2, r"C:\Program Files\PowerShell\7\pwsh.exe", 20),
            node(1, r"C:\Program Files\WindowsApps\WT\WindowsTerminal.exe", 10),
        ];
        let picked = pick_caller_from_chain(&chain, self_exe, service_pid).unwrap();
        assert_eq!(picked.pid, 2);
        assert_eq!(picked.program, "pwsh.exe");

        // Direct spawn from explorer.exe (or orphaned deskcrew) fails closed.
        let chain = vec![node(2, r"C:\Users\User\.cargo\bin\deskcrew.exe", 20), node(1, r"C:\Windows\explorer.exe", 10)];
        assert!(pick_caller_from_chain(&chain, self_exe, service_pid).is_err());
        let chain = vec![node(1, r"C:\Users\User\.cargo\bin\deskcrew.exe", 10)];
        assert!(pick_caller_from_chain(&chain, self_exe, service_pid).is_err());
        assert!(pick_caller_from_chain(&chain, "", service_pid).is_err());
    }

    #[test]
    fn real_pipe_client_resolves_its_real_process_identity() {
        use std::process::{Command, Stdio};
        let pipe_suffix = format!("deskcrew-borrow-test-{}", std::process::id());
        let pipe_wide: Vec<u16> = format!(r"\\.\pipe\{pipe_suffix}").encode_utf16().chain(Some(0)).collect();
        let server_handle = unsafe {
            windows_sys::Win32::System::Pipes::CreateNamedPipeW(
                pipe_wide.as_ptr(),
                windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX
                    | windows_sys::Win32::Storage::FileSystem::FILE_FLAG_FIRST_PIPE_INSTANCE,
                windows_sys::Win32::System::Pipes::PIPE_TYPE_BYTE
                    | windows_sys::Win32::System::Pipes::PIPE_READMODE_BYTE
                    | windows_sys::Win32::System::Pipes::PIPE_WAIT
                    | windows_sys::Win32::System::Pipes::PIPE_REJECT_REMOTE_CLIENTS,
                1,
                4096,
                4096,
                0,
                null(),
            )
        };
        assert_ne!(server_handle, windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE);

        // Spawn powershell.exe to connect to the named pipe and hold it open while we inspect its PID.
        let script = format!(
            "$p = New-Object System.IO.Pipes.NamedPipeClientStream('.', '{pipe_suffix}', [System.IO.Pipes.PipeDirection]::InOut); $p.Connect(5000); $null = $p.ReadByte(); $p.Dispose()"
        );
        let mut child = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();

        let accepted = unsafe { windows_sys::Win32::System::Pipes::ConnectNamedPipe(server_handle, std::ptr::null_mut()) } != 0
            || unsafe { GetLastError() } == windows_sys::Win32::Foundation::ERROR_PIPE_CONNECTED;
        assert!(accepted);

        let mut client_pid = 0;
        let got_pid = unsafe { windows_sys::Win32::System::Pipes::GetNamedPipeClientProcessId(server_handle, &mut client_pid) } != 0;
        assert!(got_pid);
        assert_eq!(client_pid, child.id());

        // Test harness itself spawned powershell.exe, so disable service_pid exclusion here.
        let caller = resolve_caller_with(Some(client_pid), 0).unwrap();
        assert!(!caller.exe_key.is_empty());
        assert_ne!(caller.pid, 0);

        let mut written = 0u32;
        let byte = [1u8];
        unsafe {
            windows_sys::Win32::Storage::FileSystem::WriteFile(server_handle, byte.as_ptr(), 1, &mut written, std::ptr::null_mut());
            windows_sys::Win32::Storage::FileSystem::FlushFileBuffers(server_handle);
            windows_sys::Win32::System::Pipes::DisconnectNamedPipe(server_handle);
            CloseHandle(server_handle);
        }
        let _ = child.wait();
    }
}
