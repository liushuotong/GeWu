//! Search Tools
//!
//! This module implements tools for semantic code search and web search, powered by Exa (formerly Metaphor).
//!
//! - `CodeSearchTool`: Finds relevant code snippets using semantic embedding search.
//! - `WebSearchTool`: Performs general web searches to retrieve external information.

use crate::fs_utils::into_tool_error;
use gewu_core::traits::Tool;
use anyhow::Result;
use futures_util::StreamExt;
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Value};
use async_trait::async_trait;
use std::error::Error;

/// Tool for semantic code search.
///
/// Uses an external MCP (Model Context Protocol) server (hosted by Exa) to perform
/// embedding-based search over the codebase.
pub struct CodeSearchTool;

#[derive(Deserialize)]
struct CodeSearchArgs {
    query: String,
    #[serde(rename = "tokensNum")]
    tokens_num: Option<u32>,
}

#[async_trait]
impl Tool for CodeSearchTool {
    fn name(&self) -> &str {
        "codesearch"
    }

    fn description(&self) -> &str {
        include_str!("prompts/codesearch.txt")
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Search query" },
                "tokensNum": { "type": "integer", "description": "Number of tokens (default 5000)" }
            },
            "required": ["query"]
        })
    }

    async fn call(&self, args: &str) -> Result<String, Box<dyn Error + Send + Sync>> {
        let input: CodeSearchArgs = serde_json::from_str(args).map_err(into_tool_error)?;

        let client = Client::new();
        // Call the Exa MCP endpoint
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "get_code_context_exa",
                "arguments": {
                    "query": input.query,
                    "tokensNum": input.tokens_num.unwrap_or(5000)
                }
            }
        });

        let res = client
            .post("https://mcp.exa.ai/mcp")
            .json(&body)
            .send()
            .await
            .map_err(into_tool_error)?;

        if !res.status().is_success() {
            return Err(anyhow::anyhow!("Request failed: {}", res.status()).into());
        }

        // The response is SSE (text/event-stream)
        // We parse the stream to find the result content
        let mut stream = res.bytes_stream();
        while let Some(item) = stream.next().await {
            let chunk = item.map_err(into_tool_error)?;
            let text = String::from_utf8_lossy(&chunk);
            for line in text.lines() {
                if line.starts_with("data: ") {
                    let data_str = &line[6..];
                    if let Ok(val) = serde_json::from_str::<Value>(data_str) {
                        if let Some(content) = val
                            .get("result")
                            .and_then(|r| r.get("content"))
                            .and_then(|c| c.as_array())
                        {
                            if let Some(first) = content.first() {
                                if let Some(text) = first.get("text").and_then(|t| t.as_str()) {
                                    return Ok(text.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok("No results found.".to_string())
    }
}

/// Tool for web search.
///
/// Uses Exa to retrieve search results from the web.
pub struct WebSearchTool;

#[derive(Deserialize)]
struct WebSearchArgs {
    query: String,
    #[serde(rename = "numResults")]
    num_results: Option<u32>,
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "websearch"
    }

    fn description(&self) -> &str {
        include_str!("prompts/websearch.txt")
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Search query" },
                "numResults": { "type": "integer", "description": "Number of results (default 8)" }
            },
            "required": ["query"]
        })
    }

    async fn call(&self, args: &str) -> Result<String, Box<dyn Error + Send + Sync>> {
        let input: WebSearchArgs = serde_json::from_str(args).map_err(into_tool_error)?;

        let client = Client::new();
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "web_search_exa",
                "arguments": {
                    "query": input.query,
                    "numResults": input.num_results.unwrap_or(8),
                    "type": "auto",
                    "livecrawl": "fallback"
                }
            }
        });

        let res = client
            .post("https://mcp.exa.ai/mcp")
            .json(&body)
            .send()
            .await
            .map_err(into_tool_error)?;

        if !res.status().is_success() {
            return Err(anyhow::anyhow!("Request failed: {}", res.status()).into());
        }

        let mut stream = res.bytes_stream();
        while let Some(item) = stream.next().await {
            let chunk = item.map_err(into_tool_error)?;
            let text = String::from_utf8_lossy(&chunk);
            for line in text.lines() {
                if line.starts_with("data: ") {
                    let data_str = &line[6..];
                    if let Ok(val) = serde_json::from_str::<Value>(data_str) {
                        if let Some(content) = val
                            .get("result")
                            .and_then(|r| r.get("content"))
                            .and_then(|c| c.as_array())
                        {
                            if let Some(first) = content.first() {
                                if let Some(text) = first.get("text").and_then(|t| t.as_str()) {
                                    return Ok(text.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok("No results found.".to_string())
    }
}
