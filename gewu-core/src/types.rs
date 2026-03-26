use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub r#type: String, // usually "function"
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProviderEvent {
    ReasoningDelta { delta: String },
    ContentDelta { delta: String },
    ToolCallStart { id: String, name: String },
    ToolCallDelta { id: String, delta: String },
    Finish { reason: Option<String> },
    Error { error: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    // From LLM (pass-through)
    ReasoningDelta { delta: String },
    ContentDelta { delta: String },

    // Tool execution status
    ToolStart { tool: String, input: String },
    ToolFinish { tool: String, output: String },

    // Final result (indicates end of turn)
    Finish { response: String },
    Error { error: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum Message {
    System {
        content: String,
    },
    User {
        content: String,
    },
    Assistant {
        content: Option<String>,
        tool_calls: Option<Vec<ToolCall>>,
    },
    Tool {
        tool_call_id: String,
        content: String,
    },
}
