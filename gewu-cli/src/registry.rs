use crate::tools::TaskTool;
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use gewu_core::Agent;
use gewu_core::traits::{LlmProvider, Tool, ToolRegistry};
use gewu_core::types::{ToolContext, ToolResult};
use gewu_storage::SqliteMemory;
use gewu_tools::{
    BashTool, BatchTool, CodeSearchTool, EditFileTool, GlobTool, GrepTool, ListTool, ReadFileTool,
    TodoItem, TodoReadTool, TodoWriteTool, WebFetchTool, WebSearchTool, WriteFileTool,
};
use serde_json::Value;
use sqlx::SqlitePool;
use std::sync::{Arc, Mutex};

pub struct AgentConfig {
    pub name: String,
    pub description: String,
}

pub fn list_available_agents() -> Vec<AgentConfig> {
    vec![
        AgentConfig {
            name: "build".to_string(),
            description: "The default agent. Executes tools based on configured permissions.".to_string(),
        },
        AgentConfig {
            name: "explore".to_string(),
            description: "Fast agent specialized for exploring codebases.".to_string(),
        },
        AgentConfig {
            name: "general".to_string(),
            description: "General-purpose agent for researching complex questions and executing multi-step tasks.".to_string(),
        },
    ]
}

struct ToolRegistryImpl {
    tools: Arc<Vec<Arc<dyn Tool>>>,
}

#[async_trait]
impl ToolRegistry for ToolRegistryImpl {
    async fn execute(
        &self,
        name: &str,
        args: Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, Box<dyn std::error::Error + Send + Sync>> {
        let tool = self.tools.iter().find(|t| t.name() == name);
        match tool {
            Some(t) => t.execute(args, ctx).await,
            None => Err(format!("Tool not found: {}", name).into()),
        }
    }

    fn list_tools(&self) -> Vec<String> {
        self.tools.iter().map(|t| t.name().to_string()).collect()
    }
}

pub async fn create_agent(
    name: &str,
    provider: Arc<dyn LlmProvider>,
    pool: SqlitePool,
    session_id: String,
    additional_tools: Vec<Arc<dyn Tool>>,
) -> Result<(Agent, Arc<dyn ToolRegistry>)> {
    let working_directory = std::env::current_dir()?;

    // Initialize persistent memory
    let memory = SqliteMemory::new(pool.clone(), session_id).await?;

    // Shared in-memory todo list for this agent session
    let todos = Arc::new(Mutex::new(Vec::<TodoItem>::new()));

    let tools: Vec<Box<dyn Tool>> = match name {
        "build" => {
            vec![
                Box::new(TaskTool::new(provider.clone(), pool.clone())),
                Box::new(ReadFileTool {
                    working_directory: working_directory.clone(),
                }),
                Box::new(WriteFileTool {
                    working_directory: working_directory.clone(),
                }),
                Box::new(EditFileTool {
                    working_directory: working_directory.clone(),
                }),
                Box::new(GlobTool {
                    working_directory: working_directory.clone(),
                }),
                Box::new(BashTool {
                    working_directory: working_directory.clone(),
                    safe_mode: false,
                }),
                Box::new(GrepTool {
                    working_directory: working_directory.clone(),
                }),
                Box::new(WebFetchTool),
                Box::new(ListTool {
                    working_directory: working_directory.clone(),
                }),
                Box::new(CodeSearchTool),
                Box::new(WebSearchTool),
                Box::new(TodoWriteTool::new(todos.clone())),
                Box::new(TodoReadTool::new(todos.clone())),
            ]
        }
        "explore" => {
            vec![
                Box::new(ReadFileTool {
                    working_directory: working_directory.clone(),
                }),
                Box::new(GlobTool {
                    working_directory: working_directory.clone(),
                }),
                Box::new(BashTool {
                    working_directory: working_directory.clone(),
                    safe_mode: true,
                }),
                Box::new(GrepTool {
                    working_directory: working_directory.clone(),
                }),
                Box::new(WebFetchTool),
                Box::new(ListTool {
                    working_directory: working_directory.clone(),
                }),
                Box::new(CodeSearchTool),
                Box::new(WebSearchTool),
            ]
        }
        "general" => {
            vec![
                Box::new(TaskTool::new(provider.clone(), pool.clone())),
                Box::new(ReadFileTool {
                    working_directory: working_directory.clone(),
                }),
                Box::new(WriteFileTool {
                    working_directory: working_directory.clone(),
                }),
                Box::new(EditFileTool {
                    working_directory: working_directory.clone(),
                }),
                Box::new(GlobTool {
                    working_directory: working_directory.clone(),
                }),
                Box::new(BashTool {
                    working_directory: working_directory.clone(),
                    safe_mode: false,
                }),
                Box::new(GrepTool {
                    working_directory: working_directory.clone(),
                }),
                Box::new(WebFetchTool),
                Box::new(ListTool {
                    working_directory: working_directory.clone(),
                }),
                Box::new(CodeSearchTool),
                Box::new(WebSearchTool),
                Box::new(TodoWriteTool::new(todos.clone())),
                Box::new(TodoReadTool::new(todos.clone())),
            ]
        }
        _ => return Err(anyhow!("Unknown agent type: {}", name)),
    };

    // Convert built-in tools to Arc<dyn Tool>
    let mut tools_arc: Vec<Arc<dyn Tool>> = tools.into_iter().map(|t| Arc::from(t) as Arc<dyn Tool>).collect();

    // Add additional tools (already Arc<dyn Tool>)
    tools_arc.extend(additional_tools);

    let tools_for_registry = Arc::new(tools_arc.clone());
    let registry: Arc<dyn ToolRegistry> = Arc::new(ToolRegistryImpl {
        tools: tools_for_registry,
    });

    // Create BatchTool and prepend it
    let batch_tool: Arc<dyn Tool> = Arc::new(BatchTool::new(registry.clone()));
    let mut all_tools = vec![batch_tool];
    all_tools.extend(tools_arc);

    let agent = match name {
        "build" => Agent::builder()
            .llm(provider)
            .with_tools(all_tools)
            .memory(Box::new(memory))
            .system_prompt(include_str!("prompts/build.txt"))
            .build()?,
        "explore" => Agent::builder()
            .llm(provider)
            .with_tools(all_tools)
            .memory(Box::new(memory))
            .system_prompt(include_str!("prompts/explore.txt"))
            .build()?,
        "general" => Agent::builder()
            .llm(provider)
            .with_tools(all_tools)
            .memory(Box::new(memory))
            .system_prompt(include_str!("prompts/general.txt"))
            .build()?,
        _ => return Err(anyhow!("Unknown agent type: {}", name)),
    };

    Ok((agent, registry))
}
