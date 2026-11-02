# deskcrew

**Windows, with an untested macOS version.** Every coding agent gets its own cursor on your desktop, works in the background, and costs very few tokens.
<img width="1163" height="719" alt="image" src="https://github.com/user-attachments/assets/2cad4d13-546e-41d4-84b9-0406ff783b78" />


Agents act through Windows accessibility, window messages and a synthetic pen, so your cursor stays yours. Each agent appears as a drawn cursor in its own colour with its name beside it. The cursor glides to a control, and the control lights up with an outline in that colour when it is pressed. Agents work side by side while you keep working.*even when ur doing something in fullscreen*the agents can still work and do things

**[Watch the 11-second demo](demo/deskcrew-demo.mp4):** three agents draw in Photon Studio, use Calculator and type a note at the same time. Calculator's title and mode label were overlaid in English in the video.

It is a single command, so any agent that can run a shell command on this computer can use it: Claude Code, Cursor, Codex, Gemini CLI, Copilot, Windsurf, OpenCode, Aider and the rest. There is no MCP server and no tool schema loaded into every conversation.

## Try it

```bash
cargo build --release
```

```bash
target/release/deskcrew demo --seconds 12
```

The demo moves three pretend agents around the screen and clicks nothing.

To make it available to your agents:

```bash
target/release/deskcrew setup
```

That puts `deskcrew` on PATH and gives the skill to each agent it finds. For any other agent, add one line to its rules: `To operate Windows apps on this computer, run the deskcrew command. Start with: deskcrew help`. On a Mac the rule says Mac apps instead. Agents running in the cloud, or in a sandbox without access to your desktop, cannot reach it.

## macOS

**The macOS version has not been run yet.** It was written and type-checked for both Apple Silicon and Intel Macs on a Windows PC, which cannot link or run Mac programs. Treat it as a first draft until someone tries it on a Mac.

It has the same commands and replies, built on Mac parts:

- Controls are read through the accessibility API and pressed with their own actions, such as `AXPress`, so a click works behind other windows without taking focus.
- Clicks on points, drags, scrolls and keys are posted to the one app being worked on, so your pointer does not move and your front app keeps its keys. Apps that only take keys while in front get a field's text set instead.
- Pictures come from the window itself, and text in parts with no controls is read with Apple's Vision.
- Cursors are see-through windows ordered just above the agent's window, so your other windows can cover them.
- Ctrl+Option+Esc stops every agent.

macOS asks once for Accessibility permission, and again for Screen Recording the first time a picture is taken. Both are granted to the app running your agent, such as Terminal, iTerm or Cursor. `setup` installs to `~/.cargo/bin` or `~/.local/bin`, adding the latter to `~/.zprofile` if it is not on PATH.

Not on the Mac yet: `--real`, `--mouse` and borrowing the cursor, `draw`, held keys, `select`, `check` and the other toggles, `notifications`, `place`, `focus`, `screens`, `apps`, the clipboard and the demo.

```text
$ deskcrew see Calculator
w2 "Calculator" 37 controls, 1 off screen
e12 button "One"
...
$ deskcrew click e12
ok invoke
~ e4 "Display is 1"
```

Run `deskcrew help` for every command. A background service starts itself on first use and exits after 30 idle minutes.

## Why it is cheap

- Each control is one short line, and refs stay valid, so a window is read once.
- Every action replies with only what changed, so there is nothing to read again.
- `click e1 e2 e3` and `do` run many steps in one call.
- Pictures are files that are only sent if the agent opens them. An unchanged picture is reported as `unchanged`.
- `see` points out any large part of a window with no controls, such as a canvas or a game, and reads the text in it with Windows' own offline OCR. Clicks can use the x,y it gives, so a picture is only needed for drawings, and then only of that part.
- When an action changes no control, its reply says whether the window's picture changed and where, so the agent doesn't need a picture to check.

Measured on 2026-11-01 against agent-desktop 1.0.2, in characters an agent has to read, on throwaway windows at 150% scaling. Its figures use `snapshot -i --compact`. These are characters, not tokens, and agent-desktop's `--skeleton` mode reads less by leaving detail out.

| Reading a window | Controls | agent-desktop | deskcrew | Time, agent-desktop | Time, deskcrew |
| --- | --- | --- | --- | --- | --- |
| Test form | 42 | 10,138 | 1,047 | 191 ms | 74 ms |
| Character Map | 11 | 3,603 | 260 | 177 ms | 24 ms |
| Calculator | 37 | 9,350 | 689 | 175 ms | 25 ms |
| File Explorer | 121 | 39,595 | 3,375 | 296 ms | 62 ms |

For `1 + 2 =` on Calculator, knowing the result of every press took 48,570 characters with agent-desktop, reading after each press, and 811 with deskcrew. The tool's help is 1,903 characters.

## Compared with agent-desktop

[agent-desktop](https://github.com/vercel-labs/agent-desktop) inspired this project and reaches further. The reading figures above were measured; the rest of its column comes from the agent-desktop 1.0.2 documentation.

| | agent-desktop | deskcrew |
| --- | --- | --- |
| Platforms | macOS, Windows, Linux | Windows; macOS written but not yet run |
| Used from | command line, or a library called in process | command line and an agent skill |
| Reading a window | 3,603 to 39,595 characters in the tests above | 260 to 3,375 characters, about 10 times less |
| Drawn cursors | `cursor-overlay`, with `--multi-agent` for one per agent; always on top of other windows | always on, one per agent with no setup, and covered by whatever covers their window |
| Clicks, typing, toggles, menus, scrolling, drag | yes | yes |
| Holding a key | listed, but refuses to run for now | `down` and `up`, released by itself after 30 s |
| Holding a mouse button | listed, but refuses to run for now | `down left x,y --mouse`, with your permission |
| Your cursor | moved by `--headed` mouse commands | moved only if you allow it |
| Clipboard, windows, launching apps | yes | yes |
| Notifications | yes | `notifications`, which opens the panel and takes focus until `notifications close` |
| System surfaces | `open-system-surface` | `launch` with a Windows link such as `ms-settings:` |

Pick agent-desktop if you need macOS or Linux, or want it as a library. Pick deskcrew on Windows if you want several agents on screen for fewer tokens.

## How actions reach an app

Each reply names the route it took.

| Route | Used for | Your cursor | Your focus |
| --- | --- | --- | --- |
| `invoke`, `toggle`, `select`, `expand`, `value`, `range`, `scroll` | the app's own accessibility actions | untouched | some apps take it for a moment, and it is handed straight back |
| `command`, `message` | classic Windows controls and menus, plus clicks, typing, `drag` and `draw` in browsers and Electron apps, by window message | untouched | untouched in classic apps, and a Chromium window can take it for a moment |
| `pen` | `drag`, `draw` and `--real` in apps that draw themselves and ignore messages, such as Paint | untouched | borrowed for the action |
| `keyboard` | `key`, `type` and `set` with `--real` | untouched | borrowed for the action |
| `mouse` | only `--mouse`, and only if you allow it | moved, then put back | borrowed for the action |

**Nothing moves your cursor unless you say so.** Borrowing it is off by default. Run `deskcrew cursor ask` to let agents ask, and an agent that needs it gets a prompt on screen: **Just this once**, **This session**, **Always** or **No**. No answer in a minute counts as no, and the prompt never takes focus, so your typing cannot answer it.

| `deskcrew cursor` | Effect |
| --- | --- |
| `off` | the default: no agent can borrow it, and any earlier permission is cleared |
| `ask` | agents may ask each time |
| `session` | every agent may borrow it until deskcrew stops |
| `always` | every agent may borrow it when needed, until you run `off` |

Anything except `off` opens a prompt you have to accept. This stops an agent from turning it on by accident or by habit, but it is not a security boundary: anything running as you can edit `%LOCALAPPDATA%\deskcrew\cursor.txt` or click the prompt.

Some classic controls answer an accessibility request by quietly clicking with the real mouse, so a shield blocks every synthetic mouse event while those requests run. A control that needs that is refused with a message instead. In tests, Paint drawing and Edge presses caused no synthetic mouse events at all.

**Cursors and press effects can be covered.** They sit directly above the window they work on, and above its menus and popups, not on top of everything. Put another window over that app and it covers the cursor and the effect too. `deskcrew demo` has no window to follow, so it stays on top.

**Focus is a last resort.** Every action tries a background route first, and only `--real`, `down`, `notifications` and `launch --front` take focus on purpose. `type --paste` into a classic text box and `click --with ctrl` run in the background.

Anything that borrows focus waits for a pause in your own typing, is one agent at a time, and gives focus back afterwards. It is refused if a point is not on the target window, so it cannot land on something else you have open.

Agents in different windows click, type and draw at the same time. In a test on 2026-11-01, three agents each typed in two fields, pressed a button and drew in their own visible Edge window, all at once, using separate browser profiles. Over three rounds that took 3.5 to 3.9 s against about 3 s for one agent alone. Everything landed in the right window, there were no synthetic mouse events, and focus left the person's window for about 100 ms in total each round. `type` reads a field back and sets it directly if Chromium dropped the characters.

Agents in one window take turns, because an app has one mouse state and one text cursor. In browsers and Electron apps, `draw` cuts strokes into short overlapping pieces and rotates between agents, so several can draw on one canvas at once. Three trees took about 9 s together this way. Each piece is a separate brush stroke, so undo steps back in small pieces. The pen and `--real` also take turns across all windows, because they need the window in front.

**Ctrl+Alt+Esc** stops every agent. Reading still works, and work resumes with `deskcrew resume`. Shortcuts such as Alt+F4 and Win+R are refused without `--force`.

## Tests

```bash
cargo test
```

```bash
cargo test --release --test desktop -- --ignored --nocapture
```

The second one drives throwaway windows it opens itself, including the real keyboard and mouse for a few seconds, so leave the machine alone while it runs. It has 65 checks, and it counts synthetic mouse events to prove when the cursor was left alone.

## Limits

- The Windows version is the tested one. The macOS version has never been run (see [macOS](#macos)), and Linux is not written.
- On a Mac, pictures use `CGWindowListCreateImage`, which Apple has deprecated. A future macOS may stop it working, and then pictures would need ScreenCaptureKit.
- On a Mac, typed text goes out as key events carrying the characters. An app that reads key codes instead may get the wrong letters.
- `notifications`, `down` and `--real` take focus, so they can interrupt what you are doing, and so does `type --paste` outside a classic text box. A key held with `down` reaches whichever window has focus, and is let go after 30 s, on `quit` and on Ctrl+Alt+Esc. If deskcrew is killed outright it stays down until you press that key.
- `--real` typing stops if focus leaves the window, and reports how much it had typed.
- `click --with ctrl` sends the modifier with the click. Apps that read the keyboard instead, such as WinForms, need `--real` as well.
- `expand` needs a control that offers it: a WinForms combo box does not, so use `select` there.
- The newest commands, such as `hover`, `down`, `check`, `is` and `notifications`, were tested by hand on throwaway windows and are not in the 65-check suite yet.
- Keyboard shortcuts need `--real`, because they have to go to the window in front. While a `--real` action runs, anything you type goes to that window.
- Text boxes drawn by XAML take `set`, but `type` needs `--real`.
- Browser and Electron windows need to be showing for the parallel route. A covered one drops some window messages, so clicks and `type` fall back to accessibility, which can take focus for a moment. `draw` still works behind other windows: strokes land while it is covered and show once it is uncovered. A covered browser also stops repainting, so `shot` can show an old picture of it.
- Windows of the same browser share one process, and activate each other when worked at the same time, which loses input. Parallel agents need separate apps or separate browser profiles.
- Rich text boxes, such as Notepad's, need `--real`.
- The pen needs the window in front, so a `draw` borrows your focus for as long as it runs.
- WinForms and some other classic apps ignore posted mouse clicks while another window covers them, so use `--real` there. WinForms canvases do not see a posted drag as a drag, so that needs `--mouse`.
- Classic popup menus, and some other classic controls, only answer the real mouse. Without your permission they are refused, and an agent has to find another way, such as `click x,y` or the keyboard.
- Checkboxes in WinForms, modern menus and `set` in Chromium take focus for a moment before it is given back. A modern menu left open across commands closes when focus returns, so open it and pick from it in one command.
- Right click by pen has only been tried through its code path, not confirmed in an app.
- A cursor working in a covered or fullscreen-covered window stays hidden until that window shows, even though the work carries on.
- `launch` can reopen an app's saved session, so a new window is not always empty.
- A launched window opens behind yours, and may show on top for an instant first. `launch --front` leaves it in front. Because it starts behind you, a browser opened this way is covered until you move something.
- Tested on one monitor at 150% scaling. A second monitor has not been tried.
- Apps running as administrator can be read but not driven, unless deskcrew also runs as administrator.
- Pictures are saved in `%LOCALAPPDATA%\deskcrew\shots` and can show private content.
- Text read off a picture can mistake similar letters, such as `I` and `l`. It reads in the languages set up in Windows.
- Each action takes a picture of the window before and after, which adds a little time on large windows. A blinking caret is ignored, but other animations show as a change.

## License

MIT
