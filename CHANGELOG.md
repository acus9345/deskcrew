# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.12] - 2026-11-08

- Let agents finish a task in fewer, faster calls. On the bench's order form a Claude Code agent went from 29 s and 6 model calls to 17 s and 5, and on an 80-row grid from 19 s and 7 calls to 14 s and 4, reading up to 47% fewer tokens, with every result still right.
- `do` goes on into the dialogs its steps open, and a name that fits several controls picks the one that can take the action, so `set Quantity 3` finds the text box.
- `see` leaves out spinner arrows, dropdown buttons and labels that repeat the next control's name, reads a grid row as one line, and adds a grid's off-screen rows when they fit.
- `table` reads the right header, starts at row N with `--from N`, and returns as many rows as fit in one reply. It used to take a grid's scroll bar for its header and read the same first rows again.
- Click WinForms check boxes and radio buttons with window messages. Accessibility briefly took focus for them, and in a covered window flipped them late.
- Acting on a dialog by its own title works through the window that owns it, so the reply shows what the dialog changed there.
- Each Windows desktop gets its own service, and deskcrew's own messages use plain ASCII.
- The standing rule `setup` writes, the help and the skill lead with one `see`, then every step in one `do`.
- Add `bench/`, which measures agents doing the same desktop task with each tool: time, calls, tokens, cost and whether the result is right.

## [0.1.11] - 2026-11-05

- Stop agents taking the person's focus in browser and Electron apps: clicks, `type` and `set` there always go by window message, even while the window is covered, instead of accessibility, which raised the app over the person's window.
- Never mistake an app agents work in for the person's window: after a steal outlived the focus keeper, the next action used to guard the app instead, leaving the person's window buried. The keeper also watches every 2 ms, stays on for 3 s after the last action, and lets the person switch apps themself.
- Keep an agent's cursor under any window that covers its app, even when the app shows a tooltip on top.

## [0.1.10] - 2026-11-05

- Stop an app that pulls its window forward when an agent presses it through accessibility from covering the person's window again: once the focus guard has had to take focus back from an app, later clicks on it are posted as messages, which never raise it. Seen with egui apps such as PkgStats. The first click on such an app can still cover for a few milliseconds.

## [0.1.9] - 2026-11-03

- Add commands beyond windows: `processes`, `kill`, `run`, `fetch`, `file`, `reg`, `notify` and `settings`. They need no service, refuse to end Windows' own processes or the agent's shell, never overwrite without `--force`, delete only to the Recycle Bin, and write the registry only under HKCU.
- Add `table`, which reads a table or grid as rows, and `pick-file`, which fills an Open or Save dialog in any Windows language.
- Add a feature comparison with agent-desktop, agent-cu, Windows-MCP and mcp-windows to the README.

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
