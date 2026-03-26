//! Filesystem Tools
//!
//! This module implements tools for interacting with the local filesystem:
//! - `ReadFileTool`: Reads content from files or lists directories.
//! - `WriteFileTool`: Writes content to files (overwriting).
//! - `EditFileTool`: Modifies existing files by replacing text.
//! - `GlobTool`: Finds files using glob patterns.

use crate::fs_utils::{resolve_safe_path, read_text, write, glob_with_gitignore, into_tool_error};
use gewu_core::traits::Tool;
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::error::Error;

/// Tool for reading files or listing directories.
pub struct ReadFileTool {
    pub working_directory: PathBuf,
}

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        include_str!("prompts/read.txt")
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "filePath": {
                    "type": "string",
                    "description": "The absolute path to the file or directory to read"
                },
                "limit": {
                    "type": "number",
                    "description": "The maximum number of lines to read (defaults to 2000)"
                },
                "offset": {
                    "type": "number",
                    "description": "The line number to start reading from (1-indexed)"
                }
            },
            "required": ["filePath"]
        })
    }

    async fn call(&self, args: &str) -> Result<String, Box<dyn Error + Send + Sync>> {
        let args_val: Value = serde_json::from_str(args).map_err(into_tool_error)?;
        let file_path_str = args_val["filePath"].as_str().ok_or_else(|| anyhow::anyhow!("filePath is required")).map_err(into_tool_error)?;
        let limit = args_val["limit"].as_u64().unwrap_or(2000) as usize;
        let offset = args_val["offset"].as_u64().unwrap_or(1) as usize;

        let file_path = resolve_safe_path(&self.working_directory, file_path_str).map_err(into_tool_error)?;

        // Check if it's a directory
        if tokio::fs::metadata(&file_path).await.map_err(into_tool_error)?.is_dir() {
            let mut entries = tokio::fs::read_dir(&file_path).await.map_err(into_tool_error)?;
            let mut lines = Vec::new();
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                if path.is_dir() {
                    lines.push(format!("{}/", name));
                } else {
                    lines.push(name.to_string());
                }
            }
            lines.sort();
            return Ok(lines.join("\n"));
        }

        let content = read_text(&file_path).await.map_err(into_tool_error)?;
        let lines: Vec<&str> = content.lines().collect();
        
        if offset > lines.len() {
            return Ok(String::new());
        }

        let start_index = if offset > 0 { offset - 1 } else { 0 };
        let end_index = (start_index + limit).min(lines.len());
        
        let result_lines: Vec<String> = lines[start_index..end_index]
            .iter()
            .enumerate()
            .map(|(i, line)| {
                let line_num = start_index + i + 1;
                let truncated_line = if line.len() > 2000 {
                    format!("{}... (truncated)", &line[..2000])
                } else {
                    line.to_string()
                };
                format!("{}: {}", line_num, truncated_line)
            })
            .collect();

        Ok(result_lines.join("\n"))
    }
}

/// Tool for writing files.
pub struct WriteFileTool {
    pub working_directory: PathBuf,
}

#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        include_str!("prompts/write.txt")
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "filePath": {
                    "type": "string",
                    "description": "The absolute path to the file to write (must be absolute, not relative)"
                },
                "content": {
                    "type": "string",
                    "description": "The content to write to the file"
                }
            },
            "required": ["content", "filePath"]
        })
    }

    async fn call(&self, args: &str) -> Result<String, Box<dyn Error + Send + Sync>> {
        let args_val: Value = serde_json::from_str(args).map_err(into_tool_error)?;
        let file_path_str = args_val["filePath"].as_str().ok_or_else(|| anyhow::anyhow!("filePath is required")).map_err(into_tool_error)?;
        let content = args_val["content"].as_str().ok_or_else(|| anyhow::anyhow!("content is required")).map_err(into_tool_error)?;

        let file_path = resolve_safe_path(&self.working_directory, file_path_str).map_err(into_tool_error)?;
        
        write(&file_path, content.as_bytes()).await.map_err(into_tool_error)?;
        Ok(format!("Successfully wrote to {}", file_path.to_string_lossy()))
    }
}

/// Tool for editing files by replacing text.
pub struct EditFileTool {
    pub working_directory: PathBuf,
}

#[async_trait]
impl Tool for EditFileTool {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn description(&self) -> &str {
        include_str!("prompts/edit.txt")
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "filePath": {
                    "type": "string",
                    "description": "The absolute path to the file to modify"
                },
                "oldString": {
                    "type": "string",
                    "description": "The text to replace"
                },
                "newString": {
                    "type": "string",
                    "description": "The text to replace it with (must be different from oldString)"
                },
                "replaceAll": {
                    "type": "boolean",
                    "description": "Replace all occurrences of oldString (default false)"
                }
            },
            "required": ["filePath", "oldString", "newString"]
        })
    }

    async fn call(&self, args: &str) -> Result<String, Box<dyn Error + Send + Sync>> {
        let args_val: Value = serde_json::from_str(args).map_err(into_tool_error)?;
        let file_path_str = args_val["filePath"].as_str().ok_or_else(|| anyhow::anyhow!("filePath is required")).map_err(into_tool_error)?;
        let old_string = args_val["oldString"].as_str().ok_or_else(|| anyhow::anyhow!("oldString is required")).map_err(into_tool_error)?;
        let new_string = args_val["newString"].as_str().ok_or_else(|| anyhow::anyhow!("newString is required")).map_err(into_tool_error)?;
        let replace_all = args_val["replaceAll"].as_bool().unwrap_or(false);

        let file_path = resolve_safe_path(&self.working_directory, file_path_str).map_err(into_tool_error)?;

        let content = read_text(&file_path).await.map_err(into_tool_error)?;
        
        let occurrences = content.matches(old_string).count();

        if occurrences == 0 {
            return Err(anyhow::anyhow!("oldString not found in content").into());
        }

        let new_content = if replace_all {
            content.replace(old_string, new_string)
        } else {
            if occurrences > 1 {
                return Err(anyhow::anyhow!("Found multiple matches for oldString. Provide more surrounding lines in oldString to identify the correct match.").into());
            }
            content.replacen(old_string, new_string, 1)
        };

        write(&file_path, new_content.as_bytes()).await.map_err(into_tool_error)?;
        Ok(format!("Successfully modified {}", file_path.to_string_lossy()))
    }
}

/// Tool for finding files using glob patterns.
pub struct GlobTool {
    pub working_directory: PathBuf,
}

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    fn description(&self) -> &str {
        include_str!("prompts/glob.txt")
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The directory to search in. If not specified, the current working directory will be used. IMPORTANT: Omit this field to use the default directory. DO NOT enter \"undefined\" or \"null\" - simply omit it for the default behavior. Must be a valid directory path if provided."
                },
                "pattern": {
                    "type": "string",
                    "description": "The glob pattern to match files against"
                }
            },
            "required": ["pattern"]
        })
    }

    async fn call(&self, args: &str) -> Result<String, Box<dyn Error + Send + Sync>> {
        let args_val: Value = serde_json::from_str(args).map_err(into_tool_error)?;
        let pattern = args_val["pattern"].as_str().ok_or_else(|| anyhow::anyhow!("pattern is required")).map_err(into_tool_error)?;
        
        // Optional path parameter, defaults to current working directory
        let search_dir = if let Some(p) = args_val.get("path").and_then(|v| v.as_str()) {
            resolve_safe_path(&self.working_directory, p).map_err(into_tool_error)?
        } else {
            self.working_directory.clone()
        };

        // Search using glob_with_gitignore which respects .gitignore
        let paths = glob_with_gitignore(&search_dir, pattern).map_err(into_tool_error)?;

        let paths_str: Vec<String> = paths
            .into_iter()
            .filter(|p| {
                // Normalize path to resolve .. and . components to ensure we don't return files outside safe zone
                // e.g. /app/safe/../secret.txt -> /app/secret.txt which does not start with /app/safe
                let normalized = crate::fs_utils::normalize_path(p);
                normalized.starts_with(&self.working_directory)
            })
            .map(|p| p.to_string_lossy().into_owned())
            .collect();

        Ok(serde_json::to_string_pretty(&paths_str).unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use serde_json::json;
    use tempfile::tempdir;
    use tokio::fs;

    #[tokio::test]
    async fn test_read_file_tool() -> Result<()> {
        let dir = tempdir()?;
        let working_dir = dir.path().to_path_buf();
        let file_path = dir.path().join("test_read.txt");
        fs::write(&file_path, "Hello from file").await?;

        let tool = ReadFileTool { working_directory: working_dir };
        let args = json!({
            "filePath": file_path.to_str().unwrap()
        });

        let output = tool.call(&args.to_string()).await.map_err(|e| anyhow::anyhow!(e.to_string()))?;
        assert_eq!(output, "1: Hello from file");
        Ok(())
    }

    #[tokio::test]
    async fn test_write_file_tool() -> Result<()> {
        let dir = tempdir()?;
        let working_dir = dir.path().to_path_buf();
        let file_path = dir.path().join("test_write.txt");

        let tool = WriteFileTool { working_directory: working_dir };
        let args = json!({
            "filePath": file_path.to_str().unwrap(),
            "content": "Hello written"
        });

        let output = tool.call(&args.to_string()).await.map_err(|e| anyhow::anyhow!(e.to_string()))?;
        assert!(output.contains("Successfully wrote"));

        let content = fs::read_to_string(&file_path).await?;
        assert_eq!(content, "Hello written");
        Ok(())
    }

    #[tokio::test]
    async fn test_glob_tool() -> Result<()> {
        let dir = tempdir()?;
        let working_dir = dir.path().to_path_buf();
        let file1 = dir.path().join("a.txt");
        let file2 = dir.path().join("b.txt");
        let file3 = dir.path().join("c.rs"); // Different extension
        fs::write(&file1, "").await?;
        fs::write(&file2, "").await?;
        fs::write(&file3, "").await?;

        let tool = GlobTool { working_directory: working_dir };
        // Search for .txt files
        let pattern = "*.txt";
        let args = json!({
            "pattern": pattern
        });

        let output = tool.call(&args.to_string()).await.map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let files: Vec<String> = serde_json::from_str(&output)?;

        assert_eq!(files.len(), 2);
        // Ordering is not guaranteed by glob/filesystem, so check containment
        let s1 = file1.to_string_lossy().to_string();
        let s2 = file2.to_string_lossy().to_string();
        assert!(files.contains(&s1) || files.contains(&s2));
        Ok(())
    }

    #[tokio::test]
    async fn test_edit_file_tool() -> Result<()> {
        let dir = tempdir()?;
        let working_dir = dir.path().to_path_buf();
        let file_path = dir.path().join("test_edit.txt");
        fs::write(&file_path, "foo bar foo").await?;

        let tool = EditFileTool { working_directory: working_dir };

        // 1. Multiple matches error
        let args_multiple = json!({
            "filePath": file_path.to_str().unwrap(),
            "oldString": "foo",
            "newString": "baz",
            "replaceAll": false
        });
        let result = tool.call(&args_multiple.to_string()).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Found multiple matches"));

        // 2. Replace All
        let args_all = json!({
            "filePath": file_path.to_str().unwrap(),
            "oldString": "foo",
            "newString": "baz",
            "replaceAll": true
        });
        let _ = tool.call(&args_all.to_string()).await.map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let content = fs::read_to_string(&file_path).await?;
        assert_eq!(content, "baz bar baz");

        // 3. Not found error
        let args_not_found = json!({
            "filePath": file_path.to_str().unwrap(),
            "oldString": "foo",
            "newString": "baz",
            "replaceAll": false
        });
        let result = tool.call(&args_not_found.to_string()).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("oldString not found"));

        // 4. Single replacement
        fs::write(&file_path, "one two three").await?;
        let args_single = json!({
            "filePath": file_path.to_str().unwrap(),
            "oldString": "two",
            "newString": "2",
            "replaceAll": false
        });
        let _ = tool.call(&args_single.to_string()).await.map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let content = fs::read_to_string(&file_path).await?;
        assert_eq!(content, "one 2 three");

        Ok(())
    }

    #[tokio::test]
    async fn test_sandbox_violation() -> Result<()> {
        let dir = tempdir()?;
        let working_dir = dir.path().join("safe_zone");
        tokio::fs::create_dir(&working_dir).await?;
        
        // Create a secret file outside
        let secret_path = dir.path().join("secret.txt");
        tokio::fs::write(&secret_path, "secret data").await?;
        
        // 1. ReadFileTool traversal
        let read_tool = ReadFileTool { working_directory: working_dir.clone() };
        let args_read = json!({
            "filePath": "../secret.txt"
        });
        let result_read = read_tool.call(&args_read.to_string()).await;
        assert!(result_read.is_err());
        // assert!(result_read.unwrap_err().to_string().contains("Path traversal detected"));

        // 2. WriteFileTool traversal
        let write_tool = WriteFileTool { working_directory: working_dir.clone() };
        let args_write = json!({
            "filePath": "../hacker.txt",
            "content": "hacked"
        });
        let result_write = write_tool.call(&args_write.to_string()).await;
        assert!(result_write.is_err());

        // 3. EditFileTool traversal
        let edit_tool = EditFileTool { working_directory: working_dir.clone() };
        let args_edit = json!({
            "filePath": "../secret.txt",
            "oldString": "secret",
            "newString": "leaked",
            "replaceAll": false
        });
        let result_edit = edit_tool.call(&args_edit.to_string()).await;
        assert!(result_edit.is_err());
        
        // 4. GlobTool traversal attempt
        let glob_tool = GlobTool { working_directory: working_dir.clone() };
        // Search for the secret file using relative path
        let args_glob = json!({
            "pattern": "../secret.txt"
        });
        let result_glob = glob_tool.call(&args_glob.to_string()).await.map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let files: Vec<String> = serde_json::from_str(&result_glob)?;
        // It should find nothing because results outside working_dir are filtered
        assert!(files.is_empty());

        Ok(())
    }

    #[tokio::test]
    async fn test_glob_tool_gitignore() -> Result<()> {
        let dir = tempdir()?;
        let working_dir = dir.path().to_path_buf();
        let keep_file = dir.path().join("keep_me.txt");
        let ignore_file = dir.path().join("ignore_me.txt");
        let gitignore = dir.path().join(".ignore");
        
        fs::write(&keep_file, "keep").await?;
        fs::write(&ignore_file, "ignore").await?;
        fs::write(&gitignore, "ignore_me.txt").await?;

        // Create the tool instance
        let tool = GlobTool { working_directory: working_dir.clone() };
        let args = json!({
            "pattern": "*.txt"
        });

        // Run the tool
        let output = tool.call(&args.to_string()).await.map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let files: Vec<String> = serde_json::from_str(&output)?;

        // Paths in output are absolute
        let s_keep = keep_file.to_string_lossy().to_string();
        let s_ignore = ignore_file.to_string_lossy().to_string();
        
        // Assertions
        assert!(files.contains(&s_keep), "Should contain keep_me.txt");
        assert!(!files.contains(&s_ignore), "Should NOT contain ignore_me.txt");
        
        Ok(())
    }
}
