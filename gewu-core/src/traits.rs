use crate::types::{Message, ProviderEvent};
use async_trait::async_trait;
use futures::stream::BoxStream;
use serde_json::Value;
use std::error::Error;
use std::sync::Arc;

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> Value;
    async fn call(&self, args: &str) -> Result<String, Box<dyn Error + Send + Sync>>;
}

#[async_trait]
pub trait ToolRegistry: Send + Sync {
    async fn execute(&self, name: &str, args: &str)
    -> Result<String, Box<dyn Error + Send + Sync>>;
    fn list_tools(&self) -> Vec<String>;
}

#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn stream_response<'a>(
        &'a self,
        messages: &[Message],
        tools: &[Arc<dyn Tool>],
    ) -> Result<
        BoxStream<'a, Result<ProviderEvent, Box<dyn Error + Send + Sync>>>,
        Box<dyn Error + Send + Sync>,
    >;

    async fn list_models(&self) -> Result<Vec<String>, Box<dyn Error + Send + Sync>>;
}

#[async_trait]
pub trait Memory: Send + Sync {
    fn messages(&self) -> &[Message];
    async fn add_message(&mut self, message: Message);
    async fn clear(&mut self);
    fn messages_mut(&mut self) -> &mut [Message];
    async fn replace(&mut self, messages: Vec<Message>);
}

#[async_trait]
impl Memory for Vec<Message> {
    fn messages(&self) -> &[Message] {
        self
    }

    async fn add_message(&mut self, message: Message) {
        self.push(message);
    }

    async fn clear(&mut self) {
        self.clear();
    }

    fn messages_mut(&mut self) -> &mut [Message] {
        self.as_mut_slice()
    }

    async fn replace(&mut self, messages: Vec<Message>) {
        *self = messages;
    }
}
