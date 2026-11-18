// deskcrew - every agent gets its own cursor on the desktop.
#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
compile_error!("deskcrew runs on Windows, macOS and Linux (X11) only");

#[cfg(windows)]
mod act;
#[cfg(windows)]
mod apps;
#[cfg(windows)]
mod borrow;
mod client;
#[cfg(windows)]
mod demo;
mod lines;
mod log;
#[cfg_attr(any(target_os = "linux", target_os = "macos"), allow(dead_code))]
mod look;
mod mcp;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod mac;
#[cfg_attr(any(target_os = "linux", target_os = "macos"), allow(dead_code))]
mod motion;
#[cfg(windows)]
mod overlay;
#[cfg(target_os = "macos")]
#[path = "mac/overlay.rs"]
mod overlay;
#[cfg(windows)]
mod pen;
#[cfg(windows)]
mod pipe;
#[cfg(unix)]
#[path = "mac/pipe.rs"]
mod pipe;
#[cfg(windows)]
mod real;
#[cfg(windows)]
mod service;
// The drawing code's tests reach the overlay by this name on every system.
#[cfg(all(target_os = "linux", test))]
use linux::overlay;
#[cfg(target_os = "linux")]
use linux::service;
#[cfg(target_os = "macos")]
use mac::service;
mod setup;
mod shot;
mod system;
mod trace;
#[cfg(windows)]
mod uia;
#[cfg(windows)]
mod win;

fn main() {
    let args: Vec<String> = std::env::args_os().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
    if matches!(args.first().map(String::as_str), Some("service")) {
        service::run();
        return;
    }
    if matches!(args.first().map(String::as_str), Some("mcp")) {
        mcp::serve();
        return;
    }
    let mut words = args.clone();
    let who = client::agent(&mut words);
    match words.first().map(String::as_str) {
        Some("--version" | "-V") => println!("deskcrew {}", env!("CARGO_PKG_VERSION")),
        Some("setup") => std::process::exit(setup::run()),
        #[cfg(windows)]
        Some("demo") => {
            let number = |flag: &str, default: u64| {
                words.iter().position(|a| a == flag).and_then(|i| words.get(i + 1)).and_then(|s| s.parse().ok()).unwrap_or(default)
            };
            if words.iter().any(|a| a == "--typing") {
                demo::typing();
            } else {
                demo::run(number("--seconds", 12), number("--linger", 0));
            }
        }
        Some("log") => println!("{}", log::show(&words)),
        Some("session" | "trace") => {
            let reply = if words[0] == "session" { trace::session_command(&who, &words[1..]) } else { trace::trace_command(&who, &words[1..]) };
            match reply {
                Ok(text) => println!("{text}"),
                Err(text) => {
                    println!("{text}");
                    std::process::exit(1);
                }
            }
        }
        None | Some("help" | "--help" | "-h") => println!("{}
{}
{}", service::HELP, system::HELP, trace::HELP),
        _ => {
            // Processes, files, the shell and the like need no window, so they skip the service.
            let session = system::COMMANDS.contains(&words[0].as_str()).then(|| trace::session_of(&who, &mut words)).flatten();
            let started = std::time::Instant::now();
            if let Some(reply) = system::handle(&words) {
                let (ok, text) = match &reply {
                    Ok(text) => (true, text),
                    Err(text) => (false, text),
                };
                log::record(&who, &words, ok, text);
                if let Some(session) = &session {
                    session.record(&who, &words, ok, text, started.elapsed(), None, None);
                }
                match reply {
                    Ok(text) => println!("{text}"),
                    Err(text) => {
                        println!("{text}");
                        std::process::exit(1);
                    }
                }
                return;
            }
            #[cfg(target_os = "linux")]
            if let Some(problem) = linux::x::display_problem() {
                eprintln!("{problem}");
                std::process::exit(1);
            }
            std::process::exit(client::run(args))
        }
    }
}
