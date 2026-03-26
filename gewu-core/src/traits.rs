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

pub trait Memory: Send + Sync {
    fn messages(&self) -> &[Message];
    fn add_message(&mut self, message: Message);
    fn clear(&mut self);
    // Expose mutable slice for in-place modifications (like pruning)
    fn messages_mut(&mut self) -> &mut [Message];
    // Replace entire history (for compaction/summarization)
    fn replace(&mut self, messages: Vec<Message>);
}

impl Memory for Vec<Message> {
    fn messages(&self) -> &[Message] {
        self
    }
    
    fn add_message(&mut self, message: Message) {
        self.push(message);
    }
    
    fn clear(&mut self) {
        self.clear();
    }
    
    fn messages_mut(&mut self) -> &mut [Message] {
        self.as_mut_slice()
    }
    
    fn replace(&mut self, messages: Vec<Message>) {
        *self = messages;
    }
}
