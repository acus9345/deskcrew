// Work around the desktop rather than in a window: processes, shell commands, web pages, files,
// the registry, notifications and system settings. These run in the command itself, with no
// service, and keep replies short like the rest.
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const MAX_OUTPUT: usize = 20_000;
const MAX_LIST: usize = 300;
const MAX_FOUND: usize = 200;
const MAX_FIND_DEPTH: usize = 12;
const RUN_DEFAULT_MS: u64 = 60_000;
const RUN_MOST_MS: u64 = 600_000;
const FETCH_SECONDS: u64 = 30;
// Processes an agent must never end: the system itself, the person's desktop, and session login.
#[cfg(windows)]
const VITAL: [&str; 12] = ["system", "smss", "csrss", "wininit", "winlogon", "lsass", "services", "svchost", "dwm", "fontdrvhost", "registry", "memory compression"];
#[cfg(target_os = "macos")]
const VITAL: [&str; 12] = ["launchd", "kernel_task", "windowserver", "loginwindow", "systemuiserver", "coreservicesd", "securityd", "opendirectoryd", "configd", "logd", "notifyd", "distnoted"];
#[cfg(target_os = "linux")]
const VITAL: [&str; 12] = ["systemd", "init", "kthreadd", "xorg", "xvfb", "xwayland", "gnome-shell", "dbus-daemon", "dbus-broker", "systemd-logind", "gdm", "lightdm"];

pub const COMMANDS: [&str; 8] = ["processes", "kill", "run", "fetch", "file", "reg", "notify", "settings"];

pub const HELP: &str = "\
Beyond windows: processes [filter]     kill pid|name [--force]     run \"command\" [--ms N]
fetch URL [--raw]     notify \"title\" \"message\"     settings [page]     reg get|list|set (Windows)
file list|read|write|append|find|copy|move|trash ...   write and copy refuse to overwrite without --force; trash uses the Recycle Bin";

fn cut(text: &str) -> String {
    if text.chars().count() <= MAX_OUTPUT {
        return text.to_string();
    }
    let kept: String = text.chars().take(MAX_OUTPUT).collect();
    format!("{kept}\n... cut at {MAX_OUTPUT} characters")
}

/// Run a command line through the shell, with a time limit. Output and errors come back together.
pub fn shell(line: &str, limit: Duration) -> Result<(String, Option<i32>), String> {
    #[cfg(windows)]
    let mut command = {
        let mut command = Command::new("powershell.exe");
        command.args(["-NoProfile", "-NonInteractive", "-Command", &format!("[Console]::OutputEncoding = [Text.Encoding]::UTF8; {line}")]);
        command
    };
    #[cfg(not(windows))]
    let mut command = {
        let mut command = Command::new(if cfg!(target_os = "macos") { "/bin/zsh" } else { "/bin/sh" });
        command.args(["-c", line]);
        command
    };
    let mut child = command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| format!("could not start the shell: {e}"))?;
    let mut out = child.stdout.take().ok_or("no output")?;
    let mut err = child.stderr.take().ok_or("no output")?;
    let reader = thread::spawn(move || {
        let mut text = Vec::new();
        let _ = out.read_to_end(&mut text);
        text
    });
    let errors = thread::spawn(move || {
        let mut text = Vec::new();
        let _ = err.read_to_end(&mut text);
        text
    });
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break Some(status);
        }
        if started.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        thread::sleep(Duration::from_millis(20));
    };
    let mut text = String::from_utf8_lossy(&reader.join().unwrap_or_default()).into_owned();
    let errors = String::from_utf8_lossy(&errors.join().unwrap_or_default()).into_owned();
    if !errors.trim().is_empty() {
        text.push_str(&errors);
    }
    match status {
        Some(status) => Ok((text, status.code())),
        None => Err(format!("{}stopped after {} ms", cut(text.trim_end()) + if text.trim().is_empty() { "" } else { "\n" }, limit.as_millis())),
    }
}

fn run(line: &str, ms: Option<u64>) -> Result<String, String> {
    let limit = Duration::from_millis(ms.unwrap_or(RUN_DEFAULT_MS).min(RUN_MOST_MS));
    let (text, code) = shell(line, limit)?;
    let body = cut(text.trim_end());
    match code {
        Some(0) => Ok(if body.is_empty() { "ok".into() } else { body }),
        Some(code) => Err(format!("{}exit code {code}", if body.is_empty() { String::new() } else { format!("{body}\n") })),
        None => Err(format!("{body}\nended without an exit code")),
    }
}

/// A process list as `pid name` lines, with the window title where it has one.
fn processes(filter: Option<&str>) -> Result<String, String> {
    let wanted = filter.map(str::to_lowercase);
    let mut all: Vec<(u32, String)> = crate::client::processes().into_iter().map(|(pid, (_, name))| (pid, name)).collect();
    all.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
    #[cfg(windows)]
    let titles: std::collections::HashMap<u32, String> = crate::win::list().into_iter().map(|w| (crate::win::pid(w.hwnd), w.title)).collect();
    #[cfg(not(windows))]
    let titles: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
    let shown: Vec<String> = all
        .iter()
        .filter(|(pid, name)| wanted.as_ref().is_none_or(|w| name.contains(w) || titles.get(pid).is_some_and(|t| t.to_lowercase().contains(w))))
        .take(MAX_LIST)
        .map(|(pid, name)| match titles.get(pid) {
            Some(title) => format!("{pid} {name} \"{}\"", crate::lines::clip(title, 60)),
            None => format!("{pid} {name}"),
        })
        .collect();
    Ok(if shown.is_empty() { "no processes match".into() } else { shown.join("\n") })
}

/// This command and every process above it, so an agent cannot end itself or its own shell.
fn ancestors() -> Vec<u32> {
    let all = crate::client::processes();
    let mut chain = vec![std::process::id()];
    while let Some((parent, _)) = all.get(chain.last().unwrap_or(&0)) {
        if chain.contains(parent) || *parent == 0 {
            break;
        }
        chain.push(*parent);
    }
    chain
}

fn kill(target: &str, force: bool) -> Result<String, String> {
    let all = crate::client::processes();
    let wanted = target.to_lowercase();
    let wanted = wanted.strip_suffix(".exe").unwrap_or(&wanted).to_string();
    let matches: Vec<(u32, String)> = match target.parse::<u32>() {
        Ok(pid) => all.get(&pid).map(|(_, name)| vec![(pid, name.clone())]).ok_or(format!("no process {pid}"))?,
        Err(_) => all.iter().filter(|(_, (_, name))| *name == wanted).map(|(pid, (_, name))| (*pid, name.clone())).collect(),
    };
    if matches.is_empty() {
        return Err(format!("no process named '{target}'"));
    }
    let mine = ancestors();
    for (pid, name) in &matches {
        if VITAL.contains(&name.as_str()) || *pid <= 4 {
            return Err(format!("refused: {name} ({pid}) is part of the operating system"));
        }
        if mine.contains(pid) {
            return Err(format!("refused: {name} ({pid}) is running this agent"));
        }
        if (name == "explorer" || name == "finder" || name == "dock") && !force {
            return Err(format!("{name} is the person's desktop: ask them, then add --force"));
        }
    }
    if matches.len() > 1 && !force {
        return Err(format!("'{target}' matches {} processes ({}): ask the person, then add --force to end them all", matches.len(), matches.iter().map(|m| m.0.to_string()).collect::<Vec<_>>().join(", ")));
    }
    let mut ended = Vec::new();
    for (pid, name) in &matches {
        end(*pid).map_err(|why| format!("could not end {name} ({pid}): {why}"))?;
        ended.push(format!("{name} ({pid})"));
    }
    Ok(format!("ended {}", ended.join(", ")))
}

#[cfg(windows)]
fn end(pid: u32) -> Result<(), String> {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess},
    };
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if handle.is_null() {
            return Err("Windows refused (it may run as administrator)".into());
        }
        let done = TerminateProcess(handle, 1) != 0;
        CloseHandle(handle);
        if done { Ok(()) } else { Err("Windows refused".into()) }
    }
}

#[cfg(not(windows))]
fn end(pid: u32) -> Result<(), String> {
    let done = Command::new("/bin/kill").arg(pid.to_string()).status().map_err(|e| e.to_string())?;
    if done.success() { Ok(()) } else { Err("the system refused".into()) }
}

/// A web page as readable text: scripts, styles and tags dropped, entities and spacing tidied.
fn page_text(html: &str) -> String {
    let lower = html.to_lowercase();
    let mut out = String::with_capacity(html.len() / 3);
    let mut at = 0;
    let bytes = html.as_bytes();
    while at < bytes.len() {
        if bytes[at] == b'<' {
            let rest = &lower[at..];
            let skip_to = ["script", "style", "noscript", "svg"]
                .iter()
                .find(|tag| rest.starts_with(&format!("<{tag}")))
                .and_then(|tag| rest.find(&format!("</{tag}>")).map(|end| at + end + tag.len() + 3));
            let end = skip_to.unwrap_or_else(|| rest.find('>').map_or(bytes.len(), |end| at + end + 1));
            let tag = &lower[at..end.min(lower.len())];
            if ["<p", "<br", "<div", "<li", "<tr", "<h1", "<h2", "<h3", "<h4", "</p", "</div", "</li", "</h"].iter().any(|t| tag.starts_with(t)) {
                out.push('\n');
            } else {
                out.push(' ');
            }
            at = end;
            continue;
        }
        let next = html[at..].find('<').map_or(html.len(), |n| at + n);
        out.push_str(&html[at..next]);
        at = next;
    }
    let decoded = out
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'");
    let mut lines: Vec<String> = Vec::new();
    for line in decoded.lines() {
        let flat = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if !flat.is_empty() && lines.last() != Some(&flat) {
            lines.push(flat);
        }
    }
    lines.join("\n")
}

fn fetch(url: &str, raw: bool) -> Result<String, String> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("fetch takes an http:// or https:// address".into());
    }
    let quoted = url.replace('\'', "''");
    #[cfg(windows)]
    let line = format!("$ProgressPreference = 'SilentlyContinue'; (Invoke-WebRequest -UseBasicParsing -TimeoutSec {FETCH_SECONDS} -Uri '{quoted}').Content");
    #[cfg(not(windows))]
    let line = format!("/usr/bin/curl -sSL --max-time {FETCH_SECONDS} '{quoted}'");
    let (body, code) = shell(&line, Duration::from_secs(FETCH_SECONDS + 5))?;
    if code != Some(0) {
        return Err(format!("could not fetch {url}: {}", crate::lines::clip(body.trim(), 300)));
    }
    let text = if raw { body.trim().to_string() } else { page_text(&body) };
    Ok(cut(text.trim()))
}

fn size_text(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 30 => format!("{:.1} GB", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.1} KB", b as f64 / 1024.0),
        b => format!("{b} B"),
    }
}

fn find(root: &Path, wanted: &str, depth: usize, found: &mut Vec<String>) {
    if depth > MAX_FIND_DEPTH || found.len() >= MAX_FOUND {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if name.contains(wanted) {
            found.push(path.display().to_string());
            if found.len() >= MAX_FOUND {
                return;
            }
        }
        if entry.file_type().is_ok_and(|t| t.is_dir() && !t.is_symlink()) {
            find(&path, wanted, depth + 1, found);
        }
    }
}

fn file(words: &[String], force: bool, from: usize) -> Result<String, String> {
    let usage = "file list|read|write|append|find|copy|move|trash ...";
    let (verb, rest) = words.split_first().ok_or(usage)?;
    let path = |at: usize| rest.get(at).map(PathBuf::from).ok_or(format!("file {verb} needs a path"));
    match verb.as_str() {
        "list" => {
            let folder = path(0).unwrap_or_else(|_| PathBuf::from("."));
            let mut entries: Vec<_> = fs::read_dir(&folder).map_err(|e| format!("{}: {e}", folder.display()))?.flatten().collect();
            entries.sort_by_key(|e| (!e.file_type().is_ok_and(|t| t.is_dir()), e.file_name()));
            let total = entries.len();
            let mut out: Vec<String> = entries
                .iter()
                .take(MAX_LIST)
                .map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    match e.metadata() {
                        Ok(m) if m.is_dir() => format!("{name}/"),
                        Ok(m) => format!("{name}  {}", size_text(m.len())),
                        Err(_) => name,
                    }
                })
                .collect();
            if total > MAX_LIST {
                out.push(format!("... {} more", total - MAX_LIST));
            }
            Ok(if out.is_empty() { "empty".into() } else { out.join("\n") })
        }
        "read" => {
            let file = path(0)?;
            let bytes = fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))?;
            if bytes.iter().take(4096).any(|b| *b == 0) {
                return Err(format!("{} is not text ({})", file.display(), size_text(bytes.len() as u64)));
            }
            let text = String::from_utf8_lossy(&bytes);
            let body: Vec<&str> = text.lines().skip(from).collect();
            Ok(cut(&body.join("\n")))
        }
        "write" | "append" => {
            let file = path(0)?;
            let text = rest.get(1).ok_or(format!("file {verb} needs a path and the text"))?;
            if verb == "write" && file.exists() && !force {
                return Err(format!("{} already exists: add --force to replace it, or use file append", file.display()));
            }
            if let Some(folder) = file.parent().filter(|p| !p.as_os_str().is_empty()) {
                fs::create_dir_all(folder).map_err(|e| e.to_string())?;
            }
            if verb == "write" {
                fs::write(&file, text).map_err(|e| e.to_string())?;
            } else {
                use std::io::Write;
                let mut handle = fs::OpenOptions::new().create(true).append(true).open(&file).map_err(|e| e.to_string())?;
                handle.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
            }
            Ok(format!("{} {}", if verb == "write" { "wrote" } else { "added to" }, file.display()))
        }
        "find" => {
            let (root, wanted) = match rest {
                [wanted] => (PathBuf::from("."), wanted),
                [root, wanted, ..] => (PathBuf::from(root), wanted),
                _ => return Err("file find [folder] name-part".into()),
            };
            let mut found = Vec::new();
            find(&root, &wanted.to_lowercase(), 0, &mut found);
            if found.len() >= MAX_FOUND {
                found.push(format!("... stopped at {MAX_FOUND}: narrow the folder or the name"));
            }
            Ok(if found.is_empty() { "nothing found".into() } else { found.join("\n") })
        }
        "copy" | "move" => {
            let (from_path, to) = (path(0)?, path(1)?);
            let to = if to.is_dir() { to.join(from_path.file_name().ok_or("that path has no file name")?) } else { to };
            if to.exists() && !force {
                return Err(format!("{} already exists: add --force to replace it", to.display()));
            }
            if verb == "copy" {
                fs::copy(&from_path, &to).map_err(|e| e.to_string())?;
            } else {
                fs::rename(&from_path, &to).map_err(|e| e.to_string())?;
            }
            Ok(format!("{} {} to {}", if verb == "copy" { "copied" } else { "moved" }, from_path.display(), to.display()))
        }
        "trash" => {
            let file = path(0)?;
            if !file.exists() {
                return Err(format!("{} does not exist", file.display()));
            }
            trash(&file)?;
            Ok(format!("moved {} to the {}", file.display(), if cfg!(windows) { "Recycle Bin" } else { "Trash" }))
        }
        _ => Err(usage.into()),
    }
}

/// Into the Recycle Bin, so the person can get it back.
#[cfg(windows)]
fn trash(path: &Path) -> Result<(), String> {
    use windows_sys::Win32::UI::Shell::{FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, SHFILEOPSTRUCTW, SHFileOperationW};
    let full = fs::canonicalize(path).map_err(|e| e.to_string())?;
    let text = full.display().to_string();
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
    // The list of paths ends with two nulls.
    let wide: Vec<u16> = text.encode_utf16().chain([0, 0]).collect();
    let mut operation: SHFILEOPSTRUCTW = unsafe { std::mem::zeroed() };
    operation.wFunc = FO_DELETE;
    operation.pFrom = wide.as_ptr();
    operation.fFlags = (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_SILENT) as u16;
    match unsafe { SHFileOperationW(&mut operation) } {
        0 if operation.fAnyOperationsAborted == 0 => Ok(()),
        code => Err(format!("Windows could not move it to the Recycle Bin (code {code})")),
    }
}

#[cfg(target_os = "macos")]
fn trash(path: &Path) -> Result<(), String> {
    let home = std::env::var_os("HOME").map(PathBuf::from).ok_or("no home folder")?;
    let name = path.file_name().ok_or("that path has no name")?;
    let mut target = home.join(".Trash").join(name);
    let mut n = 1;
    while target.exists() {
        target = home.join(".Trash").join(format!("{} {n}", name.to_string_lossy()));
        n += 1;
    }
    fs::rename(path, &target).map_err(|e| e.to_string())
}

/// The registry, through Windows' own reg tool. Writing is limited to the person's own settings.
#[cfg(windows)]
fn reg(words: &[String]) -> Result<String, String> {
    let usage = "reg get KEY [name] | reg list KEY | reg set HKCU\\KEY name value";
    let (verb, rest) = words.split_first().ok_or(usage)?;
    let quote = |s: &str| format!("'{}'", s.replace('\'', "''"));
    let line = match (verb.as_str(), rest) {
        ("get", [key]) | ("list", [key]) => format!("reg.exe query {}", quote(key)),
        ("get", [key, name]) => format!("reg.exe query {} /v {}", quote(key), quote(name)),
        ("set", [key, name, value]) => {
            let upper = key.to_uppercase();
            if !(upper.starts_with("HKCU\\") || upper.starts_with("HKEY_CURRENT_USER\\")) {
                return Err("reg set only changes the person's own settings, under HKCU".into());
            }
            format!("reg.exe add {} /v {} /t REG_SZ /d {} /f", quote(key), quote(name), quote(value))
        }
        _ => return Err(usage.into()),
    };
    let (text, code) = shell(&line, Duration::from_secs(15))?;
    let text = text.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect::<Vec<_>>().join("\n");
    match code {
        Some(0) => Ok(cut(if text.is_empty() { "ok" } else { &text })),
        _ => Err(if text.is_empty() { "the registry refused".into() } else { text }),
    }
}

/// The desktop's own trash, as the freedesktop.org spec lays it out, so a file manager can restore it.
#[cfg(target_os = "linux")]
fn trash(path: &Path) -> Result<(), String> {
    let home = std::env::var_os("HOME").map(PathBuf::from).ok_or("no home folder")?;
    let data = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".local").join("share"));
    let (files, info) = (data.join("Trash").join("files"), data.join("Trash").join("info"));
    fs::create_dir_all(&files).and_then(|_| fs::create_dir_all(&info)).map_err(|e| e.to_string())?;
    let name = path.file_name().ok_or("that path has no name")?.to_string_lossy().into_owned();
    let mut kept = name.clone();
    let mut n = 1;
    while files.join(&kept).exists() || info.join(format!("{kept}.trashinfo")).exists() {
        kept = format!("{name}.{n}");
        n += 1;
    }
    let full = fs::canonicalize(path).map_err(|e| e.to_string())?;
    let when = Command::new("date").arg("+%Y-%m-%dT%H:%M:%S").output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    fs::write(info.join(format!("{kept}.trashinfo")), format!("[Trash Info]
Path={}
DeletionDate={when}
", full.display())).map_err(|e| e.to_string())?;
    fs::rename(path, files.join(&kept)).map_err(|e| format!("could not move it to the trash: {e}"))
}

#[cfg(not(windows))]
fn reg(_words: &[String]) -> Result<String, String> {
    Err(if cfg!(target_os = "macos") { "a Mac has no registry: read settings with run \"defaults read ...\"" } else { "Linux has no registry: read settings with run \"gsettings get ...\"" }.into())
}

fn notify(title: &str, message: &str) -> Result<String, String> {
    #[cfg(windows)]
    let quote = |s: &str| s.replace('\'', "''");
    #[cfg(windows)]
    let line = format!(
        "$x = [Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime]::GetTemplateContent([Windows.UI.Notifications.ToastTemplateType]::ToastText02); \
         $t = $x.GetElementsByTagName('text'); [void]$t.Item(0).AppendChild($x.CreateTextNode('{}')); [void]$t.Item(1).AppendChild($x.CreateTextNode('{}')); \
         [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('{{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}}\\WindowsPowerShell\\v1.0\\powershell.exe').Show([Windows.UI.Notifications.ToastNotification]::new($x))",
        quote(title),
        quote(message)
    );
    #[cfg(target_os = "macos")]
    let line = format!("/usr/bin/osascript -e 'display notification \"{}\" with title \"{}\"'", message.replace(['"', '\''], ""), title.replace(['"', '\''], ""));
    #[cfg(target_os = "linux")]
    let line = format!("notify-send '{}' '{}'", title.replace('\'', ""), message.replace('\'', ""));
    let (text, code) = shell(&line, Duration::from_secs(15))?;
    if code == Some(0) { Ok("shown".into()) } else { Err(format!("the notification was refused: {}", crate::lines::clip(text.trim(), 200))) }
}

/// A page of system settings, by its own name such as `display`, `bluetooth` or `sound`.
fn settings(page: Option<&str>) -> Result<String, String> {
    let page: String = page.unwrap_or("").chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
    #[cfg(windows)]
    let (program, link) = ("explorer.exe", format!("ms-settings:{page}"));
    #[cfg(target_os = "macos")]
    let (program, link) = ("/usr/bin/open", if page.is_empty() { "x-apple.systempreferences:".to_string() } else { format!("x-apple.systempreferences:com.apple.{page}") });
    #[cfg(target_os = "linux")]
    let (program, link) = ("gnome-control-center", page.clone());
    Command::new(program).arg(&link).spawn().map_err(|e| format!("could not open {link}: {e}"))?;
    Ok(format!("opened {link}: it takes the front, so work in it with see and click, then close it"))
}

/// Run one of these commands, or None if the words are for the service.
pub fn handle(words: &[String]) -> Option<Result<String, String>> {
    let force = words.iter().any(|w| w == "--force");
    let raw = words.iter().any(|w| w == "--raw");
    let number = |flag: &str| words.iter().position(|w| w == flag).and_then(|at| words.get(at + 1)).and_then(|n| n.parse::<u64>().ok());
    let mut plain = Vec::new();
    let mut skip = false;
    for (at, word) in words.iter().enumerate() {
        if skip {
            skip = false;
            continue;
        }
        if word == "--ms" || word == "--from" {
            skip = true;
            continue;
        }
        if word == "--force" || word == "--raw" || (at > 0 && word == "--") {
            continue;
        }
        plain.push(word.clone());
    }
    let (verb, rest) = plain.split_first()?;
    if !COMMANDS.contains(&verb.as_str()) {
        return None;
    }
    Some(match (verb.as_str(), rest) {
        ("processes", rest) => processes(rest.first().map(String::as_str)),
        ("kill", [target]) => kill(target, force),
        ("run", [line]) => run(line, number("--ms")),
        ("fetch", [url]) => fetch(url, raw),
        ("file", rest) => file(rest, force, number("--from").unwrap_or(0) as usize),
        ("reg", rest) => reg(rest),
        ("notify", [title, message]) => notify(title, message),
        ("notify", [message]) => notify("deskcrew", message),
        ("settings", rest) => settings(rest.first().map(String::as_str)),
        _ => Err(HELP.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_reads_as_text() {
        let html = "<html><head><style>p{}</style><script>var x=1;</script></head><body><h1>Title</h1><p>One &amp; two</p><div>Three</div></body></html>";
        assert_eq!(page_text(html), "Title\nOne & two\nThree");
    }

    #[test]
    fn files_are_written_once_and_trashed_not_deleted() {
        let dir = std::env::temp_dir().join(format!("deskcrew-files-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let file_path = dir.join("a.txt").display().to_string();
        let words = |list: &[&str]| list.iter().map(|w| w.to_string()).collect::<Vec<_>>();
        assert!(file(&words(&["write", &file_path, "hello"]), false, 0).is_ok());
        assert!(file(&words(&["write", &file_path, "again"]), false, 0).unwrap_err().contains("--force"));
        assert!(file(&words(&["append", &file_path, " world"]), false, 0).is_ok());
        assert_eq!(file(&words(&["read", &file_path]), false, 0).unwrap(), "hello world");
        assert!(file(&words(&["find", &dir.display().to_string(), "A.T"]), false, 0).unwrap().ends_with("a.txt"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn vital_and_own_processes_are_refused() {
        assert!(kill(&std::process::id().to_string(), true).unwrap_err().contains("running this agent"));
        let vital = if cfg!(windows) { "csrss" } else if cfg!(target_os = "macos") { "launchd" } else { "1" };
        assert!(kill(vital, true).unwrap_err().contains("operating system"));
    }

    #[test]
    fn other_words_go_to_the_service() {
        assert!(handle(&["click".to_string(), "e1".to_string()]).is_none());
        assert!(handle(&["run".to_string(), "echo hi".to_string()]).is_some());
    }
}
