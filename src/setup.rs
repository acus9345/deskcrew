// One command that makes deskcrew reachable from every coding agent: the program goes somewhere on
// PATH, and each agent found on this computer gets the skill that tells it deskcrew exists.
use crate::pipe::Pipe;
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

const SKILL: &str = include_str!("../skills/deskcrew/SKILL.md");
const RULE: &str = "To operate Windows apps on this computer, run the deskcrew command. Start with: deskcrew help";

/// Where each agent keeps personal skills, and the folder that shows it is installed.
const AGENTS: [(&str, &str, &str); 8] = [
    ("Claude Code", ".claude", ".claude/skills"),
    ("Cursor", ".cursor", ".cursor/skills"),
    ("Codex", ".codex", ".codex/skills"),
    ("Gemini CLI", ".gemini", ".gemini/skills"),
    ("GitHub Copilot", ".copilot", ".copilot/skills"),
    ("Windsurf", ".codeium/windsurf", ".codeium/windsurf/skills"),
    ("OpenCode", ".config/opencode", ".config/opencode/skills"),
    ("Agents sharing ~/.agents", ".agents", ".agents/skills"),
];

fn path_folders() -> Vec<PathBuf> {
    env::var_os("PATH").map(|path| env::split_paths(&path).collect()).unwrap_or_default()
}

/// A folder already on PATH, so agents that are open now can find the program without a restart.
fn install_folder(home: &Path) -> (PathBuf, bool) {
    let on_path = path_folders();
    for folder in [home.join(".cargo").join("bin"), home.join(".local").join("bin")] {
        if folder.is_dir() && on_path.contains(&folder) {
            return (folder, true);
        }
    }
    let root = env::var_os("LOCALAPPDATA").map_or_else(|| home.to_path_buf(), PathBuf::from);
    (root.join("deskcrew").join("bin"), false)
}

fn add_to_user_path(folder: &Path) -> Result<(), String> {
    let script = format!(
        "$f = '{}'; $p = [Environment]::GetEnvironmentVariable('Path', 'User'); if (($p -split ';') -notcontains $f) {{ [Environment]::SetEnvironmentVariable('Path', ($p.TrimEnd(';') + ';' + $f), 'User') }}",
        folder.display()
    );
    let done = Command::new("powershell.exe").args(["-NoProfile", "-Command", &script]).status().map_err(|e| e.to_string())?;
    if done.success() { Ok(()) } else { Err("could not add it to your PATH".into()) }
}

fn install_program(home: &Path) -> Result<String, String> {
    let (folder, on_path) = install_folder(home);
    let target = folder.join("deskcrew.exe");
    let current = env::current_exe().map_err(|e| e.to_string())?;
    if current == target {
        return Ok(format!("program: already at {}", target.display()));
    }
    // A running service holds the file open.
    if let Ok(pipe) = Pipe::connect() {
        let _ = pipe.send(b"setup\0quit");
        let _ = pipe.receive();
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
    fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    fs::copy(&current, &target).map_err(|e| format!("could not copy to {}: {e}", target.display()))?;
    if on_path {
        return Ok(format!("program: {}", target.display()));
    }
    add_to_user_path(&folder)?;
    Ok(format!("program: {} (added to PATH: restart your agents)", target.display()))
}

pub fn run() -> i32 {
    let Some(home) = env::var_os("USERPROFILE").map(PathBuf::from) else {
        eprintln!("no home folder found");
        return 1;
    };
    match install_program(&home) {
        Ok(line) => println!("{line}"),
        Err(error) => {
            eprintln!("{error}");
            return 1;
        }
    }
    let mut found = 0;
    for (name, marker, skills) in AGENTS {
        if !home.join(marker).is_dir() {
            continue;
        }
        let folder = home.join(skills).join("deskcrew");
        let written = fs::create_dir_all(&folder).and_then(|_| fs::write(folder.join("SKILL.md"), SKILL));
        match written {
            Ok(()) => {
                found += 1;
                println!("skill: {name}");
            }
            Err(error) => eprintln!("skill: {name} failed: {error}"),
        }
    }
    println!("{found} agents set up. Any other agent that can run commands works too: add this to its rules:");
    println!("{RULE}");
    0
}
