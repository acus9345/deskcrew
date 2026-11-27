# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.33] - 2026-11-27

- **Browser control through CDP (Windows, macOS, Linux):** read and act on Chromium and Electron web pages directly through their live DOM instead of the OS accessibility tree, with zero added crate dependencies and no focus stealing or cursor movement. Supports open Shadow DOM, same-origin `<iframe>`s, `<canvas>` drawing, native and custom ARIA selects, password redaction, `table`, `is`, `shot`, `do`, `launch <browser> --cdp`, `cdp [status|on|off|port|tabs|use|goto|back|forward|reload]`, and direct tab targeting (`-w cdp`, `-w t1`, `-w cdp:<query>`).
- **Automatic agent adoption:** updated the bundled `deskcrew` skill, standing rules (`deskcrew setup`), MCP instructions/schemas, and `deskcrew windows` (listing live `[cdp]` tabs when configured) so coding agents automatically use CDP for web tasks.
- **Lower token and turn cost:** cut `SKILL.md` by 44% and `HELP`/MCP schemas by 37–45%, made the standing rule self-sufficient so agents skip redundant skill-load and dialog-confirm turns, extended duplicate form-label pruning (`"Name:"` / `"Email *"`) across Windows UIA, macOS AX, Linux AT-SPI and CDP DOM, and pruned inline `<svg>` noise and `aria-labelledby` duplicates in CDP snapshots.

## [0.1.32] - 2026-11-26

- **Your window switch wins:** clicking into the window an agent is working in, or reaching it with Alt+Tab, Win+number or the taskbar, keeps your focus there on the agent's next moves, including after a pause between moves. Quick clicks between checks are no longer mistaken for the app stealing focus. Apps that jump forward on their own are still put back behind your window.
- `focus` and `launch --front` make that window yours, so later actions don't hand focus back to an older one.
- The same rules apply on macOS (Cmd+Tab and clicks) and Linux (clicks and Alt/Super switches).

## [0.1.31] - 2026-11-25

- **Cursor motion:** each agent's cursor leans into the direction it moves and straightens up smoothly when it stops; moves between points take the same time and curve as on qlinter.org, and drawn strokes slow down at corners like handwriting.
- **Fixed:** after `draw --real`, the pointer sometimes stayed at the end of the stroke instead of going back.
- **Sturdier input:** a failed key chord releases every key it pressed; Windows line endings type one Enter, not two; emoji and other two-part characters are typed whole; scroll counts are checked and capped at 200.
- **Bad input is refused plainly:** `--region` and `place x,y,w,h` reject malformed or empty boxes; blank `--agent` or `DESKCREW_AGENT` falls back to detection; arguments with NUL bytes are refused.
- **No hangs or crashes on odd apps:** accessibility walks on Windows, macOS and Linux stop at a node budget and skip cycles; huge or off-screen control bounds, empty pictures and very large images no longer overflow or skew; OCR can't deadlock on large pictures.
- **Clipboard and service:** the clipboard is put back even if lending it fails, long clipboard text isn't cut mid-character, and oversized or truncated messages to the service are refused.
- **Overlays:** cursor overlays handle zero or huge sizes, cap simultaneous flashes, and close cleanly.
- **Windows:** moving a window to another monitor uses that monitor's work area without taking focus, and putting a window back keeps its place in the stack.

## [0.1.30] - 2026-11-22

- **Quiet mode:** `deskcrew quiet on` (or `DESKCREW_QUIET`) refuses everything that takes focus or real input: `focus`, `down`, `--real`, `--mouse`, `launch --front`, the pen, and Win+... shortcuts that open shell UI, even with `--force` or inside `do`. Background clicks, typing and drawing by window messages keep working.
- **Background input in more apps (Windows):** covered Chrome, Edge, Electron, WebView2 and CEF windows take clicks and typing more reliably; classic Edit controls get copy, cut, paste and undo without any focus change; Krita and other Qt apps no longer have focus pulled or their title bar flashed.
- **Clipboard safety everywhere:** agents take turns lending the clipboard, and it is always put back, even after an error.
- **Focus recovery:** if an app steals focus during an action, the window you had is given back, unless you switched apps yourself in the meantime.
- **Mac:** window pictures through ScreenCaptureKit, without the drop shadow, so x,y in a shot match clicks; better accessibility in Chrome and Electron apps; tables, scroll bars, notifications and selected text; `pick-file` for Save dialogs too.
- **Linux:** covered X11 windows take input by sent events, windows can be pictured while covered (XComposite), text in pictures is read with Tesseract when installed, and on Wayland pictures and pointer input go through the desktop portal after a one-time consent.
- **Fixed:** a one-key `key` (such as `key tab`) crashed the service.
- Idle overlays no longer restack their windows over and over.

## [0.1.29] - 2026-11-19

- **Cursor grants tied to the calling program (Windows):** "this session" and "always" cursor permissions now follow the calling agent's actual process and executable path rather than the self-reported `--agent` name. The prompt shows the real program alongside the agent name when they differ, and older name-only `always` lines in `cursor.txt` ask to be confirmed once.
- **Store and WinUI apps stay in the back:** toggles, list and combo selections, expand/collapse controls, and range/value edits keep Store and WinUI apps behind your window, with per-app process isolation for `ApplicationFrameHost` windows and tighter popup item matching.
- **Minimized WPF input and background Win shortcuts:** minimized WPF windows can receive typing and keys off-screen without coming forward, and common `Win+...` shortcuts route through shell/COM or background window placement instead of taking the real keyboard.
- **Linux Wayland and socket hardening:** accessibility actions work over AT-SPI on Wayland without requiring an X11 display, Linux CI verifies the typing caret, and Unix service sockets on Linux and macOS use private `0700` directories with owner and lock checks.
- **Reliability and safety:** `deskcrew status` reports when another process holds the `Ctrl+Alt+Esc` stop keys; log trimming is locked against concurrent writers; broken-pipe accept loops exit cleanly; risky shortcut checks are keyboard-layout independent; and `--risk` and `--` argument handling are consistent across Windows, macOS, and Linux (including MCP).
- **No focus stealing or flashing from Windows UI Automation (Krita / Qt / WPF):** disabled `IUIAutomation2::AutoSetFocus` so accessibility actions such as `set` on Krita's brush size and opacity spinners no longer try to foreground the window or flash its frame (`WM_NCACTIVATE`) while an agent draws.

## [0.1.28] - 2026-11-18

- A review of the whole codebase, fixed:
  - **Safety:** the cursor prompt answers only to the person's own click; commands go only to a service running as the same account; risky shortcuts are judged on the keys actually pressed, held keys included (`alt+f04` and `down ctrl` then `key q` are caught), and on `down` and `--with` too; focus, close, place, clip and notifications stop with Ctrl+Alt+Esc like every action.
  - **Privacy:** the log and traces keep content replies (clipboard, `get`, `fetch`, `reg`, file reads, table cells) as their length; the bench keeps its sign-in copy out of the shared Public folder.
  - **Agents side by side:** `shot --marks` can no longer deadlock against another agent's action (all three systems); held keys and buttons belong to the agent that held them, and `up` lets go of only its own; refs no longer shift to another control when a same-named one appears.
  - **Windows:** `--expect` always waits for text (not as milliseconds), per step in a `do` and on browser drawing too; a step's own `-w` or `--risk` is refused instead of dropped; a browser paste that landed is not pasted again; PowerShell strings handle typographic quotes; shell commands no longer hang on output a leftover process holds; Store apps match by their own process; `launch` passes arguments where it can; WINWORD.EXE shows as winword.
  - **Mac:** minimized windows can be restored; typing never splits an emoji; a failed paste puts the clipboard back; the stop keys come back on if macOS switches their tap off; `select` picks a table row by its text; click outlines stay out of screen copies.
  - **Linux:** a click no longer lets go of keys held with `down`; sideways scrolling goes the right way; text with accents goes in whole; a paste takes its turn on the one clipboard; accessibility calls time out after 2 s; region and screen shots set where the next `click x,y` goes.
  - **Docs and tooling:** the README, skill and plugin manifest describe the macOS and Linux versions as they are; the demo recorder and the MCP smoke test no longer hang or crash.
- Each agent's cursor and name tag wear its own brand colour: Claude orange, Codex ChatGPT green, Gemini blue, Copilot magenta, Windsurf teal, Antigravity dark navy, Amp coral, Kiro purple, Cursor black, Grok charcoal and OpenCode light grey. A dark tag gets a faint light edge so it still shows on a dark screen. Antigravity, Grok and Zed are now recognised by name, and every known agent, the demo's included, has a colour no other agent shares.

## [0.1.27] - 2026-11-16

- Linux gets `table`, `pick-file` and `notifications`, and the typing caret. Tested in CI on a GTK window under Xvfb: a three-row table, a GTK Open dialog filled from its location field, and a notification sent on the session bus.

- Linux: agents get their own X pointer (an XInput 2 master pointer named "deskcrew"), so clicks at x,y, drags, drawing, hovering and wheel scrolling leave yours where it is, and nothing is brought forward while the agent's window shows at the point. GTK drawing areas are listed as canvases.

- Fixed: after keys or typing reached Krita or Firefox in the back, your own keys did nothing in that app once you brought it forward, until you clicked inside it. deskcrew left the app active with nothing focused, and Windows does not focus a window it sees as active already. The window keeps its focus now.
- Keys, typing and held keys for a WPF, Qt or Firefox window you have in front go through the keyboard, as `--real` would, instead of asking for `--real`: the window is in front already, so nothing moves. `--real` keys for a window with a dialog open go to the dialog, which used to lose them to its disabled owner.
- Enter, Escape, Backspace and Delete reach a Store app in the back through its own buttons where it has them, as Calculator does (Equals, Clear, Backspace, Clear entry). Store apps read those keys only from the real keyboard.
- Calculator, and other Store apps, came forward over your window whenever one of their buttons was pressed through accessibility. Windows' foreground lock is now held through that moment, so they stay behind.
- Drawing and dragging in a WPF app in the back goes by window message, with the mouse capture and the button given to the app for the moment and a move one pixel off first, instead of the pen, so it no longer takes focus. WPF looks for what is under its first message on the real screen, where another window covers it, and dropped the press without that move.
- A click at x,y on a WinForms control another window covers raises its Click in the back: the window moves off every screen for about a tenth of a second, where nothing covers it, and comes back. It used to need `--real`.
- Keys and typing reach a Chromium page in the back (Edge, Chrome, Electron apps) without a click in it first: the window is told it is active, which gives the page Chromium's own focus and does not bring it forward. Before, keys went nowhere until an agent had clicked in the page.
- Key chords with two ordinary keys, such as `a+b` or `shift+a+b`, go in the back, with the first ones held down in the app's keyboard state. Only the Windows key still needs the real keyboard.

- Keys and typing for a Qt, WPF or Firefox window go to a dialog it opened over itself, which holds all its input until it closes: Escape or typing sent to Krita's main window reaches its New document or Recover Files dialog in the back. A dialog just opened gets a moment to pick its focused control first, since Qt dropped the first key before. A key whose press closed its window is no longer reported as failed.
- `key --hold` reaches Qt, WPF and Firefox in the back, with focus in the app's own window for the whole hold. It used to need --real there.
- A click in a Qt app goes to a dialog, menu or popup of the tracked window at that spot, but no longer to another main window of the same app that covers it.

- `deskcrew setup` no longer fails with "being used by another process" when an agent's MCP server, such as Antigravity's, is running the installed program. On Windows the running copy is moved aside as `deskcrew-old-N.exe`, and a later setup removes it once nothing runs it; on Linux and the Mac the new build is renamed over it.

## [0.1.26] - 2026-11-15

- Keys, shortcuts and typing reach Qt apps such as Krita, and Firefox, in the back, the way keys reach WPF apps: the window gets keyboard focus inside its own app for a moment, with the foreground lock held so it stays behind. Krita's tool shortcuts, Ctrl+N, Ctrl+Z and Escape used to need --real, which brought Krita forward. Typing with no control named now works in WPF too.
- Buttons and menu entries in Qt apps are pressed by a posted click rather than accessibility, which Qt answers only once a dialog the button opened is closed: Krita's dialogs no longer time out, and its menus open in the back. The click goes to the app's own window on top at that spot, so a dialog's or open menu's controls take it from the main window too.

## [0.1.25] - 2026-11-15

- Qt apps such as Krita, and Firefox, are drawn in and clicked by window message, in the back: no focus change and no movement of your pointer. They used to get the pen, which brought them forward for every stroke.
- The pen, still used for WPF, WinUI and Store apps, puts your pointer back where it was afterwards, unless you moved it meanwhile, and holds focus for less than half as long: it moves at 4000 pixels a second instead of 1000, and waits 50 ms instead of 350 ms after bringing the window forward. A pen or `--real` action with a point off the window or off every screen is refused before focus moves, instead of after half a second in front.
- `deskcrew setup` adds deskcrew as an MCP server to Antigravity, which runs the commands its agents type on a sandboxed Windows desktop where no window of yours can be seen. On such a desktop, `windows` and a window that matches nothing now say why and point to the MCP tools, instead of only "no windows".

## [0.1.24] - 2026-11-15

- `draw` takes shapes: `line`, `rect`, `circle`, `ellipse`, `arc`, `bezier` and `poly`, with `--fill` and `--close`, on Windows, the Mac and Linux. `drag` and `draw` take `--button right|middle` and `--with shift|ctrl|alt`; on Windows these go by window message with the keys set only in the app's own keyboard state. `deskcrew --version` prints the version. A blank picture of a fully covered window is no longer replaced by a copy of the screen, which showed the window on top. Ideas from a review of the Hydrogen team's branch.
- Fixed: picking an entry of an open classic right-click menu closed the menu without running the entry when the app was in the back. The menu is now closed and the entry's command sent to its owner, as a real pick does.
- `hover` or `expand` on a menu entry with a submenu in a browser or Electron app opens the submenu with the menu's own keys, in the back, where a posted hover could not hold it open. A name that fits both a label and a control that can take the action means the control.
- On Windows, deskcrew's pictures, log, sessions and cursor setting moved to `%USERPROFILE%\.deskcrew`. Windows gives packaged apps such as Claude and every program they start a private copy of AppData, so a picture deskcrew saved there could reach an agent missing or stale, and their log and sessions were hidden from other apps.

- Sessions and traces. `deskcrew session start [name] [--shots]` records an agent's commands into a session, which other agents join with `session join ID`, `--session ID` or `DESKCREW_SESSION`. `deskcrew trace` shows the timeline: when, which agent, the command, how long it took, whether it worked and its reply. With `--shots`, each action keeps a picture of the window before and after. `deskcrew trace export` writes the whole session as one HTML page with the pictures inside. Typed text and values are left out, as in the log. `session end`, `session list` and `session gc` close, list and delete sessions.

## [0.1.23] - 2026-11-14

- `deskcrew browsers on|off|status` turns Edge's, Chrome's and Brave's own window occlusion policy off or back on, so they keep drawing covered windows and take input there, also when they are already running. `on` and `off` need a terminal opened as administrator, since Windows lets only an administrator set browser policies. With the policy off, deskcrew no longer tells those browsers a task switcher view is open.

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
