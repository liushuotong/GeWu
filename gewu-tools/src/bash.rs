//! Bash command execution tool.
//!
//! This module provides the [`BashTool`] which allows the agent to execute
//! bash commands. It supports both full shell access and a restricted
//! "safe mode" that only allows predefined read-only commands.
//!
//! # Safety Features
//!
//! - **Safe mode**: Optional whitelist of allowed commands (ls, cat, grep, etc.)
//! - **Command chaining prevention**: Blocks `;`, `&&`, `||` operators in safe mode
//! - **Working directory restriction**: Commands run in a specified directory
//! - **Timeout**: Configurable timeout prevents hanging commands
//!
//! # Tool Definition
//!
//! - **Name**: `bash`
//! - **Parameters**:
//!   - `command` (required): The bash command to execute
//!   - `timeout` (optional): Timeout in milliseconds (default: 120000)
//!   - `workdir` (optional): Working directory for the command

use anyhow::{Context, Result};
use async_trait::async_trait;
use gewu_core::traits::Tool;
use serde_json::{Value, json};
use std::error::Error;
use std::path::PathBuf;

/// Tool for executing bash commands.
///
/// BashTool provides shell command execution capabilities to the agent.
/// It can be configured with safe mode to restrict available commands.
///
/// # Safe Mode
///
/// When `safe_mode` is enabled, only a limited set of read-only commands
/// are allowed:
/// - `ls`, `cat`, `grep`, `rg`, `find`, `pwd`, `echo`, `wc`, `head`, `tail`
/// - `whoami`, `date`, `git`, `printf`, `sort`, `uniq`, `diff`
///
/// Command chaining with `;`, `&&`, `||` is also blocked in safe mode.
///
/// # Path Safety
///
/// The tool resolves paths relative to `working_directory` and prevents
/// directory traversal attacks. See [`resolve_safe_path`](super::fs_utils::resolve_safe_path).
///
/// # Example
///
/// ```ignore
/// let tool = BashTool {
///     working_directory: PathBuf::from("/home/user/project"),
///     safe_mode: false,
/// };
///
/// let result = tool.call(r#"{"command": "ls -la", "timeout": 30000}"#).await;
/// ```
pub struct BashTool {
    /// The directory to execute commands in.
    pub working_directory: PathBuf,
    /// If true, only allow predefined read-only commands.
    pub safe_mode: bool,
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        include_str!("prompts/bash.txt")
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The command to execute"
                },
                "timeout": {
                    "type": "number",
                    "description": "Optional timeout in milliseconds (default: 120000)"
                },
                "workdir": {
                    "type": "string",
                    "description": "The working directory to run the command in. Defaults to the current working directory. Use this instead of 'cd' commands."
                }
            },
            "required": ["command"]
        })
    }

    /// Executes a bash command.
    ///
    /// # Arguments
    ///
    /// JSON string containing:
    /// - `command` (required): The bash command to execute
    /// - `timeout` (optional): Milliseconds before command times out (default: 120000)
    /// - `workdir` (optional): Override working directory
    ///
    /// # Returns
    ///
    /// Returns stdout and stderr combined, or an error message if
    /// the command was blocked, not found, or timed out.
    ///
    /// # Safety Checks
    ///
    /// In safe mode:
    /// - First word must be in the allowed command whitelist
    /// - Command chaining characters (`;`, `&&`, `||`) are blocked
    async fn call(&self, args: &str) -> Result<String, Box<dyn Error + Send + Sync>> {
        let args_val: Value = serde_json::from_str(args).map_err(|e| anyhow::anyhow!(e))?;
        let command_str = args_val["command"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("command is required"))?;
        let timeout_ms = args_val["timeout"].as_u64().unwrap_or(120000);

        if self.safe_mode {
            let trimmed = command_str.trim();
            let first_word = trimmed.split_whitespace().next().unwrap_or("");
            let allowed = [
                "ls", "cat", "grep", "rg", "find", "pwd", "echo", "wc", "head", "tail", "whoami",
                "date", "git", "printf", "sort", "uniq", "diff",
            ];

            if !allowed.contains(&first_word) {
                return Ok(format!(
                    "Command '{}' is not allowed in safe mode. Allowed commands: {:?}",
                    first_word, allowed
                ));
            }

            if trimmed.contains(';') || trimmed.contains("&&") || trimmed.contains("||") {
                return Ok(
                    "Complex command chaining (;, &&, ||) is not allowed in safe mode.".to_string(),
                );
            }
        }

        let work_dir = if let Some(wd) = args_val["workdir"].as_str() {
            crate::fs_utils::resolve_safe_path(&self.working_directory, wd)
                .map_err(|e| anyhow::anyhow!(e))?
        } else {
            self.working_directory.clone()
        };

        let mut child = tokio::process::Command::new("bash");
        child
            .arg("-c")
            .arg(command_str)
            .current_dir(&work_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        let child_process = child
            .spawn()
            .context("Failed to spawn bash command")
            .map_err(|e| anyhow::anyhow!(e))?;

        let output_result = tokio::time::timeout(
            std::time::Duration::from_millis(timeout_ms),
            child_process.wait_with_output(),
        )
        .await;

        match output_result {
            Ok(io_result) => {
                let output = io_result
                    .context("Failed to wait for child process")
                    .map_err(|e| anyhow::anyhow!(e))?;
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);

                let mut result = String::new();
                if !stdout.is_empty() {
                    result.push_str(&stdout);
                }
                if !stderr.is_empty() {
                    if !result.is_empty() {
                        result.push_str("\n--- stderr ---\n");
                    }
                    result.push_str(&stderr);
                }

                if !output.status.success() {
                    if !result.is_empty() {
                        result.push_str("\n--- status ---\n");
                    }
                    result.push_str(&format!("Command failed with status: {}", output.status));
                }

                Ok(result)
            }
            Err(_) => {
                Ok(format!("Command timed out after {} ms.", timeout_ms))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use tempfile::tempdir;
    use tokio::fs;

    #[tokio::test]
    async fn test_bash_tool_echo() -> Result<()> {
        let dir = tempdir()?;
        let working_dir = dir.path().to_path_buf();
        let tool = BashTool {
            working_directory: working_dir.clone(),
            safe_mode: false,
        };
        let args = json!({
            "command": "echo Hello world"
        });

        let output = tool
            .call(&args.to_string())
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        assert_eq!(output.trim(), "Hello world");
        Ok(())
    }

    #[tokio::test]
    async fn test_bash_tool_timeout() -> Result<()> {
        let dir = tempdir()?;
        let working_dir = dir.path().to_path_buf();
        let tool = BashTool {
            working_directory: working_dir.clone(),
            safe_mode: false,
        };
        let args = json!({
            "command": "sleep 2; echo done",
            "timeout": 500
        });

        let output = tool
            .call(&args.to_string())
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        assert!(output.contains("Command timed out"));
        Ok(())
    }

    #[tokio::test]
    async fn test_bash_tool_file_create() -> Result<()> {
        let dir = tempdir()?;
        let working_dir = dir.path().to_path_buf();
        let tool = BashTool {
            working_directory: working_dir.clone(),
            safe_mode: false,
        };

        let args = json!({
            "command": "echo 'Bash content' > test_bash.txt"
        });

        let _ = tool
            .call(&args.to_string())
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let content = fs::read_to_string(working_dir.join("test_bash.txt")).await?;
        assert_eq!(content.trim(), "Bash content");
        Ok(())
    }

    #[tokio::test]
    async fn test_bash_tool_workdir() -> Result<()> {
        let dir = tempdir()?;
        let working_dir = dir.path().to_path_buf();
        let sub_dir = working_dir.join("sub");
        fs::create_dir(&sub_dir).await?;

        let tool = BashTool {
            working_directory: working_dir.clone(),
            safe_mode: false,
        };

        let args = json!({
            "command": "pwd",
            "workdir": "sub"
        });

        let output = tool
            .call(&args.to_string())
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        assert!(output.contains("sub"));
        Ok(())
    }

    #[tokio::test]
    async fn test_bash_tool_safe_mode() -> Result<()> {
        let dir = tempdir()?;
        let working_dir = dir.path().to_path_buf();
        let tool = BashTool {
            working_directory: working_dir.clone(),
            safe_mode: true,
        };

        let args = json!({
            "command": "echo hello"
        });
        let output = tool
            .call(&args.to_string())
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        assert_eq!(output.trim(), "hello");

        let args = json!({
            "command": "rm -rf /"
        });
        let output = tool
            .call(&args.to_string())
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        assert!(output.contains("not allowed in safe mode"));

        let args = json!({
            "command": "echo hello; rm -rf /"
        });
        let output = tool
            .call(&args.to_string())
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        assert!(output.contains("not allowed in safe mode"));

        Ok(())
    }
}