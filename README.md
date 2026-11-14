# GeWu (格物)

> "致知在格物，物格而后知至。" -- 《大学》

GeWu (格物) is a coding-agent app written in Rust. One command starts an
agent server plus a browser chat UI: the agent plans, writes and runs code
with tools (bash, file editing, code search, web fetch/search), delegates to
subagents, and runs background workflows — streaming responses as it works
and keeping sessions in SQLite across restarts. It works with the OpenAI and
Anthropic APIs as well as OpenAI-compatible services (DeepSeek, GLM,
Moonshot, proxies, and local models).

## Install

A Rust toolchain is required (see [rustup.rs](https://rustup.rs)). From the
repository root:

```bash
cargo install --path gewu    # builds and installs the `gewu` binary
```

## Configure

GeWu needs one LLM endpoint. Either export credentials:

```bash
export OPENAI_API_KEY=sk-...     # or ANTHROPIC_API_KEY, DEEPSEEK_API_KEY,
                                 # GLM_API_KEY, MOONSHOT_API_KEY
export OPENAI_MODEL=gpt-4o
export GEWU_PROVIDER=openai      # openai | anthropic | openai-compatible |
                                 # deepseek | glm | moonshot
```

Or write a `config.jsonc` (searched in the working directory, then
`~/.gewu/`; pass a custom path with `--llm-config`):

```jsonc
{
  "endpoints": {
    "openai": {
      "base_url": "https://api.openai.com/v1",
      "api_key": "sk-...",
      "models": {
        "gpt": { "provider": "openai", "reasoning_effort": "high" }
      }
    }
  }
}
```

## Use

```bash
gewu                    # agent server + web GUI → open the printed URL
                        # (default http://127.0.0.1:8430)
gewu --no-gui           # agent server only
gewu serve --host 0.0.0.0 --token "$(openssl rand -hex 16)"
                        # headless server for remote clients; a token is
                        # required off loopback
```

### Workspaces

Sessions can start in any of several server-side directories. `cwd` is
always available; add named roots in `config.jsonc`:

```jsonc
{
  "workspaces": {
    "frontend": "../frontend",
    "wiki": "/srv/wiki"
  }
}
```

Relative paths resolve against the startup directory. In the web UI, pick a
root in the FILES panel (`ROOT`), use `＋` to browse and add any directory on
the server host at runtime, or choose a workdir when creating a session from
the left sidebar. Each session remembers its root across restarts; switching
is rejected while a turn is running.

For one-shot and interactive use from the terminal:

```bash
cargo run -p gewu-server -- run "Fix the failing tests"   # one-shot (or pipe via stdin)
cargo run -p gewu-server -- run -i                        # interactive shell
cargo run -p gewu-server -- run --list-models
cargo run -p gewu-server -- view                          # list persisted sessions
```

Agent state is stored in `~/.gewu/gewu.db` (override with `--db-path`).
