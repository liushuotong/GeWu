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
//! use serde_json::json;
//!
//! struct MyTool {
//!     working_directory: PathBuf,
//! }
//!
//! #[async_trait]
//! impl Tool for MyTool {
//!     fn name(&self) -> &str {
//!         "my_tool"
//!     }
//!
//!     fn description(&self) -> &str {
//!         "A description of what my tool does"
//!     }
//!
//!     fn parameters_schema(&self) -> Value {
//!         json!({
//!             "type": "object",
//!             "properties": {
//!                 "input": {
//!                     "type": "string",
//!                     "description": "The input to my tool"
//!                 }
//!             },
//!             "required": ["input"]
//!         })
//!     }
//!
//!     async fn call(&self, args: &str) -> Result<String, Box<dyn Error + Send + Sync>> {
//!         // Parse args and execute
//!         Ok("result".to_string())
//!     }
//! }
//! ```

use crate::types::{Message, ProviderEvent};
use async_trait::async_trait;
use futures::stream::BoxStream;
use serde_json::Value;
use std::error::Error;
use std::sync::Arc;

/// A tool that can be called by the agent.
///
/// Tools are the primary way for the agent to interact with the outside world.
/// Each tool has a name, description (used for LLM tool selection), a JSON schema
/// for its parameters, and an execution function.
///
/// The tool implementation should be thread-safe (`Send + Sync`) to allow
/// concurrent execution.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Returns the name of this tool.
    ///
    /// This name is used by the LLM to request tool execution and must be
    /// unique among all registered tools.
    fn name(&self) -> &str;

    /// Returns a description of this tool.
    ///
    /// The description should explain what the tool does and is used by the
    /// LLM to decide when to call this tool. Be specific about parameters
    /// and their purpose.
    fn description(&self) -> &str;

    /// Returns the JSON schema for this tool's parameters.
    ///
    /// The schema should follow the [JSON Schema](https://json-schema.org/)
    /// specification and describe all required and optional parameters.
    fn parameters_schema(&self) -> Value;

    /// Executes the tool with the given arguments.
    ///
    /// # Arguments
    ///
    /// * `args` - A JSON string containing the tool arguments, matching the schema.
    ///
    /// # Returns
    ///
    /// Returns a string result on success, or an error on failure.
    /// The result string can be any format but is typically plain text or
    /// a JSON object with structured data.
    async fn call(&self, args: &str) -> Result<String, Box<dyn Error + Send + Sync>>;
}

/// A registry of tools that can be executed by name.
///
/// This trait provides a way to look up and execute tools by their name.
/// It is implemented by the [`Agent`](super::agent::Agent) struct and can
/// be used by external code to execute tools programmatically.
#[async_trait]
pub trait ToolRegistry: Send + Sync {
    /// Executes a tool by name with the given arguments.
    ///
    /// # Arguments
    ///
    /// * `name` - The name of the tool to execute.
    /// * `args` - A JSON string containing the tool arguments.
    ///
    /// # Returns
    ///
    /// Returns the tool's result string, or an error if the tool was not
    /// found or execution failed.
    async fn execute(
        &self,
        name: &str,
        args: &str,
    ) -> Result<String, Box<dyn Error + Send + Sync>>;

    /// Returns a list of all available tool names.
    fn list_tools(&self) -> Vec<String>;
}

/// A provider for Large Language Model interactions.
///
/// LlmProvider is the abstraction that allows GeWu to work with any LLM
/// that provides a chat completions-style API. The trait uses async streaming
/// to handle the potentially large responses from LLMs.
///
/// Implementations are responsible for:
/// - Converting [`Message`] types to the provider's native format
/// - Converting tools to the provider's tool definition format
/// - Parsing the provider's streaming responses into [`ProviderEvent`] types
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Generates a streaming response from the LLM.
    ///
    /// # Arguments
    ///
    /// * `messages` - The conversation history to generate a response from.
    /// * `tools` - The available tools the LLM can call.
    ///
    /// # Returns
    ///
    /// A stream of [`ProviderEvent`] objects representing the LLM's response.
    /// The stream may emit events incrementally as the LLM generates content.
    async fn stream_response<'a>(
        &'a self,
        messages: &[Message],
        tools: &[Arc<dyn Tool>],
    ) -> Result<
        BoxStream<'a, Result<ProviderEvent, Box<dyn Error + Send + Sync>>>,
        Box<dyn Error + Send + Sync>,
    >;

    /// Lists all available models from this provider.
    ///
    /// # Returns
    ///
    /// A list of model identifiers that can be used with this provider.
    async fn list_models(&self) -> Result<Vec<String>, Box<dyn Error + Send + Sync>>;
}

/// A storage backend for conversation history.
///
/// Memory implementations provide persistence and retrieval of conversation
/// messages. GeWu provides a simple in-memory implementation ([`Vec<Message>`])
/// for single-session use, and [`SqliteMemory`](super::super::storage::SqliteMemory)
/// for persistent cross-session storage.
///
/// The trait uses `&mut self` for methods that modify state, allowing for
/// interior mutability patterns if needed.
#[async_trait]
pub trait Memory: Send + Sync {
    /// Returns a reference to the current messages.
    fn messages(&self) -> &[Message];

    /// Adds a new message to the conversation history.
    ///
    /// Implementations should append the message and persist it if applicable.
    async fn add_message(&mut self, message: Message);

    /// Clears all messages from the conversation history.
    async fn clear(&mut self);

    /// Returns a mutable reference to the messages.
    ///
    /// This is used by the compaction logic to analyze and prune history.
    fn messages_mut(&mut self) -> &mut [Message];

    /// Replaces all messages with a new set of messages.
    ///
    /// This is used after compaction to restore a summarized history.
    async fn replace(&mut self, messages: Vec<Message>);
}

/// In-memory implementation of [`Memory`] using a simple vector.
///
/// This is the default memory implementation used when no other memory
/// backend is configured. It stores all messages in memory and is lost
/// when the process exits.
#[async_trait]
impl Memory for Vec<Message> {
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