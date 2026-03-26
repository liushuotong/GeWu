use gewu_core::agent::AgentBuilder;
use gewu_providers::OpenAIProvider;
use gewu_tools::BashTool;
use std::sync::Arc;
use anyhow::Result;
use std::env;

#[tokio::main]
async fn main() -> Result<()> {
    // Get configuration from environment variables
    let api_key = env::var("CUSTOM_LLM_KEY").unwrap_or_else(|_| "sk-test-key".to_string());
    let base_url = env::var("CUSTOM_LLM_URL").unwrap_or_else(|_| "http://localhost:8080/v1".to_string());
    let model = env::var("CUSTOM_LLM_MODEL").unwrap_or_else(|_| "llama3-8b-8192".to_string());

    println!("Using LLM Provider:");
    println!("  URL: {}", base_url);
    println!("  Model: {}", model);

    // Initialize the provider with the custom endpoint
    let provider = Arc::new(OpenAIProvider::new_with_endpoint(&api_key, &base_url, &model));
    
    let bash_tool = Box::new(BashTool {
        working_directory: env::current_dir()?,
        safe_mode: false,
    });

    let mut agent = AgentBuilder::default()
        .llm(provider)
        .with_tools(vec![bash_tool])
        .system_prompt("You are a helpful assistant.")
        .build()?;

    // Skip actual call if using test key and default localhost URL to avoid connection errors in CI/Test
    if api_key == "sk-test-key" && base_url.contains("localhost") {
        println!("No CUSTOM_LLM_KEY/URL set, skipping actual network call.");
        return Ok(());
    }
    
    use futures::StreamExt;
    let mut stream = Box::pin(agent.ask("Hello, who are you?"));
    while let Some(event) = stream.next().await {
        if let Ok(gewu_core::types::AgentEvent::ContentDelta { delta }) = event {
             print!("{}", delta);
        }
    }
    println!();

    Ok(())
}
