use crate::types::Message;

// Constants for heuristics
const PRUNE_PROTECT_THRESHOLD: usize = 40_000;
const PRUNE_MINIMUM_SAVINGS: usize = 20_000;
const COMPACTION_BUFFER: usize = 20_000;

// Simplified token estimation: 1 token ~ 4 chars
pub fn estimate_tokens(text: &str) -> usize {
    text.len() / 4
}

/// Checks if the current token usage exceeds the model's capacity.
pub fn is_overflow(total_tokens: usize, limit_context: usize) -> bool {
    if limit_context == 0 {
        return false;
    }

    // Heuristic: reserve buffer or max output tokens
    let reserved = COMPACTION_BUFFER;
    let usable = limit_context.saturating_sub(reserved);

    total_tokens >= usable
}

/// Estimates tokens for a single message.
pub fn estimate_message_tokens(msg: &Message) -> usize {
    match msg {
        Message::System { content } => estimate_tokens(content),
        Message::User { content } => estimate_tokens(content),
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
        Message::Tool { content, .. } => estimate_tokens(content),
    }
}

/// Prunes session history by clearing tool outputs from older messages.
///
/// Returns the number of tokens saved.
pub fn prune(messages: &mut [Message]) -> usize {
    let mut total_tool_tokens = 0;
    let mut pruned_tokens = 0;
    let mut to_prune = Vec::new();
    let mut turns = 0;

    // We iterate backwards to accumulate token count of tool outputs
    // We skip the last 2 turns (user/assistant pair usually) to keep context fresh.
    for (i, msg) in messages.iter().enumerate().rev() {
        if let Message::User { .. } = msg {
            turns += 1;
        }

        if turns < 2 {
            continue;
        }

        // Check if message is a summary (heuristic for now, check if system/assistant says "Summary")
        if let Message::Assistant {
            content: Some(c), ..
        } = msg
        {
            if c.starts_with("Summary:") {
                break;
            }
        }

        if let Message::Tool { content, .. } = msg {
            let estimate = estimate_tokens(content);
            total_tool_tokens += estimate;

            if total_tool_tokens > PRUNE_PROTECT_THRESHOLD {
                pruned_tokens += estimate;
                to_prune.push(i);
            }
        }
    }

    if pruned_tokens > PRUNE_MINIMUM_SAVINGS {
        for idx in to_prune {
            if let Message::Tool { content, .. } = &mut messages[idx] {
                // Clear output to save space
                *content = "[Output pruned]".to_string();
            }
        }
        return pruned_tokens;
    }

    0
}
