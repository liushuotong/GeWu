//! Execution utilities for the agent loop.
//!
//! This module provides helper structs and functions used by the [`Agent::ask`](super::Agent::ask)
//! method to modularize the main agent loop logic.

use crate::types::{AgentEvent, FunctionCall, ProviderEvent, ToolCall, ToolContext, ToolResult};
use anyhow::Result;
use std::collections::VecDeque;
use tracing::warn;

use super::execute_tool_with_panic_recovery;
use crate::middleware::TruncationService;
use crate::traits::Tool;

/// Tracks tool calls to detect doom loops.
///
/// A doom loop occurs when the same tool is called multiple times with
/// identical arguments, which typically indicates the agent is stuck
/// in an infinite cycle. This detector maintains a sliding window
/// of recent tool calls and flags when the same call appears
/// `threshold` times consecutively.
pub(super) struct DoomLoopDetector {
    recent_calls: VecDeque<(String, String)>,
    threshold: usize,
}

impl DoomLoopDetector {
    /// Creates a new `DoomLoopDetector` with the specified threshold.
    ///
    /// # Arguments
    ///
    /// * `threshold` - Number of consecutive identical calls to trigger doom loop detection
    pub(super) fn new(threshold: usize) -> Self {
        Self {
            recent_calls: VecDeque::new(),
            threshold,
        }
    }

    /// Checks if the given tool call would trigger doom loop detection.
    ///
    /// Returns `Some(error_message)` if doom loop is detected, `None` otherwise.
    ///
    /// # Arguments
    ///
    /// * `tool` - The tool name
    /// * `args` - The tool arguments
    pub(super) fn check(&mut self, tool: &str, args: &str) -> Option<String> {
        let current_call_sig = (tool.to_string(), args.to_string());
        if self.recent_calls.len() == self.threshold
            && self.recent_calls.iter().all(|x| x == &current_call_sig)
        {
            return Some(format!(
                "Doom loop detected: Tool '{}' called {} times repeatedly with identical arguments. Stopping.",
                tool, self.threshold
            ));
        }
        None
    }

    /// Records a tool call for future doom loop detection.
    ///
    /// Adds the tool call to the recent calls history and removes
    /// the oldest call if the history exceeds the threshold.
    ///
    /// # Arguments
    ///
    /// * `tool` - The tool name
    /// * `args` - The tool arguments
    pub(super) fn record(&mut self, tool: String, args: String) {
        self.recent_calls.push_back((tool, args));
        if self.recent_calls.len() > self.threshold {
            self.recent_calls.pop_front();
        }
    }
}

/// Aggregates streaming LLM responses into content and tool calls.
///
/// The `LlmResponseAggregator` processes [`ProviderEvent`] items from the LLM
/// stream, collecting content deltas and building up complete tool calls
/// from start/delta events. It yields [`AgentEvent`] items that should
/// be immediately forwarded to the caller.
pub(super) struct LlmResponseAggregator {
    content: String,
    tool_calls: Vec<ToolCall>,
}

impl LlmResponseAggregator {
    /// Creates a new empty aggregator.
    pub(super) fn new() -> Self {
        Self {
            content: String::new(),
            tool_calls: Vec::new(),
        }
    }

    /// Processes a single provider event.
    ///
    /// For content and reasoning deltas, returns an `AgentEvent` to yield.
    /// For tool call events, accumulates the data internally and returns `None`.
    /// For errors, returns an `AgentEvent` to yield.
    ///
    /// # Arguments
    ///
    /// * `event` - The provider event to process
    ///
    /// # Returns
    ///
    /// `Some(AgentEvent)` if the event should be yielded, `None` if it's internal
    pub(super) fn process_event(&mut self, event: ProviderEvent) -> Option<AgentEvent> {
        match event {
            ProviderEvent::ContentDelta { delta } => {
                self.content.push_str(&delta);
                Some(AgentEvent::ContentDelta { delta })
            }
            ProviderEvent::ReasoningDelta { delta } => Some(AgentEvent::ReasoningDelta { delta }),
            ProviderEvent::ToolCallStart { id, name } => {
                self.tool_calls.push(ToolCall {
                    id,
                    r#type: "function".to_string(),
                    function: FunctionCall {
                        name,
                        arguments: String::new(),
                    },
                });
                None
            }
            ProviderEvent::ToolCallDelta { id, delta } => {
                if let Some(tc) = self.tool_calls.iter_mut().find(|tc| tc.id == id) {
                    tc.function.arguments.push_str(&delta);
                }
                None
            }
            ProviderEvent::Finish { .. } => None,
            ProviderEvent::Error { error } => Some(AgentEvent::Error { error }),
        }
    }

    /// Extracts and returns the accumulated content and tool calls.
    ///
    /// After calling this method, the aggregator is empty and ready
    /// to aggregate a new response.
    ///
    /// # Returns
    ///
    /// A tuple of (content string, tool calls vector)
    pub(super) fn take_parts(&mut self) -> (String, Vec<ToolCall>) {
        (
            std::mem::take(&mut self.content),
            std::mem::take(&mut self.tool_calls),
        )
    }
}

/// Executes a single tool call with panic recovery and output truncation.
///
/// This function handles the complete lifecycle of a tool execution:
/// 1. Parses the JSON arguments
/// 2. Executes the tool with panic recovery
/// 3. Truncates the output if necessary
/// 4. Returns the tool result with its output truncated if necessary
///
/// # Arguments
///
/// * `tool` - The tool to execute
/// * `call` - The tool call containing the function name and arguments
/// * `ctx` - The execution context
/// * `truncation` - The truncation service for handling long outputs
///
/// # Returns
///
/// The tool result, or an error if argument parsing failed
pub(super) async fn execute_tool_call(
    tool: std::sync::Arc<dyn Tool>,
    call: ToolCall,
    ctx: ToolContext,
    truncation: &TruncationService,
) -> Result<ToolResult> {
    let tool_name = tool.name().to_string();
    let args = call.function.arguments;

    let args_val: serde_json::Value = serde_json::from_str(&args)
        .map_err(|e| anyhow::anyhow!("Failed to parse arguments: {}", e))?;

    match execute_tool_with_panic_recovery(tool, args_val, ctx).await {
        Ok(mut res) => {
            let truncation_result = truncation.truncate(&res.output);
            if truncation_result.truncated {
                res.output = format!(
                    "{}\n\n[Output truncated - {} bytes saved to {}]",
                    truncation_result.content,
                    truncation_result.content.len(),
                    truncation_result
                        .output_path
                        .map(|p| p.display().to_string())
                        .unwrap_or_default()
                );
            }
            Ok(res)
        }
        Err(e) => {
            warn!("Tool '{}' failed: {}", tool_name, e);
            Ok(ToolResult::new(tool_name, format!("Error: {}", e)))
        }
    }
}
