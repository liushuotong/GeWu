//! File Listing Tool
//!
//! This tool lists files and directories in a specified path, similar to the
//! `ls` command. The listing is shallow by default (direct children only);
//! `recursive: true` opts into a depth-capped walk.

use crate::fs_utils::{into_tool_error, resolve_read_path};
use anyhow::Result;
use async_trait::async_trait;
use gewu_core::traits::Tool;
use gewu_core::types::{ToolContext, ToolResult};
use ignore::WalkBuilder;
use schemars::schema::RootSchema;
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::path::PathBuf;

/// Default cap on entries returned by a single `list` call.
const DEFAULT_LIST_LIMIT: usize = 500;
/// Default directory depth for recursive listings.
const DEFAULT_LIST_MAX_DEPTH: usize = 3;
/// Hard cap on entries collected before sorting, so a recursive walk over a
/// huge tree cannot exhaust memory.
const MAX_COLLECTED_ENTRIES: usize = 10_000;

fn default_list_limit() -> usize {
    DEFAULT_LIST_LIMIT
}

fn default_list_max_depth() -> usize {
    DEFAULT_LIST_MAX_DEPTH
}

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct ListArgs {
    /// The directory to list. If not specified, the current working directory
    /// will be used.
    pub path: Option<String>,
    /// Optional array of glob patterns to exclude from the results.
    pub ignore: Option<Vec<String>>,
    /// Recurse into subdirectories. By default only the direct children of
    /// `path` are listed, which keeps the output representative of the whole
    /// workspace instead of a sample of one subtree.
    #[serde(default)]
    pub recursive: bool,
    /// Include hidden files and dot-directories in the results. Hidden
    /// entries are excluded by default; `.git` is always excluded.
    #[serde(default)]
    pub hidden: bool,
    /// Maximum number of entries to return.
    #[serde(default = "default_list_limit")]
    pub limit: usize,
    /// Maximum depth to descend when `recursive` is true (1 lists only the
    /// direct children).
    #[serde(default = "default_list_max_depth")]
    pub max_depth: usize,
}

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

    fn schema(&self) -> RootSchema {
        schemars::schema_for!(ListArgs)
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, Box<dyn Error + Send + Sync>> {
        let input: ListArgs = serde_json::from_value(args).map_err(into_tool_error)?;

        let path_str = input.path.as_deref().unwrap_or(".");

        let working_directory = ctx.active_working_directory(&self.working_directory);
        let root_path = resolve_read_path(working_directory, path_str, ctx.enforcement)
            .await
            .map_err(into_tool_error)?;

        let depth = if input.recursive {
            Some(input.max_depth.max(1))
        } else {
            Some(1)
        };

        let mut builder = WalkBuilder::new(&root_path);
        builder.hidden(!input.hidden);
        builder.git_ignore(true);
        builder.max_depth(depth);
        builder.filter_entry(|entry| entry.file_name() != ".git");

        if let Some(ignores) = &input.ignore {
            let mut overrides = ignore::overrides::OverrideBuilder::new(&root_path);
            for ignore in ignores {
                let _ = overrides.add(&format!("!{}", ignore));
            }
            if let Ok(ov) = overrides.build() {
                builder.overrides(ov);
            }
        }

        let mut files: Vec<String> = Vec::new();
        let mut overflow = false;
        for entry in builder.build().flatten() {
            let path = entry.path();
            if path == root_path {
                continue;
            }

            let relative = path.strip_prefix(&root_path).unwrap_or(path);
            let display = relative.to_string_lossy().to_string();
            let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
            files.push(if is_dir {
                format!("{}/", display)
            } else {
                display
            });
            if files.len() > MAX_COLLECTED_ENTRIES {
                overflow = true;
                break;
            }
        }

        files.sort();

        let total = files.len();
        let truncated = overflow || total > input.limit;
        if truncated {
            files.truncate(input.limit);
        }

        let output = if truncated {
            let total_display = if overflow {
                format!("{}+", MAX_COLLECTED_ENTRIES)
            } else {
                total.to_string()
            };
            format!(
                "{}\n\n(Showing {} of {} entries. Use a more specific path or the 'ignore' parameter to narrow the results)",
                files.join("\n"),
                files.len(),
                total_display
            )
        } else {
            files.join("\n")
        };

        Ok(ToolResult::new("list", output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use tempfile::tempdir;
    use tokio::fs;

    async fn make_tool(dir: &tempfile::TempDir) -> ListTool {
        ListTool {
            working_directory: dir.path().to_path_buf(),
        }
    }

    async fn run(tool: &ListTool, args: serde_json::Value) -> Result<ToolResult> {
        let ctx = ToolContext::new("test", "msg");
        tool.execute(args, &ctx)
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))
    }

    #[tokio::test]
    async fn test_list_tool() -> Result<()> {
        let dir = tempdir()?;
        let working_dir = dir.path().to_path_buf();

        fs::write(dir.path().join("a.txt"), "").await?;
        let sub_dir = dir.path().join("sub");
        fs::create_dir(&sub_dir).await?;
        fs::write(sub_dir.join("b.rs"), "").await?;

        let tool = ListTool {
            working_directory: working_dir,
        };

        let args = serde_json::json!({});
        let ctx = ToolContext::new("test", "msg");
        let result = tool
            .execute(args, &ctx)
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;

        assert!(result.output.contains("a.txt"));
        assert!(result.output.contains("sub/"));
        Ok(())
    }

    #[tokio::test]
    async fn test_list_shallow_by_default() -> Result<()> {
        let dir = tempdir()?;
        fs::write(dir.path().join("top.txt"), "").await?;
        let nested = dir.path().join("sub/nested");
        fs::create_dir_all(&nested).await?;
        fs::write(nested.join("deep.txt"), "").await?;

        let tool = make_tool(&dir).await;
        let result = run(&tool, serde_json::json!({})).await?;

        assert!(result.output.contains("top.txt"));
        assert!(result.output.contains("sub/"));
        assert!(
            !result.output.contains("deep.txt") && !result.output.contains("nested/"),
            "shallow listing must not descend into subdirectories: {}",
            result.output
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_list_recursive_lists_descendants() -> Result<()> {
        let dir = tempdir()?;
        let nested = dir.path().join("sub/nested");
        fs::create_dir_all(&nested).await?;
        fs::write(nested.join("deep.txt"), "").await?;

        let tool = make_tool(&dir).await;
        let result = run(&tool, serde_json::json!({ "recursive": true })).await?;

        assert!(result.output.contains("sub/"));
        assert!(result.output.contains("sub/nested/"));
        assert!(result.output.contains("sub/nested/deep.txt"));
        Ok(())
    }

    #[tokio::test]
    async fn test_list_recursive_respects_max_depth() -> Result<()> {
        let dir = tempdir()?;
        let deep = dir.path().join("a/b/c");
        fs::create_dir_all(&deep).await?;
        fs::write(dir.path().join("a/b/c.txt"), "").await?;
        fs::write(deep.join("d.txt"), "").await?;

        let tool = make_tool(&dir).await;

        let default_depth = run(&tool, serde_json::json!({ "recursive": true })).await?;
        assert!(default_depth.output.contains("a/b/c.txt"));
        assert!(
            !default_depth.output.contains("a/b/c/d.txt"),
            "default recursive listing must stay within depth {}: {}",
            DEFAULT_LIST_MAX_DEPTH,
            default_depth.output
        );

        let deeper = run(
            &tool,
            serde_json::json!({ "recursive": true, "max_depth": 4 }),
        )
        .await?;
        assert!(deeper.output.contains("a/b/c/d.txt"));
        Ok(())
    }

    #[tokio::test]
    async fn test_list_hides_hidden_and_git_unless_requested() -> Result<()> {
        let dir = tempdir()?;
        let git_objects = dir.path().join(".git/objects");
        fs::create_dir_all(&git_objects).await?;
        fs::write(git_objects.join("abc"), "").await?;
        fs::write(dir.path().join(".hidden.txt"), "").await?;
        for name in ["a", "b"] {
            fs::create_dir(dir.path().join(name)).await?;
            fs::write(dir.path().join(name).join("README.md"), "").await?;
        }

        let tool = make_tool(&dir).await;

        let default = run(&tool, serde_json::json!({})).await?;
        assert!(default.output.contains("a/"));
        assert!(default.output.contains("b/"));
        assert!(
            !default.output.contains(".git") && !default.output.contains(".hidden"),
            "hidden entries must be excluded by default: {}",
            default.output
        );

        let with_hidden = run(&tool, serde_json::json!({ "hidden": true })).await?;
        assert!(with_hidden.output.contains(".hidden.txt"));
        assert!(
            !with_hidden.output.contains(".git"),
            "'.git' must be excluded even when hidden entries are requested: {}",
            with_hidden.output
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_list_issue8_git_objects_do_not_starve_siblings() -> Result<()> {
        let dir = tempdir()?;
        // Reproduction for GH issue #8: a/.git/objects holds 600 loose
        // objects; listing the workspace must still surface b/ and c/.
        let git_objects = dir.path().join("a/.git/objects");
        fs::create_dir_all(&git_objects).await?;
        for i in 0..600 {
            std::fs::write(git_objects.join(i.to_string()), "").map_err(anyhow::Error::from)?;
        }
        for name in ["a", "b", "c"] {
            fs::create_dir_all(dir.path().join(name)).await?;
            fs::write(dir.path().join(name).join("README.md"), "").await?;
        }

        let tool = make_tool(&dir).await;

        let shallow = run(&tool, serde_json::json!({})).await?;
        for expected in ["a/", "b/", "c/"] {
            assert!(
                shallow.output.contains(expected),
                "shallow listing missed {expected}: {}",
                shallow.output
            );
        }

        let recursive = run(&tool, serde_json::json!({ "recursive": true })).await?;
        for expected in ["a/", "b/", "c/", "a/README.md"] {
            assert!(
                recursive.output.contains(expected),
                "recursive listing missed {expected}: {}",
                recursive.output
            );
        }
        assert!(
            !recursive.output.contains(".git/"),
            "'.git' contents leaked into the listing: {}",
            recursive.output
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_list_sorts_and_reports_truncation() -> Result<()> {
        let dir = tempdir()?;
        for i in 0..600 {
            std::fs::write(dir.path().join(format!("f{i:03}.txt")), "")
                .map_err(anyhow::Error::from)?;
        }

        let tool = make_tool(&dir).await;

        let truncated = run(&tool, serde_json::json!({})).await?;
        assert!(
            truncated.output.contains("(Showing 500 of 600 entries"),
            "truncation total missing: {}",
            truncated.output
        );
        assert!(truncated.output.contains("f000.txt"));
        assert!(truncated.output.contains("f499.txt"));
        assert!(
            !truncated.output.contains("f599.txt"),
            "entries beyond the limit must not be returned: {}",
            truncated.output
        );

        let full = run(&tool, serde_json::json!({ "limit": 600 })).await?;
        assert!(
            !full.output.contains("(Showing"),
            "full listing must not be marked truncated: {}",
            full.output
        );
        assert!(full.output.contains("f599.txt"));
        Ok(())
    }

    #[tokio::test]
    async fn test_list_reports_overflow_cap() -> Result<()> {
        let dir = tempdir()?;
        let count = MAX_COLLECTED_ENTRIES + 1;
        for i in 0..count {
            std::fs::write(dir.path().join(format!("g{i:05}.txt")), "")
                .map_err(anyhow::Error::from)?;
        }

        let tool = make_tool(&dir).await;
        let result = run(&tool, serde_json::json!({})).await?;

        assert!(
            result.output.contains(&format!(
                "(Showing 500 of {}+ entries",
                MAX_COLLECTED_ENTRIES
            )),
            "overflow cap not reported: {}",
            result.output
        );
        Ok(())
    }
}
