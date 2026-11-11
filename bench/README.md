# Agent benchmark

Gives a headless coding agent the same desktop task several times and measures what it costs:
wall time, model calls, thinking, the tokens the API reports, cost, and whether the result is
right. The task windows are throwaway WinForms windows from `app.ps1`, which log what reaches them,
so the result is checked from the app's side, not from what the agent says.

```bash
python bench/run.py --tool deskcrew --task order --runs 3 --label mine
```

```bash
python bench/report.py --by tool --labels mine
```

- `--task order` fills an order form, confirms a dialog and reports the order number.
- `--task stock` finds the item with the lowest stock in an 80-row grid, enters it and reports the check.
- `--tool` is `deskcrew`, `deskcrew-mcp` (deskcrew as an MCP server, with no skill or rule), `agent-desktop`, `agent-cu`, `mcp-windows` or `windows-mcp`. Each gets its own
  skill or the instructions its README gives, and the same built-in tools (Bash and Skill). Paths to
  the other tools go in `bench/tools.json`: `{"agent-desktop": "...exe", "agent-cu": "...exe", "mcp-windows": "...exe"}`.
- The agent is Claude Code (`claude -p`) with a config folder holding only a copy of your sign-in,
  deleted after each run, and a work folder under `C:\Users\Public\deskcrew-bench`, outside your
  profile, so your own CLAUDE.md, skills, plugins and MCP servers are left out. `--model` and
  `--effort` pick the model and effort.

By default everything runs on a hidden Windows desktop, so nothing can reach your screen, focus,
mouse or keyboard. Tools that send real input (agent-cu, mcp-windows, Windows-MCP) only work on the
desktop you see: `--visible` runs there, and only while you are away from the computer.

Results are appended to `bench/results.jsonl`, with each run's tool calls and replies.

`awake.sh` ends the screensaver every two seconds during a `--visible` run, since real input cannot reach the desktop under it. It changes no settings.
