use gewu_core::traits::{LlmProvider, Tool};
use gewu_core::types::{Message, ProviderEvent, AgentEvent};
use gewu_core::Agent;
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use serde_json::json;
use futures::stream::BoxStream;
use futures::StreamExt;
use std::error::Error;

struct MockLlmProvider;

#[async_trait]
impl LlmProvider for MockLlmProvider {
    async fn stream_response<'a>(
        &'a self,
        messages: &[Message],
        _tools: &[Box<dyn Tool>],
    ) -> Result<BoxStream<'a, Result<ProviderEvent, Box<dyn Error + Send + Sync>>>, Box<dyn Error + Send + Sync>> {
        let tool_response_count = messages.iter().filter(|m| matches!(m, Message::Tool { .. })).count();

        // 0 -> call 0
        // output 0
        // 1 -> call 1
        // output 1
        // 2 -> call 2
        // output 2
        // 3 -> call 3 (doom loop triggers HERE)

        if tool_response_count >= 5 {
             let s = async_stream::try_stream! {
                 yield ProviderEvent::ContentDelta { delta: "Done".to_string() };
                 yield ProviderEvent::Finish { reason: Some("stop".to_string()) };
             };
             return Ok(Box::pin(s));
        }
        
        // Return a tool call
        let call_id = format!("call_{}", tool_response_count);
        let s = async_stream::try_stream! {
            yield ProviderEvent::ToolCallStart { id: call_id.clone(), name: "test_tool".to_string() };
            yield ProviderEvent::ToolCallDelta { id: call_id, delta: "{}".to_string() };
            yield ProviderEvent::Finish { reason: Some("tool_calls".to_string()) };
        };
        Ok(Box::pin(s))
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
    let mut stream = Box::pin(agent.ask("Trigger doom loop"));
    let mut response_text = String::new();
    let mut error_text = String::new();

    while let Some(event_result) = stream.next().await {
        match event_result {
            Ok(event) => {
                match event {
                    AgentEvent::ContentDelta { delta } => response_text.push_str(&delta),
                    AgentEvent::Finish { response } => response_text = response,
                    AgentEvent::Error { error } => error_text = error,
                    _ => {}
                }
            }
            Err(e) => panic!("Stream error: {}", e),
        }
    }
    
    // We expect the agent to stop early with the doom loop message
    assert!(error_text.contains("Doom loop detected"), "Error was: {}", error_text);
    assert!(error_text.contains("Stopping"), "Error was: {}", error_text);
}
