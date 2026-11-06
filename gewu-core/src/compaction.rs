//! History compaction and token management.
//!
//! This module provides functionality for managing conversation history size:
//!
//! - [`estimate_tokens`] - Rough token estimation for text
//! - [`estimate_message_tokens`] - Token estimation for complete messages
//! - [`is_overflow`] - Determines if history exceeds the token limit
//! - [`prune`] - Removes old tool outputs to free up context space
//!
//! # Token Estimation
//!
//! Token estimation is intentionally simplified: `text.len() / 4`. This is a
//! rough approximation where each token is assumed to be ~4 characters.
//! For binary media data (base64), the same approximation is used.
//!
//! # Compaction vs Pruning
//!
//! GeWu uses two strategies to manage context size:
//!
//! - **Pruning** ([`prune`]): Fast, in-memory cleanup that removes tool output
//!   content from old messages. Keeps the structure but replaces long outputs
//!   with "[Output pruned]". Triggered on every agent turn.
//!
//! - **Compaction**: Full summarization that uses the LLM to create a summary of the entire
//!   conversation history. Only triggered when history exceeds context limit.
//!   See the `Agent::compact_if_needed` method for details.

use crate::types::{ContentPart, Message};

const PRUNE_PROTECT_THRESHOLD: usize = 40_000;
const PRUNE_MINIMUM_SAVINGS: usize = 20_000;
const COMPACTION_BUFFER: usize = 20_000;

/// Estimates the number of tokens in a text string.
///
/// Uses a simple approximation: `text.len() / 4`. This assumes average
/// token length of ~4 characters, which is a reasonable approximation
/// for English text. Note that actual tokenizers (like tiktoken) may
/// produce different counts.
pub fn estimate_tokens(text: &str) -> usize {
    text.len() / 4
}

/// Determines if the current token count exceeds the context limit.
///
/// Returns `true` if compaction should be considered. A buffer of
/// tokens is reserved to ensure some working space remains after compaction.
///
/// # Arguments
///
/// * `total_tokens` - The estimated total tokens in the conversation history.
/// * `limit_context` - The maximum context window size (e.g., 128,000 for GPT-4).
///
/// # Examples
///
/// ```
/// use gewu_core::compaction::is_overflow;
///
/// // With an 8k context limit and 120k tokens, overflow is true
/// assert!(is_overflow(120_000, 128_000));
///
/// // With an 8k context limit and 100k tokens, overflow is false
/// assert!(!is_overflow(100_000, 128_000));
///
/// // Small limits never overflow on empty history: the reserved buffer is
/// // clamped to half the window.
/// assert!(!is_overflow(0, 8_000));
/// ```
pub fn is_overflow(total_tokens: usize, limit_context: usize) -> bool {
    if limit_context == 0 {
        return false;
    }
    // Reserve a buffer to leave working space after compaction, but never
    // reserve more than half the window: with the full 20k buffer, an 8k
    // local model would have `usable == 0` and every turn (even empty
    // history) would fire a compaction.
    let reserved = COMPACTION_BUFFER.min(limit_context / 2);
    let usable = limit_context.saturating_sub(reserved);
    total_tokens >= usable
}

fn estimate_content_tokens(content: &[ContentPart]) -> usize {
    content
        .iter()
        .map(|part| match part {
            ContentPart::Text { text } => estimate_tokens(text),
            ContentPart::Image { data, .. } => estimate_tokens(data),
            ContentPart::Audio { data, .. } => estimate_tokens(data),
            ContentPart::Video { data, .. } => estimate_tokens(data),
            ContentPart::File { data, .. } => estimate_tokens(data),
        })
        .sum()
}

/// Estimates the token count for a complete message.
///
/// Token estimation varies by message type:
/// - **System**: Simple text length
/// - **User**: Sum of all content parts (text + media data)
/// - **Assistant**: Text content plus function names and arguments from tool calls
/// - **Tool**: Original content plus any additional media parts
pub fn estimate_message_tokens(msg: &Message) -> usize {
    match msg {
        Message::System { content } => estimate_tokens(content),
        Message::User { content } => estimate_content_tokens(content),
        Message::Assistant {
            content,
            tool_calls,
        } => {
            let mut count = content.as_ref().map(|s| estimate_tokens(s)).unwrap_or(0);
            if let Some(calls) = tool_calls {
                for call in calls {
                    count += estimate_tokens(&call.function.arguments);
                    count += estimate_tokens(&call.function.name);
                }
            }
            count
        }
        Message::Tool { content, media, .. } => {
            let mut count = estimate_tokens(content);
            count += estimate_content_tokens(media);
            count
        }
    }
}

/// Prunes tool outputs from older messages to free context space.
///
/// This function walks backwards through the message history, skipping the
/// most recent 2 turns (to keep recent context), and marks old tool outputs
/// for pruning once the total tool output size exceeds a threshold.
///
/// A message starting with the compaction summary prefix ([`SUMMARY_PREFIX`])
/// is treated as a compaction summary and stops the pruning process
/// (preserving the summarized history intact).
///
/// # Arguments
///
/// * `messages` - The message history to prune (modified in place).
///
/// # Returns
///
/// The number of tokens saved by pruning, or 0 if pruning was not performed
/// (either not enough savings or pruning threshold not reached).
///
/// # Pruning Behavior
///
/// 1. Iterates backwards from the second-to-last message
/// 2. Skips the 2 most recent turns (user/assistant pairs)
/// 3. Accumulates tool output tokens until exceeding `PRUNE_PROTECT_THRESHOLD`
/// 4. Once threshold exceeded, marks those messages for pruning
/// 5. Only actually prunes if total savings exceed `PRUNE_MINIMUM_SAVINGS`
/// 6. Replaces tool content with "[Output pruned]" and clears media
///
/// A message starting with the compaction summary prefix ([`SUMMARY_PREFIX`])
/// stops the pruning process (preserving the summarized history intact).
pub const SUMMARY_PREFIX: &str = "Summary of previous conversation:";

/// The number of most recent turns that are protected from pruning.
const PROTECTED_TURNS: usize = 2;

pub fn prune(messages: &mut [Message]) -> usize {
    let mut total_tool_tokens = 0;
    let mut pruned_tokens = 0;
    let mut to_prune = Vec::new();
    let mut turns = 0;

    for (i, msg) in messages.iter().enumerate().rev() {
        if let Message::User { .. } = msg {
            turns += 1;
        }

        if turns < PROTECTED_TURNS {
            continue;
        }

        if let Message::Assistant {
            content: Some(c), ..
        } = msg
        {
            // Recognize the exact prefix the compactor writes (see
            // [`SUMMARY_PREFIX`]); a bare "Summary:" check never matched real
            // summaries, so pruning walked straight through them.
            if c.starts_with(SUMMARY_PREFIX) || c.starts_with("Summary:") {
                break;
            }
        }

        if let Message::Tool { content, media, .. } = msg {
            let estimate = estimate_tokens(content) + estimate_content_tokens(media);
            total_tool_tokens += estimate;

            if total_tool_tokens > PRUNE_PROTECT_THRESHOLD {
                pruned_tokens += estimate;
                to_prune.push(i);
            }
        }
    }

    if pruned_tokens > PRUNE_MINIMUM_SAVINGS {
        for idx in to_prune {
            if let Message::Tool { content, media, .. } = &mut messages[idx] {
                *content = "[Output pruned]".to_string();
                media.clear();
            }
        }
        return pruned_tokens;
    }

    0
}
