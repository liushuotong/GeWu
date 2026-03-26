use gewu_core::agent::AgentBuilder;
use gewu_core::traits::{LlmProvider, Tool};
use gewu_core::types::{Message, ToolCall, FunctionCall};
use async_trait::async_trait;
use std::sync::{Arc, Mutex};
use std::error::Error;
use serde_json::Value;

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
    async fn generate_response(&self, _messages: &[Message], _tools: &[Box<dyn Tool>]) -> Result<Message, Box<dyn Error + Send + Sync>> {
        let mut responses = self.responses.lock().unwrap();
        if responses.is_empty() {
             return Err(Box::new(MockError("No more mock responses".to_string())));
        }
        Ok(responses.remove(0))
    }
}

struct MockTool;

#[async_trait]
impl Tool for MockTool {
    fn name(&self) -> &str { "mock_tool" }
    fn description(&self) -> &str { "Mock tool" }
    fn parameters_schema(&self) -> Value { serde_json::json!({}) }
    async fn call(&self, _args: &str) -> Result<String, Box<dyn Error + Send + Sync>> {
        Ok("Tool output".to_string())
    }
}

#[tokio::test]
async fn test_agent_loop() {
    let tool_call = ToolCall {
        id: "call_1".to_string(),
        r#type: "function".to_string(),
        function: FunctionCall {
            name: "mock_tool".to_string(),
            arguments: "{}".to_string(),
        },
    };

    let responses = vec![
        // 1. LLM decides to call tool
        Message::Assistant {
            content: None,
            tool_calls: Some(vec![tool_call]),
        },
        // 2. LLM sees tool output and gives final answer
        Message::Assistant {
            content: Some("Done: Tool output".to_string()),
            tool_calls: None,
        },
    ];
    
    let llm = Arc::new(MockLlm {
        responses: Arc::new(Mutex::new(responses)),
    });
    
    let tool = Box::new(MockTool);
    
    let mut agent = AgentBuilder::default()
        .llm(llm)
        .with_tools(vec![tool])
        .system_prompt("System")
        .build()
        .unwrap();
        
    let result = agent.ask("Do something").await.unwrap();
    assert_eq!(result, "Done: Tool output");
}
