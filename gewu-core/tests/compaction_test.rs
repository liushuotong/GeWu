use gewu_core::agent::AgentBuilder;
use gewu_core::traits::{LlmProvider, Tool};
use gewu_core::types::Message;
use async_trait::async_trait;
use std::sync::{Arc, Mutex};
use std::error::Error;

struct MockLlm {
    responses: Arc<Mutex<Vec<Message>>>,
}

#[derive(Debug)]
struct MockError(String);
impl std::fmt::Display for MockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{}", self.0) }
}
impl Error for MockError {}

#[async_trait]
impl LlmProvider for MockLlm {
    async fn generate_response(&self, messages: &[Message], _tools: &[Box<dyn Tool>]) -> Result<Message, Box<dyn Error + Send + Sync>> {
        // Check if this is a summary request
        let last_msg = messages.last().unwrap();
        if let Message::User { content } = last_msg {
            if content.contains("Provide a detailed prompt for continuing") {
                return Ok(Message::Assistant {
                    content: Some("## Goal\nTest compaction\n## Accomplished\nNothing yet".to_string()),
                    tool_calls: None,
                });
            }
        }

        let mut responses = self.responses.lock().unwrap();
        if responses.is_empty() {
             // Fallback default response
             return Ok(Message::Assistant { content: Some("Default response".to_string()), tool_calls: None });
        }
        Ok(responses.remove(0))
    }
}

#[tokio::test]
async fn test_compaction_trigger() {
    let responses = vec![
        // Response to "Hello" (after compaction)
        Message::Assistant {
            content: Some("I am ready.".to_string()),
            tool_calls: None,
        },
    ];
    
    let llm = Arc::new(MockLlm {
        responses: Arc::new(Mutex::new(responses)),
    });
    
    // Set a very low limit (e.g., 100 tokens). 
    // "Hello" is small, but we will pre-fill history.
    let mut agent = AgentBuilder::default()
        .llm(llm)
        .context_limit(100) 
        .system_prompt("System")
        .build()
        .unwrap();

    // Fill history with junk to exceed 100 tokens
    // 1 token ~ 4 chars. 100 tokens ~ 400 chars.
    let long_text = "A".repeat(500); 
    agent.memory.add_message(Message::User { content: long_text });

    // This call should trigger compaction because history > 100
    let response = agent.ask("Hello").await.unwrap();
    
    assert_eq!(response, "I am ready.");

    // Verify history structure
    // 1. System
    // 2. Summary (Assistant)
    // 3. User ("Hello")
    // 4. Assistant ("I am ready.")
    
    assert_eq!(agent.memory.messages().len(), 4);
    
    match &agent.memory.messages()[1] {
        Message::Assistant { content, .. } => {
            let text = content.as_ref().unwrap();
            assert!(text.contains("Summary of previous conversation"));
            assert!(text.contains("## Goal"));
        },
        _ => panic!("Expected summary message at index 1"),
    }
}
