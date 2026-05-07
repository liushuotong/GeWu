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
//! # Architecture
//!
//! The crate is organized into:
//!
//! - [`openai`] - OpenAI API types, HTTP client, and SSE parsing
//! - [`anthropic`] - Anthropic API types, HTTP client, and SSE parsing
//! - [`providers`] - Provider implementations and traits
//! - [`sse`] - Shared SSE parsing utilities
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

pub mod anthropic;
pub mod providers;
pub mod openai;
pub mod sse;

pub use anthropic::{AnthropicError, Client as AnthropicClient, AnthropicConfig};
pub use providers::{AnthropicProvider, OpenAIProvider, RequestBuilder, ResponseParser};
pub use openai::{OpenAIClient, OpenAIConfig, OpenAIError};
