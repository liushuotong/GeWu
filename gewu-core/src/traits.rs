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

use crate::types::{Message, ProviderEvent, ToolContext, ToolResult};
use async_trait::async_trait;
use futures::stream::BoxStream;
use schemars::schema::RootSchema;
use std::error::Error;
use std::sync::Arc;

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;

    fn description(&self) -> &str;

    fn schema(&self) -> RootSchema;

    async fn init(&self) -> Result<(), Box<dyn Error + Send + Sync>> {
        Ok(())
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, Box<dyn Error + Send + Sync>>;
}

#[async_trait]
pub trait ToolRegistry: Send + Sync {
    async fn execute(
        &self,
        name: &str,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, Box<dyn Error + Send + Sync>>;

    fn list_tools(&self) -> Vec<String>;
}

#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn init(&mut self) -> Result<(), Box<dyn Error + Send + Sync>>
    where
        Self: Sized,
    {
        Ok(())
    }

    async fn stream_response<'a>(
        &'a self,
        messages: &[Message],
        tools: &[Arc<dyn Tool>],
    ) -> Result<
        BoxStream<'a, Result<ProviderEvent, Box<dyn Error + Send + Sync>>>,
        Box<dyn Error + Send + Sync>,
    >;

    async fn list_models(&self) -> Result<Vec<String>, Box<dyn Error + Send + Sync>>;
}

#[async_trait]
pub trait Memory: Send + Sync {
    async fn init(&mut self) -> Result<(), Box<dyn Error + Send + Sync>>
    where
        Self: Sized,
    {
        Ok(())
    }

    fn messages(&self) -> &[Message];

    async fn add_message(&mut self, message: Message);

    async fn clear(&mut self);

    fn messages_mut(&mut self) -> &mut [Message];

    async fn replace(&mut self, messages: Vec<Message>);
}

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
