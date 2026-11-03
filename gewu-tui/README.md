# gewu-tui

Full-screen terminal UI for a GeWu server (`gewu-server serve`), built on the
[`gewu-client`](../gewu-client/README.md) library.

Start a server first:

```bash
cargo run -p gewu-server -- serve
```

Then launch the TUI:

```bash
cargo run -p gewu-tui
```

## Options

| Flag | Default | Description |
|------|---------|-------------|
| `--host` | `127.0.0.1` | Server host |
| `--tcp-port` | `8421` | Server TCP port (newline-delimited JSON) |
| `--session-id` | — | Resume a saved conversation (also via `GEWU_SESSION_ID`) |

On exit, the client prints the session id to stderr (`[session: <id>]`) so you
can capture it and resume later:

```bash
GEWU_SESSION_ID=<id> cargo run -p gewu-tui   # resume a conversation
```

## Keybindings

| Key | Action |
|-----|--------|
| `Enter` | Send the prompt (streams the reply into the transcript) |
| `Esc` | Interrupt the running ask |
| `↑`/`↓` | Scroll the transcript (when the input is empty) |
| `PageUp`/`PageDown` | Scroll faster |
| `Backspace`/`Delete`, `←`/`→`, `Home`/`End` | Edit the input |
| `Ctrl+C`/`Ctrl+D` | Quit (Ctrl+D only when idle with an empty input) |

While a reply streams, the status line shows `thinking…`; tool calls appear as
`$ <command>` lines with their output beneath. Scrolling up pins the history
so streaming output doesn't yank you back to the bottom; scroll back to the
bottom to re-follow.

## Layout

```
gewu-tui
  main.rs      CLI flags + terminal event loop
  tui.rs       App state (composer, scroll) over gewu_client::session::SessionModel
  tui_view.rs  ratatui rendering
```

Conversation state lives in `gewu-client::session` and is shared with the
desktop UI, so both front-ends render events and replay history identically.
