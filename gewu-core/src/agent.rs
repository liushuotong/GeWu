use crate::compaction::{self, estimate_message_tokens, is_overflow};
use crate::traits::{LlmProvider, Memory, Tool, ToolRegistry};
use crate::types::{AgentEvent, ContentPart, FunctionCall, Message, ProviderEvent, ToolCall};
use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::{Stream, StreamExt};
use serde::Deserialize;
use std::sync::Arc;
use tracing::{debug, info};

pub struct Agent {
    llm: Arc<dyn LlmProvider>,
    tools: Vec<Arc<dyn Tool>>,
    system_prompt: String,
    max_turns: usize,
    pub memory: Box<dyn Memory>,
    pub context_limit: usize,
}

pub struct AgentBuilder {
    llm: Option<Arc<dyn LlmProvider>>,
    tools: Vec<Arc<dyn Tool>>,
    system_prompt: String,
    max_turns: usize,
    memory: Option<Box<dyn Memory>>,
    context_limit: usize,
}

impl Default for AgentBuilder {
    fn default() -> Self {
        AgentBuilder {
            llm: None,
            tools: Vec::new(),
            system_prompt: String::new(),
            max_turns: 10,
            memory: None,
            context_limit: 128_000,
        }
    }
}

impl Agent {
    pub fn builder() -> AgentBuilder {
        AgentBuilder::default()
    }

    /// Resets the conversation history, preserving the system prompt.
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

    /// Sends a message to the agent and gets a response stream.
    /// This method maintains the conversation history.
    pub fn ask<'a>(
        &'a mut self,
        user_message: &'a str,
    ) -> impl Stream<Item = Result<AgentEvent>> + 'a {
        async_stream::try_stream! {
            // Ensure system prompt is the first message if history is empty
            if self.memory.messages().is_empty() && !self.system_prompt.is_empty() {
                 self.memory.add_message(Message::System { content: self.system_prompt.clone() }).await;
            }

            self.memory.add_message(Message::User { content: vec![ContentPart::Text { text: user_message.to_string() }] }).await;

            let mut turn = 0;
            let mut recent_tool_calls: Vec<(String, String)> = Vec::new();

            loop {
                // Check for compaction at the start of each turn
                if self.compact_if_needed().await? {
                    info!("History compacted.");
                }

                if turn >= self.max_turns {
                    yield AgentEvent::Error { error: "Max turns reached without final answer.".to_string() };
                    break;
                }
                turn += 1;

                // Prune old tool outputs before generating response
                compaction::prune(self.memory.messages_mut());

                let mut current_content = String::new();
                let mut current_tool_calls: Vec<ToolCall> = Vec::new();

                // Call LLM provider
                // We clone tools to avoid borrowing self while iterating the stream
                let provider = self.llm.clone();
                let messages = self.memory.messages().to_vec(); // Clone messages
                debug!("LLM request messages: {:?}", messages);

                // Clone tools for the LLM call
                let tools_clone = self.tools.clone();
                let mut stream = provider.stream_response(&messages, &tools_clone).await
                    .map_err(|e| anyhow::anyhow!(e))?;

                while let Some(event_result) = stream.next().await {
                    let event = event_result.map_err(|e| anyhow::anyhow!(e))?;

                    match event {
                        ProviderEvent::ContentDelta { delta } => {
                            current_content.push_str(&delta);
                            yield AgentEvent::ContentDelta { delta };
                        }
                        ProviderEvent::ReasoningDelta { delta } => {
                            yield AgentEvent::ReasoningDelta { delta };
                        }
                        ProviderEvent::ToolCallStart { id, name } => {
                            current_tool_calls.push(ToolCall {
                                id,
                                r#type: "function".to_string(),
                                function: FunctionCall {
                                    name,
                                    arguments: String::new(),
                                },
                            });
                        }
                        ProviderEvent::ToolCallDelta { id, delta } => {
                            if let Some(tc) = current_tool_calls.iter_mut().find(|tc| tc.id == id) {
                                tc.function.arguments.push_str(&delta);
                            }
                        }
                        ProviderEvent::Finish { .. } => {
                            // Provider stream finished.
                        }
                        ProviderEvent::Error { error } => {
                            yield AgentEvent::Error { error };
                            return; // Stop stream
                        }
                    }
                }

                // Add assistant message to history
                let assistant_msg = Message::Assistant {
                    content: if current_content.is_empty() { None } else { Some(current_content.clone()) },
                    tool_calls: if current_tool_calls.is_empty() { None } else { Some(current_tool_calls.clone()) },
                };
                self.memory.add_message(assistant_msg).await;

                // Handle tool calls
                if current_tool_calls.is_empty() {
                    // No tools called, we are done
                    yield AgentEvent::Finish { response: current_content };
                    break;
                }

                info!("Agent requested {} tool calls", current_tool_calls.len());

                for call in current_tool_calls {
                    let tool_name = call.function.name.clone();
                    let args = call.function.arguments.clone();
                    let call_id = call.id;

                    // Doom loop detection
                    let current_call_sig = (tool_name.clone(), args.clone());
                    recent_tool_calls.push(current_call_sig.clone());
                    if recent_tool_calls.len() > 3 {
                        recent_tool_calls.remove(0);
                    }

                    if recent_tool_calls.len() == 3 && recent_tool_calls.iter().all(|x| x == &current_call_sig) {
                         let error_msg = format!("Doom loop detected: Tool '{}' called 3 times repeatedly with identical arguments. Stopping.", tool_name);
                         yield AgentEvent::Error { error: error_msg };
                         return;
                    }

                    if tool_name.is_empty() {
                         let error_msg = "Tool name is empty, skipping execution".to_string();
                         yield AgentEvent::Error { error: error_msg };
                         continue;
                    }

                    yield AgentEvent::ToolStart { tool: tool_name.clone(), input: args.clone() };

                    let tool = self.tools.iter().find(|t| t.name() == tool_name);

                    let result_content = if let Some(t) = tool {
                        info!("Executing tool: {}", tool_name);
                        match t.call(&args).await {
                            Ok(res) => res,
                            Err(e) => format!("Error executing tool: {}", e),
                        }
                    } else {
                        format!("Tool not found: {}", tool_name)
                    };

                    yield AgentEvent::ToolFinish { tool: tool_name.clone(), output: result_content.clone() };

                    let media_parts = extract_media_parts(&result_content);
                    self.memory.add_message(Message::Tool {
                        tool_call_id: call_id,
                        tool_name: Some(tool_name.clone()),
                        content: result_content,
                        media: media_parts,
                    }).await;
                }

                // Continue loop for next turn
            }
        }
    }

    /// Checks if the current history exceeds the token limit and triggers summarization.
    async fn compact_if_needed(&mut self) -> Result<bool> {
        let mut total_tokens: usize = 0;
        for msg in self.memory.messages() {
            total_tokens += estimate_message_tokens(msg);
        }

        if !is_overflow(total_tokens, self.context_limit) {
            return Ok(false);
        }

        info!("Compacting history (total tokens: {})", total_tokens);

        let prompt = "Provide a detailed prompt for continuing our conversation above.
Focus on information that would be helpful for continuing the conversation, including what we did, what we're doing, which files we're working on, and what we're going to do next.
The summary that you construct will be used so that another agent can read it and continue the work.

When constructing the summary, try to stick to this template:
---
## Goal

[What goal(s) is the user trying to accomplish?]

## Instructions

- [What important instructions did the user give you that are relevant]
- [If there is a plan or spec, include information about it so next agent can continue using it]

## Discoveries

[What notable things were learned during this conversation that would be useful for the next agent to know when continuing the work]

## Accomplished

[What work has been completed, what work is still in progress, and what work is left?]

## Relevant files / directories

[Construct a structured list of relevant files that have been read, edited, or created that pertain to the task at hand. If all the files in a directory are relevant, include the path to the directory.]
---";

        // Prepare messages for summarization
        let mut request_messages = self.memory.messages().to_vec();
        request_messages.push(Message::User {
            content: vec![ContentPart::Text {
                text: prompt.to_string(),
            }],
        });

        // Call LLM for summary
        let provider = self.llm.clone();
        let mut stream = provider
            .stream_response(&request_messages, &[])
            .await
            .map_err(|e| anyhow::anyhow!("Compaction failed: {}", e))?;

        let mut summary_text = String::new();
        while let Some(event_result) = stream.next().await {
            let event =
                event_result.map_err(|e| anyhow::anyhow!("Compaction stream error: {}", e))?;
            if let ProviderEvent::ContentDelta { delta } = event {
                summary_text.push_str(&delta);
            }
        }

        if summary_text.is_empty() {
            return Ok(false);
        }

        let mut new_history = Vec::new();

        // Preserve system prompt
        if let Some(sys_msg) = self
            .memory
            .messages()
            .iter()
            .find(|m| matches!(m, Message::System { .. }))
        {
            new_history.push(sys_msg.clone());
        }

        // Add Summary
        new_history.push(Message::Assistant {
            content: Some(format!(
                "Summary of previous conversation:\n{}",
                summary_text
            )),
            tool_calls: None,
        });

        // Replay logic: Find the last user message
        // We only replay if it's not the one that triggered the overflow directly?
        // Actually, we want to ensure the agent remembers what the user *just* asked.
        if let Some(last_user_msg) = self
            .memory
            .messages()
            .iter()
            .rev()
            .find(|m| matches!(m, Message::User { .. }))
        {
            new_history.push(last_user_msg.clone());
        }

        self.memory.replace(new_history).await;
        Ok(true)
    }
}

#[async_trait]
impl ToolRegistry for Agent {
    async fn execute(
        &self,
        name: &str,
        args: &str,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let tool = self.tools.iter().find(|t| t.name() == name);
        match tool {
            Some(t) => t.call(args).await,
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
        self.tools = tools;
        self
    }

    pub fn system_prompt(mut self, prompt: &str) -> Self {
        self.system_prompt = prompt.to_string();
        self
    }

    pub fn max_turns(mut self, max_turns: usize) -> Self {
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
            tools: self.tools.clone(),
            system_prompt: self.system_prompt.clone(),
            max_turns: self.max_turns,
            memory: self.memory.unwrap_or_else(|| Box::new(Vec::new())),
            context_limit: self.context_limit,
        };
        // Removed initial synchronous add_message call.
        // It will be handled in reset() or ask() or should be pre-seeded.
        // Since we removed it, we rely on ask() to add system prompt if empty.
        Ok(agent)
    }
}

#[derive(Deserialize)]
struct MediaPayload {
    #[serde(rename = "type")]
    kind: String,
    mime: String,
    data: String,
    #[serde(default)]
    message: Option<String>,
}

fn extract_media_parts(content: &str) -> Vec<ContentPart> {
    let parsed: MediaPayload = match serde_json::from_str(content) {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };

    if parsed.kind != "media" {
        return Vec::new();
    }

    let mut parts = Vec::new();
    if let Some(text) = parsed.message {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            parts.push(ContentPart::Text {
                text: trimmed.to_string(),
            });
        }
    }

    let mime = parsed.mime.as_str();
    let content_part = if mime.starts_with("image/") {
        ContentPart::Image {
            data: parsed.data,
            mime_type: parsed.mime,
        }
    } else if mime.starts_with("audio/") {
        ContentPart::Audio {
            data: parsed.data,
            mime_type: parsed.mime,
        }
    } else if mime.starts_with("video/") {
        ContentPart::Video {
            data: parsed.data,
            mime_type: parsed.mime,
        }
    } else {
        ContentPart::File {
            data: parsed.data,
            mime_type: parsed.mime,
        }
    };
    parts.push(content_part);

    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_media_parts_parses_image() {
        let json = r#"{"type":"media","mime":"image/png","data":"ZmFrZQ==","message":"Image read successfully"}"#;
        let parts = extract_media_parts(json);
        assert_eq!(parts.len(), 2);
        match &parts[0] {
            ContentPart::Text { text } => assert!(text.contains("Image")),
            _ => panic!("first part should be text"),
        }
        match &parts[1] {
            ContentPart::Image { mime_type, data } => {
                assert_eq!(mime_type, "image/png");
                assert_eq!(data, "ZmFrZQ==");
            }
            _ => panic!("second part should be image"),
        }
    }

    #[test]
    fn extract_media_parts_ignores_non_media() {
        let json = r#"{"type":"text","text":"hello"}"#;
        let parts = extract_media_parts(json);
        assert!(parts.is_empty());
    }
}
