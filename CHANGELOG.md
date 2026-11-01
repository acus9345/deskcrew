# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

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
