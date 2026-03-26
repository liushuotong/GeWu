use gewu_core::agent::AgentBuilder;
use gewu_providers::OpenAIProvider;
use gewu_tools::BashTool;
use std::sync::Arc;
use anyhow::Result;

const ENDPOINT: &str = "http://10.28.0.22:30530/v1";
const API_KEY: &str = "sk-Ie7Kric4qcPEHli7GRYv39GmCFGuBZkewduwm6ZTE1LKrrx8";

#[tokio::main]
async fn main() -> Result<()> {
    // Check if API key is set, otherwise mock or skip
    let api_key = API_KEY.to_string();
    
    let provider = OpenAIProvider::new_with_endpoint(&api_key, ENDPOINT, "minimax-m2.5");
    let provider = Arc::new(provider);
    let bash_tool: Box<dyn gewu_core::traits::Tool> = Box::new(BashTool {
        working_directory: std::env::current_dir()?,
    });

    let mut agent = AgentBuilder::default()
        .llm(provider)
        .with_tools(vec![bash_tool])
        .system_prompt("You are a helpful assistant. Use the bash tool to answer questions about the system.")
        .max_turns(5)
        .build()?;

    if api_key == "sk-test-key" {
        println!("No OPENAI_API_KEY set, skipping actual call.");
        return Ok(());
    }

    println!("Agent: Asking 'List the files in the current directory'");
    let response = agent.ask("List the files in the current directory").await?;
    println!("Response: {}", response);

    Ok(())
}
