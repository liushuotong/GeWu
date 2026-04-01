use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub r#type: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ContentPart {
    Text { text: String },
    Image { data: String, mime_type: String },
    Audio { data: String, mime_type: String },
    Video { data: String, mime_type: String },
    File { data: String, mime_type: String },
}

impl ContentPart {
    pub fn mime_type(&self) -> String {
        match self {
            ContentPart::Text { .. } => "text/plain".to_string(),
            ContentPart::Image { mime_type, .. } => mime_type.clone(),
            ContentPart::Audio { mime_type, .. } => mime_type.clone(),
            ContentPart::Video { mime_type, .. } => mime_type.clone(),
            ContentPart::File { mime_type, .. } => mime_type.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Capabilities {
    pub input: InputCapabilities,
    pub output: OutputCapabilities,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InputCapabilities {
    pub text: bool,
    pub image: bool,
    pub audio: bool,
    pub video: bool,
    pub pdf: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OutputCapabilities {
    pub text: bool,
    pub image: bool,
    pub audio: bool,
    pub video: bool,
    pub pdf: bool,
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
    ReasoningDelta { delta: String },
    ContentDelta { delta: String },
    ToolStart { tool: String, input: String },
    ToolFinish { tool: String, output: String },
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
        content: Vec<ContentPart>,
    },
    Assistant {
        content: Option<String>,
        tool_calls: Option<Vec<ToolCall>>,
    },
    Tool {
        tool_call_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_name: Option<String>,
        content: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        media: Vec<ContentPart>,
    },
}
