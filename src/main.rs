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
#[cfg(windows)]
mod uia;
#[cfg(windows)]
mod win;

fn main() {
    let args: Vec<String> = std::env::args_os().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
    match args.first().map(String::as_str) {
        Some("service") => service::run(),
        Some("setup") => std::process::exit(setup::run()),
        #[cfg(windows)]
        Some("demo") => {
            let number = |flag: &str, default: u64| {
                args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).and_then(|s| s.parse().ok()).unwrap_or(default)
            };
            demo::run(number("--seconds", 12), number("--linger", 0));
        }
        Some("--version") => println!("deskcrew {}", env!("CARGO_PKG_VERSION")),
        None | Some("help" | "--help" | "-h") => println!("{}", service::HELP),
        _ => std::process::exit(client::run(args)),
    }
}
