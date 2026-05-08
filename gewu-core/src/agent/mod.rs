//! Agent implementation for the GeWu framework.
//!
//! The [`Agent`] is the core runtime that orchestrates conversation flow,
//! tool execution, and context management. It follows a loop-based model
//! where it processes user input, calls the LLM, handles tool executions,
//! and continues until a final response is generated.
//!
//! # Agent Loop
//!
//! The [`ask`](Agent::ask) method implements the main agent loop:
//!
//! 1. **System prompt setup**: If no conversation history exists, add system prompt
//! 2. **Add user message**: Add the user's input to history
//! 3. **Compaction check**: If history exceeds token limit, summarize older messages
//! 4. **LLM request**: Send conversation to LLM with available tools
//! 5. **Stream response**: Yield content/reasoning deltas as they arrive
//! 6. **Tool execution**: For each tool call, execute the tool and add result to history
//! 7. **Repeat**: Continue from step 3 until LLM produces a final response
//!
//! # Doom Loop Detection
//!
//! To prevent infinite loops of identical tool calls, the agent tracks the
//! last 3 tool calls. If the same tool is called 3 times in a row with
//! identical arguments, execution is stopped with an error.
//!
//! # Example
//!
//! ```ignore
//! use gewu_core::Agent;
//! use gewu_providers::OpenAIProvider;
//!
//! let provider = OpenAIProvider::new(&api_key, "gpt-4");
//! let agent = Agent::builder()
//!     .llm(Arc::new(provider))
//!     .with_tools(vec![...])
//!     .system_prompt("You are a helpful assistant.")
//!     .build()?;
//! ```

mod compaction;
mod execution;
mod media;
mod tools;

pub use compaction::CompactionError;
pub use tools::execute_tool_with_panic_recovery;

use crate::middleware::TruncationService;
use crate::traits::{LlmProvider, Memory, Tool, ToolRegistry};
use crate::types::{Message, ToolContext, ToolResult};
use anyhow::{Context, Result};
use async_trait::async_trait;
use std::error::Error;
use std::sync::Arc;
use uuid::Uuid;

pub struct Agent {
    llm: Arc<dyn LlmProvider>,
    tools: Arc<Vec<Arc<dyn Tool>>>,
    system_prompt: String,
    max_turns: Option<usize>,
    pub memory: Box<dyn Memory>,
    pub context_limit: usize,
    truncation: TruncationService,
    session_id: String,
}

pub struct AgentBuilder {
    llm: Option<Arc<dyn LlmProvider>>,
    tools: Arc<Vec<Arc<dyn Tool>>>,
    system_prompt: String,
    max_turns: Option<usize>,
    memory: Option<Box<dyn Memory>>,
    context_limit: usize,
}

pub fn extract_panic_message(panic_payload: Box<dyn Any + Send>) -> String {
    if let Some(s) = panic_payload.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = panic_payload.downcast_ref::<String>() {
        s.clone()
    } else if let Some(e) = panic_payload.downcast_ref::<Box<dyn Error + Send + 'static>>() {
        e.to_string()
    } else {
        "Unknown panic cause".to_string()
    }
}

use std::any::Any;

impl Default for AgentBuilder {
    fn default() -> Self {
        AgentBuilder {
            llm: None,
            tools: Arc::new(Vec::new()),
            system_prompt: String::new(),
            max_turns: None,
            memory: None,
            context_limit: 128_000,
        }
    }
}

impl Agent {
    pub fn builder() -> AgentBuilder {
        AgentBuilder::default()
    }

    pub async fn reset(&mut self) {
        self.memory.clear().await;
        if !self.system_prompt.is_empty() {
            self.memory
                .add_message(Message::System {
                    content: self.system_prompt.clone(),
                })
                .await;
        }
    }
}

#[async_trait]
impl ToolRegistry for Agent {
    async fn execute(
        &self,
        name: &str,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, Box<dyn Error + Send + Sync>> {
        let tool = self.tools.iter().find(|t| t.name() == name);
        match tool {
            Some(t) => t.execute(args, ctx).await,
            None => Err(format!("Tool not found: {}", name).into()),
        }
    }

    fn list_tools(&self) -> Vec<String> {
        self.tools.iter().map(|t| t.name().to_string()).collect()
    }
}

impl AgentBuilder {
    pub fn llm(mut self, llm: Arc<dyn LlmProvider>) -> Self {
        self.llm = Some(llm);
        self
    }

    pub fn with_tools(mut self, tools: Vec<Arc<dyn Tool>>) -> Self {
        self.tools = Arc::new(tools);
        self
    }

    pub fn system_prompt(mut self, prompt: &str) -> Self {
        self.system_prompt = prompt.to_string();
        self
    }

    pub fn max_turns(mut self, max_turns: Option<usize>) -> Self {
        self.max_turns = max_turns;
        self
    }

    pub fn context_limit(mut self, limit: usize) -> Self {
        self.context_limit = limit;
        self
    }

    pub fn memory(mut self, memory: Box<dyn Memory>) -> Self {
        self.memory = Some(memory);
        self
    }

    pub fn build(self) -> Result<Agent> {
        let llm = self.llm.context("LLM provider is required")?;
        let agent = Agent {
            llm,
            tools: self.tools,
            system_prompt: self.system_prompt.clone(),
            max_turns: self.max_turns,
            memory: self.memory.unwrap_or_else(|| Box::new(Vec::new())),
            context_limit: self.context_limit,
            truncation: TruncationService::new(),
            session_id: Uuid::new_v4().to_string(),
        };
        Ok(agent)
    }
}