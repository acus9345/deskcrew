# deskcrew

**Windows only for now.** Every coding agent gets its own cursor on your desktop, works in the background, and costs very few tokens.

Agents act through Windows accessibility, so your real mouse and keyboard stay yours. Each agent appears as a drawn cursor in its own colour with its name beside it. The cursor glides to a control, and the control lights up with an outline in that colour when it is pressed. Agents work side by side while you keep working.

It is a single command, so any agent that can run a shell command can use it. There is no MCP server and no tool schema loaded into every conversation.

## Try it

```bash
cargo build --release
```

```bash
target/release/deskcrew demo --seconds 12
```

The demo moves three pretend agents around the screen and clicks nothing.

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

| Route | Used for | Touches your mouse or focus |
| --- | --- | --- |
| `invoke`, `toggle`, `select`, `expand`, `value`, `range`, `scroll` | the app's own accessibility actions | no, though some apps take focus: it is given back and the reply says `focus returned` |
| `command`, `message` | classic Windows controls, by window message | no |
| `real` | anything else, with `--real`, plus `drag` and `draw` | yes, briefly |

Real input waits for a pause in your own typing, is one agent at a time, and puts your pointer and focus back afterwards. It is refused if a point is not on the target window, so it cannot land on something else you have open.

Agents in different windows run at the same time. Agents in one window take turns.

**Ctrl+Alt+Esc** stops every agent. Reading still works, and work resumes with `deskcrew resume`. Shortcuts such as Alt+F4 and Win+R are refused without `--force`.

## Tests

```bash
cargo test
```

```bash
cargo test --release --test desktop -- --ignored --nocapture
```

The second one drives throwaway windows it opens itself, including a real mouse and keyboard for a few seconds, so leave the machine alone while it runs. It has 58 checks.

## Limits

- Windows only. macOS and Linux are not written, and could not be tested on the machine this was built on.
- Canvas apps such as Paint need the real mouse. So do keyboard shortcuts, which have to go to the window in front.
- Text boxes drawn by XAML or Chromium take `set`, but `type` needs `--real`.
- Rich text boxes, such as Notepad's, need `--real`.
- WinForms and some other classic apps ignore posted mouse clicks while another window covers them.
- Checkboxes in WinForms, menus and Edge take focus for a moment before it is given back. A menu that is opened and left open across commands closes when focus returns, so open it and pick from it in one command.
- A drawn cursor floats above whatever covers its target window.
- `launch` can reopen an app's saved session, so a new window is not always empty.
- Tested on one monitor at 150% scaling. A second monitor has not been tried.
- Apps running as administrator can be read but not driven, unless deskcrew also runs as administrator.
- Pictures are saved in `%LOCALAPPDATA%\deskcrew\shots` and can show private content.

## License

MIT
