use crate::tools::TaskTool;
use anyhow::{anyhow, Result};
use gewu_core::traits::{LlmProvider, Tool};
use gewu_core::Agent;
use gewu_tools::{
    BashTool, CodeSearchTool, EditFileTool, GlobTool, GrepTool, ListTool, ReadFileTool,
    WebFetchTool, WebSearchTool, WriteFileTool,
};
use std::sync::Arc;
use gewu_storage::SqliteMemory;
use sqlx::SqlitePool;

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

pub async fn create_agent(name: &str, provider: Arc<dyn LlmProvider>, pool: SqlitePool, session_id: String) -> Result<Agent> {
    let working_directory = std::env::current_dir()?;

    // Initialize persistent memory
    let memory = SqliteMemory::new(pool.clone(), session_id).await?;

    match name {
        "build" => {
            let tools: Vec<Box<dyn Tool>> = vec![
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
            ];

            Agent::builder()
                .llm(provider)
                .with_tools(tools)
                .memory(Box::new(memory))
                .system_prompt(include_str!("prompts/build.txt"))
                .build()
        }
        "explore" => {
            let tools: Vec<Box<dyn Tool>> = vec![
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
            ];

            Agent::builder()
                .llm(provider)
                .with_tools(tools)
                .memory(Box::new(memory))
                .system_prompt(include_str!("prompts/explore.txt"))
                .build()
        }
        "general" => {
            let tools: Vec<Box<dyn Tool>> = vec![
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
            ];

            Agent::builder()
                .llm(provider)
                .with_tools(tools)
                .memory(Box::new(memory))
                .system_prompt(include_str!("prompts/general.txt"))
                .build()
        }
        _ => Err(anyhow!("Unknown agent type: {}", name)),
    }
}
