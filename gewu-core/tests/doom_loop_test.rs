use gewu_core::traits::{LlmProvider, Tool};
use gewu_core::types::{Message, ToolCall};
use gewu_core::Agent;
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use serde_json::json;

struct MockLlmProvider;

#[async_trait]
impl LlmProvider for MockLlmProvider {
    async fn generate_response(
        &self,
        messages: &[Message],
        _tools: &[Box<dyn Tool>],
    ) -> Result<Message, Box<dyn std::error::Error + Send + Sync>> {
        // Simple logic: If we see < 3 tool calls, output another one.
        // If we see >= 3, maybe we stop? No, we want to trigger the detection.
        
        // Count tool responses in history
        let tool_response_count = messages.iter().filter(|m| matches!(m, Message::Tool { .. })).count();

        // If we have seen 3 tool outputs (0, 1, 2), the NEXT call (3) should trigger doom loop.
        // 0 -> call 0
        // output 0
        // 1 -> call 1
        // output 1
        // 2 -> call 2
        // output 2
        // 3 -> call 3 (doom loop triggers HERE)

        if tool_response_count >= 5 {
             return Ok(Message::Assistant {
                 content: Some("Done".to_string()),
                 tool_calls: None
             });
        }
        
        Ok(Message::Assistant {
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: format!("call_{}", tool_response_count),
                r#type: "function".to_string(),
                function: gewu_core::types::FunctionCall {
                    name: "test_tool".to_string(),
                    arguments: "{}".to_string(),
                }
            }])
        })
    }
}

struct TestTool;

#[async_trait]
impl Tool for TestTool {
    fn name(&self) -> &str { "test_tool" }
    fn description(&self) -> &str { "test" }
    fn parameters_schema(&self) -> Value { json!({}) }
    async fn call(&self, _args: &str) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok("output".to_string())
    }
}

#[tokio::test]
async fn test_doom_loop_detection() {
    let agent = Agent::builder()
        .llm(Arc::new(MockLlmProvider))
        .with_tools(vec![Box::new(TestTool)])
        .build()
        .unwrap();

    let mut agent = agent;
    let response = agent.ask("Trigger doom loop").await.unwrap();
    
    // We expect the agent to stop early with the doom loop message
    assert!(response.contains("Doom loop detected"), "Response was: {}", response);
    assert!(response.contains("Stopping"), "Response was: {}", response);
}
