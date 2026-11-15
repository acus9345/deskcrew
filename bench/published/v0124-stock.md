stock task, label `v0124`: sonnet at default effort, median of each tool's runs.

| Tool | Right | Time | Model calls | Tokens read | Tokens written | Cost |
| --- | --- | --- | --- | --- | --- | --- |
| deskcrew | 3 of 3 | 10 s | 4 | 39,262 | 317 | $0.036 |
| agent-cu | 3 of 3 | 14 s | 4 | 77,389 | 429 | $0.101 |
| mcp-windows | 3 of 3 | 21 s | 10 | 496,677 | 1,812 | $0.202 |
| agent-desktop | 3 of 3 | 30 s | 13 | 255,474 | 1,424 | $0.124 |
| windows-mcp | 3 of 3 | 52 s | 13 | 333,375 | 2,047 | $0.150 |

deskcrew against agent-cu: 1.3x faster, 2.0x fewer tokens read, 2.8x cheaper.
deskcrew against mcp-windows: 2.0x faster, 12.7x fewer tokens read, 5.7x cheaper.
deskcrew against agent-desktop: 2.9x faster, 6.5x fewer tokens read, 3.5x cheaper.
deskcrew against windows-mcp: 5.0x faster, 8.5x fewer tokens read, 4.2x cheaper.
