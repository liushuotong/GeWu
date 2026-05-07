use crate::evolve_tools::{PythonExecuteTool, SkillCreateTool, SkillListTool, SkillLoadTool};
use crate::tools::TaskTool;
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use dirs::home_dir;
use gewu_core::Agent;
use gewu_core::traits::{LlmProvider, Tool, ToolRegistry};
use gewu_core::types::{ToolContext, ToolResult};
use gewu_experience::{ExperienceRecallTool, ExperienceStore, ExperienceStoreTool};
use gewu_storage::SqliteMemory;
use gewu_tools::{
    BashTool, BatchTool, CodeSearchTool, EditFileTool, GlobTool, GrepTool, ListTool, ReadFileTool,
    TodoItem, TodoReadTool, TodoWriteTool, WebFetchTool, WebSearchTool, WriteFileTool,
};
use serde_json::Value;
use sqlx::SqlitePool;
use std::path::PathBuf;
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
        AgentConfig {
            name: "evolve".to_string(),
            description: "Self-evolving agent with multi-agent planning, review, and execution.".to_string(),
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

pub enum AgentResult {
    Standard(Agent),
    Evolve {
        tools: Arc<dyn ToolRegistry>,
        tools_list: Arc<Vec<Arc<dyn Tool>>>,
        experience_config: Option<gewu_experience::Config>,
        skill_dir: PathBuf,
    },
}

fn build_tool_registry_and_list(
    name: &str,
    provider: Arc<dyn LlmProvider>,
    pool: SqlitePool,
    working_directory: std::path::PathBuf,
    additional_tools: Vec<Arc<dyn Tool>>,
    experience_store: Option<Arc<ExperienceStore>>,
    skill_dir: PathBuf,
) -> Result<(Arc<dyn ToolRegistry>, Vec<Arc<dyn Tool>>)> {
    let tools: Vec<Box<dyn Tool>> = match name {
        "build" => {
            let todos = Arc::new(Mutex::new(Vec::<TodoItem>::new()));
            let mut tool_list: Vec<Box<dyn Tool>> = vec![
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
            ];
            if let Some(ref store) = experience_store {
                tool_list.push(Box::new(ExperienceStoreTool::new(store.clone())));
                tool_list.push(Box::new(ExperienceRecallTool::new(store.clone())));
            }
            tool_list
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
            let todos = Arc::new(Mutex::new(Vec::<TodoItem>::new()));
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
        "evolve" => {
            let todos = Arc::new(Mutex::new(Vec::<TodoItem>::new()));
            let mut tool_list: Vec<Box<dyn Tool>> = vec![
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
                Box::new(PythonExecuteTool::new(30)),
                Box::new(SkillListTool::new(skill_dir.clone())),
                Box::new(SkillLoadTool::new(skill_dir.clone())),
                Box::new(SkillCreateTool::new(skill_dir.clone(), provider.clone())),
            ];
            if let Some(ref store) = experience_store {
                tool_list.push(Box::new(ExperienceStoreTool::new(store.clone())));
                tool_list.push(Box::new(ExperienceRecallTool::new(store.clone())));
            }
            tool_list
        }
        _ => return Err(anyhow!("Unknown agent type: {}", name)),
    };

    let mut tools_arc: Vec<Arc<dyn Tool>> = tools.into_iter().map(|t| Arc::from(t) as Arc<dyn Tool>).collect();
    tools_arc.extend(additional_tools);

    let registry: Arc<dyn ToolRegistry> = Arc::new(ToolRegistryImpl {
        tools: Arc::new(tools_arc.clone()),
    });

    Ok((registry, tools_arc))
}

pub async fn create_agent(
    name: &str,
    provider: Arc<dyn LlmProvider>,
    pool: SqlitePool,
    session_id: String,
    additional_tools: Vec<Arc<dyn Tool>>,
    experience_config: Option<gewu_experience::Config>,
) -> Result<(AgentResult, Arc<dyn ToolRegistry>, Option<Arc<ExperienceStore>>)> {
    let working_directory = std::env::current_dir()?;

    let memory = SqliteMemory::new(pool.clone(), session_id.clone()).await?;

    let experience_store = if let Some(ref config) = experience_config {
        if !config.qdrant_url().is_empty() {
            match gewu_experience::ExperienceStore::new(config.clone()).await {
                Ok(store) => {
                    tracing::info!("Experience store initialized successfully");
                    Some(Arc::new(store))
                }
                Err(e) => {
                    tracing::warn!("Failed to initialize experience store: {}. Experience tools will not be available.", e);
                    None
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    let skill_dir = home_dir()
        .map(|h| h.join(".gewu").join("skills"))
        .unwrap_or_else(|| PathBuf::from("~/.gewu/skills"));

    if name == "evolve" {
        let (registry, tools_arc) = build_tool_registry_and_list(
            name,
            provider.clone(),
            pool,
            working_directory,
            additional_tools,
            experience_store.clone(),
            skill_dir.clone(),
        )?;

        return Ok((
            AgentResult::Evolve {
                tools: registry.clone(),
                tools_list: Arc::new(tools_arc.clone()),
                experience_config,
                skill_dir,
            },
            registry,
            experience_store,
        ));
    }

    let (registry, tools_arc) = build_tool_registry_and_list(
        name,
        provider.clone(),
        pool,
        working_directory.clone(),
        additional_tools,
        experience_store.clone(),
        skill_dir.clone(),
    )?;

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

    Ok((AgentResult::Standard(agent), registry, experience_store))
}
