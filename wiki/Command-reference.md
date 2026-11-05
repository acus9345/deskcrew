# Command reference

`deskcrew help` (also `--help`, `-h`, or no arguments) prints the command list. `deskcrew --version` prints `deskcrew 0.1.9`. Commands below are the Windows CLI. Mac differences are marked. Anything not listed here is not a command: the reply is `unknown command`.

Flags can sit before or after the command. `-w` / `--window` picks a window by title, app name, or `w2`. With no `-w`, the agent's last window is used. `--agent name` names the cursor. `DESKCREW_AGENT` does the same when the flag is absent. See [For coding agents](For-coding-agents).

## Reading

| Command | What it does |
| --- | --- |
| `windows [filter]` | Windows whose title or app contains the filter. `w2* "Calculator" Calculator` — the star is this agent's current window. At most 40. |
| `see [window] [-f] [-a] [--find word] [--from N]` | Controls as `e12 button "One"`. Later calls show only what changed, unless more than half the list changed, in which case the full list comes back. `-f` always lists everything. `-a` includes off-screen controls. `--find` keeps lines containing the word. `--from N` skips N matching lines (120 per page). A large area with no controls is described, and text there is read with Windows OCR. See [Seeing and acting](Seeing-and-acting). |
| `get e3` | That control's full text, up to 20,000 characters. A password field is refused. |
| `is e3 on\|off\|open\|closed\|selected\|enabled\|disabled` | `yes` or `no`. |
| `table e5 [--from N]` | A table or grid as tab-separated rows, headers first. Default 50 rows. `--from N` reads the first N, up to 500. |
| `shot [window\|e3] [-f] [--marks] [--rulers] [--screen [-a]] [--region x,y,w,h] [--changed] [--edge N]` | PNG path, size, and scale. `unchanged` when the window picture matches the last one. Details under [Seeing and acting](Seeing-and-acting). |
| `apps` | App names of the windows that are open, one each, sorted. |
| `screens` | `0: x,y wxh primary` for each monitor. |
| `status` | Version, `running` or `stopped by the person`, and agent names. |

## Acting on controls

Targets are a ref (`e3`), a control name (`"Save"`), or a point (`x,y`). A name that matches several controls lists them and does nothing. Several names that are a menu path work in one command: `click File Save`.

| Command | What it does |
| --- | --- |
| `click e3 [e4 ...] \| "Name" \| x,y` | Press. Several targets run in order and return one reply. |
| `hover e3 \| x,y` | Move the drawn cursor there and post a hover (`message`). A window that draws itself needs `--real`. `--mouse` moves the real cursor. |
| `check e3` / `uncheck e3` | Leave it alone when it is already on or off. |
| `toggle e3` | Click a control that toggles. |
| `expand e3` / `collapse e3` | Open or close. A WinForms combo box does not offer expand; use `select`. |
| `clear e3` | Set the control's value to empty. |
| `set e3 "value"` | Replace a field's text. |
| `type [e3] "text" [--paste]` | Type. `--paste` uses the clipboard and puts the previous clipboard back. |
| `key enter [--hold ms --real]` | A key or a chord such as `ctrl+s`. Names: enter, tab, esc, space, backspace, delete, arrows, home, end, pageup, pagedown, f1–f24, ctrl, shift, alt, win, or one character. |
| `select e3 "Item"` | Pick an item in a list or combo box. |
| `scroll [e3] up\|down [n]` | Scroll a control, or the one marked scroll. `n` defaults to 1. |
| `show e3` | Scroll that item into view, when the control supports it. |
| `wait "text" [--gone] [--ms N]` | Wait until the text appears, or disappears with `--gone`. Default 10 s, at most 60 s. `wait 500` sleeps 500 ms. |
| `drag a b` | Drag from one ref or point to another. |
| `draw [e3] "x,y x,y; x,y x,y" [--smooth]` | Strokes separated by `;`. Points in a stroke are separated by spaces. See [Seeing and acting](Seeing-and-acting). |
| `down shift --real` | Hold a key. It reaches whatever has focus. |
| `down left x,y --mouse` | Hold a mouse button. Needs permission to borrow the cursor. Buttons: left, right, middle. |
| `up` | Release whatever this service is holding. |
| `pick-file "path"` | Fill an Open or Save dialog and press its button. The field and button are found by their Windows ids, so the dialog language does not matter. |
| `do "click e1; set e2 hi; key enter" [--dry] [--risk low\|medium\|high]` | Many steps, one reply. Steps split on `;` or a newline. Quotes keep spaces. `--dry` prints the plan and changes nothing. |

`check`, `uncheck`, `expand`, and `collapse` are the right tools when the goal is a state. `click` always presses.

Any action accepts `--expect "text"`. DeskCrew then waits for that text and fails if it never shows.

A reply names the route and then the changes:

```text
ok invoke
~ e4 "Display is 1"
```

Several steps: `ok 4 steps`. New controls are `+`, changed ones `~`, gone ones `- e40-e41`. At most 30 change lines, then `… N more changes: see`. Routes are `invoke`, `toggle`, `select`, `expand`, `value`, `range`, `scroll`, `command`, `message`, `pen`, `keyboard`, `mouse`, `paste`, and `dialog closed`.

## Windows and the clipboard

| Command | What it does |
| --- | --- |
| `launch app [args] [--front]` | Start an app by path, `.exe`, WindowsApps alias, or Start Menu name. The new window opens behind yours unless `--front`. It can reopen a saved session, so a new window is not always empty. |
| `focus [window]` | Bring that window forward. |
| `close [window]` | Close it. The desktop and the taskbar are refused. |
| `place x,y,w,h\|min\|max\|restore\|show` | Move and resize, or minimize, maximize, restore, or show without activating. `x,y,w,h` are screen pixels. |
| `clip` | Clipboard text, or `(no text on the clipboard)`. |
| `clip set "text"` / `clip clear` | Replace or clear the clipboard. |
| `notifications [close]` | Open the notification panel with Win+N, list it, and keep focus until `notifications close`. Always close it. Not on the Mac. |
| `cursor [off\|ask\|session\|always]` | Who may borrow the real mouse. See [Safety](Safety). |
| `resume` / `quit` | Clear a stop, or exit the service. |

## Beyond windows

These run in the command itself, with no service, so they work even when the service is stopped.

| Command | What it does |
| --- | --- |
| `processes [filter]` | `pid name "window title"`, up to 300. |
| `kill pid\|name [--force]` | End a process. Windows' own processes and this agent's parent processes are refused even with `--force`. Explorer (Finder or Dock on a Mac) and a name that matches several processes need `--force`. |
| `run "command" [--ms N]` | PowerShell on Windows, zsh on a Mac. Output and exit code. Default 60 s, at most 10 minutes. Output cuts at 20,000 characters. |
| `fetch URL [--raw]` | Page text, or the raw response with `--raw`. Gives up after 30 s. |
| `file list [dir]` | Names, directories first. Up to 300. |
| `file read path [--from N]` | Text. A file with a null byte in the first 4 KB is refused. |
| `file write path "text"` / `file append path "text"` | `write` refuses to replace an existing file without `--force`. |
| `file find [dir] name-part` | Paths whose names contain the text. Depth 12, up to 200. |
| `file copy a b` / `file move a b` | Refuse to replace an existing target without `--force`. |
| `file trash path` | Recycle Bin on Windows, `~/.Trash` on a Mac. |
| `reg get KEY [name]` / `reg list KEY` / `reg set HKCU\KEY name value` | `set` only writes `HKCU` or `HKEY_CURRENT_USER`. On a Mac, `reg` is refused. |
| `notify "title" "message"` | A toast. One argument uses the title `deskcrew`. |
| `settings [page]` | Opens Settings, for example `display` or `bluetooth`. The page comes to the front. |

## Flags

| Flag | Meaning |
| --- | --- |
| `-w`, `--window` | Window title, app, or `w2`. |
| `--agent name` | Cursor name. Overrides recognition and `DESKCREW_AGENT`. |
| `--fast` | Put the drawn cursor in place. No glide. |
| `--force` | Allow a refused shortcut, a risky `do`, an overwrite, or ending Explorer. |
| `--real` | Bring the window forward briefly and use the keyboard or a pen. Last resort. The person's cursor stays put. |
| `--mouse` | Borrow the real cursor, then put it back. Refused until the person allows asking. Windows only. |
| `--expect "text"` | Fail the action unless this text shows afterwards. |
| `--dry` | On `do` only: print the steps, change nothing. |
| `--risk low\|medium\|high` | On `do` only. `medium` and `high` are refused until `--force` is added. DeskCrew does not score the script; it trusts the word the agent passes. |
| `--right` `--double` `--triple` `--middle` `--back` `--forward` | Click kind. |
| `--with ctrl+shift` | Modifiers on a click. WinForms reads the keyboard itself, so it also needs `--real`. |
| `--paste` | `type` through the clipboard. |
| `--smooth` | Curve a `draw` stroke through its points. |
| `--hold ms` | On `key`, hold then release. Requires `--real`. Capped at 10 s. |
| `--gone` | `wait` until the text is gone. |
| `--ms N` | Time limit for `wait` or `run`. |
| `--front` | `launch` leaves the new window in front. |
| `-f`, `--full` | `see` lists every control. `shot` writes a new PNG even when the picture matches. |
| `-a`, `--all` | `see` includes off-screen controls. `shot --screen -a` captures every monitor. |
| `--find word` `--from N` | Filter or page a `see`, or set how many table rows to read. |
| `--marks` `--rulers` `--changed` `--edge N` `--region x,y,w,h` `--screen` | Shot options. See [Seeing and acting](Seeing-and-acting). |

`--` stops flag parsing so the next word is an argument.

## Keys that are refused

On Windows, `key` refuses these even with extra modifiers (`shift+alt+f4` counts as `alt+f4`) unless `--force` is set: `alt+f4`, `win+l`, `win+r`, `win+d`, `win+x`, `alt+tab`, `ctrl+shift+esc`, `win+e`. `cmd` is treated as `win`.

On a Mac the refused chords are `cmd+q`, `cmd+w`, and `cmd+option+esc`.

## Cursor policy

`deskcrew cursor` with no argument prints whether agents may ask, who may borrow for this service session, and who may always borrow.

| Command | Effect |
| --- | --- |
| `off` | Default. No agent can borrow the cursor. Clears earlier permission. Takes effect without a prompt. |
| `ask` | Agents may ask each time. You confirm on screen. |
| `session` | Every agent may borrow until DeskCrew stops. You confirm on screen. |
| `always` | Every agent may borrow until `off`. You confirm on screen. |

The confirm prompt is Allow or No. It does not take focus. No answer within a minute counts as no. An agent that then passes `--mouse` gets a second prompt: Just this once, This session, Always, or No. Same one-minute rule.

This stops an agent turning the cursor on by habit. It is not a security boundary: anything running as you can edit `%LOCALAPPDATA%\deskcrew\cursor.txt` or click the prompt. The file contains a line `ask` and lines `always <name>`. Every agent is stored as `always *`.

## Stop

**Ctrl+Alt+Esc** stops every agent. **Ctrl+Option+Esc** on a Mac. The reply is `stopped by the person with Ctrl+Alt+Esc: ask them before doing more (deskcrew resume clears it)`.
