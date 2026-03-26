# gewu-providers

`gewu-providers` contains implementations of the `LlmProvider` trait for `gewu-core`. It enables the agent to communicate with various LLM backends.

## Features

- **OpenAI Compatible**: Includes `OpenAIProvider` which supports OpenAI-compatible APIs (including Minimax, deepseek-coder, etc.).

## Usage

### OpenAIProvider

To use the OpenAI provider, instantiate `OpenAIProvider` with your API key and desired model.

```rust
use gewu_providers::OpenAIProvider;
use gewu_core::LlmProvider;
use std::sync::Arc;

let api_key = "your-api-key";
let model = "gpt-4o";

// Basic initialization
let provider = Arc::new(OpenAIProvider::new(api_key, model));

// With custom base URL (e.g., for local models or alternative APIs)
let base_url = "http://localhost:8080/v1";
let provider_custom = Arc::new(OpenAIProvider::new_with_endpoint(api_key, base_url, model));
```

Then pass this provider to your agent builder:

```rust
use gewu_core::Agent;

let agent = Agent::builder()
    .llm(provider)
    // ...
    .build()?;
```
