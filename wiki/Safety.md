# Safety

DeskCrew is built so an agent can work while the person keeps their mouse, their keyboard, and their place. A few commands step outside that on purpose. Those are the ones to treat as asks, not habits.

## The real cursor

Borrowing the cursor is off. `click`, `type`, `draw`, and `drag` do not move it. The only command that moves it is `--mouse`, and only after the person has allowed asking.

| `deskcrew cursor` | Who may borrow it |
| --- | --- |
| `off` | Nobody. This is the default, and it clears any earlier yes. |
| `ask` | An agent may ask, each time. |
| `session` | Every agent, until DeskCrew stops. |
| `always` | Every agent, until `off`. |

`ask`, `session`, and `always` open a prompt the person has to accept. The prompt does not take focus, so typing cannot answer it. No answer within a minute is a no. The `--mouse` prompt is Just this once, This session, Always, or No, with the same timeout.

Some classic controls answer an accessibility call by quietly clicking with the real mouse. A shield blocks synthetic mouse events while those calls run. A control that still needs the real mouse is refused: `this control only answers the real mouse: try click x,y on it, or --mouse to ask the person to borrow their cursor`. Try a point or the keyboard before `--mouse`. If the person says no, find another way. Do not edit `%LOCALAPPDATA%\deskcrew\cursor.txt` to get around a no. That file is how the permission is stored, and anything running as the person can change it. That is why this is a guard against accidents, not a security boundary.

## Focus

Background first. Only these take focus on purpose: `--real`, `down`, `notifications`, and `launch --front`. `type --paste` into a classic text box stays in the background. `type --paste` anywhere else brings the window forward. `click --with ctrl` stays in the background, except WinForms, which needs `--real` as well.

Anything that borrows focus waits for a pause in the person's typing, runs one agent at a time, and gives focus back. It is refused if a point is not on the target window.

`--real` is the reply to `no background way to do this here: add --real`. Keyboard shortcuts, rich text such as Notepad, and `type` into an XAML text box need it. `set` works on XAML text boxes without it. While `--real` runs, keys the person types go to that window.

`notifications` holds focus until `notifications close`.

## What is refused without `--force`

- Shortcuts that close, lock, or leave the session. On Windows: `alt+f4`, `win+l`, `win+r`, `win+d`, `win+x`, `alt+tab`, `ctrl+shift+esc`, `win+e`. Extra modifiers still count (`shift+alt+f4`). On a Mac: `cmd+q`, `cmd+w`, `cmd+option+esc`.
- `do --risk medium` and `do --risk high`. The word is the agent's own label. DeskCrew does not decide that a batch is safe.
- `file write`, `file copy`, and `file move` when the destination exists.
- `kill` of Explorer, Finder, or Dock, and `kill` of a name that matches more than one process.

Ask the person, then add `--force`. `kill` of Windows' own processes (such as `csrss` or `lsass`) and of the agent's own shell is refused even with `--force`. `reg set` only writes under `HKCU`. `file trash` uses the Recycle Bin or the Mac Trash. `close` will not close the desktop or the taskbar. `get` will not read a password field.

## What not to automate

- Do not pass `--mouse`, `--real`, or `--force` as the first attempt.
- Do not run `deskcrew cursor ask`, `session`, or `always` unless the person asked. `cursor off` is always fine.
- Do not `deskcrew resume` after Ctrl+Alt+Esc until the person says to continue.
- Do not `down` a key and walk away. It is released after 30 seconds, on `quit`, on idle exit, and on Ctrl+Alt+Esc. If the service is killed outright, the key stays down until someone presses it.
- Do not leave `notifications` open.
- Do not assume `launch` produced an empty window.
- Do not drive an app that is running as administrator from a normal DeskCrew. It can be read and not driven. The reply is `blocked: the window runs as administrator`.

## Other limits that surprise people

- One monitor at 150% scaling has been tested. A second monitor has not. `screens` still lists them, and `shot --screen -a` captures their bounding box.
- A cursor working behind another window, including a fullscreen window, keeps working and stays hidden until its window shows.
- WinForms ignores posted clicks while it is covered, so that case needs `--real`. A WinForms canvas does not see a posted drag as a drag, so that needs `--mouse`.
- Classic popup menus answer the real mouse. Without permission they are refused.
- Apps that share one browser process steal input from each other. Use separate apps or separate profiles.
- The Mac build has not been run on a Mac. `--mouse`, holding a mouse button, `notifications`, and `demo` are absent there. Pictures on a Mac use an API Apple has deprecated.
- Right-click by the synthetic pen has been tried in code, not confirmed in an app.
- Each action compares a picture of the window before and after, which adds a little time on a large window.
