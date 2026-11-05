# Seeing and acting

Read a window once, keep the refs, and let each action tell you what changed. Take a picture when the work is a drawing, or when the control list has a hole where the content is.

## Refs

`windows` numbers windows `w1`, `w2`. `see` numbers controls `e1`, `e2`. The same control keeps its number across reads, including when the app rebuilds it. A ref that is really gone returns `e12 is gone: see`.

A line looks like `e12 button "One"`, `e9 combobox "Orientation" =Landscape`, or `checkbox "Wrap" [on]`. Names are cut at 80 characters, values at 60. A later `see` prints changes:

```text
+ e40 edit "File name:"
~ e4 "Display is 1"
- e40-e41
```

`no change` means the list is the same. Pass `-f` when you no longer have the list.

If you know the name, you can skip the read: `deskcrew click "Save" -w notepad`. An ambiguous name lists the matches and stops.

## Where there are no controls

`see` looks for a large area no control covers (a canvas, a game, a custom drawing). When it finds one, it reads text there with the OCR built into Windows, in the languages set up on the machine:

```text
w1 "Crew Canvas" 1 controls
e1 button "Paint"
no controls in 0,125 878x540; text read there (x,y for click):
"Total 42" 129,231
"Start game" 560,453
for drawings: shot e1
```

Those x,y are pixels from the window's top-left, at the centre of the text Windows read. `click 560,453` uses them as they stand. OCR can confuse similar letters such as `I` and `l`. Up to 30 lines. If the area is only a drawing, the line says `only drawing` and gives the `shot` command for that part.

When an action changes no control, the reply still says whether the window picture changed:

```text
ok message
controls unchanged; the picture changed at 14,53 54x17
```

That box is in the window's own pixels. `nothing changed, on screen either` means there was no visible change. A message route that changed nothing adds `try --real if it should have`. A blinking caret is ignored. Other animation shows up as a change.

## Shots

```text
C:\Users\you\AppData\Local\deskcrew\shots\Claude-Code-w3.png 1356x848 scale 1.42
```

Pictures are taken from the window itself, so another window can cover it and stay out of the frame. They are saved under `%LOCALAPPDATA%\deskcrew\shots` on Windows and `~/Library/Caches/deskcrew/shots` on a Mac. The file name is the agent name, with non-letters turned into `-`, then `-w` and the window number. Each new shot of that pair replaces the last one. Shots can contain private content.

The long edge is shrunk to at most 1356 pixels (`--edge N` changes that) so small text survives. `scale` is how many source pixels one PNG pixel covers. A picture already smaller than the edge is left alone and prints `scale 1.00`. A second `shot` of an unchanged window prints `unchanged` and does not write again. `-f` writes anyway.

| Flag | Effect |
| --- | --- |
| `shot e3` | Crop to that control. |
| `--marks` | Box and number buttons, edits, menu items, checkboxes, radios, combo boxes, tabs, items, links, sliders, and spinners with their refs. |
| `--rulers` | A line every 100 PNG pixels, labelled with the x,y that `click` and `draw` take on this picture. |
| `--changed` | Crop to the part that differs from the previous window shot, and make that crop the coordinate space. |
| `--screen` | The primary monitor, copied from the screen. `-a` includes every monitor. |
| `--region x,y,w,h` | A screen rectangle. |

A minimized window is refused: `that window is minimized: place restore first`. If Windows cannot see the window's own pixels, DeskCrew copies that rectangle off the screen and says so. Anything covering the window is then in the picture.

## Coordinates

`click x,y` and `draw` without a control use pixels of the last window shot, or the text coordinates from `see`.

Pass the PNG's own pixels. `--rulers` prints those numbers. The `scale` in the shot line is the factor DeskCrew applies when it maps a PNG pixel back onto the window. For a DPI-aware app that factor is the printed scale. An older app that still draws in 96-DPI units on a high-DPI monitor is mapped with the printed shrink times that app scale, which DeskCrew stores with the shot.

`see` text coordinates are a different space, and they replace the shot mapping until the next window shot: scale 1, origin at the window's top-left. `click 560,453` after the OCR example above is already in that space.

`draw e3 "10,10 200,10"` is different again. Those x,y are added to the control's top-left in screen pixels.

`shot --screen` and `shot --region` save a picture (`-w0` in the file name) and leave the click/draw mapping unchanged. `place` uses screen pixels. The "picture changed at x,y" box is in window pixels, not PNG pixels.

A session on 0.1.4 drew in Photon Studio (window title `Untitled-1`) from a shot under `%LOCALAPPDATA%\deskcrew\shots` whose reply included a scale around 1.22. That number is what 0.1.9 still prints when the window's long side is about 1654 picture pixels (`1654 / 1356 ≈ 1.22`). Photon Studio is an Electron window, so it is DPI-aware: the coordinates that land are the PNG pixels, the ones `--rulers` labels.

If no shot or OCR read has set a mapping yet, a point is refused: `x,y are pixels in a shot: take one first`.

## Draw

```bash
deskcrew shot -w "Untitled-1" --rulers
deskcrew draw "300,300 500,300 500,500 300,500 300,300; 350,350 450,450" --smooth -w "Untitled-1"
```

One quoted string. Spaces separate points. Semicolons separate strokes. `--smooth` curves the stroke through those points. An empty stroke is skipped. `draw` with no points returns `nothing to draw`. A point that is not `x,y` is named in the error.

The route depends on the app:

| App | Route | Focus |
| --- | --- | --- |
| Browser or Electron, without `--real` or `--mouse` | `message` | Stays with you. Strokes land while the window is covered and show when it is uncovered. Several agents on one canvas take turns in short overlapping pieces, so undo steps back in small pieces. |
| An app that draws itself, such as Paint | `pen` | Borrowed for the stroke, then returned. |
| `--mouse`, after you allow it | `mouse` | The real cursor moves, then returns. |

A point that is not on the target window is refused, so a stroke cannot land on something else you have open.

## How a click chooses a route

Every action tries a background route first. The reply's first line is the route that worked.

| Route | Used for | Your cursor | Your focus |
| --- | --- | --- | --- |
| `invoke`, `toggle`, `select`, `expand`, `value`, `range`, `scroll` | The app's own accessibility actions | Untouched | Some apps take it for a moment, and it is handed straight back |
| `command`, `message` | Classic controls and menus, and clicks, typing, `drag`, and `draw` in browsers and Electron | Untouched | Untouched in classic apps. A Chromium window can take it for a moment |
| `pen` | `drag`, `draw`, and `--real` in apps that draw themselves | Untouched | Borrowed for the action |
| `keyboard` | `key`, `type`, and `set` with `--real` | Untouched | Borrowed for the action |
| `mouse` | Only `--mouse`, and only if you allow it | Moved, then put back | Borrowed for the action |

`--fast` skips the glide of the drawn cursor. The press outline still draws.

Agents in different windows run at the same time. Agents in one window take turns. The pen and `--real` also take turns across windows, because they need the window in front. Windows of the same browser share a process and steal input from each other; parallel agents need separate apps or separate browser profiles.

A covered browser drops some window messages, so clicks and `type` fall back to accessibility, which can take focus briefly. `type` into a Chromium field reads the field back and sets the text directly if characters were dropped. Keep the window showing for the background route. A covered browser also stops repainting, so `shot` can show an old picture. `draw` still lands.

`add --real` means there was no background way. `--real` waits for a pause in your typing, brings the window forward, does one agent's work, and gives focus back. While it runs, what you type goes to that window. Typing stops if focus leaves, and the reply says how many characters had been typed.
