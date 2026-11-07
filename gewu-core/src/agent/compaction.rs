//! Agent-side compaction logic.
//!
//! Implements opencode V2-style compaction: when history overflows the
//! context limit, everything older than a small verbatim tail is summarized
//! by the LLM (tool outputs truncated in the summarizer's view), and history
//! collapses into a single checkpoint message carrying the summary plus the
//! serialized recent tail.

use crate::compaction::{
    self, build_checkpoint_message, build_summary_prompt, estimate_message_tokens, find_checkpoint,
    is_overflow, serialize_messages, select_recent_tail, KEEP_TOKENS, SUMMARY_OUTPUT_TOKENS,
};
use crate::types::{ContentPart, Message, ProviderEvent};
use anyhow::{Result, anyhow};
use futures::StreamExt;
use tracing::{info, warn};

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

        // Body = everything the summarizer may touch: after the system
        // prompt, and after any prior checkpoint (whose summary/recent
        // already represent the older history).
        let messages = self.memory.messages().to_vec();
        let body_start = messages
            .iter()
            .position(|m| !matches!(m, Message::System { .. }))
            .unwrap_or(messages.len());
        let prior_checkpoint = find_checkpoint(&messages);
        let body_start = match &prior_checkpoint {
            // Skip past the checkpoint message itself.
            Some(_) => body_start + 1,
            None => body_start,
        };
        let body = &messages[body_start..];

        // Keep the newest slice verbatim; summarize everything before it.
        let split = select_recent_tail(body, KEEP_TOKENS);
        let head = &body[..split];
        let recent = &body[split..];

        // Prior checkpoint's recent tail is context for the new summary too.
        let prior_recent = prior_checkpoint.as_ref().map(|(_, recent)| recent.clone());

        if head.is_empty() && prior_recent.as_ref().map_or(true, |r| r.is_empty()) {
            // Nothing older than the tail exists to summarize (e.g. one huge
            // message). Compaction cannot shrink this further; let the
            // request through and surface any provider overflow directly.
            warn!("Compaction skipped: no history older than the kept tail");
            return Ok(false);
        }

        let mut context = Vec::new();
        if let Some(recent) = &prior_recent
            && !recent.is_empty()
        {
            context.push(recent.clone());
        }
        context.push(serialize_messages(head));
        let prior_summary = prior_checkpoint.as_ref().map(|(summary, _)| summary.as_str());
        let prompt = build_summary_prompt(prior_summary, &context);

        // The summarizer sees a capped transcript, so the prompt is far
        // smaller than the raw history; still, respect the window.
        if compaction::estimate_tokens(&prompt) + SUMMARY_OUTPUT_TOKENS > self.context_limit {
            warn!("Compaction skipped: summary prompt would not fit the context window");
            return Ok(false);
        }

        let summary_request = vec![Message::User {
            content: vec![ContentPart::Text { text: prompt }],
        }];
        let provider = self.llm.clone();
        let mut stream = provider
            .stream_response(&summary_request, &[], self.log.as_ref())
            .await
            .map_err(|e| anyhow!("Compaction failed: {}", e))?;

        let mut summary_text = String::new();
        while let Some(event_result) = stream.next().await {
            let event = event_result.map_err(|e| anyhow!("Compaction stream error: {}", e))?;
            match event {
                ProviderEvent::ContentDelta { delta } => summary_text.push_str(&delta),
                // The compaction request is a real LLM request; its usage
                // counts toward the session's provider-reported totals.
                ProviderEvent::Usage { usage } => self.provider_usage.add(&usage),
                _ => {}
            }
        }

        if summary_text.trim().is_empty() {
            // Returning an error (instead of silently continuing) keeps the
            // loop from retrying the expensive summary on every iteration.
            return Err(anyhow!("Compaction failed: LLM returned an empty summary"));
        }

        let checkpoint = build_checkpoint_message(summary_text.trim(), &serialize_messages(recent));

        let mut new_history = Vec::new();
        if let Some(sys_msg) = messages.iter().find(|m| matches!(m, Message::System { .. })) {
            new_history.push(sys_msg.clone());
        }
        new_history.push(checkpoint);
        self.memory.replace(new_history).await;
        Ok(true)
    }
}
