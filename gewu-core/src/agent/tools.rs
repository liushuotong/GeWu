//! Tool execution utilities.
//!
//! This module provides utility functions for safe tool execution,
//! including panic recovery and error handling.

use crate::types::{ToolContext, ToolResult};
use anyhow::Result;
use std::error::Error;
use std::panic::{AssertUnwindSafe, catch_unwind};
use tokio::task::spawn_blocking;

use super::extract_panic_message;

/// Executes a tool with panic recovery.
///
/// This function wraps tool execution in a `spawn_blocking` call with panic recovery.
/// If the tool panics, the panic is caught and converted into an error return value
/// rather than propagating as an unhandled panic.
///
/// # Arguments
///
/// * `tool` - The tool to execute
/// * `args` - JSON arguments for the tool
/// * `ctx` - Execution context
///
/// # Returns
///
/// Returns the tool's result on success, or an error if the tool panicked or
/// the task was aborted.
pub async fn execute_tool_with_panic_recovery(
    tool: std::sync::Arc<dyn crate::traits::Tool>,
    args: serde_json::Value,
    ctx: ToolContext,
) -> Result<ToolResult, Box<dyn Error + Send + Sync>> {
    let tool_name = tool.name().to_string();

    let result = spawn_blocking(move || {
        catch_unwind(AssertUnwindSafe(|| {
            let rt = tokio::runtime::Handle::current();
            rt.block_on(tool.execute(args, &ctx))
        }))
    })
    .await;

    match result {
        Ok(Ok(res)) => res,
        Ok(Err(e)) => {
            let panic_msg = extract_panic_message(e);
            Err(Box::new(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!(
                    "Tool '{}' panicked during execution: {}. This may indicate an issue with \
                    the tool implementation or invalid input. Consider adjusting the tool arguments \
                    or checking the tool's requirements.",
                    tool_name, panic_msg
                ),
            )) as Box<dyn Error + Send + Sync>)
        }
        Err(join_err) => {
            if join_err.is_panic() {
                match join_err.try_into_panic() {
                    Ok(panic_payload) => {
                        let panic_msg = extract_panic_message(panic_payload);
                        Err(Box::new(std::io::Error::new(
                            std::io::ErrorKind::Other,
                            format!(
                                "Tool '{}' panicked during execution: {}. This may indicate an issue with \
                                the tool implementation or invalid input. Consider adjusting the tool \
                                arguments or checking the tool's requirements.",
                                tool_name, panic_msg
                            ),
                        )) as Box<dyn Error + Send + Sync>)
                    }
                    Err(_) => Err(Box::new(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!(
                            "Tool '{}' panicked but could not retrieve panic payload",
                            tool_name
                        ),
                    )) as Box<dyn Error + Send + Sync>),
                }
            } else {
                Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("Tool '{}' task was aborted unexpectedly", tool_name),
                )) as Box<dyn Error + Send + Sync>)
            }
        }
    }
}
