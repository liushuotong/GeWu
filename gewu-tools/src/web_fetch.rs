//! Web Fetch Tool
//!
//! This tool allows the AI to retrieve content from specified URLs.
//! It supports converting HTML responses into Markdown for easier consumption by the LLM.

use crate::fs_utils::into_tool_error;
use gewu_core::traits::Tool;
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::time::Duration;
use std::error::Error;

/// Tool for fetching web content.
pub struct WebFetchTool;

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "webfetch"
    }

    fn description(&self) -> &str {
        include_str!("prompts/webfetch.txt")
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to fetch content from"
                },
                "format": {
                    "type": "string",
                    "enum": ["markdown", "text", "html"],
                    "default": "markdown",
                    "description": "The format to return the content in"
                }
            },
            "required": ["url"]
        })
    }

    async fn call(&self, args: &str) -> Result<String, Box<dyn Error + Send + Sync>> {
        let args_val: Value = serde_json::from_str(args).map_err(into_tool_error)?;
        let mut url = args_val["url"].as_str().ok_or_else(|| anyhow::anyhow!("url is required")).map_err(into_tool_error)?.to_string();
        let format = args_val["format"].as_str().unwrap_or("markdown");

        if url.starts_with("http://") && !url.contains("127.0.0.1") && !url.contains("localhost") {
            url = url.replacen("http://", "https://", 1);
        } else if !url.starts_with("http://") && !url.starts_with("https://") {
            url = format!("https://{}", url);
        }

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("opencode-cli/0.1.0")
            .build()
            .map_err(into_tool_error)?;

        let response = client.get(url).send().await.map_err(into_tool_error)?;
        let status = response.status();

        if !status.is_success() {
            return Err(anyhow::anyhow!("Request failed with status: {}", status).into());
        }

        let html = response.text().await.map_err(into_tool_error)?;

        match format {
            "html" => Ok(html),
            _ => {
                // Convert HTML to Markdown for better LLM readability
                // html2md is a crate that does this conversion
                let markdown = html2md::parse_html(&html);
                Ok(markdown)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use mockito;
    use serde_json::json;

    #[tokio::test]
    async fn test_web_fetch_tool() -> Result<()> {
        let mut server = mockito::Server::new_async().await;
        
        let _m = server.mock("GET", "/hello")
            .with_status(200)
            .with_header("content-type", "text/html")
            .with_body("<html><body><h1>Hello, World!</h1></body></html>")
            .create_async().await;

        let tool = WebFetchTool;
        let url = format!("{}/hello", server.url());

        // Test Markdown format (default)
        let args = json!({ "url": url });
        let output_md = tool.call(&args.to_string()).await.map_err(|e| anyhow::anyhow!(e.to_string()))?;
        
        // Assert content is converted
        assert!(output_md.contains("Hello, World!"));
        // The title should be in markdown format, so no HTML tags
        assert!(!output_md.contains("<h1>"));
        assert!(!output_md.contains("<html>"));

        Ok(())
    }

    #[tokio::test]
    async fn test_web_fetch_error() -> Result<()> {
        let mut server = mockito::Server::new_async().await;
        let _m = server.mock("GET", "/404")
            .with_status(404)
            .create_async().await;

        let tool = WebFetchTool;
        let url = format!("{}/404", server.url());

        let args = json!({ "url": url });
        let result = tool.call(&args.to_string()).await;
        
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("404"));
        Ok(())
    }
}
