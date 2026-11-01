// The command agents run: hand the words to the service, print what comes back.
use crate::pipe::Pipe;
use std::{
    collections::HashMap,
    env,
    io::Write,
    mem::zeroed,
    ptr::null,
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
    System::{
        Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS},
        Threading::{CreateProcessW, PROCESS_INFORMATION, STARTUPINFOW},
    },
};

const START_TIMEOUT: Duration = Duration::from_secs(4);
const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

/// Who is asking, so each agent keeps its own cursor. A flag beats the environment.
fn agent(args: &mut Vec<String>) -> String {
    if let Some(at) = args.iter().position(|a| a == "--agent")
        && at + 1 < args.len() {
            let name = args.remove(at + 1);
            args.remove(at);
            return name;
        }
    let has = |name: &str| env::var_os(name).is_some();
    if let Ok(name) = env::var("DESKCREW_AGENT") {
        name
    } else if has("CLAUDECODE") || has("CLAUDE_CODE_ENTRYPOINT") {
        "Claude Code".into()
    } else if has("CURSOR_TRACE_ID") || has("CURSOR_AGENT") {
        "Cursor".into()
    } else if has("CODEX_HOME") || has("CODEX_SANDBOX") {
        "Codex".into()
    } else if has("GEMINI_CLI") {
        "Gemini".into()
    } else if has("COPILOT_CLI") {
        "Copilot".into()
    } else if has("OPENCODE") {
        "OpenCode".into()
    } else if has("CLINE_ACTIVE") {
        "Cline".into()
    } else if has("AMP_CURRENT_THREAD_ID") {
        "Amp".into()
    } else {
        launched_by().unwrap_or("your agent").into()
    }
}

/// Program names of the agents that set nothing in the environment.
const AGENT_PROGRAMS: [(&str, &str); 13] = [
    ("claude.exe", "Claude Code"),
    ("cursor.exe", "Cursor"),
    ("cursor-agent.exe", "Cursor"),
    ("codex.exe", "Codex"),
    ("gemini.exe", "Gemini"),
    ("copilot.exe", "Copilot"),
    ("windsurf.exe", "Windsurf"),
    ("opencode.exe", "OpenCode"),
    ("aider.exe", "Aider"),
    ("goose.exe", "Goose"),
    ("amp.exe", "Amp"),
    ("kiro.exe", "Kiro"),
    ("kiro-cli.exe", "Kiro"),
];
const MOST_ANCESTORS: usize = 16;

/// The nearest agent among the processes that led to this one.
fn launched_by() -> Option<&'static str> {
    let mut parents = HashMap::new();
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut entry: PROCESSENTRY32W = zeroed();
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut more = Process32FirstW(snapshot, &mut entry) != 0;
        while more {
            let end = entry.szExeFile.iter().position(|c| *c == 0).unwrap_or(entry.szExeFile.len());
            parents.insert(entry.th32ProcessID, (entry.th32ParentProcessID, String::from_utf16_lossy(&entry.szExeFile[..end]).to_lowercase()));
            more = Process32NextW(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
    }
    let mut id = std::process::id();
    for _ in 0..MOST_ANCESTORS {
        let (parent, _) = parents.get(&id)?;
        let (_, program) = parents.get(parent)?;
        if let Some((_, name)) = AGENT_PROGRAMS.iter().find(|(known, _)| known == program) {
            return Some(name);
        }
        id = *parent;
    }
    None
}

/// Nothing is inherited: a service holding the caller's output pipe would hang the agent's shell.
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

pub fn run(mut args: Vec<String>) -> i32 {
    let who = agent(&mut args);
    let mut words = vec![who];
    words.extend(args);
    let message = words.join("\0");
    let answer = connect().and_then(|pipe| {
        pipe.send(message.as_bytes()).map_err(|e| e.to_string())?;
        pipe.receive().map_err(|e| format!("the deskcrew service stopped answering: {e}"))
    });
    match answer {
        Ok(reply) if !reply.is_empty() => {
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(&reply[1..]);
            let _ = out.write_all(b"\n");
            reply[0] as i32
        }
        Ok(_) => 1,
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}
