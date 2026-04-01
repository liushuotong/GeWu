//! Grep Tool
//!
//! This tool performs fast content searches across the codebase using regular expressions.
//! It uses the `ignore` crate to respect `.gitignore` files and the `grep` crate for
//! efficient searching.

use crate::fs_utils::{into_tool_error, resolve_safe_path};
use anyhow::Result;
use async_trait::async_trait;
use gewu_core::traits::Tool;
use grep::regex::RegexMatcher;
use grep::searcher::{BinaryDetection, SearcherBuilder, Sink, SinkMatch};
use ignore::WalkBuilder;
use serde_json::{Value, json};
use std::error::Error;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Tool for searching text within files.
pub struct GrepTool {
    pub working_directory: PathBuf,
}

/// Represents a single match found by grep.
#[derive(Clone)]
struct Match {
    path: String,
    line_number: u64,
    line_text: String,
}

/// A thread-safe collector for search results.
struct MatchCollector {
    matches: Arc<Mutex<Vec<Match>>>,
    path: PathBuf,
}

impl Sink for MatchCollector {
    type Error = std::io::Error;

    fn matched(
        &mut self,
        _searcher: &grep::searcher::Searcher,
        mat: &SinkMatch,
    ) -> Result<bool, Self::Error> {
        let line_number = mat.line_number().unwrap_or(0);
        let line_text = String::from_utf8_lossy(mat.bytes()).to_string();

        let m = Match {
            path: self.path.to_string_lossy().to_string(),
            line_number,
            line_text: line_text.trim_end().to_string(),
        };

        if let Ok(mut matches) = self.matches.lock() {
            // Hard limit of 1000 matches to prevent OOM or massive outputs
            if matches.len() < 1000 {
                matches.push(m);
            } else {
                return Ok(false); // Stop searching if limit reached
            }
        }
        Ok(true)
    }
}

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        include_str!("prompts/grep.txt")
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "The regex pattern to search for in file contents"
                },
                "path": {
                    "type": "string",
                    "description": "The directory to search in. Defaults to the current working directory."
                },
                "include": {
                    "type": "string",
                    "description": "File pattern to include in the search (e.g. \"*.js\", \"*.{ts,tsx}\")"
                }
            },
            "required": ["pattern"]
        })
    }

    async fn call(&self, args: &str) -> Result<String, Box<dyn Error + Send + Sync>> {
        let args_val: Value = serde_json::from_str(args).map_err(into_tool_error)?;
        let pattern = args_val["pattern"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("pattern is required"))
            .map_err(into_tool_error)?
            .to_string();
        let search_path_str = args_val["path"].as_str().unwrap_or(".");
        let include = args_val["include"].as_str().map(|s| s.to_string());

        let search_path =
            resolve_safe_path(&self.working_directory, search_path_str).map_err(into_tool_error)?;

        // Since `grep` operations are blocking/CPU-bound, we wrap them in `tokio::task::spawn_blocking`
        let result =
            tokio::task::spawn_blocking(move || -> Result<String, Box<dyn Error + Send + Sync>> {
                let matcher = RegexMatcher::new(&pattern).map_err(into_tool_error)?;
                let matches = Arc::new(Mutex::new(Vec::new()));

                let mut builder = WalkBuilder::new(&search_path);
                if let Some(inc) = include {
                    let mut glob_builder = ignore::overrides::OverrideBuilder::new(&search_path);
                    if let Ok(_) = glob_builder.add(&inc) {
                        if let Ok(overrides) = glob_builder.build() {
                            builder.overrides(overrides);
                        }
                    }
                }

                let walker = builder.build();

                let matches_clone_outer = matches.clone();
                let matcher_clone_outer = matcher.clone();

                // Walk the directory tree
                for entry in walker
                    .filter_map(|result| result.ok())
                    .filter(|entry| entry.file_type().is_some_and(|ft| ft.is_file()))
                {
                    let path = entry.path().to_path_buf();
                    let matches_clone = matches_clone_outer.clone();
                    let matcher_clone = matcher_clone_outer.clone();

                    let collector = MatchCollector {
                        matches: matches_clone,
                        path: path.clone(),
                    };

                    // Configure searcher to skip binary files
                    let mut searcher = SearcherBuilder::new()
                        .binary_detection(BinaryDetection::quit(b'\x00'))
                        .build();

                    let _ = searcher.search_path(&matcher_clone, &path, collector);
                }

                let matches_guard = matches.lock().unwrap();
                if matches_guard.is_empty() {
                    return Ok("No matches found.".to_string());
                }

                // Limit output to prevent huge context
                let limit = 100;
                let truncated = matches_guard.len() > limit;
                let display_matches = if truncated {
                    &matches_guard[..limit]
                } else {
                    &matches_guard[..]
                };

                let mut output = display_matches
                    .iter()
                    .map(|m| format!("{}:{}: {}", m.path, m.line_number, m.line_text))
                    .collect::<Vec<String>>()
                    .join("\n");

                if !output.is_empty() {
                    output.push('\n');
                }

                if truncated {
                    output.push_str(&format!(
                        "\n... and {} more matches not shown.",
                        matches_guard.len() - limit
                    ));
                }

                Ok(output)
            })
            .await
            .map_err(into_tool_error)??;

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use tempfile::tempdir;
    use tokio::fs;

    #[tokio::test]
    async fn test_grep_tool() -> Result<()> {
        let dir = tempdir()?;
        let working_dir = dir.path().to_path_buf();
        let file1 = dir.path().join("a.txt");
        let file2 = dir.path().join("b.rs");
        let file3 = dir.path().join("c.txt");

        fs::write(&file1, "fn hello_world() {}").await?;
        fs::write(&file2, "pub fn hello_rust() {}").await?;
        fs::write(&file3, "just a greeting: hello").await?;

        let tool = GrepTool {
            working_directory: working_dir.clone(),
        };

        // Test 1: Basic search
        let args1 = json!({
            "pattern": "hello",
            "path": dir.path().to_str().unwrap()
        });
        let output1 = tool
            .call(&args1.to_string())
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;

        assert!(output1.contains("a.txt"));
        assert!(output1.contains("b.rs"));
        assert!(output1.contains("c.txt"));

        // Test 2: Include filter
        let args2 = json!({
            "pattern": "hello",
            "path": dir.path().to_str().unwrap(),
            "include": "*.rs"
        });
        let output2 = tool
            .call(&args2.to_string())
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;

        assert!(!output2.contains("a.txt"));
        assert!(output2.contains("b.rs"));
        assert!(!output2.contains("c.txt"));

        // Test 3: No matches
        let args3 = json!({
            "pattern": "goodbye",
            "path": dir.path().to_str().unwrap()
        });
        let output3 = tool
            .call(&args3.to_string())
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;

        assert!(output3.contains("No matches found"));

        Ok(())
    }
}
