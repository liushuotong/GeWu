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
| `--session-id` | — | Resume a specific conversation (also via `GEWU_SESSION_ID`) |
| `--new` | off | Start a fresh conversation instead of resuming the last one |

On startup the TUI **resumes your most recent conversation**: it lists the
stored sessions, picks the newest, and replays its transcript before the first
screen is drawn. Pass `--new` to start empty, or `--session-id <id>` to open a
specific one (which wins over `--new`).

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
| `↑`/`↓` | Scroll the transcript (when the input is empty; works while streaming) |
| `PageUp`/`PageDown` | Scroll faster |
| `Home`/`End` | Jump to transcript top / bottom (when the input is empty), else move the input cursor |
| `Backspace`/`Delete`, `←`/`→` | Edit the input |
| `Ctrl+C`/`Ctrl+D` | Quit (Ctrl+D only when idle with an empty input) |
| `Ctrl+N` | Start a new chat (reconnects, clearing the transcript and session) |

Touchpad and mouse-wheel scrolling is captured by the TUI and scrolls the
transcript, including over SSH (where the terminal's own scrollback is not
usable in the alternate screen). Because mouse capture is on, dragging to
select text requires holding **Shift**, as in vim or tmux.

While a reply streams, the status line shows `thinking…`. Tool calls render
like OpenCode's TUI: an inline row per call, with no `tool` label and no box —
`$ <command>` for the shell, `→ <path>` for reads, `← <path>` for writes and
edits, `✱ <pattern>` for searches, `% <url>` for fetches, and
`⚙ <tool> <input>` for anything else.
The row is dim, and a tool still running ends in `…`. Output appears beneath
the row, collapsed to the first few lines with `… +N lines` when it overflows;
a tool's row plus output can be **clicked** to show or hide the rest. Runs of two
or more consecutive tool calls collapse into a `+ Explored: 2 shells, 1 read`
summary that expands on click (`Explored` once every call finished, `Exploring`
while one still runs). The status line also shows the server's model as
`provider/model` (e.g. `openai/gpt-4o`), whenever the server reports it.
Scrolling up pins the history so streaming output doesn't yank you back to the
bottom (the status line shows `↑N more`); scroll back to the bottom, or press
`End`, to re-follow.

## Colors

Important text — user prompts, assistant replies, and errors — uses the
terminal's default foreground (white on most themes). Markdown headings are
the exception: they use the accent cyan. Supporting details are dimmed: tool
rows and their output, informational status notes, and the status line.

## Markdown

Finished **assistant replies** are rendered as Markdown: headings, bold and
italic text, inline code, fenced code blocks with syntax highlighting, lists,
block quotes, links, and Unicode-bordered tables. Long lines and tables are
wrapped to the width of the transcript pane.

Heading markers (`#`/`##`) are concealed. Headings render in accent cyan and
bold — H1 is also underlined — mirroring OpenCode's TUI, since terminals have
no font sizes to convey hierarchy.

The reply currently streaming is shown as raw text until it finishes, so
half-typed syntax (such as an unclosed code fence) doesn't flicker. User
prompts, errors, and status notes always stay plain.

Rendered blocks are cached per transcript entry and re-rendered only when their
text changes or the window is resized, so long conversations stay cheap to
redraw.

## Layout

```
gewu-tui
  main.rs      CLI flags + terminal event loop
  resume.rs    startup history resume (pick newest session, replay it)
  tui.rs       App state (composer, scroll) over gewu_client::session::SessionModel
  tui_view.rs  ratatui rendering
```

Conversation state lives in `gewu-client::session` and is shared with the
desktop UI, so both front-ends render events and replay history identically.
