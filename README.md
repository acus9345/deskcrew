# deskcrew

**Windows only for now.** Every coding agent gets its own cursor on your desktop, works in the background, and costs very few tokens.

Agents act through Windows accessibility, window messages and a synthetic pen, so your cursor stays yours. Each agent appears as a drawn cursor in its own colour with its name beside it. The cursor glides to a control, and the control lights up with an outline in that colour when it is pressed. Agents work side by side while you keep working.

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

That puts `deskcrew` on PATH and gives the skill to each agent it finds. For any other agent, add one line to its rules: `To operate Windows apps on this computer, run the deskcrew command. Start with: deskcrew help`. Agents running in the cloud, or in a sandbox without access to your desktop, cannot reach it.

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

Measured on 2026-11-01 against agent-desktop 1.0.2, in characters an agent has to read, on throwaway windows at 150% scaling. Its figures use `snapshot -i --compact`. These are characters, not tokens, and agent-desktop's `--skeleton` mode reads less by leaving detail out.

| Reading a window | Controls | agent-desktop | deskcrew | Time, agent-desktop | Time, deskcrew |
| --- | --- | --- | --- | --- | --- |
| Test form | 42 | 10,138 | 1,047 | 191 ms | 74 ms |
| Character Map | 11 | 3,603 | 260 | 177 ms | 24 ms |
| Calculator | 37 | 9,350 | 689 | 175 ms | 25 ms |
| File Explorer | 121 | 39,595 | 3,375 | 296 ms | 62 ms |

For `1 + 2 =` on Calculator, knowing the result of every press took 48,570 characters with agent-desktop, reading after each press, and 811 with deskcrew. The tool's help is 1,169 characters.

## How actions reach an app

Each reply names the route it took.

| Route | Used for | Your cursor | Your focus |
| --- | --- | --- | --- |
| `invoke`, `toggle`, `select`, `expand`, `value`, `range`, `scroll` | the app's own accessibility actions | untouched | some apps take it for a moment, and it is handed straight back |
| `command`, `message` | classic Windows controls and menus, plus clicks, typing, `drag` and `draw` in browsers and Electron apps, by window message | untouched | untouched in classic apps, and a Chromium window can take it for a moment |
| `pen` | `drag`, `draw` and `--real` in apps that draw themselves and ignore messages, such as Paint | untouched | borrowed for the action |
| `keyboard` | `key`, `type` and `set` with `--real` | untouched | borrowed for the action |
| `mouse` | only with `--mouse`, when nothing else works | moved, then put back | borrowed for the action |

**Only `--mouse` moves your cursor.** Some classic controls answer an accessibility request by quietly clicking with the real mouse, so a shield blocks every synthetic mouse event while those requests run. A control that needs that is refused with a message instead. In tests, Paint drawing and Edge presses caused no synthetic mouse events at all.

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

The second one drives throwaway windows it opens itself, including a real mouse and keyboard for a few seconds, so leave the machine alone while it runs. It has 65 checks, and it counts synthetic mouse events to prove when the cursor was left alone.

## Limits

- Windows only. macOS and Linux are not written, and could not be tested on the machine this was built on.
- Keyboard shortcuts need `--real`, because they have to go to the window in front. While a `--real` action runs, anything you type goes to that window.
- Text boxes drawn by XAML take `set`, but `type` needs `--real`.
- Browser and Electron windows need to be showing for the parallel route. A covered one drops window messages, so clicks and `type` fall back to accessibility, which can take focus for a moment, and `draw` is refused without `--real`. A covered browser also stops repainting, so `shot` can show an old picture of it.
- Windows of the same browser share one process, and activate each other when worked at the same time, which loses input. Parallel agents need separate apps or separate browser profiles.
- Rich text boxes, such as Notepad's, need `--real`.
- The pen needs the window in front, so a `draw` borrows your focus for as long as it runs.
- WinForms and some other classic apps ignore posted mouse clicks while another window covers them, and WinForms canvases do not see a posted drag as a drag. Use `--real` for the first and `--mouse` for the second.
- Classic popup menus, and some other classic controls, are refused without `--mouse`.
- Checkboxes in WinForms, modern menus and `set` in Chromium take focus for a moment before it is given back. A modern menu left open across commands closes when focus returns, so open it and pick from it in one command.
- Right click by pen has only been tried through its code path, not confirmed in an app.
- A drawn cursor floats above whatever covers its target window.
- `launch` can reopen an app's saved session, so a new window is not always empty.
- A launched window opens behind yours, and may show on top for an instant first. `launch --front` leaves it in front. Because it starts behind you, a browser opened this way is covered until you move something.
- Tested on one monitor at 150% scaling. A second monitor has not been tried.
- Apps running as administrator can be read but not driven, unless deskcrew also runs as administrator.
- Pictures are saved in `%LOCALAPPDATA%\deskcrew\shots` and can show private content.

## License

MIT
