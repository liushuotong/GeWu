//! Filesystem Tools
//!
//! This module implements tools for interacting with the local filesystem.
//! All file operations are restricted to a configured working directory
//! for security.
//!
//! # Security
//!
//! All file paths are resolved through [`resolve_safe_path`](crate::fs_utils::resolve_safe_path)
//! which prevents directory traversal attacks. Paths cannot escape the
//! configured working directory.
//!
//! # Available Tools
//!
//! - [`ReadFileTool`] - Reads content from files or lists directories
//! - [`WriteFileTool`] - Writes content to files (overwrites existing)
//! - [`EditFileTool`] - Modifies existing files by replacing text
//! - [`GlobTool`] - Finds files using glob patterns, respects .gitignore
//!
//! # Media Handling
//!
//! Image and PDF files are returned as base64-encoded data URIs with
//! the format `{"type":"media","mime":"<type>","data":"<base64>","message":"..."}`.

use crate::fs_utils::{
    glob_with_gitignore, into_tool_error, is_binary_file, mime_type, parse_empty_args, read_bytes,
    read_text, resolve_safe_path, write,
};
use anyhow::Result;
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use gewu_core::traits::Tool;
use serde_json::{Value, json};
use std::error::Error;
use std::path::PathBuf;
use tokio::io::BufReader;

const DEFAULT_READ_LIMIT: usize = 2000;
const MAX_LINE_LENGTH: usize = 2000;
const MAX_LINE_SUFFIX: &str = "... (line truncated to 2000 chars)";
const MAX_BYTES: usize = 50 * 1024;
const MAX_BYTES_LABEL: &str = "50 KB";

/// Tool for reading files or listing directories.
///
/// # Capabilities
///
/// - **Files**: Reads text files with line numbering and pagination
/// - **Binary detection**: Refuses to read binary files
/// - **Images/PDFs**: Returns as base64-encoded data URIs for vision models
/// - **Directories**: Lists directory contents with type indicators
/// - **Fuzzy matching**: Suggests similar files when a file is not found
///
/// # Output Format
///
/// File output includes:
/// - `<path>`: The file path
/// - `<type>`: "file" or "directory"
/// - `<content>`: Line-numbered file content
/// - Truncation indicators for large files
pub struct ReadFileTool {
    /// The root directory for file operations. All paths are resolved relative to this.
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

    /// Reads a file or lists a directory.
    ///
    /// # Arguments
    ///
    /// * `filePath` - Path to the file or directory
    /// * `limit` - Maximum lines to read (default: 2000)
    /// * `offset` - Starting line number, 1-indexed (default: 1)
    ///
    /// # Returns
    ///
    /// For files: `<path>`, `<type>file</type>`, `<content>` with line numbers
    /// For directories: `<path>`, `<type>directory</type>`, `<entries>`
    /// For images/PDFs: JSON media payload with base64 data
    ///
    /// # Errors
    ///
    /// - File not found (with fuzzy matching suggestions)
    /// - Binary file attempted to be read
    /// - Offset out of range
    async fn call(&self, args: &str) -> Result<String, Box<dyn Error + Send + Sync>> {
        if args.trim().is_empty() {
            return Err(anyhow::anyhow!(
                "{}",
                parse_empty_args(
                    "read_file",
                    &[
                        ("filePath", "string"),
                        ("limit", "number (optional)"),
                        ("offset", "number (optional)")
                    ]
                )
            )
            .into());
        }
        let args_val: Value = serde_json::from_str(args).map_err(into_tool_error)?;
        let file_path_str = args_val["filePath"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("filePath is required"))
            .map_err(into_tool_error)?;

        let file_path =
            resolve_safe_path(&self.working_directory, file_path_str).map_err(into_tool_error)?;

        let metadata = tokio::fs::metadata(&file_path).await;

        if metadata.is_err() {
            let parent = file_path.parent().unwrap_or(file_path.as_path());
            let basename = file_path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.rsplit('.').next())
                .unwrap_or("");

            if tokio::fs::metadata(parent).await.is_ok() {
                let mut suggestions: Vec<String> = Vec::new();
                let mut dir_stream = tokio::fs::read_dir(parent).await.map_err(into_tool_error)?;

                while let Some(entry) = dir_stream.next_entry().await.map_err(into_tool_error)? {
                    let name = entry.file_name().to_string_lossy().to_string();
                    let name_lower = name.to_lowercase();
                    let base_lower = basename.to_lowercase();
                    if name_lower.contains(&base_lower) || base_lower.contains(&name_lower) {
                        suggestions.push(name);
                    }
                    if suggestions.len() >= 3 {
                        break;
                    }
                }

                if !suggestions.is_empty() {
                    return Err(anyhow::anyhow!(
                        "File not found: {}\n\nDid you mean one of these?\n{}",
                        file_path_str,
                        suggestions.join("\n")
                    )
                    .into());
                }
            }

            return Err(anyhow::anyhow!("File not found: {}", file_path_str).into());
        }

        let meta = metadata.unwrap();

        if meta.is_dir() {
            let mut entries: Vec<String> = Vec::new();
            let mut dir_stream = tokio::fs::read_dir(&file_path)
                .await
                .map_err(into_tool_error)?;

            while let Some(entry) = dir_stream.next_entry().await.map_err(into_tool_error)? {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().to_string();

                if path.is_symlink() {
                    let sym_meta = tokio::fs::symlink_metadata(&path).await.ok();
                    if sym_meta.as_ref().is_some_and(|m| m.is_dir()) {
                        entries.push(format!("{}/", name));
                        continue;
                    }
                }

                if path.is_dir() {
                    entries.push(format!("{}/", name));
                } else {
                    entries.push(name);
                }
            }

            entries.sort();

            let limit = args_val["limit"]
                .as_u64()
                .unwrap_or(DEFAULT_READ_LIMIT as u64) as usize;
            let offset = args_val["offset"].as_u64().unwrap_or(1) as usize;
            let start = offset.saturating_sub(1);
            let sliced: Vec<&str> = entries
                .iter()
                .skip(start)
                .take(limit)
                .map(|s| s.as_str())
                .collect();
            let truncated = start + sliced.len() < entries.len();

            let output = if truncated {
                format!(
                    "<path>{}</path>\n<type>directory</type>\n<entries>\n{}\n</entries>\n\n(Showing {} of {} entries. Use 'offset' parameter to read beyond entry {})",
                    file_path_str,
                    sliced.join("\n"),
                    sliced.len(),
                    entries.len(),
                    offset + sliced.len()
                )
            } else {
                format!(
                    "<path>{}</path>\n<type>directory</type>\n<entries>\n{}\n</entries>\n\n({} entries)",
                    file_path_str,
                    sliced.join("\n"),
                    entries.len()
                )
            };

            return Ok(output);
        }

        let mime = mime_type(&file_path);
        let is_image = mime.starts_with("image/")
            && mime != "image/svg+xml"
            && mime != "image/vnd.fastbidsheet";
        let is_pdf = mime == "application/pdf";

        if is_image || is_pdf {
            let data = read_bytes(&file_path).await.map_err(into_tool_error)?;
            let encoded = BASE64.encode(&data);
            let msg = if is_image { "Image" } else { "PDF" };
            let result = json!({
                "type": "media",
                "mime": mime,
                "data": encoded,
                "message": format!("{} read successfully", msg)
            });
            return Ok(result.to_string());
        }

        let is_binary = is_binary_file(&file_path, meta.len())
            .await
            .map_err(into_tool_error)?;
        if is_binary {
            return Err(anyhow::anyhow!("Cannot read binary file: {}", file_path_str).into());
        }

        let limit = args_val["limit"]
            .as_u64()
            .unwrap_or(DEFAULT_READ_LIMIT as u64) as usize;
        let offset = args_val["offset"].as_u64().unwrap_or(1) as usize;
        let start = offset.saturating_sub(1);

        let file = tokio::fs::File::open(&file_path)
            .await
            .map_err(into_tool_error)?;
        let reader = BufReader::new(file);
        let mut lines: Vec<String> = Vec::new();
        let mut total_lines: usize = 0;
        let mut bytes: usize = 0;
        let mut truncated_by_bytes = false;
        let mut has_more_lines = false;

        let mut reader = reader;
        let mut line_iter = tokio::io::AsyncBufReadExt::lines(&mut reader);

        while let Some(text) = line_iter.next_line().await.map_err(into_tool_error)? {
            total_lines += 1;
            if total_lines <= start {
                continue;
            }

            if lines.len() >= limit {
                has_more_lines = true;
                continue;
            }

            let truncated_line = if text.len() > MAX_LINE_LENGTH {
                format!("{}{}", &text[..MAX_LINE_LENGTH], MAX_LINE_SUFFIX)
            } else {
                text.clone()
            };

            let line_size = truncated_line.len() + 1;
            if bytes + line_size > MAX_BYTES {
                truncated_by_bytes = true;
                has_more_lines = true;
                break;
            }

            lines.push(truncated_line);
            bytes += line_size;
        }

        if total_lines < offset && !(total_lines == 0 && offset == 1) {
            return Err(anyhow::anyhow!(
                "Offset {} is out of range for this file ({} lines)",
                offset,
                total_lines
            )
            .into());
        }

        let content: Vec<String> = lines
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{}: {}", start + i + 1, line))
            .collect();

        let total_lines_count = total_lines;
        let last_read_line = offset + lines.len() - 1;
        let next_offset = last_read_line + 1;
        let has_more_lines = has_more_lines || truncated_by_bytes;

        let mut output = format!(
            "<path>{}</path>\n<type>file</type>\n<content>\n{}\n</content>",
            file_path_str,
            content.join("\n")
        );

        if truncated_by_bytes {
            output.push_str(&format!(
                "\n\n(Output capped at {}. Showing lines {}-{}. Use offset={} to continue.)",
                MAX_BYTES_LABEL, offset, last_read_line, next_offset
            ));
        } else if has_more_lines {
            output.push_str(&format!(
                "\n\n(Showing lines {}-{} of {}. Use offset={} to continue.)",
                offset, last_read_line, total_lines_count, next_offset
            ));
        } else {
            output.push_str(&format!(
                "\n\n(End of file - total {} lines)",
                total_lines_count
            ));
        }

        Ok(output)
    }
}

/// Tool for writing files.
///
/// Creates new files or overwrites existing files with new content.
/// The file path must be within the configured working directory.
pub struct WriteFileTool {
    /// The root directory for file operations.
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

    /// Writes content to a file.
    ///
    /// # Arguments
    ///
    /// * `filePath` - Absolute path to the file (must be within working directory)
    /// * `content` - The content to write
    ///
    /// # Returns
    ///
    /// Success message with the file path.
    async fn call(&self, args: &str) -> Result<String, Box<dyn Error + Send + Sync>> {
        if args.trim().is_empty() {
            return Err(anyhow::anyhow!(
                "{}",
                parse_empty_args(
                    "write_file",
                    &[("filePath", "string"), ("content", "string")]
                )
            )
            .into());
        }
        let args_val: Value = serde_json::from_str(args).map_err(into_tool_error)?;
        let file_path_str = args_val["filePath"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("filePath is required"))
            .map_err(into_tool_error)?;
        let content = args_val["content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("content is required"))
            .map_err(into_tool_error)?;

        let file_path =
            resolve_safe_path(&self.working_directory, file_path_str).map_err(into_tool_error)?;

        write(&file_path, content.as_bytes())
            .await
            .map_err(into_tool_error)?;
        Ok(format!(
            "Successfully wrote to {}",
            file_path.to_string_lossy()
        ))
    }
}

/// Tool for editing files by replacing text.
///
/// Finds and replaces text in existing files. By default, only performs
/// a single replacement and errors if multiple matches are found (use
/// `replaceAll: true` to replace all occurrences).
pub struct EditFileTool {
    /// The root directory for file operations.
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

    /// Edits a file by replacing text.
    ///
    /// # Arguments
    ///
    /// * `filePath` - Path to the file
    /// * `oldString` - The exact text to find and replace
    /// * `newString` - The replacement text
    /// * `replaceAll` - If true, replace all occurrences (default: false)
    ///
    /// # Errors
    ///
    /// - `oldString` not found in file
    /// - Multiple matches found when `replaceAll` is false
    async fn call(&self, args: &str) -> Result<String, Box<dyn Error + Send + Sync>> {
        if args.trim().is_empty() {
            return Err(anyhow::anyhow!(
                "{}",
                parse_empty_args(
                    "edit_file",
                    &[
                        ("filePath", "string"),
                        ("oldString", "string"),
                        ("newString", "string"),
                        ("replaceAll", "boolean (optional)")
                    ]
                )
            )
            .into());
        }
        let args_val: Value = serde_json::from_str(args).map_err(into_tool_error)?;
        let file_path_str = args_val["filePath"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("filePath is required"))
            .map_err(into_tool_error)?;
        let old_string = args_val["oldString"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("oldString is required"))
            .map_err(into_tool_error)?;
        let new_string = args_val["newString"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("newString is required"))
            .map_err(into_tool_error)?;
        let replace_all = args_val["replaceAll"].as_bool().unwrap_or(false);

        let file_path =
            resolve_safe_path(&self.working_directory, file_path_str).map_err(into_tool_error)?;

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

        write(&file_path, new_content.as_bytes())
            .await
            .map_err(into_tool_error)?;
        Ok(format!(
            "Successfully modified {}",
            file_path.to_string_lossy()
        ))
    }
}

/// Tool for finding files using glob patterns.
///
/// Searches for files matching a glob pattern within the working directory.
/// Automatically respects `.gitignore` files to exclude ignored files.
pub struct GlobTool {
    /// The root directory for file operations.
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

    /// Finds files matching a glob pattern.
    ///
    /// # Arguments
    ///
    /// * `pattern` - Glob pattern (e.g., "**/*.rs", "src/**/*.ts")
    /// * `path` - Directory to search in (default: working directory)
    ///
    /// # Returns
    ///
    /// JSON array of matching file paths as strings.
    ///
    /// # Gitignore
    ///
    /// Files matching patterns in `.gitignore` are automatically excluded.
    async fn call(&self, args: &str) -> Result<String, Box<dyn Error + Send + Sync>> {
        if args.trim().is_empty() {
            return Err(anyhow::anyhow!(
                "{}",
                parse_empty_args(
                    "glob",
                    &[("pattern", "string"), ("path", "string (optional)")]
                )
            )
            .into());
        }
        let args_val: Value = serde_json::from_str(args).map_err(into_tool_error)?;
        let pattern = args_val["pattern"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("pattern is required"))
            .map_err(into_tool_error)?;

        let search_dir = if let Some(p) = args_val.get("path").and_then(|v| v.as_str()) {
            resolve_safe_path(&self.working_directory, p).map_err(into_tool_error)?
        } else {
            self.working_directory.clone()
        };

        let paths = glob_with_gitignore(&search_dir, pattern).map_err(into_tool_error)?;

        let paths_str: Vec<String> = paths
            .into_iter()
            .filter(|p| {
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

        let tool = ReadFileTool {
            working_directory: working_dir,
        };
        let args = json!({
            "filePath": file_path.to_str().unwrap()
        });

        let output = tool
            .call(&args.to_string())
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        assert!(output.contains("<path>"));
        assert!(output.contains("<type>file</type>"));
        assert!(output.contains("1: Hello from file"));
        assert!(output.contains("(End of file - total 1 lines)"));
        Ok(())
    }

    #[tokio::test]
    async fn test_write_file_tool() -> Result<()> {
        let dir = tempdir()?;
        let working_dir = dir.path().to_path_buf();
        let file_path = dir.path().join("test_write.txt");

        let tool = WriteFileTool {
            working_directory: working_dir,
        };
        let args = json!({
            "filePath": file_path.to_str().unwrap(),
            "content": "Hello written"
        });

        let output = tool
            .call(&args.to_string())
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
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
        let file3 = dir.path().join("c.rs");
        fs::write(&file1, "").await?;
        fs::write(&file2, "").await?;
        fs::write(&file3, "").await?;

        let tool = GlobTool {
            working_directory: working_dir,
        };
        let pattern = "*.txt";
        let args = json!({
            "pattern": pattern
        });

        let output = tool
            .call(&args.to_string())
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let files: Vec<String> = serde_json::from_str(&output)?;

        assert_eq!(files.len(), 2);
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

        let tool = EditFileTool {
            working_directory: working_dir,
        };

        let args_multiple = json!({
            "filePath": file_path.to_str().unwrap(),
            "oldString": "foo",
            "newString": "baz",
            "replaceAll": false
        });
        let result = tool.call(&args_multiple.to_string()).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Found multiple matches")
        );

        let args_all = json!({
            "filePath": file_path.to_str().unwrap(),
            "oldString": "foo",
            "newString": "baz",
            "replaceAll": true
        });
        let _ = tool
            .call(&args_all.to_string())
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let content = fs::read_to_string(&file_path).await?;
        assert_eq!(content, "baz bar baz");

        let args_not_found = json!({
            "filePath": file_path.to_str().unwrap(),
            "oldString": "foo",
            "newString": "baz",
            "replaceAll": false
        });
        let result = tool.call(&args_not_found.to_string()).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("oldString not found")
        );

        fs::write(&file_path, "one two three").await?;
        let args_single = json!({
            "filePath": file_path.to_str().unwrap(),
            "oldString": "two",
            "newString": "2",
            "replaceAll": false
        });
        let _ = tool
            .call(&args_single.to_string())
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let content = fs::read_to_string(&file_path).await?;
        assert_eq!(content, "one 2 three");

        Ok(())
    }

    #[tokio::test]
    async fn test_sandbox_violation() -> Result<()> {
        let dir = tempdir()?;
        let working_dir = dir.path().join("safe_zone");
        tokio::fs::create_dir(&working_dir).await?;

        let secret_path = dir.path().join("secret.txt");
        tokio::fs::write(&secret_path, "secret data").await?;

        let read_tool = ReadFileTool {
            working_directory: working_dir.clone(),
        };
        let args_read = json!({
            "filePath": "../secret.txt"
        });
        let result_read = read_tool.call(&args_read.to_string()).await;
        assert!(result_read.is_err());

        let write_tool = WriteFileTool {
            working_directory: working_dir.clone(),
        };
        let args_write = json!({
            "filePath": "../hacker.txt",
            "content": "hacked"
        });
        let result_write = write_tool.call(&args_write.to_string()).await;
        assert!(result_write.is_err());

        let edit_tool = EditFileTool {
            working_directory: working_dir.clone(),
        };
        let args_edit = json!({
            "filePath": "../secret.txt",
            "oldString": "secret",
            "newString": "leaked",
            "replaceAll": false
        });
        let result_edit = edit_tool.call(&args_edit.to_string()).await;
        assert!(result_edit.is_err());

        let glob_tool = GlobTool {
            working_directory: working_dir.clone(),
        };
        let args_glob = json!({
            "pattern": "../secret.txt"
        });
        let result_glob = glob_tool
            .call(&args_glob.to_string())
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let files: Vec<String> = serde_json::from_str(&result_glob)?;
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

        let tool = GlobTool {
            working_directory: working_dir.clone(),
        };
        let args = json!({
            "pattern": "*.txt"
        });

        let output = tool
            .call(&args.to_string())
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let files: Vec<String> = serde_json::from_str(&output)?;

        let s_keep = keep_file.to_string_lossy().to_string();
        let s_ignore = ignore_file.to_string_lossy().to_string();

        assert!(files.contains(&s_keep), "Should contain keep_me.txt");
        assert!(
            !files.contains(&s_ignore),
            "Should NOT contain ignore_me.txt"
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_read_file_tool_directory() -> Result<()> {
        let dir = tempdir()?;
        let working_dir = dir.path().to_path_buf();
        let subdir = dir.path().join("subdir");
        tokio::fs::create_dir(&subdir).await?;
        fs::write(&dir.path().join("file1.txt"), "content1").await?;
        fs::write(&dir.path().join("file2.txt"), "content2").await?;
        fs::write(&subdir.join("nested.txt"), "nested").await?;

        let tool = ReadFileTool {
            working_directory: working_dir.clone(),
        };
        let args = json!({
            "filePath": dir.path().to_str().unwrap()
        });

        let output = tool
            .call(&args.to_string())
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        assert!(output.contains("<path>"));
        assert!(output.contains("<type>directory</type>"));
        assert!(output.contains("<entries>"));
        assert!(output.contains("file1.txt"));
        assert!(output.contains("file2.txt"));
        assert!(output.contains("subdir/"));
        assert!(output.contains("(3 entries)"));
        Ok(())
    }

    #[tokio::test]
    async fn test_read_file_tool_not_found() -> Result<()> {
        let dir = tempdir()?;
        let working_dir = dir.path().to_path_buf();
        fs::write(&dir.path().join("similar.txt"), "content").await?;

        let tool = ReadFileTool {
            working_directory: working_dir.clone(),
        };
        let args = json!({
            "filePath": dir.path().join("similr.txt").to_str().unwrap()
        });

        let result = tool.call(&args.to_string()).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        eprintln!("ERROR: {}", err);
        assert!(err.contains("Did you mean"));
        assert!(err.contains("similar.txt"));
        Ok(())
    }

    #[tokio::test]
    async fn test_read_file_tool_offset_limit() -> Result<()> {
        let dir = tempdir()?;
        let working_dir = dir.path().to_path_buf();
        let file_path = dir.path().join("multiline.txt");
        let content: String = (1..=10).map(|i| format!("Line {}\n", i)).collect();
        fs::write(&file_path, content).await?;

        let tool = ReadFileTool {
            working_directory: working_dir,
        };
        let args = json!({
            "filePath": file_path.to_str().unwrap(),
            "offset": 3,
            "limit": 4
        });

        let output = tool
            .call(&args.to_string())
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        assert!(output.contains("3: Line 3"));
        assert!(output.contains("6: Line 6"));
        assert!(output.contains("Showing lines 3-6 of 10"));
        Ok(())
    }

    #[tokio::test]
    async fn test_read_file_tool_truncated_bytes() -> Result<()> {
        let dir = tempdir()?;
        let working_dir = dir.path().to_path_buf();
        let file_path = dir.path().join("large.txt");
        let line = "x".repeat(100);
        let content = line.clone() + "\n";
        let repeated: String = content.repeat(600);
        fs::write(&file_path, repeated).await?;

        let tool = ReadFileTool {
            working_directory: working_dir,
        };
        let args = json!({
            "filePath": file_path.to_str().unwrap()
        });

        let output = tool
            .call(&args.to_string())
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        assert!(output.contains("Output capped at 50 KB"));
        Ok(())
    }
}