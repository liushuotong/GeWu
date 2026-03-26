mod registry;
mod tools;
mod mcp_loader;

use anyhow::{Result, Context};
use clap::Parser;
use gewu_providers::OpenAIProvider;
use std::sync::Arc;
use tracing::{info, error, Level};
use std::env;
use uuid::Uuid;
use sqlx::sqlite::SqlitePoolOptions;
use gewu_core::types::AgentEvent;
use futures::StreamExt;
use std::io::Write;

async fn run_agent(agent: &mut gewu_core::Agent, prompt: &str) -> Result<()> {
    let mut stream = Box::pin(agent.ask(prompt));
    while let Some(event_result) = stream.next().await {
        match event_result {
            Ok(event) => match event {
                AgentEvent::ContentDelta { delta } => {
                    print!("{}", delta);
                    std::io::stdout().flush()?;
                }
                AgentEvent::ReasoningDelta { delta: _ } => {
                    // Optional: print reasoning in a different color or format
                    // For now, just print it? Or maybe ignore/log it?
                    // Let's print it in a dim color if we had terminal colors
                    // print!("{}", delta); 
                }
                AgentEvent::ToolStart { tool, input } => {
                     println!("\n[Tool Start: {}] Input: {}", tool, input);
                }
                AgentEvent::ToolFinish { tool, output } => {
                     // Truncate output if too long
                     let display_output = if output.len() > 200 {
                         format!("{}...", &output[..200])
                     } else {
                         output
                     };
                     println!("\n[Tool Finish: {}] Output: {}", tool, display_output);
                }
                AgentEvent::Finish { .. } => {
                    println!(); // Newline at end
                }
                AgentEvent::Error { error } => {
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
    /// Initial prompt to start the session
    #[arg(trailing_var_arg = true)]
    prompt: Option<Vec<String>>,

    /// Custom OpenAI Base URL
    #[arg(long, env = "OPENAI_BASE_URL")]
    base_url: Option<String>,

    /// Custom Model Name
    #[arg(long, short, env = "OPENAI_MODEL", default_value = "minimax-m2.5")]
    model: String,

    /// Initial Agent (default: build)
    #[arg(long, default_value = "build")]
    agent: String,

    /// Log level
    #[arg(long, default_value = "info")]
    log_level: String,

    /// List available agents
    #[arg(long)]
    list_agents: bool,

    /// Session ID for persistent memory (optional, generated if not provided)
    #[arg(long)]
    session_id: Option<String>,

    /// Path to SQLite database file
    #[arg(long)]
    db_path: Option<String>,

    /// Path to MCP configuration file
    #[arg(long)]
    config: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Load .env file
    dotenvy::dotenv().ok();
    
    let cli = Cli::parse();

    if cli.list_agents {
        println!("Available Agents:");
        for agent in registry::list_available_agents() {
            println!("  - {}: {}", agent.name, agent.description);
        }
        return Ok(());
    }

    // Initialize logging
    let log_level = match cli.log_level.to_lowercase().as_str() {
        "error" => Level::ERROR,
        "warn" => Level::WARN,
        "info" => Level::INFO,
        "debug" => Level::DEBUG,
        "trace" => Level::TRACE,
        _ => Level::INFO,
    };
    
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(log_level)
        .finish();
        
    tracing::subscriber::set_global_default(subscriber)?;

    // Configure Environment for LLM (Internal/Legacy Defaults)
    let api_key = env::var("OPENAI_API_KEY").unwrap_or_else(|_| "sk-Ie7Kric4qcPEHli7GRYv39GmCFGuBZkewduwm6ZTE1LKrrx8".to_string());
    
    let base_url = if let Some(url) = cli.base_url {
        Some(url)
    } else if let Ok(url) = env::var("OPENAI_BASE_URL") {
        Some(url)
    } else {
        // Default internal endpoint
        Some("http://10.28.0.22:30530/v1".to_string())
    };

    // Initialize Provider
    let provider = if let Some(url) = base_url {
        Arc::new(OpenAIProvider::new_with_endpoint(&api_key, &url, &cli.model))
    } else {
        Arc::new(OpenAIProvider::new(&api_key, &cli.model))
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
            std::fs::create_dir_all(&gewu_dir)
                .context("Failed to create .gewu directory")?;
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
    println!("Session ID: {}", session_id);
    
    // Load MCP tools
    let mcp_tools = mcp_loader::load_mcp_tools(cli.config.clone()).await?;

    // Create Agent
    let mut agent = registry::create_agent(&cli.agent, provider, pool, session_id, mcp_tools).await?;
    info!("Initialized agent: {}", cli.agent);

    // Initial Prompt handling
    if let Some(prompt_parts) = cli.prompt {
        let prompt = prompt_parts.join(" ");
        if !prompt.is_empty() {
            println!("> {}", prompt);
            if let Err(e) = run_agent(&mut agent, &prompt).await {
                error!("Error: {}", e);
            }
        }
    }

    // Interactive Loop
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

    Ok(())
}
