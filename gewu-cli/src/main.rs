pub mod config;
pub mod evolve_tools;
pub mod mcp_loader;
pub mod registry;
pub mod tools;

use anyhow::{Context, Result};
use clap::Parser;
use futures::StreamExt;
use gewu_core::traits::LlmProvider;
use gewu_core::types::AgentEvent;
use gewu_providers::{AnthropicProvider, OpenAIProvider};
use sqlx::sqlite::SqlitePoolOptions;
use std::env;
use std::io::{Read, Write};
use std::sync::Arc;
use tracing::{error, info};
use uuid::Uuid;

const RESET: &str = "\x1b[0m";

async fn build_experience_context(
    store: &gewu_experience::ExperienceStore,
    query: &str,
) -> Option<String> {
    let experience_query = gewu_experience::ExperienceQuery {
        query_text: query.to_string(),
        task_type_filter: None,
        limit: 3,
        score_threshold: 0.5,
    };

    match store.recall(&experience_query).await {
        Ok(results) if !results.is_empty() => {
            let mut context = String::from("\n\n[Relevant past experiences for this task]\n");
            for (i, recalled) in results.iter().enumerate() {
                let exp = &recalled.experience;
                context.push_str(&format!(
                    "{}. [{}] {}\n   Solution: {}\n",
                    i + 1,
                    exp.task_type,
                    exp.task_description,
                    exp.solution_summary
                ));
            }
            context.push_str("Use these experiences to inform your approach.\n");
            context.push_str("---\n\n");
            tracing::info!("Recalled {} relevant experiences", results.len());
            Some(context)
        }
        Ok(results) => {
            tracing::debug!("Recall returned {} results (empty)", results.len());
            None
        }
        Err(e) => {
            tracing::warn!("Failed to recall experiences: {}", e);
            None
        }
    }
}

async fn run_agent(
    agent: &mut gewu_core::Agent,
    prompt: &str,
    experience_store: Option<Arc<gewu_experience::ExperienceStore>>,
) -> Result<()> {
    let full_prompt = if let Some(ref store) = experience_store {
        if let Some(context) = build_experience_context(store, prompt).await {
            context + prompt
        } else {
            prompt.to_string()
        }
    } else {
        prompt.to_string()
    };

    let mut stream = Box::pin(agent.ask(&full_prompt));
    let mut reasoning = String::new();
    let mut content = String::new();
    let mut reasoning_started = false;
    let mut content_started = false;
    let mut tools_used: Vec<String> = Vec::new();
    let mut task_completed = false;
    let mut task_failed = false;
    let mut final_response = String::new();

    while let Some(event_result) = stream.next().await {
        match event_result {
            Ok(event) => match event {
                AgentEvent::ContentDelta { delta } => {
                    if !content_started {
                        content_started = true;
                        if reasoning_started {
                            print!("{}", RESET);
                        }
                        println!();
                    }
                    print!("{}", delta);
                    std::io::stdout().flush()?;
                    content.push_str(&delta);
                    final_response.push_str(&delta);
                }
                AgentEvent::ReasoningDelta { delta } => {
                    if !reasoning_started {
                        reasoning_started = true;
                        print!("\x1b[90m[thinking]\x1b[0m ");
                        print!("\x1b[90m");
                    }
                    print!("{}", delta);
                    std::io::stdout().flush()?;
                    reasoning.push_str(&delta);
                }
                AgentEvent::ToolStart { tool, input: _ } => {
                    if reasoning_started {
                        print!("{}", RESET);
                    }
                    if !tools_used.contains(&tool) {
                        tools_used.push(tool.clone());
                    }
                }
                AgentEvent::ToolFinish { tool, output: _ } => {
                    let tool_count = tools_used.len();
                    println!("\n[Tool Finish: {}] ({} tools used total)", tool, tool_count);
                }
                AgentEvent::Finish { response } => {
                    if reasoning_started {
                        print!("{}", RESET);
                    }
                    println!();
                    final_response = response;
                    task_completed = true;
                }
                AgentEvent::Error { error } => {
                    if reasoning_started {
                        print!("{}", RESET);
                    }
                    error!("Error from agent: {}", error);
                    task_failed = true;
                }
            },
            Err(e) => error!("Stream error: {}", e),
        }
    }

    if let Some(store) = experience_store {
        if task_completed && !final_response.is_empty() {
            let user_message = prompt.to_string();
            let assistant_response = final_response;

            let experience = gewu_experience::Experience::new(
                user_message.clone(),
                "general".to_string(),
                user_message,
                assistant_response,
                tools_used,
                1,
                true,
                String::new(),
            );

            if let Err(e) = store.store(&experience).await {
                tracing::warn!("Failed to store experience: {}", e);
            } else {
                tracing::info!("Experience stored successfully");
            }
        }
    }

    Ok(())
}

#[derive(Parser)]
#[command(name = "gewu-cli")]
#[command(version = "0.1.0")]
#[command(about = "GeWu CLI Agent", long_about = None)]
struct Cli {
    /// Prompt to send (without -i flag, runs once and exits)
    #[arg(trailing_var_arg = true)]
    prompt: Option<Vec<String>>,

    /// Custom OpenAI Base URL
    #[arg(long, env = "OPENAI_BASE_URL")]
    base_url: Option<String>,

    /// OpenAI API Key
    #[arg(long, env = "OPENAI_API_KEY")]
    api_key: Option<String>,

    /// OpenAI Endpoint URL
    #[arg(long, env = "OPENAI_ENDPOINT")]
    endpoint: Option<String>,

    /// Custom Model Name
    #[arg(long, short, env = "OPENAI_MODEL")]
    model: Option<String>,

    /// LLM Provider to use (openai or anthropic)
    #[arg(long, env = "GEWU_PROVIDER")]
    provider: Option<String>,

    /// Initial Agent (default: build)
    #[arg(long, default_value = "build")]
    agent: String,

    /// Log level
    #[arg(long, default_value = "info")]
    log_level: String,

    /// List available agents
    #[arg(long)]
    list_agents: bool,

    /// List available models from the provider
    #[arg(long)]
    list_models: bool,

    /// Session ID for persistent memory (optional, generated if not provided)
    #[arg(long)]
    session_id: Option<String>,

    /// Path to SQLite database file
    #[arg(long)]
    db_path: Option<String>,

    /// Path to LLM configuration file
    #[arg(long)]
    llm_config: Option<String>,

    /// Run in interactive mode
    #[arg(short, long)]
    interactive: bool,

    /// List all stored experiences and exit
    #[arg(long)]
    list_experiences: bool,

    /// Dump all experiences to a markdown file
    #[arg(long)]
    dump_experiences: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Load .env file
    dotenvy::dotenv().ok();

    let cli = Cli::parse();

    // Initialize logging
    let log_level = match cli.log_level.to_lowercase().as_str() {
        "error" => tracing::Level::ERROR,
        "warn" => tracing::Level::WARN,
        "info" => tracing::Level::INFO,
        "debug" => tracing::Level::DEBUG,
        "trace" => tracing::Level::TRACE,
        _ => tracing::Level::INFO,
    };

    let subscriber = tracing_subscriber::fmt().with_max_level(log_level).finish();

    tracing::subscriber::set_global_default(subscriber)?;

    if cli.list_agents {
        println!("Available Agents:");
        for agent in registry::list_available_agents() {
            println!("  - {}: {}", agent.name, agent.description);
        }
        return Ok(());
    }

    let loaded_config = config::LoadedConfig::load(cli.llm_config.as_deref())?;

    if cli.list_experiences {
        let experience_config = if let Some(ref url) = loaded_config.config.qdrant_url {
            if !url.is_empty() {
                Some(gewu_experience::Config::with_qdrant(
                    url,
                    loaded_config.config.qdrant_api_key.clone(),
                ))
            } else {
                None
            }
        } else {
            None
        };

        if let Some(config) = experience_config {
            match gewu_experience::ExperienceStore::new(config).await {
                Ok(store) => {
                    let experiences = store.list_all().await?;
                    if experiences.is_empty() {
                        println!("No experiences stored yet.");
                    } else {
                        println!("{:4} | {:12} | {:40} | {:8} | {}", "ID", "Type", "Task Description", "Success", "Created");
                        println!("{}", "-".repeat(120));
                        for exp in &experiences {
                            let desc = if exp.task_description.len() > 38 {
                                format!("{}...", &exp.task_description[..38])
                            } else {
                                exp.task_description.clone()
                            };
                            println!("{:4} | {:12} | {:40} | {:8} | {}",
                                &exp.id[..4],
                                exp.task_type,
                                desc,
                                if exp.success { "yes" } else { "no" },
                                exp.created_at.format("%Y-%m-%d %H:%M")
                            );
                        }
                        println!("\nTotal: {} experiences", experiences.len());
                    }
                }
                Err(e) => {
                    eprintln!("Failed to initialize experience store: {}", e);
                }
            }
        } else {
            eprintln!("Qdrant not configured. Cannot list experiences.");
        }
        return Ok(());
    }

    if let Some(dump_path) = &cli.dump_experiences {
        let experience_config = if let Some(ref url) = loaded_config.config.qdrant_url {
            if !url.is_empty() {
                Some(gewu_experience::Config::with_qdrant(
                    url,
                    loaded_config.config.qdrant_api_key.clone(),
                ))
            } else {
                None
            }
        } else {
            None
        };

        if let Some(config) = experience_config {
            match gewu_experience::ExperienceStore::new(config).await {
                Ok(store) => {
                    let experiences = store.list_all().await?;
                    if experiences.is_empty() {
                        println!("No experiences to dump.");
                    } else {
                        let mut md = String::from("# Experience Library\n\n");
                        md.push_str(&format!("Total: {} experiences\n\n", experiences.len()));

                        for exp in &experiences {
                            md.push_str(&format!("## {} [{}\\]\n\n", exp.task_description, exp.task_type));
                            md.push_str(&format!("**ID:** `{}`\n\n", exp.id));
                            md.push_str(&format!("**Created:** {}\n\n", exp.created_at.format("%Y-%m-%d %H:%M:%S")));
                            md.push_str(&format!("**Success:** {}\n\n", if exp.success { "Yes" } else { "No" }));
                            md.push_str(&format!("**Attempts:** {}\n\n", exp.iteration_count));
                            md.push_str("### Problem\n\n");
                            md.push_str(&format!("{}\n\n", exp.problem_summary));
                            md.push_str("### Solution\n\n");
                            md.push_str(&format!("{}\n\n", exp.solution_summary));
                            md.push_str(&format!("### Tools Used\n\n{}\n\n", exp.tools_used.join(", ")));
                            md.push_str("---\n\n");
                        }

                        std::fs::write(dump_path, &md)?;
                        println!("Dumped {} experiences to {}", experiences.len(), dump_path);
                    }
                }
                Err(e) => {
                    eprintln!("Failed to initialize experience store: {}", e);
                }
            }
        } else {
            eprintln!("Qdrant not configured. Cannot dump experiences.");
        }
        return Ok(());
    }

    if let Some(path) = &loaded_config.path {
        info!("Loaded config from: {}", path.display());
    } else {
        info!("No config file found");
    }

    let file_config = &loaded_config.config;

    let api_key = cli.api_key
        .or_else(|| env::var("OPENAI_API_KEY").ok())
        .or(file_config.api_key.clone())
        .context("OPENAI_API_KEY not provided. Set via --api-key, OPENAI_API_KEY env var, or config.json")?;

    let base_url = cli
        .base_url
        .clone()
        .or_else(|| env::var("OPENAI_BASE_URL").ok())
        .or(cli.endpoint.clone())
        .or_else(|| env::var("OPENAI_ENDPOINT").ok())
        .or(file_config.base_url.clone());

    let provider_name = cli
        .provider
        .or_else(|| env::var("GEWU_PROVIDER").ok())
        .or_else(|| file_config.provider.clone())
        .unwrap_or_else(|| "openai".to_string());

    if cli.list_models {
        let model = cli
            .model
            .or_else(|| env::var("OPENAI_MODEL").ok())
            .or(file_config.model.clone())
            .unwrap_or_else(|| "gpt-5.3-codex".to_string());
        let url = base_url.unwrap_or_else(|| "https://api.openai.com/v1".to_string());
        let provider = OpenAIProvider::new_with_endpoint(&api_key, &url, &model);
        match provider.list_models().await {
            Ok(models) => {
                println!("Available Models:");
                for model in models {
                    println!("  - {}", model);
                }
            }
            Err(e) => {
                eprintln!("Error listing models: {}", e);
            }
        }
        return Ok(());
    }

    let model = cli
        .model
        .or_else(|| env::var("OPENAI_MODEL").ok())
        .or(file_config.model.clone())
        .unwrap_or_else(|| "gpt-5.3-codex".to_string());

    info!("Using model: {}", model);
    info!("Using provider: {}", provider_name);

    // Initialize Provider
    let provider: Arc<dyn LlmProvider> = match provider_name.to_lowercase().as_str() {
        "anthropic" => {
            if let Some(url) = base_url {
                Arc::new(AnthropicProvider::new_with_endpoint(&api_key, &url, &model))
            } else {
                Arc::new(AnthropicProvider::new(&api_key, &model))
            }
        }
        "openai" | _ => {
            if let Some(url) = base_url {
                Arc::new(OpenAIProvider::new_with_endpoint(&api_key, &url, &model))
            } else {
                Arc::new(OpenAIProvider::new(&api_key, &model))
            }
        }
    };

    // Initialize SQLite Pool
    let db_path = if let Some(path) = cli.db_path {
        path
    } else {
        let home_dir = dirs::home_dir()
            .context("Failed to determine home directory. Please provide --db-path explicitly.")?;
        let gewu_dir = home_dir.join(".gewu");

        // Ensure .gewu directory exists
        if !gewu_dir.exists() {
            std::fs::create_dir_all(&gewu_dir).context("Failed to create .gewu directory")?;
        }

        gewu_dir.join("gewu.db").to_string_lossy().to_string()
    };

    info!("Using database: {}", db_path);
    let db_url = format!("sqlite://{}?mode=rwc", db_path);

    let pool = SqlitePoolOptions::new()
        .connect(&db_url)
        .await
        .context("Failed to connect to SQLite database")?;

    // Determine Session ID
    let session_id = cli.session_id.unwrap_or_else(|| Uuid::new_v4().to_string());

    // Load MCP tools
    let mcp_tools = mcp_loader::load_mcp_tools(loaded_config.config.mcp_servers.clone()).await?;

    // Build experience config from config file or env
    let experience_config = if let Some(ref url) = loaded_config.config.qdrant_url {
        if !url.is_empty() {
            Some(gewu_experience::Config::with_qdrant(
                url,
                loaded_config.config.qdrant_api_key.clone(),
            ))
        } else {
            None
        }
    } else {
        // Fall back to env vars
        let env_url = std::env::var("QDRANT_URL").unwrap_or_default();
        if !env_url.is_empty() {
            Some(gewu_experience::Config::from_env())
        } else {
            None
        }
    };

    // Create Agent
    let (mut agent, _registry, experience_store) =
        registry::create_agent(&cli.agent, provider, pool, session_id, mcp_tools, experience_config).await?;
    info!("Initialized agent: {}", cli.agent);

    // Determine the prompt: from args, from stdin, or error
    let prompt_from_args = cli.prompt.clone();
    let prompt_from_stdin = if prompt_from_args.is_none() && !atty::is(atty::Stream::Stdin) {
        let mut buffer = String::new();
        std::io::stdin().read_to_string(&mut buffer).ok();
        if buffer.trim().is_empty() {
            None
        } else {
            Some(buffer)
        }
    } else {
        None
    };

    // Validate that we have a prompt or interactive mode
    if prompt_from_args.is_none() && prompt_from_stdin.is_none() && !cli.interactive {
        eprintln!("Error: No prompt provided. Either provide a prompt as argument, pipe content to stdin, or use -i/--interactive for interactive mode.");
        std::process::exit(1);
    }

    // Initial Prompt handling
    if let Some(prompt_parts) = &prompt_from_args {
        let prompt = prompt_parts.join(" ");
        if !prompt.is_empty() {
            println!("> {}", prompt);
            if let Err(e) = run_agent(&mut agent, &prompt, experience_store.clone()).await {
                error!("Error: {}", e);
            }
        }
    } else if let Some(stdin_prompt) = &prompt_from_stdin {
        let trimmed = stdin_prompt.trim();
        if !trimmed.is_empty() {
            if trimmed.len() > 100 {
                println!("> {}...", &trimmed[..100]);
            } else {
                println!("> {}", trimmed);
            }
            if let Err(e) = run_agent(&mut agent, trimmed, experience_store.clone()).await {
                error!("Error: {}", e);
            }
        }
    }

    // Interactive Loop (only if -i flag is set)
    if cli.interactive {
        use std::io::{self, Write};
        println!("--- GeWu CLI (Agent: {}) ---", cli.agent);
        println!("Type 'exit' or 'quit' to end.");

        loop {
            print!("> ");
            io::stdout().flush()?;

            let mut input = String::new();
            io::stdin().read_line(&mut input)?;
            let input = input.trim();

            if input.is_empty() {
                continue;
            }

            if input == "exit" || input == "quit" {
                break;
            }

            if let Err(e) = run_agent(&mut agent, input, experience_store.clone()).await {
                error!("Error processing message: {}", e);
            }
        }
    }

    Ok(())
}
