# gewu-cli

`gewu-cli` is the command-line interface for the GeWu agent framework. It provides an interactive shell for chatting with an AI agent and executing tools.

## Features

- **Interactive Shell**: Chat with an AI agent directly from your terminal.
- **Persistent Memory**: Automatically saves conversation history to a local SQLite database (`~/.gewu/gewu.db`).
- **Tool Execution**: Supports various tools (File System, Bash, Web Fetch, etc.) out-of-the-box.
- **MCP Support**: Loads and uses MCP tools configured via `--config`.
- **Customizable LLM**: Supports OpenAI-compatible endpoints via `OPENAI_BASE_URL` and `OPENAI_API_KEY`.

## Usage

### Run an Agent

Start a session with a specific agent type:

```bash
cargo run --bin gewu-cli -- --agent build
```

This will launch an interactive chat session with the `build` agent.

### Provide Initial Prompt

You can provide an initial prompt to start the conversation:

```bash
cargo run --bin gewu-cli -- --agent explore "Analyze the project structure"
```

### Configuration

- **`OPENAI_API_KEY`**: Set your API key via this environment variable.
- **`OPENAI_BASE_URL`**: Set a custom base URL for your LLM provider (e.g., `http://localhost:8080/v1`).
- **`OPENAI_MODEL`**: Set the model to use (default: `minimax-m2.5`).

Example:

```bash
export OPENAI_API_KEY="sk-..."
export OPENAI_BASE_URL="https://api.openai.com/v1"
cargo run --bin gewu-cli -- --model gpt-4o
```

### CLI Options

- `--agent <AGENT>`: Specify the agent type (default: `build`).
- `--session-id <ID>`: Resume a specific session.
- `--db-path <PATH>`: Use a custom database file.
- `--config <PATH>`: Load MCP configuration from a JSON file.
- `--list-agents`: List available agent types.
