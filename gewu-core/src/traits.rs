use crate::types::Message;
use async_trait::async_trait;
use serde_json::Value;
use std::error::Error;

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> Value;
    async fn call(&self, args: &str) -> Result<String, Box<dyn Error + Send + Sync>>;
}

#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn generate_response(
        &self,
        messages: &[Message],
        tools: &[Box<dyn Tool>],
    ) -> Result<Message, Box<dyn Error + Send + Sync>>;
}
