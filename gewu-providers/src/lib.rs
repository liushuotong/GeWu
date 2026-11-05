//! LLM provider implementations for GeWu.
//!
//! This crate provides `LlmProvider` trait implementations (from `gewu_core`)
//! for various LLM APIs, enabling GeWu to communicate with different AI providers
//! through a unified interface.
//!
//! # Supported Providers
//!
//! - **OpenAI** - OpenAI's official API and OpenAI-compatible endpoints
//! - **Anthropic** - Anthropic's Claude API
//!
//! # Example
//!
//! ```ignore
//! use gewu_providers::{OpenAIProvider, AnthropicProvider};
//!
//! // OpenAI provider
//! let openai = OpenAIProvider::new("api-key", "gpt-4");
//!
//! // Anthropic provider
//! let anthropic = AnthropicProvider::new("api-key", "claude-3-5-sonnet");
//! ```

mod anthropic;
mod http;
mod openai;
mod providers;

pub use providers::{AnthropicProvider, OpenAIProvider};
