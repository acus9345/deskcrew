---
name: deskcrew
description: Operate native Windows apps in the background with the deskcrew command, without taking the person's mouse or focus. Use for looking at, clicking, typing into or drawing in a real Windows app.
---

# deskcrew

Run `deskcrew help` once for the commands. Windows only.

- Pick a window, then work by ref: `deskcrew windows`, `deskcrew see Calculator`, `deskcrew click e12`. Refs stay valid across reads.
- Every action replies with what changed, so do not read again after it. `see` also shows only changes; `see -f` lists everything, for when you no longer have the list.
- If you know the control's name, skip the read: `deskcrew click "Save" -w notepad`.
- Batch steps: `deskcrew click e1 e2 e3`, or `deskcrew do "click e1; set e2 'hi'; key enter"`. Open a menu and pick from it in one command: `deskcrew click File Save`.
- Pictures only when text is not enough: `deskcrew shot` prints a PNG path to open. `unchanged` means you already have it. `click x,y` uses pixels in the last shot.
- Name yourself once per session, so parallel agents get separate cursors: `--agent name` or `DESKCREW_AGENT`.

Background first. A failure saying `add --real` means the action needs the window in front: `--real` borrows the person's focus briefly, and uses the keyboard or a pen, so their cursor stays put. `drag` and `draw` pick the quietest way themselves. Agents take turns on this, and it is refused if a point is not on the target window.

`--mouse` is the last resort: it moves the person's cursor. Use it only when a reply asks for it.

Check before overwriting: `launch` can reopen an app's saved session, so read what is open before `set`, and never assume a new window is empty.

`stopped by the person` means Ctrl+Alt+Esc was pressed. Stop and ask them before running `deskcrew resume`.
