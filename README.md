# GeWu (格物)

> "致知在格物，物格而后知至。"  -- 《大学》

GeWu (格物) is a comprehensive framework for building and deploying AI agents. It provides a modular architecture with reusable components for core logic, memory management, provider integrations, and a suite of standard tools.

## Crates

- **`gewu-core`**: The core library providing the foundational abstractions for agents, tools, memory, and LLM providers.
- **`gewu-providers`**: Implementations of LLM providers (e.g., OpenAI, Minimax) for `gewu-core`.
- **`gewu-storage`**: Persistent memory implementation using SQLite via `sqlx`.
- **`gewu-tools`**: A collection of standard tools (file system, web fetch, search, bash) and MCP (Model Context Protocol) support.
- **`gewu-cli`**: A command-line interface for interacting with GeWu agents.

## Getting Started

To use GeWu, add the necessary crates to your project:

```toml
[dependencies]
gewu-core = "0.1.0"
gewu-providers = "0.1.0"
gewu-tools = "0.1.0"
```

### Running the CLI

You can interact with GeWu agents directly using the CLI:

```bash
cargo run --bin gewu-cli -- --help
```