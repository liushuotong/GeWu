pub mod config;
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

async fn run_agent(agent: &mut gewu_core::Agent, prompt: &str) -> Result<()> {
    let mut stream = Box::pin(agent.ask(prompt));
    let mut reasoning = String::new();
    let mut content = String::new();
    let mut reasoning_started = false;
    let mut content_started = false;
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
                AgentEvent::ToolStart { tool, input } => {
                    if reasoning_started {
                        print!("{}", RESET);
                    }
                    println!("\n[Tool Start: {}] Input: {}", tool, input);
                }
                AgentEvent::ToolFinish { tool, output } => {
                    let display_output = if output.len() > 200 {
                        format!("{}...", &output[..200])
                    } else {
                        output
                    };
                    println!("\n[Tool Finish: {}] Output: {}", tool, display_output);
                }
                AgentEvent::Finish { .. } => {
                    if reasoning_started {
                        print!("{}", RESET);
                    }
                    println!();
                }
                AgentEvent::Error { error } => {
                    if reasoning_started {
                        print!("{}", RESET);
                    }
                    error!("Error from agent: {}", error);
                }
            },
            Err(e) => error!("Stream error: {}", e),
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
    let mcp_tools = mcp_loader::load_mcp_tools(loaded_config.config.mcp_servers).await?;

    // Create Agent
    let (mut agent, _registry) =
        registry::create_agent(&cli.agent, provider, pool, session_id, mcp_tools).await?;
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
            if let Err(e) = run_agent(&mut agent, &prompt).await {
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
            if let Err(e) = run_agent(&mut agent, trimmed).await {
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

            if let Err(e) = run_agent(&mut agent, input).await {
                error!("Error processing message: {}", e);
            }
        }
    }

    Ok(())
}
