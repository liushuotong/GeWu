# GeWu (格物)

> "致知在格物，物格而后知至。" -- 《大学》

GeWu (格物) is a Rust framework for building and deploying AI agents. The
agent is the minimal unit: a self-contained loop of LLM, prompt, memory, and
tools. Agents compose in two ways, both unified by the `Tool` interface —
dynamic delegation through a `task` tool, and deterministic orchestration
through a state graph.

## Crates

| Crate | Description |
|-------|-------------|
| **`gewu`** | The one-command app: runs the agent server and the web GUI in a single process (`gewu` → open the printed URL). Also exposes `gewu serve` and `gewu gui` for split deployments. |
| **`gewu-core`** | Core contracts and runtime: `LlmProvider`, `Tool`, `Memory`, and `Experiences` traits, the streaming agent loop with tool calling, doom loop detection, history compaction, mode reminders, and tool-output truncation. |
| **`gewu-providers`** | `LlmProvider` implementations for OpenAI-compatible APIs and Anthropic Claude, with SSE streaming, tool calling, retry/backoff, multi-modal input, and model listing. |
| **`gewu-storage`** | Persistent `Memory` implementation on SQLite via `sqlx`, scoped by session ID. Supports multiple sessions that survive process restarts, plus session list/load/delete/revert helpers. |
| **`gewu-tools`** | Built-in `Tool` implementations: filesystem (read/write/edit/apply_patch/glob/grep/list), bash, web fetch/search, code search, todo lists, task delegation, skills, and MCP (Model Context Protocol) client support. |
| **`gewu-server`** | Composition root and CLI (`run`/`serve`/`view`). Resolves config, builds providers, loads MCP servers and skills, defines agent profiles, and exposes agents over WebSocket and TCP. |
| **`gewu-protocol`** | Wire protocol shared by the server and its clients: request parsing/serialization (`ClientRequest`) and server reply types (`ServerMessage`). |
| **`gewu-client`** | Async Rust client library for the server (streaming asks, interrupt, sessions, agent switching) and the UI-agnostic conversation model (`SessionModel`) shared by the GUI. |
| **`gewu-gui`** | Browser-based chat UI served by a small axum proxy: Rust-rendered HTML/Markdown, streaming transcript, tabs, tool cards, context usage, and session resume over one HTTP/WebSocket port. |
| **`gewu-graph`** | Stateful graph execution for deterministic workflows: typed shared state, conditional routing, parallel sends, channels, streaming, and graph-to-tool adaptation (`GraphTool`). |

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                  gewu (server + GUI app)                    │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                         gewu-server                         │
│        config · profiles · MCP loading · CLI · serve        │
└─────────────────────────────────────────────────────────────┘
        │                     │                      │
        ▼                     ▼                      ▼
┌──────────────┐   ┌────────────────────┐   ┌──────────────────┐
│  gewu-gui    │   │     gewu-core      │   │    gewu-graph    │
│ (browser UI) │   │ ┌──────┐ ┌───────┐ │   │ StateGraph       │
│  axum proxy  │   │ │Agent │ │Traits │ │   │ routing/parallel │
└──────┬───────┘   │ └──────┘ └───────┘ │   │ GraphTool        │
       │           └──┬───────┬───────┬─┘   └────────┬─────────┘
       ▼              │       │       │              │
┌──────────────┐      ▼       ▼       ▼              ▼
│gewu-protocol │  ┌──────┐ ┌──────┐ ┌───────┐  (consumes gewu-core
└──────────────┘  │tools │ │LLM   │ │Memory │   tools & contracts)
     ▲            └──┬───┘ └──┬───┘ └──┬────┘
     │               │        │        │
┌────┴─────────┐     ▼        ▼        ▼
│ gewu-client  │ ┌───────┐ ┌─────────┐ ┌─────────┐
└──────────────┘ │gewu-  │ │ gewu-   │ │ gewu-   │
                 │tools  │ │providers│ │ storage │
                 │(+ MCP)│ │OpenAI/  │ │ SQLite  │
                 └───────┘ │Anthropic│ └─────────┘
                           └─────────┘
```

Arrows show compile-time dependencies. `gewu-core` owns the contracts and
depends on no other workspace crate; adapters (`gewu-providers`,
`gewu-tools`, `gewu-storage`) and orchestration (`gewu-graph`) implement or
consume those contracts; application crates (`gewu-server`, `gewu-gui`,
`gewu-client`, `gewu`) compose them. See `DEVELOPMENT.md` for the full
crate-by-crate breakdown.

## Agent Types

The server provides pre-configured agent profiles, selected with `--agent`:

| Agent | Description | Tools |
|-------|-------------|-------|
| **build** | Default agent for software engineering tasks | All tools, bash without safe mode, `task` (all agents) |
| **explore** | Fast read-only agent for exploring codebases | Read-only tools, bash restricted to safe mode |
| **plan** | Read-only planning agent for discussing implementation plans | Read-only tools, full bash (prompt-restricted to read-only), `task` (explore only); no MCP/additional tools |
| **general** | General-purpose agent for research and multi-step tasks | All tools, bash without safe mode, `task` (all agents) |

Subagents launched via `task` run as fresh `Agent` instances with their own
memory and cannot spawn further subagents (depth limit 1).

## Built-in Tools

| Tool | Name | Description |
|------|------|-------------|
| Bash | `bash` | Execute shell commands with timeout, working directory, and optional safe-mode whitelist |
| Apply Patch | `apply_patch` | Apply validated multi-file add/update/delete/move patches |
| Read File | `read_file` | Read files, list directories, handle images/PDFs as base64 |
| Write File | `write_file` | Create or overwrite files |
| Edit File | `edit_file` | Exact string replacements in files |
| Glob | `glob` | Find files by glob patterns, respecting .gitignore |
| Grep | `grep` | Search file contents with regex |
| List | `list` | List directory contents |
| Web Fetch | `webfetch` | Fetch web pages as markdown/text/html |
| Web Search | `websearch` | Search the web (Exa) |
| Code Search | `codesearch` | Semantic code search with library/API context (Exa) |
| Todo Read/Write | `todoread` / `todowrite` | Maintain the agent task list |
| Task | `task` | Launch sub-agents (build/explore/general/plan) for complex tasks; resumable via `task_id` |
| Skill | `skill` | Load skill instructions from config-declared `SKILL.md` directories into the conversation |
| MCP tools | varies | Tools loaded from configured MCP servers (stdio or SSE) |

## Getting Started

### One command: server + GUI

```bash
cargo run -p gewu                                      # server + GUI → open the printed URL
cargo run -p gewu -- --no-gui                          # agent server only
cargo run -p gewu -- --host 0.0.0.0 --token "$(openssl rand -hex 16)"  # remote, token-protected
```

### Running the CLI

```bash
cargo run -p gewu-server -- run --help
cargo run -p gewu-server -- run "Hello"                # one-shot prompt (or pipe via stdin)
cargo run -p gewu-server -- run -i                     # interactive shell
cargo run -p gewu-server -- run --agent explore --list-agents
cargo run -p gewu-server -- run --list-models
cargo run -p gewu-server -- view                       # list persisted sessions
cargo run -p gewu-server -- view <SESSION_ID>          # inspect one session
```

### Running as a server

Expose an agent so other programs can connect (WebSocket on port 8420,
newline-delimited JSON TCP on port 8421 — see `gewu-server/README.md` for the
wire protocol). Pass `--token` (or `GEWU_SERVE_TOKEN`) to require client
authentication; binding a non-loopback host without a token is refused:

```bash
cargo run -p gewu-server -- serve
cargo run -p gewu-server -- serve --host 0.0.0.0 --token "$(openssl rand -hex 16)"
```

Or open the web UI in a browser (see `gewu-gui/README.md`):

```bash
cargo run -p gewu gui                                  # same as cargo run -p gewu-gui
cargo run -p gewu-gui -- --open                        # open a browser too
cargo run -p gewu-gui -- --host 0.0.0.0 --token "$(openssl rand -hex 16)"  # remote, token-protected
```

### Embedding from Rust

```rust
use gewu_client::Client;

let client = Client::tcp("127.0.0.1", 8421).await?;
let response = client.ask("Hello").send().await?.final_response().await?;
```

Or compose an agent directly on the core contracts:

```rust
use gewu_core::Agent;
use gewu_providers::OpenAIProvider;
use gewu_tools::{BashTool, ReadFileTool};
use std::sync::Arc;

let provider = Arc::new(OpenAIProvider::new("sk-...", "gpt-4o"));
let mut agent = Agent::builder()
    .system_prompt("You are a helpful assistant.")
    .llm(provider)
    .with_tools(vec![
        Arc::new(ReadFileTool { working_directory: ".".into() }),
        Arc::new(BashTool { working_directory: ".".into(), safe_mode: true }),
    ])
    .build()?;

// agent.ask("...") streams AgentEvents (content deltas, tool calls, usage)
```

### Configuration

Configuration is loaded from `config.json` or `config.jsonc` (JSON with
comments). Search order: `--llm-config <path>` flag, then `./config.json(c)`
in the project directory, then `~/.gewu/config.json(c)`. If both `.json` and
`.jsonc` exist in the same location, `.json` wins (with a warning).

**Example `config.jsonc`:**

```jsonc
{
  // LLM settings
  "model": "coding",                         // catalog key selected by default
  "models": {
    "coding": { "provider": "openai", "model": "gpt-4o" },
    "deepseek-reasoner": { "provider": "deepseek" }
  },
  "providers": {
    "openai": { "base_url": "https://api.openai.com/v1", "api_key": "sk-..." },
    "deepseek": { "api_key": "your-deepseek-key" }
  },

  // Optional
  "log": "gewu-requests.jsonl",              // record every wire LLM request/response as JSONL
  "max_tokens": 8192,                        // Anthropic output budget (incl. thinking tokens)
  "context_limit": 131072,                   // model context window; triggers history compaction
  "sandbox": "none",                         // path confinement: "none" | "contain" | "strict"
  "skills": ["skills"],                      // directories scanned for SKILL.md
  "mcpServers": {                            // MCP servers (stdio or SSE)
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/dir"]
    },
    "remote": { "url": "https://mcp.example.com/sse" }
  }
}
```

**Configuration priority (highest to lowest):**
1. CLI arguments (`--base-url`, `--endpoint`, `--api-key`, `--model`, `--provider`, `--llm-config`)
2. Environment variables (`OPENAI_BASE_URL`, `OPENAI_API_KEY`, `OPENAI_ENDPOINT`, `OPENAI_MODEL`, `GEWU_PROVIDER`)
3. Config file
4. Provider-specific endpoint defaults (where available)

Every model entry requires `provider`; its optional `model` is the API model
ID and defaults to the catalog key. `--model` / `OPENAI_MODEL` selects a catalog
key when `models` is populated. A conflicting `--provider` is rejected. Without
a model catalog, direct CLI use requires both `--model` and `--provider`.
Top-level `provider`, `api_key`, `base_url`, `reasoning_field`, and
`require_reasoning` configuration fields are not supported.

Provider credentials may instead come from `OPENAI_API_KEY`,
`ANTHROPIC_API_KEY`, `DEEPSEEK_API_KEY`, `MOONSHOT_API_KEY`, or `GLM_API_KEY`.
Explicit `--api-key` overrides provider credentials. `glm` and
`openai-compatible` require an explicit endpoint (config or CLI).

Agent state is persisted by default in `~/.gewu/gewu.db` (override with
`--db-path`).

## Key Features

### Agent Runtime (`gewu-core`)
- **Loop-based execution**: The agent repeatedly calls the LLM and executes tools until a final response is generated
- **Doom loop detection**: Aborts turns that repeat identical tool calls
- **History compaction**: Automatically summarizes conversation history when approaching the context limit
- **Tool output truncation**: Long outputs are truncated to line/byte limits; the full content is saved to a file
- **Panic recovery**: Tool executions are sandboxed so panics do not propagate
- **Mode reminders**: Plan/build-style profiles inject reminder prompts on mode switches and turn cadence
- **Experiences**: Optional semantic recall of past problem-solving experiences via the `Experiences` trait

### LLM Providers (`gewu-providers`)
- **OpenAI-compatible**: Official API or any compatible endpoint (proxies, local models, MiniMax, deepseek, etc.)
- **Anthropic**: Claude with extended thinking support and configurable `max_tokens`
- **Multi-modal**: Image, audio, video, and file inputs where model capabilities allow (detected from the model name)
- **Streaming**: Server-Sent Events (SSE) with reasoning/content deltas

Reasoning is persisted and replayed to the same endpoint and model. Chat
Completions uses fixed provider profiles, not model-name detection or user
reasoning settings:

`DeepSeekProvider`, `GlmProvider`, and `MoonshotProvider` each implement
`LlmProvider` with fixed service policies. They share the Chat Completions
request builder, SSE parser, and HTTP retry implementation with
`OpenAIProvider`. `AnthropicProvider` handles the Messages protocol.
The configuration provider ID selects the implementation at startup; agents
continue to depend only on `Arc<dyn LlmProvider>`.

| Provider | Reasoning field | Empty field required |
| --- | --- | --- |
| `deepseek`, `moonshot` | `reasoning_content` | yes |
| `glm` | `reasoning` | yes |
| `openai`, `openai-compatible` | detected standard streamed field | no |
| `anthropic` | native signed thinking blocks | not applicable |

Anthropic replay preserves ordered thinking, text, and tool-use blocks,
including streamed signatures and redacted thinking. Incomplete thinking
without a signature produces an explicit error. Compaction replaces the
original blocks with a text checkpoint, so it does not replay signatures
against compacted context. OpenAI Responses API encrypted reasoning is not
supported by the Chat Completions transport.
- **Resilience**: Automatic retry with exponential backoff; honors `Retry-After`; transient waits surface as status events

### Memory (`gewu-storage`)
- **In-memory**: Default volatile memory for single-session use
- **SQLite persistence**: Multi-session storage that survives restarts (`~/.gewu/gewu.db`)
- **Session operations**: List, load, delete, and revert sessions; subagent sessions are tracked with parent links
- **Message types**: System, User, Assistant, and Tool messages with media content parts

### Graph Workflows (`gewu-graph`)
- **Typed state**: Shared state flows through graph nodes with partial updates and reducers (`LastValue`, `Append`, `Sum`)
- **Channels**: Last-value, ephemeral, binary-operator, barrier, and topic channels
- **Conditional routing**: Edges control flow based on node results
- **Parallel execution**: Send state to multiple nodes simultaneously
- **Streaming**: Real-time state updates during execution (`invoke` or `stream`)
- **Bridges**: `GraphTool` exposes a whole graph as one tool callable by any agent
