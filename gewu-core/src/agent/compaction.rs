//! Agent-side compaction logic.
//!
//! This module provides the compaction functionality specific to the Agent,
//! including history summarization when context limits are exceeded.

use crate::compaction::{self, estimate_message_tokens, is_overflow};
use crate::types::{ContentPart, Message, ProviderEvent};
use anyhow::{Result, anyhow};
use futures::StreamExt;
use tracing::info;

use super::Agent;

/// Error type for compaction failures.
#[derive(Debug)]
pub struct CompactionError {
    /// Human-readable error message.
    pub message: String,
}

impl std::fmt::Display for CompactionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CompactionError: {}", self.message)
    }
}

impl std::error::Error for CompactionError {}

impl Agent {
    pub(super) async fn compact_if_needed(&mut self) -> Result<bool> {
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

        let mut request_messages = self.memory.messages().to_vec();
        request_messages.push(Message::User {
            content: vec![ContentPart::Text {
                text: prompt.to_string(),
            }],
        });

        let provider = self.llm.clone();
        let mut stream = provider
            .stream_response(&request_messages, &[], self.log.as_ref())
            .await
            .map_err(|e| anyhow!("Compaction failed: {}", e))?;

        let mut summary_text = String::new();
        while let Some(event_result) = stream.next().await {
            let event = event_result.map_err(|e| anyhow!("Compaction stream error: {}", e))?;
            if let ProviderEvent::ContentDelta { delta } = event {
                summary_text.push_str(&delta);
            }
        }

        if summary_text.is_empty() {
            compaction::prune(self.memory.messages_mut());
            if is_overflow(
                self.memory
                    .messages()
                    .iter()
                    .map(|m| estimate_message_tokens(m))
                    .sum(),
                self.context_limit,
            ) {
                return Err(anyhow!(
                    "Compaction failed: LLM returned empty summary and pruning was insufficient"
                ));
            }
            return Ok(false);
        }

        let mut new_history = Vec::new();

        if let Some(sys_msg) = self
            .memory
            .messages()
            .iter()
            .find(|m| matches!(m, Message::System { .. }))
        {
            new_history.push(sys_msg.clone());
        }

        new_history.push(Message::Assistant {
            content: Some(format!(
                "Summary of previous conversation:\n{}",
                summary_text
            )),
            tool_calls: None,
        });

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
