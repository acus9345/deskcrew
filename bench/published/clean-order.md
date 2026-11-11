order task, label `clean`: sonnet at default effort, median of each tool's runs.

| Tool | Right | Time | Model calls | Tokens read | Tokens written | Cost |
| --- | --- | --- | --- | --- | --- | --- |
| deskcrew | 3 of 3 | 17 s | 5 | 50,187 | 420 | $0.039 |
| agent-desktop | 0 of 3 | 55 s | 11 | 209,974 | 1,419 | $0.119 |
| mcp-windows | 3 of 3 | 73 s | 20 | 760,113 | 3,154 | $0.205 |
| windows-mcp | 3 of 3 | 98 s | 10 | 372,519 | 1,394 | $0.210 |
| agent-cu | 3 of 3 | 348 s | 15 | 305,637 | 2,013 | $0.138 |

deskcrew against mcp-windows: 4.4x faster, 15.1x fewer tokens read, 5.2x cheaper.
deskcrew against windows-mcp: 5.8x faster, 7.4x fewer tokens read, 5.4x cheaper.
deskcrew against agent-cu: 20.7x faster, 6.1x fewer tokens read, 3.5x cheaper.
