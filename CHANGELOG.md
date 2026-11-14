# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.22] - 2026-11-14

- A click or hover at x,y off the window, or on a minimized window deskcrew could not show, is refused with the reason instead of reported as done. `launch` reports the launched program's window even when another app opens one at the same moment. `type ""` says there is nothing to type.

## [0.1.21] - 2026-11-14

- More works in minimized windows on Windows. `shot`, `click x,y` and drags work in a minimized window of any app, and keys reach a minimized WPF app: the window is shown far off screen for the moment, under the foreground lock, and minimized again. A browser opened with `deskcrew launch` is shown that way too when minimized; it used to report clicks and keys as done that it had dropped. When Windows refuses the lock, which it does while the person is using another app, the reply says why, `see` says the page was not read, and it no longer suggests `--real`. Agents working in the same minimized window at once share one showing, and agents holding the foreground lock at once share it. `place min`, `place max` and `place restore` no longer bring a window in the back forward: `place max` makes it fill its screen there instead, and `place restore` puts it back. A click on a WPF text box puts its caret there in the background. x,y from a shot still fit after the window moves. When an action changes nothing but the window's title, the reply shows the title.

## [0.1.20] - 2026-11-13

- Fewer things need --real on Windows. click --with ctrl and drags also set the keys and the left button down in the app's own keyboard state for that moment, so WinForms sees modifier clicks and canvas drags. key x --hold ms holds a key in the background, with repeats. An entry of an open classic right-click menu is picked with the menu's own keys. Classic rich text boxes are set as an edit the app knows to save, and typing goes to their end. In browsers and Electron apps, type --paste posts Ctrl+V instead of taking focus, and a field Chromium dropped letters in is filled by a posted Ctrl+A and Ctrl+V. Minimized browser and Electron windows take input too: they are shown far off screen for the moment, under the foreground lock, and minimized again. On Linux, a click at x,y on a button, check box, tab, link, menu entry or list item presses it through AT-SPI instead of the pointer.

## [0.1.19] - 2026-11-13

- Browser and Electron windows that other windows cover all over take clicks, keys and reads in the background again, without --real and without coming forward: while an agent works in one, deskcrew tells Chromium a task switcher view is open, which makes it draw every window it has, and Chromium hides the window again afterwards. Minimized ones are still refused. A click at x,y on an app that draws itself, such as a WPF or Store app, presses the button, check box, tab or list item drawn there through accessibility, instead of asking for --real. Keys and shortcuts such as Enter and Ctrl+S reach WPF apps in the background: their window gets keyboard focus inside its own app for a moment, with the foreground lock held so it stays behind.

## [0.1.18] - 2026-11-13

- deskcrew log shows every command agents gave it: when, which agent, the command, whether it worked and the first line of the reply, with typed text, clipboard text and file contents shown only as their length. Taskbar buttons no longer stay flashing after agents work in an app: deskcrew puts out the flash Windows starts when such an app tries to come forward. Browser and Electron windows that are covered all over or minimized hold clicks and keys until they show again, so deskcrew now refuses input to them instead of reporting clicks that have not happened, and browsers opened with deskcrew launch take input behind other windows. The focus keeper counts a window as the person's choice only if their mouse press began on it.

## [0.1.17] - 2026-11-13

- Fewer actions need --real. On Windows, shortcuts such as Ctrl+S and Ctrl+A go to classic apps, WinForms, Windows Terminal and Store apps in the background, to the control an agent last used; Windows Terminal and Store apps take typing in the background; WPF and XAML text boxes take type and paste through accessibility; and paste no longer presses Ctrl+V for real. On the Mac, a paste Chrome ignored in the back is filled in through accessibility. On Linux, scroll moves the scroll bar without the pointer. type --paste now puts back everything on the clipboard, pictures and copied files included, keeps the lent text out of clipboard history, and keeps anything copied in the meantime. A shot of a window not yet read points to see, which costs far fewer tokens.

## [0.1.16] - 2026-11-12

- Linux draws each agent's cursor too, with the same look, curve, lean and typing pulse as Windows and the Mac. On the Mac and Linux, typing reaches Chrome, Electron and other apps that ignore keys while behind other windows, without bringing them forward: letter by letter through accessibility on the Mac, and by lending the window the keyboard without raising it on Linux. On Windows this is not possible for Chromium apps, and the README says why.

## [0.1.15] - 2026-11-12

- Cursors look and move like Claude Whiteboards: a white-ringed dart with the name in Arial Bold on a tag of the agent's colour (Claude is now its own orange). Moves follow a curve, and the arrow leans into sideways travel. While typing, the cursor becomes a caret that follows the text with the tag above it, and the tag pulses with the keys. All typing runs at 600 words a minute, and --real typing into a named field adds to the end. A window the person clicks, drags or Alt-Tabs into is no longer taken back. New demo video.

## [0.1.14] - 2026-11-11

- Write the README in plain sentences, and bring its comparison with agent-desktop up to date now that deskcrew runs on macOS and Linux and as an MCP server. The crates.io page shows the new README.
- Add GitHub issue forms for bug reports and for apps deskcrew can't read, and open Discussions for questions.

## [0.1.13] - 2026-11-11

- Add a Linux version for X11, including headless Xvfb: controls through AT-SPI in the background, points and keys through XTest, one service per X display. It also works without at-spi2-core, on a bus named by `AT_SPI_BUS_ADDRESS`. On Wayland it explains that Wayland is not supported. Tested in CI on Ubuntu with a GTK window and a toolkit-free one, and on a Debian 13 machine.
- Add `deskcrew mcp`, an MCP server over stdio with `see`, `do`, `shot` and a `deskcrew` tool for every other command, for Claude Desktop, Codex, Cursor and other apps that call tools instead of running a shell. A shot comes back as the picture itself.
- `scroll left|right` scrolls sideways, and `click --hold ms` is a long press, by window message like any other click. These were the last things desk-pilot did that deskcrew did not.
- When a name matches several windows, use the one the agent is already working in. On a Mac, also prefer an exact name and the only window with a title, so the untitled panels TextEdit opens while typing no longer make `-w TextEdit` ambiguous.
- CI drives TextEdit on GitHub's Mac runners: the first time the Mac version has run.
- On a Mac, `kill` refuses the system's own processes, such as WindowServer and loginwindow, as it already did for Windows'.
- `bench/` can run deskcrew as an MCP server (`--tool deskcrew-mcp`).

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
