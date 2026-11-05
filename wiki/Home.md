# DeskCrew

DeskCrew is a Windows command-line tool that lets each coding agent drive apps in the background. Every agent gets its own drawn cursor, in its own colour, with its name beside it. The cursor sits just above the window that agent is working in, so your other windows can cover it. Your real mouse stays where you left it.

Agents act through Windows accessibility, window messages, and a synthetic pen. There is no MCP server and no tool schema loaded into every conversation. Any agent that can run a shell command on this computer can use it: Claude Code, Cursor, Codex, Gemini CLI, Copilot, Windsurf, OpenCode, Aider, and the rest. Agents running in the cloud, or in a sandbox that cannot see this desktop, cannot reach it.

This wiki describes **deskcrew 0.1.9**, the version in the repository and on the [project homepage](https://deskcrew.qlinter.org). The Windows build is the one that has been run. A macOS build is in the same source, type-checked for Apple Silicon and Intel, and has not been run on a Mac. Linux is not supported.

## Start here

| | |
| --- | --- |
| Install | [Install and setup](Install-and-setup) |
| Every command and flag | [Command reference](Command-reference) |
| Refs, shots, and x,y | [Seeing and acting](Seeing-and-acting) |
| How an agent should work | [For coding agents](For-coding-agents) |
| Cursor, focus, and refusals | [Safety](Safety) |

Longer narrative docs, with the same commands, live at [qlinter.org/docs/deskcrew](https://qlinter.org/docs/deskcrew/). This wiki is the short working copy for people installing DeskCrew and for the agents that run it.

## A window is read once

```text
$ deskcrew see Calculator
w2 "Calculator" 37 controls, 1 off screen
e4 "Display is 0"
e12 button "One"
...
$ deskcrew click e12
ok invoke
~ e4 "Display is 1"
```

`e12` stays valid, so the window does not need to be listed again. The reply after a click is only the control that changed. `click e12 e20 e13 e24` and `do` run many steps in one call. Pictures are files; an agent opens one only when it needs the drawing. The same idea is measured in the [README](https://github.com/acus9345/deskcrew#why-it-is-cheap): on Calculator, knowing the result of every press of `1 + 2 =` was 811 characters with DeskCrew.

## What a person sees

Each agent is recognised from its environment or from the program that launched it, and gets a name and a colour. `deskcrew status` looks like:

```text
deskcrew 0.1.9 running; agents: Cursor
```

**Ctrl+Alt+Esc** stops every agent (Ctrl+Option+Esc on a Mac). Reading still works. `deskcrew resume` clears the stop. Nothing moves your real cursor unless you allow it with `deskcrew cursor`. See [Safety](Safety).
