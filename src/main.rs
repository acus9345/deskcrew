// deskcrew - every agent gets its own cursor on the desktop.
#[cfg(not(any(windows, target_os = "macos")))]
compile_error!("deskcrew runs on Windows and macOS only");

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
mod look;
mod mcp;
#[cfg(target_os = "macos")]
mod mac;
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
#[cfg(target_os = "macos")]
#[path = "mac/pipe.rs"]
mod pipe;
#[cfg(windows)]
mod real;
#[cfg(windows)]
mod service;
#[cfg(target_os = "macos")]
use mac::service;
mod setup;
mod shot;
mod system;
#[cfg(windows)]
mod uia;
#[cfg(windows)]
mod win;

fn main() {
    let args: Vec<String> = std::env::args_os().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
    match args.first().map(String::as_str) {
        Some("service") => service::run(),
        Some("mcp") => mcp::serve(),
        Some("setup") => std::process::exit(setup::run()),
        #[cfg(windows)]
        Some("demo") => {
            let number = |flag: &str, default: u64| {
                args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).and_then(|s| s.parse().ok()).unwrap_or(default)
            };
            demo::run(number("--seconds", 12), number("--linger", 0));
        }
        Some("--version") => println!("deskcrew {}", env!("CARGO_PKG_VERSION")),
        None | Some("help" | "--help" | "-h") => println!("{}
{}", service::HELP, system::HELP),
        _ => {
            // Processes, files, the shell and the like need no window, so they skip the service.
            let words: Vec<String> = args.iter().enumerate().filter(|(at, _)| !(args.get(at.wrapping_sub(1)).is_some_and(|w| w == "--agent") || args[*at] == "--agent")).map(|(_, w)| w.clone()).collect();
            if let Some(reply) = system::handle(&words) {
                match reply {
                    Ok(text) => println!("{text}"),
                    Err(text) => {
                        println!("{text}");
                        std::process::exit(1);
                    }
                }
                return;
            }
            std::process::exit(client::run(args))
        }
    }
}
