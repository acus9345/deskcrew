// The command agents run: hand the words to the service, print what comes back.
use crate::pipe::Pipe;
use std::{
    env,
    io::Write,
    mem::zeroed,
    ptr::null,
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::CloseHandle,
    System::Threading::{CreateProcessW, PROCESS_INFORMATION, STARTUPINFOW},
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
    } else {
        "agent".into()
    }
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
