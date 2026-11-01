// deskcrew - every agent gets its own cursor on the desktop.
#[cfg(not(windows))]
compile_error!("deskcrew only runs on Windows for now");

mod act;
mod apps;
mod client;
mod demo;
mod lines;
mod motion;
mod overlay;
mod pen;
mod pipe;
mod real;
mod service;
mod shot;
mod uia;
mod win;

fn main() {
    let args: Vec<String> = std::env::args_os().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
    match args.first().map(String::as_str) {
        Some("service") => service::run(),
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
