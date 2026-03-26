use anyhow::{Context, Result};
use gewu_core::traits::Tool;
use gewu_tools::mcp::{McpClient, McpConfig, McpToolWrapper};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn};

pub async fn load_mcp_tools(config_path_override: Option<String>) -> Result<Vec<Box<dyn Tool>>> {
    let (config_path, is_override) = if let Some(path_str) = config_path_override {
        (PathBuf::from(path_str), true)
    } else {
        (get_config_path()?, false)
    };
    
    if !config_path.exists() {
        if is_override {
            warn!("MCP config file specified but not found: {:?}", config_path);
        }
        return Ok(Vec::new());
    }
    
    info!("Loading MCP config from: {:?}", config_path);
    
    let content = tokio::fs::read_to_string(&config_path).await
        .context("Failed to read MCP config file")?;
        
    let config: McpConfig = serde_json::from_str(&content)
        .context("Failed to parse MCP config JSON")?;
        
    let mut tools: Vec<Box<dyn Tool>> = Vec::new();
    
    for (server_name, server_config) in config.mcp_servers {
        info!("Connecting to MCP server: {}", server_name);
        
        match McpClient::connect(&server_config).await {
            Ok(mut client) => {
                if let Err(e) = client.initialize().await {
                    warn!("Failed to initialize MCP server '{}': {}", server_name, e);
                    continue;
                }
                
                let mcp_tools = match client.list_tools().await {
                    Ok(t) => t,
                    Err(e) => {
                        warn!("Failed to list tools from MCP server '{}': {}", server_name, e);
                        continue;
                    }
                };
                
                info!("Loaded {} tools from '{}'", mcp_tools.len(), server_name);
                
                let client_arc = Arc::new(Mutex::new(client));
                
                for tool in mcp_tools {
                    tools.push(Box::new(McpToolWrapper {
                        client: client_arc.clone(),
                        name: tool.name,
                        description: tool.description.unwrap_or_default(),
                        schema: tool.input_schema,
                    }));
                }
            }
            Err(e) => {
                warn!("Failed to connect to MCP server '{}': {}", server_name, e);
            }
        }
    }
    
    Ok(tools)
}

fn get_config_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    Ok(home.join(".gewu").join("mcp.json"))
}
