//! File Listing Tool
//!
//! This tool lists files and directories in a specified path, similar to the `ls` command
//! but with recursive capabilities.

use crate::fs_utils::{resolve_safe_path, into_tool_error, parse_empty_args};
use gewu_core::traits::Tool;
use anyhow::Result;
use ignore::WalkBuilder;
use serde_json::{Value, json};
use std::path::PathBuf;
use async_trait::async_trait;
use std::error::Error;

/// Tool for listing files.
pub struct ListTool {
    pub working_directory: PathBuf,
}

#[async_trait]
impl Tool for ListTool {
    fn name(&self) -> &str {
        "list"
    }

    fn description(&self) -> &str {
        include_str!("prompts/ls.txt")
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "The directory to list (default: current directory)" },
                "ignore": { "type": "array", "items": { "type": "string" }, "description": "List of glob patterns to ignore" }
            }
        })
    }

    async fn call(&self, args: &str) -> Result<String, Box<dyn Error + Send + Sync>> {
        if args.trim().is_empty() {
            return Err(anyhow::anyhow!(
                "{}", parse_empty_args("glob", &[("pattern", "string"), ("path", "string (optional)")])
            ).into());
        }
        let args_val: Value = serde_json::from_str(args).map_err(into_tool_error)?;
        
        let path_str = args_val["path"].as_str().unwrap_or(".");
        let ignores: Option<Vec<String>> = if let Some(arr) = args_val["ignore"].as_array() {
            Some(arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
        } else {
            None
        };

        let root_path = resolve_safe_path(&self.working_directory, path_str).map_err(into_tool_error)?;

        let mut builder = WalkBuilder::new(&root_path);
        builder.hidden(false); // Default to showing hidden unless ignored
        builder.git_ignore(true);
        
        if let Some(ignores) = ignores {
            let mut overrides = ignore::overrides::OverrideBuilder::new(&root_path);
            for ignore in ignores {
                 let _ = overrides.add(&format!("!{}", ignore));
            }
            if let Ok(ov) = overrides.build() {
                builder.overrides(ov);
            }
        }

        let limit = 500;
        let mut files: Vec<String> = builder
            .build()
            .filter_map(|result| result.ok())
            .filter_map(|entry| {
                let path = entry.path();
                if path == root_path {
                    return None;
                }

                let relative = path.strip_prefix(&root_path).unwrap_or(path);
                let display = relative.to_string_lossy().to_string();
                let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
                Some(if is_dir {
                    format!("{}/", display)
                } else {
                    display
                })
            })
            .take(limit + 1)
            .collect();

        let truncated = files.len() > limit;
        if truncated {
            files.truncate(limit);
            files.push("... (truncated)".to_string());
        }
        
        files.sort();
        Ok(files.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use tempfile::tempdir;
    use tokio::fs;

    #[tokio::test]
    async fn test_list_tool() -> Result<()> {
        let dir = tempdir()?;
        let working_dir = dir.path().to_path_buf();
        
        // Create structure:
        // /
        //   a.txt
        //   sub/
        //     b.rs
        //     ignored.log
        
        fs::write(dir.path().join("a.txt"), "").await?;
        let sub_dir = dir.path().join("sub");
        fs::create_dir(&sub_dir).await?;
        fs::write(sub_dir.join("b.rs"), "").await?;
        fs::write(sub_dir.join("ignored.log"), "").await?;

        let tool = ListTool { working_directory: working_dir.clone() };

        // 1. Basic list
        let args1 = json!({});
        let output1 = tool.call(&args1.to_string()).await.map_err(|e| anyhow::anyhow!(e.to_string()))?;
        
        // Output should contain relative paths
        assert!(output1.contains("a.txt"));
        assert!(output1.contains("sub/"));
        assert!(output1.contains("sub/b.rs"));
        assert!(output1.contains("sub/ignored.log"));

        // 2. Ignore pattern
        let args2 = json!({
            "ignore": ["*.log"]
        });
        let output2 = tool.call(&args2.to_string()).await.map_err(|e| anyhow::anyhow!(e.to_string()))?;
        
        assert!(output2.contains("a.txt"));
        assert!(output2.contains("sub/b.rs"));
        assert!(!output2.contains("sub/ignored.log"));

        // 3. List subdirectory
        let args3 = json!({
            "path": "sub"
        });
        let output3 = tool.call(&args3.to_string()).await.map_err(|e| anyhow::anyhow!(e.to_string()))?;
        
        // Should list contents relative to 'sub'
        assert!(!output3.contains("a.txt"));
        assert!(output3.contains("b.rs")); // not sub/b.rs

        // 4. Sandbox violation
        let args4 = json!({
            "path": "../"
        });
        let result = tool.call(&args4.to_string()).await;
        assert!(result.is_err());

        Ok(())
    }
}
