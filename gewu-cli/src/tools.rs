use anyhow::Result;
use async_trait::async_trait;
use gewu_core::traits::{LlmProvider, Tool};
use gewu_core::types::{ToolContext, ToolResult};
use schemars::schema::RootSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use std::error::Error;
use std::sync::Arc;
use tracing::info;
use uuid::Uuid;

pub struct TaskTool {
    pub provider: Arc<dyn LlmProvider>,
    pub pool: SqlitePool,
    description: String,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct TaskArgs {
    description: String,
    prompt: String,
    subagent_type: String,
    #[serde(default)]
    task_id: Option<String>,
}

#[derive(Deserialize)]
struct TaskArgsJson {
    description: String,
    prompt: String,
    subagent_type: String,
    #[serde(default)]
    task_id: Option<String>,
}

impl TaskTool {
    pub fn new(provider: Arc<dyn LlmProvider>, pool: SqlitePool) -> Self {
        let description = Self::generate_description();
        Self {
            provider,
            pool,
            description,
        }
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

    fn build_schema() -> RootSchema {
        serde_json::from_value(json!({
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
        })).unwrap()
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

    fn schema(&self) -> RootSchema {
        Self::build_schema()
    }

    async fn execute(
        &self,
        args: Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, Box<dyn Error + Send + Sync>> {
        let input: TaskArgsJson = serde_json::from_value(args)?;

        let session_id = input.task_id.unwrap_or_else(|| Uuid::new_v4().to_string());

        info!(
            "Spawning sub-agent '{}' for task: {} (session: {})",
            input.subagent_type, input.description, session_id
        );

        let (mut agent, _registry) = crate::registry::create_agent(
            &input.subagent_type,
            self.provider.clone(),
            self.pool.clone(),
            session_id.clone(),
            Vec::new(),
        )
        .await
        .map_err(|e| format!("Failed to create agent: {}", e))?;

        let mut stream = Box::pin(agent.ask(&input.prompt));
        let mut result = String::new();
        use futures::StreamExt;
        use gewu_core::types::AgentEvent;

        while let Some(event_result) = stream.next().await {
            match event_result {
                Ok(event) => match event {
                    AgentEvent::ContentDelta { delta } => {
                        result.push_str(&delta);
                    }
                    AgentEvent::Finish { response } => {
                        result = response;
                    }
                    AgentEvent::Error { error } => {
                        return Err(format!("Agent execution failed: {}", error).into());
                    }
                    _ => {}
                },
                Err(e) => return Err(format!("Agent stream failed: {}", e).into()),
            }
        }

        Ok(ToolResult::new(
            "task",
            format!(
                "task_id: {}\n\n<task_result>\n{}\n</task_result>",
                session_id, result
            ),
        ))
    }
}
