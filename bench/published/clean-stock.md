stock task, label `clean`: sonnet at default effort, median of each tool's runs.

| Tool | Right | Time | Model calls | Tokens read | Tokens written | Cost |
| --- | --- | --- | --- | --- | --- | --- |
| deskcrew | 3 of 3 | 11 s | 4 | 41,373 | 293 | $0.041 |
| agent-desktop | 3 of 3 | 26 s | 8 | 142,283 | 1,338 | $0.107 |
| agent-cu | 3 of 3 | 26 s | 6 | 129,703 | 676 | $0.111 |
| mcp-windows | 3 of 3 | 57 s | 19 | 1,035,417 | 4,486 | $0.311 |
| windows-mcp | 3 of 3 | 127 s | 15 | 413,186 | 2,490 | $0.165 |

deskcrew against agent-desktop: 2.3x faster, 3.4x fewer tokens read, 2.6x cheaper.
deskcrew against agent-cu: 2.4x faster, 3.1x fewer tokens read, 2.7x cheaper.
deskcrew against mcp-windows: 5.2x faster, 25.0x fewer tokens read, 7.7x cheaper.
deskcrew against windows-mcp: 11.6x faster, 10.0x fewer tokens read, 4.1x cheaper.
