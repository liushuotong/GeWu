use anyhow::Result;
use async_trait::async_trait;
use gewu_core::traits::{LlmProvider, Tool};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use std::error::Error;
use tracing::info;

pub struct TaskTool {
    pub provider: Arc<dyn LlmProvider>,
    description: String,
}

#[derive(Deserialize)]
struct TaskArgs {
    description: String,
    prompt: String,
    subagent_type: String,
    #[serde(default)]
    task_id: Option<String>,
}

impl TaskTool {
    pub fn new(provider: Arc<dyn LlmProvider>) -> Self {
        let description = Self::generate_description();
        Self { provider, description }
    }

    fn generate_description() -> String {
        let agents = crate::registry::list_available_agents();
        let agent_list = agents
            .iter()
            .map(|a| format!("- \"{}\": {}", a.name, a.description))
            .collect::<Vec<_>>()
            .join("\n");
        
        include_str!("prompts/task.txt").replace("{agents}", &agent_list)
    }
}

#[async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &str {
        "task"
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "description": { "type": "string", "description": "A short (3-5 words) description of the task" },
                "prompt": { "type": "string", "description": "The task for the agent to perform" },
                "subagent_type": { "type": "string", "description": "The type of specialized agent to use for this task" },
                "task_id": { 
                    "type": "string", 
                    "description": "This should only be set if you mean to resume a previous task (you can pass a prior task_id and the task will continue the same subagent session as before instead of creating a fresh one)"
                }
            },
            "required": ["description", "prompt", "subagent_type"]
        })
    }

    async fn call(&self, args: &str) -> Result<String, Box<dyn Error + Send + Sync>> {
        let input: TaskArgs = serde_json::from_str(args)?;
        
        // TODO: Handle task_id for resumption. For now, we always create a fresh agent.
        if input.task_id.is_some() {
            // Log warning or just proceed?
        }

        info!("Spawning sub-agent '{}' for task: {}", input.subagent_type, input.description);

        let mut agent = crate::registry::create_agent(&input.subagent_type, self.provider.clone())
            .map_err(|e| format!("Failed to create agent: {}", e))?;

        // Run the agent
        let result = agent.ask(&input.prompt).await
            .map_err(|e| format!("Agent execution failed: {}", e))?;

        Ok(format!(
            "task_id: {} (stateless-mock-id)\n\n<task_result>\n{}\n</task_result>",
            "stateless-session", result
        ))
    }
}
