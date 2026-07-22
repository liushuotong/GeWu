use crate::fs_utils::into_tool_error;
use anyhow::Result;
use async_trait::async_trait;
use gewu_core::traits::{Tool, ToolRegistry};
use gewu_core::types::{ToolContext, ToolResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::error::Error;
use std::sync::Arc;
use tokio::task::JoinSet;

const DISALLOWED: &[&str] = &["batch"];
const MAX_TOOLS: usize = 25;

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct ToolCallInput {
    tool: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct BatchArgs {
    tool_calls: Vec<ToolCallInput>,
}

pub struct BatchTool {
    pub registry: Arc<dyn ToolRegistry>,
}

impl BatchTool {
    pub fn new(registry: Arc<dyn ToolRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for BatchTool {
    fn name(&self) -> &str {
        "batch"
    }

    fn description(&self) -> &str {
        "Execute independent tool calls in parallel. Shell and mutation tools are allowed, but nested batch calls are not."
    }

    fn schema(&self) -> schemars::schema::RootSchema {
        schemars::schema_for!(BatchArgs)
    }

    async fn execute(
        &self,
        args: Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, Box<dyn Error + Send + Sync>> {
        let batch_args: BatchArgs = serde_json::from_value(args).map_err(into_tool_error)?;

        let tool_calls = batch_args
            .tool_calls
            .iter()
            .take(MAX_TOOLS)
            .collect::<Vec<_>>();
        let discarded = batch_args.tool_calls.len() - tool_calls.len();

        let available_tools = self.registry.list_tools();
        let allowed_tools = available_tools
            .iter()
            .filter(|tool| !DISALLOWED.contains(&tool.as_str()))
            .cloned()
            .collect::<std::collections::HashSet<_>>();

        let mut join_set = JoinSet::new();

        for call in &tool_calls {
            let tool_name = call.tool.clone();
            let params = call.parameters.clone();
            let registry = self.registry.clone();
            let allowed = allowed_tools.clone();
            let ctx = ctx.clone();

            if tool_name.is_empty() {
                let tool_name_clone = tool_name.clone();
                join_set.spawn(async move { (tool_name_clone, Err("Empty tool name".into())) });
                continue;
            }

            join_set.spawn(async move {
                let result = async {
                    if DISALLOWED.contains(&tool_name.as_str()) {
                        return Err(format!(
                            "Tool '{}' is not allowed in batch. Disallowed tools: {:?}",
                            tool_name, DISALLOWED
                        )
                        .into());
                    }

                    if !allowed.contains(&tool_name) {
                        return Err(format!(
                            "Tool '{}' not in registry. Available tools: {}",
                            tool_name,
                            allowed.iter().cloned().collect::<Vec<_>>().join(", ")
                        )
                        .into());
                    }

                    registry.execute(&tool_name, params, &ctx).await
                }
                .await;

                (tool_name, result)
            });
        }

        let mut results = Vec::new();
        while let Some(result) = join_set.join_next().await {
            if let Ok((tool_name, res)) = result {
                results.push((tool_name, res));
            }
        }

        let mut errors = Vec::new();
        let mut successes = Vec::new();

        for (tool_name, result) in &results {
            match result {
                Ok(output) => successes.push((tool_name.clone(), output.output.clone())),
                Err(e) => errors.push((tool_name.clone(), e.to_string())),
            }
        }

        let total = results.len();
        let successful = successes.len();
        let failed = errors.len();

        let mut output = String::new();

        if failed > 0 {
            output.push_str(&format!(
                "Executed {}/{} tools successfully. {} failed.\n\n",
                successful, total, failed
            ));
        } else {
            output.push_str(&format!(
                "All {} tools executed successfully.\n\n",
                successful
            ));
        }

        if !successes.is_empty() {
            output.push_str("Results:\n\n");
            for (index, (tool_name, tool_output)) in successes.iter().enumerate() {
                if index > 0 {
                    output.push_str("\n---\n\n");
                }
                output.push_str(&format!("## {}\n\n{}\n", tool_name, tool_output));
            }
        }

        if !errors.is_empty() {
            output.push_str("\nErrors:\n");
            for (tool_name, err) in &errors {
                output.push_str(&format!("- {}: {}\n", tool_name, err));
            }
        }

        if discarded > 0 {
            output.push_str(&format!(
                "\nNote: {} tool(s) discarded (max {} allowed)",
                discarded, MAX_TOOLS
            ));
        }

        if successful == total {
            output.push_str(
                "\n\nKeep using the batch tool for optimal performance in your next response!",
            );
        }

        Ok(ToolResult::new("batch", output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct TestRegistry {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ToolRegistry for TestRegistry {
        async fn execute(
            &self,
            name: &str,
            _args: Value,
            _ctx: &ToolContext,
        ) -> Result<ToolResult, Box<dyn Error + Send + Sync>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ToolResult::new(name, "search result content"))
        }

        fn list_tools(&self) -> Vec<String> {
            vec![
                "websearch".to_string(),
                "apply_patch".to_string(),
                "bash".to_string(),
                "task".to_string(),
                "write_file".to_string(),
                "batch".to_string(),
            ]
        }
    }

    #[tokio::test]
    async fn includes_successful_tool_output() {
        let tool = BatchTool::new(Arc::new(TestRegistry {
            calls: AtomicUsize::new(0),
        }));
        let result = tool
            .execute(
                serde_json::json!({
                    "tool_calls": [{
                        "tool": "websearch",
                        "parameters": {"query": "rust"}
                    }]
                }),
                &ToolContext::new("test", "test"),
            )
            .await
            .unwrap();

        assert!(result.output.contains("## websearch"));
        assert!(result.output.contains("search result content"));
    }

    #[tokio::test]
    async fn allows_shell_and_mutation_tools() {
        let registry = Arc::new(TestRegistry {
            calls: AtomicUsize::new(0),
        });
        let tool = BatchTool::new(registry.clone());
        let result = tool
            .execute(
                serde_json::json!({
                    "tool_calls": [
                        {"tool": "apply_patch", "parameters": {}},
                        {"tool": "bash", "parameters": {"command": "true"}},
                        {"tool": "task", "parameters": {}},
                        {"tool": "write_file", "parameters": {}}
                    ]
                }),
                &ToolContext::new("test", "test"),
            )
            .await
            .unwrap();

        assert!(result.output.contains("All 4 tools executed successfully"));
        assert_eq!(registry.calls.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn rejects_nested_batch() {
        let registry = Arc::new(TestRegistry {
            calls: AtomicUsize::new(0),
        });
        let tool = BatchTool::new(registry.clone());
        let result = tool
            .execute(
                serde_json::json!({
                    "tool_calls": [{"tool": "batch", "parameters": {}}]
                }),
                &ToolContext::new("test", "test"),
            )
            .await
            .unwrap();

        assert!(result.output.contains("not allowed in batch"));
        assert_eq!(registry.calls.load(Ordering::SeqCst), 0);
    }
}
