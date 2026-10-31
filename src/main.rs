// deskcrew - every agent gets its own cursor on the desktop.
#[cfg(not(windows))]
compile_error!("deskcrew only runs on Windows for now");

mod demo;
mod motion;
mod overlay;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("demo") => {
            let number = |flag: &str, default: u64| {
                args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).and_then(|s| s.parse().ok()).unwrap_or(default)
            };
            demo::run(number("--seconds", 12), number("--linger", 0));
        }
        Some("--version") => println!("deskcrew {}", env!("CARGO_PKG_VERSION")),
        _ => {
            println!("usage: deskcrew demo [--seconds N] [--linger N]");
            println!("Moves three pretend agents around the primary screen. It clicks nothing.");
        }
    }
}
