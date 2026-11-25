// The command agents run: hand the words to the service, print what comes back.
use crate::pipe::Pipe;
use std::{
    collections::HashMap,
    env,
    io::Write,
    thread,
    time::{Duration, Instant},
};
#[cfg(windows)]
use std::{mem::zeroed, ptr::null};
#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
    System::{
        Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS},
        Threading::{CreateProcessW, PROCESS_INFORMATION, STARTUPINFOW},
    },
};

const START_TIMEOUT: Duration = Duration::from_secs(4);
#[cfg(windows)]
const DETACHED_PROCESS: u32 = 0x0000_0008;
#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
#[cfg(windows)]
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

/// Who is asking, so each agent keeps its own cursor. A flag beats the environment.
pub fn agent(args: &mut Vec<String>) -> String {
    let env_name = env::var("DESKCREW_AGENT").ok();
    agent_from(args, env_name.as_deref(), || set_by_agent().or_else(launched_by).unwrap_or("your agent").into())
}

fn agent_from(args: &mut Vec<String>, env_agent: Option<&str>, fallback: impl FnOnce() -> String) -> String {
    let limit = args.iter().position(|a| a == "--").unwrap_or(args.len());
    if let Some(at) = args[..limit].iter().position(|a| a == "--agent") {
        if at + 1 < limit {
            let name = args.remove(at + 1);
            args.remove(at);
            if !name.trim().is_empty() {
                return name.trim().to_string();
            }
        } else {
            args.remove(at);
        }
    }
    env_agent.map(str::trim).filter(|n| !n.is_empty()).map_or_else(fallback, str::to_string)
}

/// The agent named by a variable it sets in the environment.
fn set_by_agent() -> Option<&'static str> {
    let has = |name: &str| env::var_os(name).is_some();
    if has("CLAUDECODE") || has("CLAUDE_CODE_ENTRYPOINT") {
        Some("Claude Code")
    } else if has("CURSOR_TRACE_ID") || has("CURSOR_AGENT") {
        Some("Cursor")
    } else if has("CODEX_HOME") || has("CODEX_SANDBOX") {
        Some("Codex")
    } else if has("GEMINI_CLI") {
        Some("Gemini")
    } else if has("COPILOT_CLI") {
        Some("Copilot")
    } else if has("OPENCODE") {
        Some("OpenCode")
    } else if has("CLINE_ACTIVE") {
        Some("Cline")
    } else if has("AMP_CURRENT_THREAD_ID") {
        Some("Amp")
    } else {
        None
    }
}

/// Program names of the agents that set nothing in the environment.
const AGENT_PROGRAMS: [(&str, &str); 16] = [
    ("claude", "Claude Code"),
    ("cursor", "Cursor"),
    ("cursor-agent", "Cursor"),
    ("codex", "Codex"),
    ("gemini", "Gemini"),
    ("copilot", "Copilot"),
    ("windsurf", "Windsurf"),
    ("opencode", "OpenCode"),
    ("aider", "Aider"),
    ("goose", "Goose"),
    ("amp", "Amp"),
    ("kiro", "Kiro"),
    ("kiro-cli", "Kiro"),
    ("antigravity", "Antigravity"),
    ("grok", "Grok"),
    ("zed", "Zed"),
];
pub const MOST_ANCESTORS: usize = 16;

/// Display name for a known agent program stem, or `None` if it is not a recognised agent binary.
pub fn agent_program(program: &str) -> Option<&'static str> {
    AGENT_PROGRAMS.iter().find(|(known, _)| *known == program).map(|(_, name)| *name)
}

/// Every process's parent and lower-case program name, without folder or `.exe`.
#[cfg(not(windows))]
pub fn processes() -> HashMap<u32, (u32, String)> {
    let flags = if cfg!(target_os = "linux") { "-eo" } else { "-axo" };
    let listed = std::process::Command::new("ps").args([flags, "pid=,ppid=,comm="]).output();
    let text = listed.map(|out| String::from_utf8_lossy(&out.stdout).into_owned()).unwrap_or_default();
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let pid = parts.next()?.parse().ok()?;
            let parent = parts.next()?.parse().ok()?;
            let program = parts.collect::<Vec<_>>().join(" ");
            let program = program.rsplit('/').next().unwrap_or("").to_lowercase();
            Some((pid, (parent, program)))
        })
        .collect()
}

#[cfg(windows)]
pub fn processes() -> HashMap<u32, (u32, String)> {
    let mut parents = HashMap::new();
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return parents;
        }
        let mut entry: PROCESSENTRY32W = zeroed();
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut more = Process32FirstW(snapshot, &mut entry) != 0;
        while more {
            let end = entry.szExeFile.iter().position(|c| *c == 0).unwrap_or(entry.szExeFile.len());
            let program = String::from_utf16_lossy(&entry.szExeFile[..end]).to_lowercase();
            let program = program.strip_suffix(".exe").map(str::to_string).unwrap_or(program);
            parents.insert(entry.th32ProcessID, (entry.th32ParentProcessID, program));
            more = Process32NextW(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
    }
    parents
}

/// The nearest agent among the processes that led to this one.
fn launched_by() -> Option<&'static str> {
    let parents = processes();
    let mut id = std::process::id();
    for _ in 0..MOST_ANCESTORS {
        let (parent, _) = parents.get(&id)?;
        let (_, program) = parents.get(parent)?;
        if let Some(name) = agent_program(program) {
            return Some(name);
        }
        id = *parent;
    }
    None
}

/// Nothing is inherited: a service holding the caller's output pipe would hang the agent's shell.
/// Its own process group keeps it running when the agent's shell ends.
#[cfg(not(windows))]
fn start_service() -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let exe = env::current_exe().map_err(|e| e.to_string())?;
    Command::new(exe)
        .arg("service")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("could not start the deskcrew service: {e}"))
}

#[cfg(windows)]
fn spawn(exe: &str, flags: u32) -> bool {
    let wide = |text: &str| text.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let application = wide(exe);
    let mut line = wide(&format!("\"{exe}\" service"));
    unsafe {
        let mut startup: STARTUPINFOW = zeroed();
        startup.cb = size_of::<STARTUPINFOW>() as u32;
        let mut process: PROCESS_INFORMATION = zeroed();
        let started = CreateProcessW(application.as_ptr(), line.as_mut_ptr(), null(), null(), 0, flags, null(), null(), &startup, &mut process);
        if started != 0 {
            CloseHandle(process.hThread);
            CloseHandle(process.hProcess);
        }
        started != 0
    }
}

#[cfg(windows)]
fn start_service() -> Result<(), String> {
    let exe = env::current_exe().map_err(|e| e.to_string())?;
    let exe = exe.to_string_lossy();
    // Outlive whatever job the calling tool runs commands in, when that job allows it.
    if spawn(&exe, DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB) || spawn(&exe, DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP) {
        Ok(())
    } else {
        Err("could not start the deskcrew service".into())
    }
}

fn connect() -> Result<Pipe, String> {
    if let Ok(pipe) = Pipe::connect() {
        return Ok(pipe);
    }
    start_service()?;
    let started = Instant::now();
    loop {
        match Pipe::connect() {
            Ok(pipe) => return Ok(pipe),
            Err(e) if started.elapsed() > START_TIMEOUT => return Err(format!("the deskcrew service did not start: {e}")),
            Err(_) => thread::sleep(Duration::from_millis(15)),
        }
    }
}

fn encode_words(words: &[String]) -> Result<String, String> {
    if words.iter().any(|w| w.contains('\0')) {
        return Err("arguments cannot contain NUL bytes".into());
    }
    Ok(words.join("\0"))
}

/// One request to the service as this agent, without logging it: whether it worked, and the reply.
pub fn ask(who: &str, args: &[String]) -> Result<(bool, String), String> {
    let mut words = vec![who.to_string()];
    words.extend(args.iter().cloned());
    let message = encode_words(&words)?;
    let reply = connect().and_then(|pipe| {
        pipe.send(message.as_bytes()).map_err(|e| e.to_string())?;
        pipe.receive().map_err(|e| format!("the deskcrew service stopped answering: {e}"))
    })?;
    let (code, text) = reply.split_first().ok_or("the deskcrew service sent nothing back")?;
    Ok((*code == 0, String::from_utf8_lossy(text).into_owned()))
}

pub fn run(mut args: Vec<String>) -> i32 {
    let who = agent(&mut args);
    let session = crate::trace::session_of(&who, &mut args);
    let before = session.as_ref().and_then(|s| s.picture(&who, &args, "before"));
    let started = Instant::now();
    let mut words = vec![who];
    words.extend(args);
    let answer = encode_words(&words).and_then(|message| {
        connect().and_then(|pipe| {
            pipe.send(message.as_bytes()).map_err(|e| e.to_string())?;
            pipe.receive().map_err(|e| format!("the deskcrew service stopped answering: {e}"))
        })
    });
    let log_reply = |ok: bool, text: &str| {
        crate::log::record(&words[0], &words[1..], ok, text);
        if let Some(session) = &session {
            let after = before.is_some().then(|| session.picture(&words[0], &words[1..], "after")).flatten();
            session.record(&words[0], &words[1..], ok, text, started.elapsed(), before.as_deref(), after.as_deref());
        }
    };
    match answer {
        Ok(reply) if !reply.is_empty() => {
            log_reply(reply[0] == 0, &String::from_utf8_lossy(&reply[1..]));
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(&reply[1..]);
            let _ = out.write_all(b"\n");
            reply[0] as i32
        }
        Ok(_) => {
            let error = "the deskcrew service sent nothing back";
            log_reply(false, error);
            eprintln!("{error}");
            1
        }
        Err(error) => {
            log_reply(false, &error);
            eprintln!("{error}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_flag_and_env_fallbacks_handle_blank_values() {
        let mut args = vec!["status".into(), "--agent".into(), " Alice ".into()];
        assert_eq!(agent_from(&mut args, Some("Bob"), || "Fallback".into()), "Alice");
        assert_eq!(args, vec!["status"]);

        let mut trailing = vec!["status".into(), "--agent".into()];
        assert_eq!(agent_from(&mut trailing, Some("   "), || "Fallback".into()), "Fallback");
        assert_eq!(trailing, vec!["status"]);

        let mut before_sep = vec!["click".into(), "e1".into(), "--agent".into(), "--".into(), "literal".into()];
        assert_eq!(agent_from(&mut before_sep, Some("Bob"), || "Fallback".into()), "Bob");
        assert_eq!(before_sep, vec!["click", "e1", "--", "literal"]);

        let mut after_sep = vec!["type".into(), "e1".into(), "--".into(), "--agent".into(), "foo".into()];
        assert_eq!(agent_from(&mut after_sep, None, || "Fallback".into()), "Fallback");
        assert_eq!(after_sep, vec!["type", "e1", "--", "--agent", "foo"]);

        let mut blank_flag = vec!["--agent".into(), "  ".into(), "see".into()];
        assert_eq!(agent_from(&mut blank_flag, Some("Bob"), || "Fallback".into()), "Bob");
        assert_eq!(blank_flag, vec!["see"]);
    }

    #[test]
    fn encode_words_rejects_embedded_nul_bytes() {
        assert_eq!(encode_words(&["Agent".into(), "status".into()]).unwrap(), "Agent\0status");
        assert!(encode_words(&["Bad\0Agent".into(), "status".into()]).is_err());
        assert!(encode_words(&["Agent".into(), "bad\0arg".into()]).is_err());
    }
}
