# gewu-cli

`gewu-cli` is the command-line interface for the GeWu agent framework. It provides an interactive shell for chatting with an AI agent and executing tools.

## Features

- **Interactive Shell**: Chat with an AI agent directly from your terminal.
- **Persistent Memory**: Automatically saves conversation history to a local SQLite database (`~/.gewu/gewu.db`).
- **Tool Execution**: Supports various tools (File System, Bash, Web Fetch, etc.) out-of-the-box.
- **MCP Support**: Loads MCP tools from the unified config file.
- **Customizable LLM**: Supports OpenAI-compatible endpoints via `OPENAI_BASE_URL` and `OPENAI_API_KEY`.

## Usage

### Run an Agent

Start a session with a specific agent type:

```bash
cargo run -p gewu-cli -- run --agent build --interactive
```

This will launch an interactive chat session with the `build` agent.

### Provide Initial Prompt

You can provide an initial prompt to start the conversation:

```bash
cargo run -p gewu-cli -- run --agent explore "Analyze the project structure"
```

### View Persistent Memory

List stored sessions without initializing an LLM provider:

```bash
cargo run -p gewu-cli -- view
```

Display one session's messages:

```bash
cargo run -p gewu-cli -- view <SESSION_ID>
```

Use `--db-path` with either subcommand to select another database:

```bash
cargo run -p gewu-cli -- view <SESSION_ID> --db-path /path/to/gewu.db
```

### Configuration

Configuration is loaded from `config.json` or `config.jsonc` (with comments support). The CLI searches for config in this order:
1. `./config.json` or `./config.jsonc` (project directory)
2. `~/.gewu/config.json` or `~/.gewu/config.jsonc` (home directory)

If both `.json` and `.jsonc` exist in the same location, `.json` takes precedence and a warning is shown.

**Config options:**

| Option | Description |
|--------|-------------|
| `base_url` | Custom provider base URL (e.g., `http://localhost:8080/v1`) |
| `api_key` | API key for the LLM provider |
| `model` | Model to use (default: `gpt-5.3-codex`) |
| `provider` | Provider protocol: `openai` or `anthropic` |
| `mcpServers` | MCP server configurations (see below) |

**Environment variables** (take precedence over config file):
- `OPENAI_API_KEY`
- `OPENAI_BASE_URL`
- `OPENAI_MODEL`

**Example `config.json`:**

```jsonc
{
  // LLM settings
  "base_url": "http://localhost:8080/v1",
  "api_key": "sk-...",
  "model": "gpt-4o",
  "provider": "openai",

  // MCP servers
  "mcpServers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/dir"]
    }
  }
}
```

### Run Options

- `--llm-config <PATH>`: Load configuration from a specific file (overrides default search path).
- `--list-agents`: List available agent types.
- `--list-models`: List available models from the configured provider.
- `--session-id <ID>`: Resume a specific session.
- `--db-path <PATH>`: Use a custom database file.

Show command-specific options with `gewu-cli run --help` or `gewu-cli view --help`.
