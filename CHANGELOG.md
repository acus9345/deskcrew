# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.8] - 2026-11-03

- `setup` writes a standing rule into every agent app and CLI it finds (Claude Code, Codex, Gemini CLI and Antigravity, Copilot CLI and VS Code Copilot, Windsurf, OpenCode, Amp, Qwen Code, Factory Droid, Kiro, Continue and Cline), telling them to use deskcrew on their own and instead of any other desktop control. Cursor gets the skill, and setup prints the rule for its User Rules.
- The skill now says deskcrew is the only way to control the computer.

## [0.1.7] - 2026-11-03

- Let agents use deskcrew on their own whenever a task needs the desktop: the skill now says it is the default way to control the computer, ahead of screenshots and other computer-use tools.

## [0.1.6] - 2026-11-03

- Bring the macOS version close to Windows: toggles, `select`, `hover`, `show`, `is`, `draw`, held modifier keys, window placement, `focus`, `close`, the clipboard, screens, `--real`, `--paste`, `--fast`, `--with`, more click kinds, `do --dry` and `--risk`, and `wait --gone`. It type-checks for Apple Silicon and Intel but has still not been run on a Mac.

## [0.1.5] - 2026-11-02

- Draw in a covered browser or Electron window in the background, instead of refusing and leading agents to `--real`, which lifted the window over yours and made it flicker.
- Draw each stroke in one press when one agent draws alone, and stop the taskbar flash a browser starts when it is pressed in the background: 26 flashes for three strokes became 2.

## [0.1.4] - 2026-11-02

- Add a macOS version with the main commands: windows, see, get, click, set, type, key, scroll, drag, wait, shot, launch and do. It type-checks for Apple Silicon and Intel but has not been run on a Mac yet.
- `see` points out any large part of a window with no controls and reads its text with Windows OCR, giving x,y that `click` takes.
- When an action changes no control, its reply says whether the window's picture changed and where.

## [0.1.3] - 2026-11-01

- Add `deskcrew cursor`: `--mouse` is back, off by default, and an agent can only borrow the cursor if the person allows it once, for the session or always.
- Stop `--real` typing if focus leaves the window, and let Ctrl+Alt+Esc stop it.
- Refuse risky shortcuts with extra modifiers, such as `shift+alt+f4`.
- Release held keys on `quit`, on idle exit and on Ctrl+Alt+Esc.
- Hide password fields from `get`.
- Notice when a window handle has been reused by a different process.
- Refuse to close the Windows desktop and taskbar.

## [0.1.2] - 2026-11-01

- Remove `--mouse` and every real-mouse route, so nothing moves the person's cursor.
- Keep cursors and press effects just above their window, so other windows can cover them.
- Run `type --paste` in classic text boxes and `click --with` without focus.

## [0.1.1] - 2026-11-01

- Add hover, triple, middle, back and forward clicks, modifier clicks, and held keys and mouse buttons.
- Add check, uncheck, toggle, expand, collapse, clear and is.
- Add region, all-monitor, marked and ruled shots, plus screens, apps, notifications and clip clear.
- Add paste typing, --expect on any action, and --dry and --risk on do.
- Keep a drawing cursor above always-on-top windows.
