# gewu-core

`gewu-core` is the foundational library for the GeWu agent framework. It provides the core abstractions for building LLM agents, managing conversation memory, and executing tools.

## Key Concepts

### Agent

The `Agent` struct is the main entry point. It manages the conversation loop, interacts with the LLM provider, executes tools, and handles memory persistence.

```rust
use gewu_core::{Agent, AgentBuilder};

let agent = Agent::builder()
    .system_prompt("You are a helpful assistant.")
    .llm(my_llm_provider)
    .memory(my_memory)
    .build()?;
```

### Traits

- **`LlmProvider`**: Defines the interface for LLM backends. Implement this trait to support different model providers (e.g., OpenAI, Anthropic, local models).
- **`Tool`**: Defines the interface for tools that the agent can use. Tools must provide a name, description, parameter schema (JSON Schema), and an execution method.
- **`Memory`**: Defines the interface for storing conversation history. The default implementation uses an in-memory `Vec<Message>`, but persistent implementations (like `gewu-storage`) can be used.

### Memory Management

`gewu-core` includes a sophisticated memory management system that automatically handles context window limits. When the conversation history exceeds the configured token limit, the agent automatically summarizes the conversation to free up space while retaining critical context.

## Usage

Add `gewu-core` to your `Cargo.toml`:

```toml
[dependencies]
gewu-core = { path = "../gewu-core" } # or version
```

## Example

See `examples/simple_agent.rs` for a complete example of how to build and run an agent.
