mod registry;
mod tools;

use anyhow::Result;
use clap::Parser;
use gewu_providers::OpenAIProvider;
use std::sync::Arc;
use tracing::{info, error, Level};
use std::env;

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

    // Create Agent
    let mut agent = registry::create_agent(&cli.agent, provider)?;
    info!("Initialized agent: {}", cli.agent);

    // Initial Prompt handling
    if let Some(prompt_parts) = cli.prompt {
        let prompt = prompt_parts.join(" ");
        if !prompt.is_empty() {
            println!("> {}", prompt);
            match agent.ask(&prompt).await {
                Ok(response) => println!("{}", response),
                Err(e) => error!("Error: {}", e),
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

        match agent.ask(input).await {
            Ok(response) => println!("{}", response),
            Err(e) => error!("Error processing message: {}", e),
        }
    }

    Ok(())
}
