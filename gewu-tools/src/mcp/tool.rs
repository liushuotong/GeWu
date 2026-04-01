use crate::mcp::client::McpClient;
use anyhow::Result;
use gewu_core::traits::Tool;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct McpToolWrapper {
    pub client: Arc<Mutex<McpClient>>,
    pub name: String,
    pub description: String,
    pub schema: Value,
}

#[async_trait::async_trait]
impl Tool for McpToolWrapper {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Value {
        self.schema.clone()
    }

    async fn call(&self, args: &str) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let args_val: Value = serde_json::from_str(args)?;

        let client = self.client.clone();

        // Lock client to send request - we need to drop the lock before awaiting potentially?
        // No, client.call_tool handles the protocol.
        let client_guard = client.lock().await;

        let result = client_guard
            .call_tool(&self.name, args_val)
            .await
            .map_err(|e| {
                // Convert anyhow::Error to Box<dyn std::error::Error + Send + Sync>
                // anyhow::Error doesn't impl std::error::Error directly in a way that allows simple casting sometimes?
                // Actually it does impl it if 'std' feature is on (default).
                // But Box<anyhow::Error> is not Box<dyn Error>.
                // We can format it to string and make a new error? Or use .into()?
                let s = e.to_string();
                Box::new(std::io::Error::new(std::io::ErrorKind::Other, s))
                    as Box<dyn std::error::Error + Send + Sync>
            })?;

        Ok(result)
    }
}
