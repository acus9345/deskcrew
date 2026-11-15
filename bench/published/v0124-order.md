order task, label `v0124`: sonnet at default effort, median of each tool's runs.

| Tool | Right | Time | Model calls | Tokens read | Tokens written | Cost |
| --- | --- | --- | --- | --- | --- | --- |
| deskcrew | 3 of 3 | 12 s | 5 | 50,155 | 400 | $0.040 |
| windows-mcp | 3 of 3 | 37 s | 8 | 176,123 | 1,117 | $0.096 |
| agent-desktop | 0 of 3 | 65 s | 13 | 261,099 | 1,814 | $0.133 |
| mcp-windows | 3 of 3 | 74 s | 25 | 1,003,453 | 4,344 | $0.269 |
| agent-cu | 3 of 3 | 355 s | 16 | 329,400 | 2,086 | $0.139 |

deskcrew against windows-mcp: 3.0x faster, 3.5x fewer tokens read, 2.4x cheaper.
deskcrew against mcp-windows: 6.0x faster, 20.0x fewer tokens read, 6.7x cheaper.
deskcrew against agent-cu: 29.1x faster, 6.6x fewer tokens read, 3.5x cheaper.
