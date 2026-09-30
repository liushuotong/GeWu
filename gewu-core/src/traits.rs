//! Core traits for the GeWu agent framework.
//!
//! This module defines the fundamental traits that must be implemented to
//! extend GeWu with custom tools, LLM providers, and memory backends:
//!
//! - [`Tool`] - Implement to create custom tools that the agent can call
//! - [`LlmProvider`] - Implement to add support for different LLM backends
//! - [`ToolRegistry`] - Implement to provide a collection of tools to the agent
//! - [`Memory`] - Implement to add custom conversation history storage
//!
//! # Example: Implementing a Custom Tool
//!
//! ```ignore
//! use async_trait::async_trait;
//! use gewu_core::traits::Tool;
//! use gewu_core::types::{ToolResult, ToolContext};
//! use schemars::JsonSchema;
//! use serde::Deserialize;
//!
//! #[derive(JsonSchema, Deserialize)]
//! struct MyParams {
//!     input: String,
//! }
//!
//! struct MyTool;
//!
//! #[async_trait]
//! impl Tool for MyTool {
//!     fn name(&self) -> &str { "my_tool" }
//!
//!     fn description(&self) -> &str { "A description of what my tool does" }
//!
//!     fn schema() -> schemars::schema::RootSchema {
//!         schemars::schema_for!(MyParams)
//!     }
//!
//!     async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult, Box<dyn Error + Send + Sync>> {
//!         let params: MyParams = serde_json::from_value(args)?;
//!         Ok(ToolResult::new("my_tool", format!("result: {}", params.input)))
//!     }
//! }
//! ```

use crate::types::{ExperienceQuery, Message, ProviderEvent, RecalledExperience, ToolContext, ToolResult};
use async_trait::async_trait;
use chrono::Utc;
use futures::stream::BoxStream;
use schemars::schema::RootSchema;
use std::error::Error;
use std::io::Write;
use std::sync::{Arc, Mutex};
use tracing::warn;

/// Defines a tool that the agent can call during execution.
///
/// The `Tool` trait is the core extension point for GeWu. Implement this trait
/// to create custom tools that the agent can invoke via the LLM.
///
/// # Example
///
/// ```
/// use async_trait::async_trait;
/// use std::error::Error;
/// use gewu_core::traits::Tool;
/// use gewu_core::types::{ToolResult, ToolContext};
/// use schemars::schema::RootSchema;
/// use schemars::JsonSchema;
/// use serde::Deserialize;
///
/// #[derive(JsonSchema, Deserialize)]
/// struct MyParams {
///     input: String,
/// }
///
/// struct MyTool;
///
/// #[async_trait]
/// impl Tool for MyTool {
///     fn name(&self) -> &str { "my_tool" }
///     fn description(&self) -> &str { "A description of what my tool does" }
///     fn schema(&self) -> RootSchema { schemars::schema_for!(MyParams) }
///     async fn execute(&self, args: serde_json::Value, _ctx: &ToolContext) -> Result<ToolResult, Box<dyn Error + Send + Sync>> {
///         let params: MyParams = serde_json::from_value(args)?;
///         Ok(ToolResult::new("my_tool", format!("result: {}", params.input)))
///     }
/// }
/// ```
#[async_trait]
pub trait Tool: Send + Sync {
    /// Returns the unique name of this tool.
    ///
    /// This name is used by the LLM to request this tool and must be
    /// unique among all tools registered with the agent.
    fn name(&self) -> &str;

    /// Returns a description of what this tool does.
    ///
    /// The description is passed to the LLM so it can decide when to
    /// call this tool based on user requests.
    fn description(&self) -> &str;

    /// Returns the JSON schema for this tool's parameters.
    ///
    /// The schema is used by the LLM to generate valid arguments
    /// when calling this tool.
    fn schema(&self) -> RootSchema;

    /// Initializes the tool (optional).
    ///
    /// Called once before the tool is first used. Override to perform
    /// setup such as opening connections or loading resources.
    async fn init(&self) -> Result<(), Box<dyn Error + Send + Sync>> {
        Ok(())
    }

    /// Executes the tool with the given arguments.
    ///
    /// # Arguments
    ///
    /// * `args` - JSON value containing the tool arguments (validated against the schema)
    /// * `ctx` - Execution context containing session ID, message ID, and abort signal
    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, Box<dyn Error + Send + Sync>>;
}

/// Provides access to a collection of tools for execution.
///
/// The `ToolRegistry` trait is implemented by types that manage multiple tools,
/// such as the `Agent` itself. This abstraction allows tools to be loaded
/// dynamically and executed through a unified interface.
#[async_trait]
pub trait ToolRegistry: Send + Sync {
    /// Executes a tool by name with the given arguments.
    ///
    /// # Arguments
    ///
    /// * `name` - The name of the tool to execute
    /// * `args` - JSON arguments to pass to the tool
    /// * `ctx` - Execution context
    async fn execute(
        &self,
        name: &str,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, Box<dyn Error + Send + Sync>>;

    /// Returns a list of all available tool names.
    fn list_tools(&self) -> Vec<String>;
}

/// Shareable handle to a request-log writer.
///
/// The writer is wrapped in `Arc<Mutex<...>>` so `LlmProvider::stream_response`
/// can take `&self` and one handle can be shared across agents.
pub type RequestLog = Arc<Mutex<Box<dyn Write + Send + Sync>>>;

/// Records one wire request as a JSONL line. Generic across providers.
///
/// Best-effort: write failures emit a warning and never fail the request.
fn log_wire_request(log: &RequestLog, request: &serde_json::Value) {
    let entry = serde_json::json!({
        "timestamp": Utc::now().to_rfc3339(),
        "request": request,
    });
    if let Ok(mut writer) = log.lock()
        && let Err(e) = writeln!(writer, "{entry}").and_then(|()| writer.flush())
    {
        warn!("Failed to write LLM request log: {}", e);
    }
}

/// Provides access to a language model for generating responses.
///
/// The `LlmProvider` trait abstracts over different LLM backends (OpenAI, Anthropic, etc.).
/// Implement this trait to add support for new LLM providers.
///
/// Message building and message sending are isolated: `build` converts core
/// messages and tools into the provider's wire request, and `send` delivers it.
/// `stream_response` is generic across providers: it builds the request, logs
/// it when a request log is supplied, then sends it.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Initializes the provider (optional).
    ///
    /// Called once before the provider is first used. Override to perform
    /// setup such as validating credentials or checking API availability.
    async fn init(&mut self) -> Result<(), Box<dyn Error + Send + Sync>>
    where
        Self: Sized,
    {
        Ok(())
    }

    /// Builds the provider-specific wire request from core messages and tools.
    ///
    /// The returned JSON value is the exact request body sent to the
    /// provider's API, and what gets recorded when a request log is supplied.
    fn build(
        &self,
        messages: &[Message],
        tools: &[Arc<dyn Tool>],
    ) -> Result<serde_json::Value, Box<dyn Error + Send + Sync>>;

    /// Sends a built wire request and streams the provider's response.
    async fn send<'a>(
        &'a self,
        request: serde_json::Value,
    ) -> Result<
        BoxStream<'a, Result<ProviderEvent, Box<dyn Error + Send + Sync>>>,
        Box<dyn Error + Send + Sync>,
    >;

    /// Streams a response from the LLM: build -> log -> send.
    ///
    /// # Arguments
    ///
    /// * `messages` - The conversation history to send to the LLM
    /// * `tools` - Available tools the LLM can call
    /// * `log` - Optional request log; when supplied, the built wire request
    ///   is recorded as one JSONL line before sending
    ///
    /// # Returns
    ///
    /// A stream of provider events representing the LLM's response
    async fn stream_response<'a>(
        &'a self,
        messages: &[Message],
        tools: &[Arc<dyn Tool>],
        log: Option<&RequestLog>,
    ) -> Result<
        BoxStream<'a, Result<ProviderEvent, Box<dyn Error + Send + Sync>>>,
        Box<dyn Error + Send + Sync>,
    > {
        let request = self.build(messages, tools)?;
        if let Some(log) = log {
            log_wire_request(log, &request);
        }
        self.send(request).await
    }

    /// Lists available models for this provider.
    async fn list_models(&self) -> Result<Vec<String>, Box<dyn Error + Send + Sync>>;
}

/// Stores and manages conversation history.
///
/// The `Memory` trait abstracts over different storage backends for
/// conversation history. Implement this trait to add custom storage
/// solutions such as databases or persistent storage.
#[async_trait]
pub trait Memory: Send + Sync {
    /// Initializes the memory store (optional).
    async fn init(&mut self) -> Result<(), Box<dyn Error + Send + Sync>>
    where
        Self: Sized,
    {
        Ok(())
    }

    /// Returns a slice of all messages in the conversation history.
    fn messages(&self) -> &[Message];

    /// Adds a message to the conversation history.
    async fn add_message(&mut self, message: Message);

    /// Clears all messages from the conversation history.
    async fn clear(&mut self);

    /// Returns a mutable slice of all messages.
    fn messages_mut(&mut self) -> &mut [Message];

    /// Replaces all messages with a new set of messages.
    async fn replace(&mut self, messages: Vec<Message>);
}

/// In-memory implementation of `Memory` using a `Vec<Message>`.
#[async_trait]
impl Memory for Vec<Message> {
    async fn init(&mut self) -> Result<(), Box<dyn Error + Send + Sync>>
    where
        Self: Sized,
    {
        Ok(())
    }

    fn messages(&self) -> &[Message] {
        self
    }

    async fn add_message(&mut self, message: Message) {
        self.push(message);
    }

    async fn clear(&mut self) {
        self.clear();
    }

    fn messages_mut(&mut self) -> &mut [Message] {
        self.as_mut_slice()
    }

    async fn replace(&mut self, messages: Vec<Message>) {
        *self = messages;
    }
}

/// Abstraction for storing and retrieving experiences.
///
/// The `Experiences` trait provides a simple interface for recording
/// and retrieving past experiences to inform future problem-solving.
///
/// When used with an [`Agent`] via
/// [`AgentBuilder::with_experiences`](crate::agent::AgentBuilder::with_experiences),
/// a `search_experiences` tool is automatically registered, allowing the LLM
/// to recall relevant past experiences during problem-solving.
///
/// [`Agent`]: crate::agent::Agent
#[async_trait]
pub trait Experiences: Send + Sync {
    /// Stores a new experience and returns its unique ID.
    async fn store(&self, content: &str) -> Result<String, Box<dyn Error + Send + Sync>>;

    /// Recalls experiences similar to the query, sorted by similarity (highest first).
    async fn recall(
        &self,
        query: &ExperienceQuery,
    ) -> Result<Vec<RecalledExperience>, Box<dyn Error + Send + Sync>>;
}
