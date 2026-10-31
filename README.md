# deskcrew

**Windows only for now.** Every coding agent gets its own cursor on your desktop, for very few tokens.

Agents will act through Windows accessibility, so your real mouse stays yours. Each one appears as a drawn cursor in its own colour with its name beside it. The cursor glides to a control, and the control lights up with an outline in that colour when it is pressed.

This is an early build. Only the cursor overlay exists so far, and agents cannot drive anything yet.

```bash
cargo run --release -- demo --seconds 12
```

The demo moves three pretend agents around the primary screen. It clicks nothing.

## License

MIT
