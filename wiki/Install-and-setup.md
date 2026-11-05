# Install and setup

DeskCrew is one program, `deskcrew`. A background service starts the first time you use it and exits after 30 idle minutes. The next command starts it again.

## Install

From crates.io, with Rust:

```bash
cargo install deskcrew
deskcrew setup
```

`cargo install` puts the program in `~/.cargo/bin`. Check the version with `deskcrew --version`. This wiki matches **0.1.9**.

On Windows you can instead download the installer from the homepage: [deskcrew-setup-0.1.9.exe](https://qlinter.org/downloads/deskcrew-setup-0.1.9.exe) (2.6 MB). It is not code-signed yet, so Windows may say "Windows protected your PC". Choose More info, then Run anyway. SHA-256:

```text
5aa0af1f020f4fd17547d99198617ac87c8082b29a83e9edca9b5e50987dae75
```

The installer's optional task "Set up deskcrew for my coding agents" runs `deskcrew setup`.

To build from this repository:

```bash
cargo build --release
target/release/deskcrew setup
```

## What `deskcrew setup` does

It copies `deskcrew` onto a folder already on PATH (`~/.cargo/bin` or `~/.local/bin`). If neither is on PATH, Windows gets `%LOCALAPPDATA%\deskcrew\bin` added to the user PATH, and a Mac gets `~/.local/bin` added to `~/.zprofile`. Then it writes a skill, a standing rule, or both, for every agent app it finds. Run it again after installing a new agent. It replaces its own block and leaves the rest of each file alone. The block sits between `<!-- deskcrew: start -->` and `<!-- deskcrew: end -->`.

| Agent | What setup writes |
| --- | --- |
| Claude Code | skill in `~/.claude/skills`, rule in `~/.claude/CLAUDE.md` |
| Codex | skill in `~/.codex/skills`, rule in `~/.codex/AGENTS.md` |
| Gemini CLI and Antigravity | skill in `~/.gemini/skills`, rule in `~/.gemini/GEMINI.md` |
| GitHub Copilot CLI | skill in `~/.copilot/skills`, rule in `~/.copilot/copilot-instructions.md` |
| GitHub Copilot in VS Code | `prompts/deskcrew.instructions.md` in the VS Code user folder |
| Windsurf | skill, and a rule in `~/.codeium/windsurf/memories/global_rules.md` |
| OpenCode | skill, and a rule in `~/.config/opencode/AGENTS.md` |
| Amp, Qwen Code, Factory Droid | rule in `~/.config/amp/AGENTS.md`, `~/.qwen/QWEN.md`, `~/.factory/AGENTS.md` |
| Kiro, Continue, Cline | `~/.kiro/steering/deskcrew.md`, `~/.continue/rules/deskcrew.md`, `~/Documents/Cline/Rules/deskcrew.md` |
| Cursor | skill in `~/.cursor/skills`. User Rules live in the Cursor account, so setup prints the rule to add under Customize, Rules, New |
| Grok, and agents that share `~/.agents` | skill |

For any other agent, paste the rule setup prints. The short form is: to operate apps on this computer, run `deskcrew`, and start with `deskcrew help`.

## First commands

```bash
deskcrew windows
deskcrew see Calculator
deskcrew click e12
deskcrew status
```

`windows` lists top-level windows as `w1`, `w2`, with a `*` on the one this agent last used. `see` lists that window's controls as `e1`, `e2`. `click` presses one. `status` prints the version, whether agents are running or stopped, and the agent names the service has seen.

To watch the cursors without clicking anything (Windows only):

```bash
deskcrew demo --seconds 12
```

The demo moves three pretend agents (Claude Code, Cursor, and fx) and clicks nothing. `--linger N` keeps the overlay up for N seconds after the motion stops. The demo has no window to follow, so its cursors stay on top. Real cursors sit just above their own window.

## Service, stop, resume, quit

| Command | Effect |
| --- | --- |
| `deskcrew status` | `deskcrew 0.1.9 running; agents: Cursor`, or `stopped by the person` |
| `deskcrew resume` | Clears a stop from Ctrl+Alt+Esc. Ask the person first. |
| `deskcrew quit` | Stops the service and releases keys or buttons it was holding |

The Windows service listens on `\\.\pipe\deskcrew-<version>-<session>`. On a Mac the socket is `$TMPDIR/deskcrew-<version>.sock`.

**Ctrl+Alt+Esc** stops every agent (Ctrl+Option+Esc on a Mac). In-progress typing stops, and held keys are released. Reading still works. If the service process is killed outright, a key held with `down` stays down until you press that key.

## macOS

The Mac commands match Windows closely from 0.1.6 on, and they have not been run on a Mac. Treat them as a first draft.

macOS asks once for Accessibility, and again for Screen Recording the first time a picture is taken. Grant both to the app running the agent (Terminal, iTerm, Cursor).

Not on the Mac yet: `--mouse` and borrowing the cursor, holding a mouse button, `notifications`, and `demo`. `down` holds modifier keys only (shift, ctrl, alt, cmd), and those modifiers apply to that agent's later clicks and keys in that app. `reg` is refused, because a Mac has no registry; use `run "defaults read ..."`. `run` uses zsh. `settings` opens System Settings. `trash` moves a file to `~/.Trash`.

## Uninstall

Run `deskcrew quit`. Then `cargo uninstall deskcrew`, or remove the app in Windows Settings if you used the installer. Delete `%LOCALAPPDATA%\deskcrew` (shots and cursor permission), the `deskcrew` skill folder for each agent, the text between the deskcrew comment markers, rule files named `deskcrew.md` or `deskcrew.instructions.md`, and the Cursor User Rule if you added one.
