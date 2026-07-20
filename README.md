# GeWu (格物)

> "致知在格物，物格而后知至。" -- 《大学》

GeWu (格物) is a comprehensive framework for building and deploying AI agents. It provides a modular architecture with reusable components for core logic, memory management, provider integrations, workflow orchestration, RAG capabilities, and a suite of standard tools.

## Crates

| Crate | Description |
|-------|-------------|
| **`gewu-core`** | Core library providing the foundational abstractions for agents, tools, memory, and LLM providers. Contains the main Agent runtime with loop-based execution, tool calling, doom loop detection, and history compaction. |
| **`gewu-providers`** | LLM provider implementations supporting OpenAI-compatible APIs and Anthropic Claude. Features streaming responses, tool/function calling, and multi-modal content handling. |
| **`gewu-storage`** | Persistent memory implementation using SQLite via `sqlx`. Enables multi-session support where conversations persist across process restarts. |
| **`gewu-tools`** | Collection of built-in tools for filesystem operations, bash execution, web fetching, code search, and MCP (Model Context Protocol) integration. |
| **`gewu-cli`** | Command-line interface for interacting with GeWu agents. Supports build, explore, and general agent profiles with configurable tools and permissions. |
| **`gewu-web`** | Web UI built with Axum, providing a browser-based interface with SSE streaming for real-time agent interactions. |
| **`gewu-graph`** | Stateful graph execution library for building complex workflow orchestration with conditional routing, parallel execution, and nested subgraphs. |
| **`gewu-rag`** | RAG (Retrieval Augmented Generation) system with document processing (PDF, text), embedding generation, and vector storage integration (Qdrant). |
| **`gewu-experience`** | Experience memory system with Qdrant vector embeddings for semantic recall of past problem-solving experiences. Includes experience store and recall tools. |
| **`gewu-evolve`** | Self-evolution capabilities including self-critique, Python sandbox execution, skill library for tool persistence, and LLM-based tool generation. |

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                         gewu-cli                            │
│                         gewu-web                            │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                        gewu-core                            │
│  ┌─────────┐  ┌──────────┐  ┌─────────┐  ┌─────────────┐    │
│  │ Agent   │  │  Tool    │  │  LLM    │  │   Memory    │    │
│  │ Runtime │  │  Trait   │  │Provider │  │   Trait     │    │
│  └─────────┘  └──────────┘  └─────────┘  └─────────────┘    │
└─────────────────────────────────────────────────────────────┘
         │              │            │              │
         ▼              ▼            ▼              ▼
┌────────────┐  ┌──────────┐  ┌────────────┐  ┌────────────┐
│gewu-tools  │  │   MCP    │  │   OpenAI   │  │   SQLite   │
│            │  │ Protocol │  │ Anthropic  │  │  Storage   │
│ - bash     │  │          │  │            │  │            │
│ - fs ops   │  └──────────┘  └────────────┘  └────────────┘
│ - grep     │
│ - search   │
│ - batch    │
└────────────┘

┌────────────┐  ┌────────────┐  ┌─────────────────┐  ┌────────────────┐
│ gewu-graph │  │  gewu-rag  │  │gewu-experience  │  │  gewu-evolve  │
│            │  │            │  │                 │  │               │
│ StateGraph │  │  Document  │  │  Experience    │  │ Self-critique │
│ Workflows  │  │  Processing│  │  Store with    │  │ Python sandbox│
│            │  │  Embedding │  │  Qdrant        │  │ Skill library │
│            │  │  Retrieval │  │  embeddings    │  │ Tool builder  │
└────────────┘  └────────────┘  └─────────────────┘  └────────────────┘
```

## Agent Types

The CLI provides pre-configured agent types optimized for different use cases:

| Agent | Description | Safe Mode | Tools |
|-------|-------------|-----------|-------|
| **build** | Default agent for software engineering tasks | Off | All tools including bash, file edit, task management |
| **explore** | Fast agent for exploring codebases | On | Read-only tools, restricted bash |
| **general** | General-purpose agent for research and multi-step tasks | Off | All tools including task management |

## Built-in Tools

| Tool | Name | Description |
|------|------|-------------|
| Bash | `bash` | Execute shell commands with timeout and safe mode |
| Read File | `read_file` | Read files, list directories, handle images/PDFs as base64 |
| Write File | `write_file` | Create or overwrite files |
| Edit File | `edit_file` | Edit files by replacing text with smart matching |
| Glob | `glob` | Find files by glob patterns respecting .gitignore |
| Grep | `grep` | Search file contents with regex support |
| List | `list` | List directory contents |
| Web Fetch | `webfetch` | Fetch and parse web pages to markdown |
| Web Search | `websearch` | Search the web |
| Code Search | `codesearch` | Search code using semantic search |
| Todo Read | `todoread` | Read the task list |
| Todo Write | `todowrite` | Write/update the task list |
| Batch | `batch` | Execute multiple tools in parallel |

## Getting Started

### Running the CLI

```bash
cargo run --bin gewu-cli -- --help
```

### Running the Web UI

```bash
cargo run --bin gewu-web -- --port 8080
```

### Configuration

The CLI and Web UI use a JSON config file. By default, they look for `config.json` in the project root, or `~/.gewu/config.json`.

**Config file location priority:**
1. `--config <path>` CLI argument
2. `config.json` in project root (if exists)
3. `~/.gewu/config.json` (if exists)

**Example `config.json`:**
```json
{
  "endpoint": "https://api.openai.com/v1",
  "api_key": "sk-...",
  "model": "gpt-4",
  "mcp_servers": []
}
```

**Configuration priority (highest to lowest):**
1. CLI arguments (`--endpoint`, `--api-key`, `--model`, `--config`)
2. Environment variables (`OPENAI_ENDPOINT`, `OPENAI_API_KEY`, `OPENAI_MODEL`)
3. Config file
4. Defaults (none - all values must be provided)

## Key Features

### Agent Runtime
- **Loop-based execution**: Agent repeatedly calls LLM and executes tools until a final response is generated
- **Doom loop detection**: Prevents infinite loops by detecting repeated identical tool calls
- **History compaction**: Automatically summarizes conversation history when approaching context limits
- **Panic recovery**: Tool executions are sandboxed to prevent crashes from propagating

### LLM Providers
- **OpenAI**: GPT-4, GPT-4-turbo, GPT-3.5-turbo with automatic capability detection
- **Anthropic**: Claude with extended thinking support
- **Multi-modal**: Support for images, audio, video, and PDF inputs where model capabilities allow
- **Streaming**: Server-Sent Events (SSE) for real-time response streaming

### Memory
- **In-memory**: Default volatile memory for single-session use
- **SQLite persistence**: Optional persistent storage for multi-session support
- **Message types**: System, User, Assistant, and Tool messages with media support

### Graph Workflows
- **State management**: Shared state flows through graph nodes with partial updates
- **Conditional routing**: Edges control flow based on node results
- **Parallel execution**: Send state to multiple nodes simultaneously
- **Streaming**: Real-time state updates during execution
