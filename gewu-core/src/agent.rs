use crate::traits::{LlmProvider, Tool};
use crate::types::Message;
use crate::compaction::{self, is_overflow, estimate_message_tokens};
use std::sync::Arc;
use anyhow::{Result, Context};
use tracing::info;

pub struct Agent {
    llm: Arc<dyn LlmProvider>,
    tools: Vec<Box<dyn Tool>>,
    system_prompt: String,
    max_turns: usize,
    // History is now stateful
    pub history: Vec<Message>,
    // Context limit for compaction (default 128k for modern models, but configurable)
    pub context_limit: usize,
}

pub struct AgentBuilder {
    llm: Option<Arc<dyn LlmProvider>>,
    tools: Vec<Box<dyn Tool>>,
    system_prompt: String,
    max_turns: usize,
    context_limit: usize,
}

impl Default for AgentBuilder {
    fn default() -> Self {
        AgentBuilder {
            llm: None,
            tools: Vec::new(),
            system_prompt: String::new(),
            max_turns: 10,
            context_limit: 128_000,
        }
    }
}

impl Agent {
    /// Resets the conversation history, preserving the system prompt.
    pub fn reset(&mut self) {
        self.history.clear();
        if !self.system_prompt.is_empty() {
             self.history.push(Message::System { content: self.system_prompt.clone() });
        }
    }

    /// Sends a message to the agent and gets a response.
    /// This method maintains the conversation history.
    pub async fn ask(&mut self, user_message: &str) -> Result<String> {
        // Ensure system prompt is the first message if history is empty
        if self.history.is_empty() && !self.system_prompt.is_empty() {
             self.history.push(Message::System { content: self.system_prompt.clone() });
        }

        self.history.push(Message::User { content: user_message.to_string() });
        
        let mut turn = 0;
        loop {
            // Check for compaction at the start of each turn
            if self.compact_if_needed().await? {
                info!("History compacted.");
                // After compaction, history is modified (summarized).
                // We continue the loop with the summarized history.
            }

            if turn >= self.max_turns {
                return Ok("Max turns reached without final answer.".to_string());
            }
            turn += 1;

            // Prune old tool outputs before generating response
            compaction::prune(&mut self.history);

            let response = self.llm.generate_response(&self.history, &self.tools).await
                .map_err(|e| anyhow::anyhow!(e))?;
            
            self.history.push(response.clone());

            match response {
                Message::Assistant { content, tool_calls } => {
                    if let Some(calls) = tool_calls {
                        if calls.is_empty() {
                            return Ok(content.unwrap_or_default());
                        }

                        info!("Agent requested {} tool calls", calls.len());

                        for call in calls {
                            let tool_name = call.function.name;
                            let args = call.function.arguments;
                            let call_id = call.id;

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

                            self.history.push(Message::Tool {
                                tool_call_id: call_id,
                                content: result_content,
                            });
                        }
                    } else {
                        return Ok(content.unwrap_or_default());
                    }
                }
                _ => return Err(anyhow::anyhow!("Unexpected response type from LLM")),
            }
        }
    }

    /// Checks if the current history exceeds the token limit and triggers summarization.
    async fn compact_if_needed(&mut self) -> Result<bool> {
        let mut total_tokens: usize = 0;
        for msg in &self.history {
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
        let mut request_messages = self.history.clone();
        request_messages.push(Message::User {
            content: prompt.to_string(),
        });

        // Call LLM for summary
        let summary_msg = self.llm.generate_response(&request_messages, &[]).await
            .map_err(|e| anyhow::anyhow!("Compaction failed: {}", e))?;

        let summary_text = match summary_msg {
            Message::Assistant { content, .. } => content.unwrap_or_default(),
            _ => return Err(anyhow::anyhow!("Unexpected summary response type")),
        };

        if summary_text.is_empty() {
            return Ok(false);
        }

        let mut new_history = Vec::new();

        // Preserve system prompt
        if let Some(sys_msg) = self.history.iter().find(|m| matches!(m, Message::System { .. })) {
            new_history.push(sys_msg.clone());
        }

        // Add Summary
        new_history.push(Message::Assistant {
            content: Some(format!("Summary of previous conversation:\n{}", summary_text)),
            tool_calls: None,
        });

        // Replay logic: Find the last user message
        // We only replay if it's not the one that triggered the overflow directly?
        // Actually, we want to ensure the agent remembers what the user *just* asked.
        if let Some(last_user_msg) = self.history.iter().rev().find(|m| matches!(m, Message::User { .. })) {
            new_history.push(last_user_msg.clone());
        }

        self.history = new_history;
        Ok(true)
    }
}

impl AgentBuilder {
    pub fn llm(mut self, llm: Arc<dyn LlmProvider>) -> Self {
        self.llm = Some(llm);
        self
    }
    
    pub fn with_tools(mut self, tools: Vec<Box<dyn Tool>>) -> Self {
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
    
    pub fn build(self) -> Result<Agent> {
        let llm = self.llm.context("LLM provider is required")?;
        let mut agent = Agent {
            llm,
            tools: self.tools,
            system_prompt: self.system_prompt.clone(),
            max_turns: self.max_turns,
            history: Vec::new(),
            context_limit: self.context_limit,
        };
        // Initialize history with system prompt
        if !agent.system_prompt.is_empty() {
             agent.history.push(Message::System { content: agent.system_prompt.clone() });
        }
        Ok(agent)
    }
}
