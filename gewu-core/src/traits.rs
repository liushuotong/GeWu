use crate::types::{Message, ProviderEvent};
use async_trait::async_trait;
use serde_json::Value;
use std::error::Error;
use futures::stream::BoxStream;

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> Value;
    async fn call(&self, args: &str) -> Result<String, Box<dyn Error + Send + Sync>>;
}

#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn stream_response<'a>(
        &'a self,
        messages: &[Message],
        tools: &[Box<dyn Tool>],
    ) -> Result<BoxStream<'a, Result<ProviderEvent, Box<dyn Error + Send + Sync>>>, Box<dyn Error + Send + Sync>>;
}

#[async_trait]
pub trait Memory: Send + Sync {
    fn messages(&self) -> &[Message];
    async fn add_message(&mut self, message: Message);
    async fn clear(&mut self);
    // Expose mutable slice for in-place modifications (like pruning)
    fn messages_mut(&mut self) -> &mut [Message];
    // Replace entire history (for compaction/summarization)
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
