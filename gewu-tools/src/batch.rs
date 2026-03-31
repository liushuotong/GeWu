use crate::fs_utils::into_tool_error;
use gewu_core::traits::{Tool, ToolRegistry};
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use std::error::Error;
use tokio::task::JoinSet;

const DISALLOWED: &[&str] = &["batch", "invalid", "patch"];
const MAX_TOOLS: usize = 25;

#[derive(Debug, Deserialize)]
struct ToolCallInput {
    tool: String,
    parameters: Value,
}

#[derive(Debug, Deserialize)]
struct BatchArgs {
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
        "Execute multiple tools in parallel. Use this to run independent tool calls simultaneously for better performance."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "tool_calls": {
                    "type": "array",
                    "description": "Array of tool calls to execute in parallel",
                    "items": {
                        "type": "object",
                        "properties": {
                            "tool": { "type": "string", "description": "The name of the tool to execute" },
                            "parameters": { "type": "object", "description": "Parameters for the tool" }
                        },
                        "required": ["tool", "parameters"]
                    }
                }
            },
            "required": ["tool_calls"]
        })
    }

    async fn call(&self, args: &str) -> Result<String, Box<dyn Error + Send + Sync>> {
        let batch_args: BatchArgs = serde_json::from_str(args).map_err(into_tool_error)?;

        let tool_calls = batch_args.tool_calls.iter().take(MAX_TOOLS).collect::<Vec<_>>();
        let discarded = batch_args.tool_calls.len() - tool_calls.len();

        let available_tools = self.registry.list_tools();
        let allowed_tools = available_tools.iter()
            .filter(|t| !DISALLOWED.contains(&t.as_str()))
            .cloned()
            .collect::<std::collections::HashSet<_>>();

        let mut join_set = JoinSet::new();

        for call in &tool_calls {
            let tool_name = call.tool.clone();
            let params = call.parameters.clone();
            let registry = self.registry.clone();
            let allowed = allowed_tools.clone();

            if tool_name.is_empty() {
                let tool_name_clone = tool_name.clone();
                join_set.spawn(async move {
                    (tool_name_clone, Err("Empty tool name".into()))
                });
                continue;
            }

            join_set.spawn(async move {
                let result = async {
                    if DISALLOWED.contains(&tool_name.as_str()) {
                        return Err(format!("Tool '{}' is not allowed in batch. Disallowed tools: {:?}", tool_name, DISALLOWED).into());
                    }

                    if !allowed.contains(&tool_name) {
                        return Err(format!(
                            "Tool '{}' not in registry. Available tools: {}",
                            tool_name,
                            allowed.iter().cloned().collect::<Vec<_>>().join(", ")
                        ).into());
                    }

                    let params_str = serde_json::to_string(&params).map_err(into_tool_error)?;
                    registry.execute(&tool_name, &params_str).await
                }.await;

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
                Ok(output) => successes.push((tool_name.clone(), output.clone())),
                Err(e) => errors.push((tool_name.clone(), e.to_string())),
            }
        }

        let total = results.len();
        let successful = successes.len();
        let failed = errors.len();

        let mut output = String::new();

        if failed > 0 {
            output.push_str(&format!("Executed {}/{} tools successfully. {} failed.\n\n", successful, total, failed));
        } else {
            output.push_str(&format!("All {} tools executed successfully.\n\n", successful));
        }

        if !successes.is_empty() {
            output.push_str("Results:\n");
            for (tool_name, _) in &successes {
                output.push_str(&format!("- {}: success\n", tool_name));
            }
        }

        if !errors.is_empty() {
            output.push_str("\nErrors:\n");
            for (tool_name, err) in &errors {
                output.push_str(&format!("- {}: {}\n", tool_name, err));
            }
        }

        if discarded > 0 {
            output.push_str(&format!("\nNote: {} tool(s) discarded (max {} allowed)", discarded, MAX_TOOLS));
        }

        if successful == total {
            output.push_str("\n\nKeep using the batch tool for optimal performance in your next response!");
        }

        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    struct MockTool {
        name: String,
        output: String,
    }

    #[async_trait]
    impl Tool for MockTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "Mock tool"
        }

        fn parameters_schema(&self) -> Value {
            json!({})
        }

        async fn call(&self, _args: &str) -> Result<String, Box<dyn Error + Send + Sync>> {
            Ok(self.output.clone())
        }
    }

    struct MockRegistry {
        tools: Vec<Arc<dyn Tool>>,
    }

    #[async_trait]
    impl ToolRegistry for MockRegistry {
        async fn execute(&self, name: &str, args: &str) -> Result<String, Box<dyn Error + Send + Sync>> {
            let tool = self.tools.iter().find(|t| t.name() == name);
            match tool {
                Some(t) => t.call(args).await,
                None => Err(format!("Tool not found: {}", name).into()),
            }
        }

        fn list_tools(&self) -> Vec<String> {
            self.tools.iter().map(|t| t.name().to_string()).collect()
        }
    }

    #[tokio::test]
    async fn test_batch_single_tool() {
        let mock_tool = Arc::new(MockTool {
            name: "read".to_string(),
            output: "file content".to_string(),
        });

        let registry = Arc::new(MockRegistry {
            tools: vec![mock_tool.clone()],
        });

        let batch = BatchTool::new(registry);

        let args = json!({
            "tool_calls": [
                {"tool": "read", "parameters": {}}
            ]
        });

        let result = batch.call(&args.to_string()).await.unwrap();
        assert!(result.contains("All 1 tools executed successfully"));
    }

    #[tokio::test]
    async fn test_batch_multiple_tools() {
        let read_tool = Arc::new(MockTool {
            name: "read".to_string(),
            output: "file content".to_string(),
        });

        let glob_tool = Arc::new(MockTool {
            name: "glob".to_string(),
            output: "*.rs files".to_string(),
        });

        let registry = Arc::new(MockRegistry {
            tools: vec![read_tool.clone(), glob_tool.clone()],
        });

        let batch = BatchTool::new(registry);

        let args = json!({
            "tool_calls": [
                {"tool": "read", "parameters": {}},
                {"tool": "glob", "parameters": {}}
            ]
        });

        let result = batch.call(&args.to_string()).await.unwrap();
        assert!(result.contains("All 2 tools executed successfully"));
    }

    #[tokio::test]
    async fn test_batch_disallowed_tool() {
        let registry = Arc::new(MockRegistry {
            tools: vec![],
        });

        let batch = BatchTool::new(registry);

        let args = json!({
            "tool_calls": [
                {"tool": "batch", "parameters": {}}
            ]
        });

        let result = batch.call(&args.to_string()).await.unwrap();
        assert!(result.contains("not allowed in batch"));
    }

    #[tokio::test]
    async fn test_batch_unknown_tool() {
        let registry = Arc::new(MockRegistry {
            tools: vec![],
        });

        let batch = BatchTool::new(registry);

        let args = json!({
            "tool_calls": [
                {"tool": "unknown_tool", "parameters": {}}
            ]
        });

        let result = batch.call(&args.to_string()).await.unwrap();
        assert!(result.contains("not in registry"));
    }
}