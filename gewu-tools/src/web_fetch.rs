//! Web Fetch Tool
//!
//! This tool allows the AI to retrieve content from specified URLs.
//! It supports converting HTML responses into Markdown for easier consumption by the LLM.
//!
//! # SSRF protections
//!
//! - Non-`http(s)` schemes are rejected (no `file:`, `gopher:`, ...).
//! - Private and link-local network addresses are blocked, including
//!   cloud-metadata endpoints like `169.254.169.254`. Loopback is allowed so
//!   the tool keeps working against local development servers.
//! - Redirects are validated hop-by-hop with the same rules, so a public
//!   URL cannot redirect into an internal address.

use crate::fs_utils::into_tool_error;
use anyhow::Result;
use async_trait::async_trait;
use gewu_core::traits::Tool;
use gewu_core::types::{ToolContext, ToolResult};
use schemars::schema::RootSchema;
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::net::IpAddr;
use std::time::Duration;
use url::Url;

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct WebFetchArgs {
    pub url: String,
    #[serde(default = "default_format")]
    pub format: String,
}

fn default_format() -> String {
    "markdown".to_string()
}

pub struct WebFetchTool;

/// Returns true if the host resolves to a loopback address or is a
/// loopback-style name. Loopback is intentionally allowed (local dev
/// servers), everything else internal is blocked by [`is_blocked_host`].
fn is_loopback_host(host: &str) -> bool {
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    let host = host.to_lowercase();
    if host == "localhost" || host.ends_with(".localhost") {
        return true;
    }
    matches!(
        host.parse::<IpAddr>(),
        Ok(IpAddr::V4(v4)) if v4.is_loopback()
    ) || matches!(host.parse::<IpAddr>(), Ok(IpAddr::V6(v6)) if v6.is_loopback())
}

/// Returns true if the host must not be fetched: private networks,
/// link-local (incl. cloud metadata endpoints), and unspecified addresses.
fn is_blocked_host(host: &str) -> bool {
    let host = host.trim().to_lowercase();
    if host == "metadata.google.internal" {
        return true;
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => {
            v4.is_private() || v4.is_link_local() || v4.is_unspecified() || v4.is_broadcast()
        }
        Ok(IpAddr::V6(v6)) => {
            v6.is_unique_local() || v6.is_unicast_link_local() || v6.is_unspecified()
        }
        Err(_) => false,
    }
}

/// Validates a URL is fetchable: http(s) only, host present, and the host is
/// not an internal address.
fn validate_url(url: &Url) -> Result<(), String> {
    match url.scheme() {
        "http" | "https" => {}
        other => return Err(format!("unsupported URL scheme: {}", other)),
    }
    let host = url
        .host_str()
        .ok_or_else(|| "URL has no host".to_string())?;
    if is_blocked_host(host) {
        return Err(format!(
            "access to internal host '{}' is blocked (private/link-local network)",
            host
        ));
    }
    Ok(())
}

/// Redirect policy that re-validates every hop, so a public URL cannot
/// redirect into an internal address.
fn redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= 10 {
            return attempt.error("too many redirects");
        }
        match validate_url(attempt.url()) {
            Ok(()) => attempt.follow(),
            Err(e) => attempt.error(e),
        }
    })
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "webfetch"
    }

    fn description(&self) -> &str {
        include_str!("prompts/webfetch.txt")
    }

    fn schema(&self) -> RootSchema {
        schemars::schema_for!(WebFetchArgs)
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, Box<dyn Error + Send + Sync>> {
        let input: WebFetchArgs = serde_json::from_value(args).map_err(into_tool_error)?;

        let mut raw = input.url;
        if !raw.contains("://") {
            raw = format!("https://{}", raw);
        }
        let parsed =
            Url::parse(&raw).map_err(|e| into_tool_error(anyhow::anyhow!(e.to_string())))?;

        // Upgrade plain http to https for public hosts; keep http for
        // loopback so local development servers keep working.
        let loopback = parsed.host_str().map(is_loopback_host).unwrap_or(false);
        let url = if parsed.scheme() == "http" && !loopback {
            parsed.to_string().replacen("http://", "https://", 1)
        } else {
            parsed.to_string()
        };
        let final_url =
            Url::parse(&url).map_err(|e| into_tool_error(anyhow::anyhow!(e.to_string())))?;
        validate_url(&final_url).map_err(|e| into_tool_error(anyhow::anyhow!(e)))?;

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(redirect_policy())
            .user_agent("opencode-cli/0.1.0")
            .build()
            .map_err(into_tool_error)?;

        let response = client
            .get(final_url.clone())
            .send()
            .await
            .map_err(into_tool_error)?;
        let status = response.status();

        if !status.is_success() {
            return Ok(ToolResult::new(
                "webfetch",
                format!("Request failed with status: {}", status),
            ));
        }

        let html = response.text().await.map_err(into_tool_error)?;

        let output = match input.format.as_str() {
            "html" => html,
            _ => html2md::parse_html(&html),
        };

        Ok(ToolResult::new("webfetch", output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use mockito;

    #[tokio::test]
    async fn test_web_fetch_tool() -> Result<()> {
        let mut server = mockito::Server::new_async().await;

        let _m = server
            .mock("GET", "/hello")
            .with_status(200)
            .with_header("content-type", "text/html")
            .with_body("<html><body><h1>Hello, World!</h1></body></html>")
            .create_async()
            .await;

        let tool = WebFetchTool;
        let url = format!("{}/hello", server.url());

        let args = serde_json::json!({ "url": url });
        let ctx = ToolContext::new("test", "msg");
        let result = tool
            .execute(args, &ctx)
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;

        assert!(result.output.contains("Hello, World!"));
        assert!(!result.output.contains("<h1>"));
        Ok(())
    }

    #[test]
    fn private_addresses_are_blocked() {
        assert!(is_blocked_host("10.0.0.5"));
        assert!(is_blocked_host("192.168.1.1"));
        assert!(is_blocked_host("172.16.0.9"));
        assert!(is_blocked_host("169.254.169.254"));
        assert!(is_blocked_host("0.0.0.0"));
        assert!(is_blocked_host("fd00::1"));
        assert!(is_blocked_host("fe80::1"));
        assert!(is_blocked_host("metadata.google.internal"));
    }

    #[test]
    fn public_and_loopback_are_allowed() {
        assert!(!is_blocked_host("93.184.216.34"));
        assert!(!is_blocked_host("example.com"));
        assert!(!is_blocked_host("127.0.0.1"));
        assert!(!is_blocked_host("::1"));
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("[::1]"));
    }

    #[test]
    fn non_http_schemes_are_rejected() {
        let url = Url::parse("file:///etc/passwd").unwrap();
        assert!(validate_url(&url).is_err());
        let url = Url::parse("gopher://example.com").unwrap();
        assert!(validate_url(&url).is_err());
    }

    #[tokio::test]
    async fn private_urls_fail_at_runtime() {
        let tool = WebFetchTool;
        let args = serde_json::json!({ "url": "http://169.254.169.254/latest/meta-data/" });
        let ctx = ToolContext::new("test", "msg");
        let err = tool
            .execute(args, &ctx)
            .await
            .expect_err("internal host must be rejected");
        assert!(err.to_string().contains("blocked"));
    }
}
