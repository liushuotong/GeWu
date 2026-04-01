use async_openai::types::chat::{
    ChatCompletionRequestSystemMessageArgs, ChatCompletionRequestUserMessageArgs,
    ChatCompletionRequestAssistantMessageArgs, ChatCompletionRequestToolMessageArgs,
    CreateChatCompletionRequest, CreateChatCompletionRequestArgs, ChatCompletionTools,
    FunctionObjectArgs, ChatCompletionMessageToolCalls, FunctionCall as OpenAIFunctionCall,
    ChatCompletionRequestMessage, ChatCompletionTool, ChatCompletionMessageToolCall,
    ChatCompletionResponseStream, ChatCompletionRequestUserMessageContentPart,
    ChatCompletionRequestMessageContentPartImage, ImageUrl, ChatCompletionMessageToolCallChunk,
};
use futures::{StreamExt};
use futures::stream::BoxStream;
use gewu_core::traits::{LlmProvider, Tool};
use gewu_core::types::{Message, ProviderEvent, ContentPart};
use async_trait::async_trait;
use async_openai::Client;
use std::error::Error;
use std::sync::Arc;
use serde_json::Value;

pub trait RequestBuilder: Send + Sync {
    fn build(
        &self,
        messages: &[Message],
        tools: &[Arc<dyn Tool>],
    ) -> Result<CreateChatCompletionRequest, Box<dyn Error + Send + Sync>>;
}

pub trait ResponseParser: Send + Sync {
    fn parse_stream(
        &self,
        stream: ChatCompletionResponseStream,
    ) -> BoxStream<'static, Result<ProviderEvent, Box<dyn Error + Send + Sync>>>;
}

pub struct OpenAIProvider {
    client: Client<async_openai::config::OpenAIConfig>,
    model: String,
}

impl OpenAIProvider {
    pub fn new(api_key: &str, model: &str) -> Self {
        let config = async_openai::config::OpenAIConfig::new().with_api_key(api_key);
        Self {
            client: Client::with_config(config),
            model: model.to_string(),
        }
    }

    pub fn new_with_endpoint(api_key: &str, base_url: &str, model: &str) -> Self {
        let config = async_openai::config::OpenAIConfig::new()
            .with_api_key(api_key)
            .with_api_base(base_url);
        Self {
            client: Client::with_config(config),
            model: model.to_string(),
        }
    }

    fn convert_content_part(&self, part: &ContentPart) -> ChatCompletionRequestUserMessageContentPart {
        match part {
            ContentPart::Text { text } => {
                ChatCompletionRequestUserMessageContentPart::Text(
                    async_openai::types::chat::ChatCompletionRequestMessageContentPartText {
                        text: text.clone(),
                    },
                )
            }
            ContentPart::Image { data, mime_type } => {
                let url = format!("data:{};base64,{}", mime_type, data);
                ChatCompletionRequestUserMessageContentPart::ImageUrl(
                    ChatCompletionRequestMessageContentPartImage {
                        image_url: ImageUrl {
                            url: url.into(),
                            detail: None,
                        },
                    },
                )
            }
        }
    }

    fn convert_message(&self, msg: &Message) -> Result<Vec<ChatCompletionRequestMessage>, Box<dyn Error + Send + Sync>> {
        match msg {
            Message::System { content } => {
                let m = ChatCompletionRequestSystemMessageArgs::default()
                    .content(content.clone())
                    .build()
                    .map_err(|e| Box::new(e) as Box<dyn Error + Send + Sync>)?;
                Ok(vec![ChatCompletionRequestMessage::System(m)])
            }
            Message::User { content } => {
                let parts: Vec<ChatCompletionRequestUserMessageContentPart> = content
                    .iter()
                    .map(|p| self.convert_content_part(p))
                    .collect();
                let m = ChatCompletionRequestUserMessageArgs::default()
                    .content(parts)
                    .build()
                    .map_err(|e| Box::new(e) as Box<dyn Error + Send + Sync>)?;
                Ok(vec![ChatCompletionRequestMessage::User(m)])
            }
            Message::Assistant { content, tool_calls } => {
                let mut builder = ChatCompletionRequestAssistantMessageArgs::default();
                if let Some(c) = content {
                    builder.content(c.clone());
                }
                if let Some(calls) = tool_calls {
                    let openai_calls: Vec<ChatCompletionMessageToolCalls> = calls
                        .iter()
                        .map(|tc| {
                            ChatCompletionMessageToolCalls::Function(ChatCompletionMessageToolCall {
                                id: tc.id.clone(),
                                function: OpenAIFunctionCall {
                                    name: tc.function.name.clone(),
                                    arguments: tc.function.arguments.clone(),
                                },
                            })
                        })
                        .collect();
                    builder.tool_calls(openai_calls);
                }
                let m = builder
                    .build()
                    .map_err(|e| Box::new(e) as Box<dyn Error + Send + Sync>)?;
                Ok(vec![ChatCompletionRequestMessage::Assistant(m)])
            }
            Message::Tool {
                tool_call_id,
                tool_name,
                content,
                media,
            } => {
                let m = ChatCompletionRequestToolMessageArgs::default()
                    .tool_call_id(tool_call_id.clone())
                    .content(content.clone())
                    .build()
                    .map_err(|e| Box::new(e) as Box<dyn Error + Send + Sync>)?;
                let mut messages = vec![ChatCompletionRequestMessage::Tool(m)];

                if !media.is_empty() {
                    let mut parts: Vec<ChatCompletionRequestUserMessageContentPart> = Vec::new();
                    let label = match tool_name {
                        Some(name) => format!("Tool '{}' returned additional media:", name),
                        None => "Tool returned additional media:".to_string(),
                    };
                    parts.push(ChatCompletionRequestUserMessageContentPart::Text(
                        async_openai::types::chat::ChatCompletionRequestMessageContentPartText { text: label },
                    ));
                    for part in media {
                        parts.push(self.convert_content_part(part));
                    }
                    let extra = ChatCompletionRequestUserMessageArgs::default()
                        .content(parts)
                        .build()
                        .map_err(|e| Box::new(e) as Box<dyn Error + Send + Sync>)?;
                    messages.push(ChatCompletionRequestMessage::User(extra));
                }

                Ok(messages)
            }
        }
    }

    fn convert_tools(&self, tools: &[Arc<dyn Tool>]) -> Result<Vec<ChatCompletionTools>, Box<dyn Error + Send + Sync>> {
        let mut openai_tools: Vec<ChatCompletionTools> = Vec::new();
        for tool in tools {
            let function = FunctionObjectArgs::default()
                .name(tool.name())
                .description(tool.description())
                .parameters(tool.parameters_schema())
                .build()
                .map_err(|e| Box::new(e) as Box<dyn Error + Send + Sync>)?;

            let t = ChatCompletionTools::Function(ChatCompletionTool { function });
            openai_tools.push(t);
        }
        Ok(openai_tools)
    }
}

impl RequestBuilder for OpenAIProvider {
    fn build(
        &self,
        messages: &[Message],
        tools: &[Arc<dyn Tool>],
    ) -> Result<CreateChatCompletionRequest, Box<dyn Error + Send + Sync>> {
        let mut openai_messages: Vec<ChatCompletionRequestMessage> = Vec::new();
        for msg in messages {
            let mut converted = self.convert_message(msg)?;
            openai_messages.append(&mut converted);
        }

        let openai_tools = self.convert_tools(tools)?;

        let mut request_builder = CreateChatCompletionRequestArgs::default();
        request_builder.model(&self.model).messages(openai_messages);

        if !openai_tools.is_empty() {
            request_builder.tools(openai_tools);
        }

        request_builder
            .build()
            .map_err(|e| Box::new(e) as Box<dyn Error + Send + Sync>)
    }
}

#[derive(Debug, Default, Clone)]
struct ToolCallState {
    id: Option<String>,
    name: Option<String>,
    argument_buffer: String,
    sent_len: usize,
    started: bool,
    finalized: bool,
}

#[derive(Default)]
struct ToolCallAggregator {
    states: Vec<ToolCallState>,
}

impl ToolCallAggregator {
    fn process_chunk(
        &mut self,
        chunk: &ChatCompletionMessageToolCallChunk,
    ) -> Vec<ProviderEvent> {
        let mut events = Vec::new();
        let index = chunk.index as usize;

        if index >= self.states.len() {
            self.states
                .resize_with(index + 1, ToolCallState::default);
        }

        let state = &mut self.states[index];

        if let Some(id) = &chunk.id {
            if !id.is_empty() {
                state.id = Some(id.clone());
            }
        }

        if let Some(function) = &chunk.function {
            if let Some(name) = &function.name {
                if !name.is_empty() {
                    state.name = Some(name.clone());
                }
            }

            if let Some(args) = &function.arguments {
                if !args.is_empty() && !state.finalized {
                    state.argument_buffer.push_str(args);
                    Self::update_finalized_flag(state);
                    if state.started {
                        if let Some(id) = state.id.clone() {
                            Self::emit_pending_arguments(state, &id, &mut events);
                        }
                    }
            }
        }
        }

        if !state.started {
            if let (Some(id), Some(name)) = (state.id.clone(), state.name.clone()) {
                events.push(ProviderEvent::ToolCallStart {
                    id: id.clone(),
                    name,
                });
                state.started = true;

                Self::emit_pending_arguments(state, &id, &mut events);
            }
        }

        events
    }

    fn emit_pending_arguments(
        state: &mut ToolCallState,
        id: &str,
        events: &mut Vec<ProviderEvent>,
    ) {
        let new_len = state.argument_buffer.len();
        if new_len <= state.sent_len {
            return;
        }

        let delta = state.argument_buffer[state.sent_len..new_len].to_string();
        state.sent_len = new_len;
        if delta.is_empty() {
            return;
        }

        events.push(ProviderEvent::ToolCallDelta {
            id: id.to_string(),
            delta,
        });
    }

    fn update_finalized_flag(state: &mut ToolCallState) {
        if state.finalized {
            return;
        }

        if serde_json::from_str::<Value>(&state.argument_buffer).is_ok() {
            state.finalized = true;
        }
    }
}

impl ResponseParser for OpenAIProvider {
    fn parse_stream(
        &self,
        stream: ChatCompletionResponseStream,
    ) -> BoxStream<'static, Result<ProviderEvent, Box<dyn Error + Send + Sync>>> {
        let s = async_stream::try_stream! {
            let mut tool_aggregator = ToolCallAggregator::default();

            let mut stream = stream;
            while let Some(result) = stream.next().await {
                let response = result.map_err(|e| Box::new(e) as Box<dyn Error + Send + Sync>)?;
                if let Some(choice) = response.choices.first() {
                    let delta = &choice.delta;

                    if let Some(content) = &delta.content {
                        if !content.is_empty() {
                            yield ProviderEvent::ContentDelta { delta: content.clone() };
                        }
                    }

                    if let Some(tool_calls) = &delta.tool_calls {
                        for tool_call in tool_calls {
                            for event in tool_aggregator.process_chunk(tool_call) {
                                yield event;
                            }
                        }
                    }

                    if let Some(reason) = &choice.finish_reason {
                        yield ProviderEvent::Finish {
                            reason: Some(format!("{:?}", reason)),
                        };
                    }
                }
            }
        };

        Box::pin(s)
    }
}

#[async_trait]
impl LlmProvider for OpenAIProvider {
    async fn stream_response<'a>(
        &'a self,
        messages: &[Message],
        tools: &[Arc<dyn Tool>],
    ) -> Result<BoxStream<'a, Result<ProviderEvent, Box<dyn Error + Send + Sync>>>, Box<dyn Error + Send + Sync>> {
        let request = self.build(messages, tools)?;

        let stream = self.client.chat().create_stream(request).await
            .map_err(|e| Box::new(e) as Box<dyn Error + Send + Sync>)?;

        let parsed_stream = self.parse_stream(stream);

        Ok(parsed_stream)
    }

    async fn list_models(&self) -> Result<Vec<String>, Box<dyn Error + Send + Sync>> {
        let models = self.client.models();
        let response = models.list().await
            .map_err(|e| Box::new(e) as Box<dyn Error + Send + Sync>)?;
        Ok(response.data.into_iter().map(|m| m.id).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_openai::types::chat::FunctionCallStream;

    #[test]
    fn test_new_provider() {
        let api_key = "test-key";
        let model = "gpt-4";
        let provider = OpenAIProvider::new(api_key, model);

        assert!(provider.build(&[], &[]).is_ok());
    }

    #[test]
    fn test_new_provider_with_endpoint() {
        let api_key = "test-key";
        let base_url = "http://localhost:8080/v1";
        let model = "gpt-3.5-turbo";
        let provider = OpenAIProvider::new_with_endpoint(api_key, base_url, model);

        assert!(provider.build(&[], &[]).is_ok());
    }

    #[test]
    fn convert_message_emits_media_followup() {
        let provider = OpenAIProvider::new("test-key", "gpt-4");
        let msg = Message::Tool {
            tool_call_id: "call_1".to_string(),
            tool_name: Some("read_file".to_string()),
            content: "{\"type\":\"media\"}".to_string(),
            media: vec![
                ContentPart::Text {
                    text: "Image read successfully".to_string(),
                },
                ContentPart::Image {
                    data: "ZGF0YQ==".to_string(),
                    mime_type: "image/png".to_string(),
                },
            ],
        };

        let converted = provider.convert_message(&msg).expect("convert success");
        assert_eq!(converted.len(), 2);
        match &converted[0] {
            ChatCompletionRequestMessage::Tool(tool_msg) => {
                assert_eq!(tool_msg.tool_call_id.as_str(), "call_1");
            }
            _ => panic!("first message should be tool"),
        }
        match &converted[1] {
            ChatCompletionRequestMessage::User(_) => {}
            _ => panic!("second message should be user"),
        }
    }

    fn make_chunk(
        index: u32,
        id: Option<&str>,
        name: Option<&str>,
        arguments: Option<&str>,
    ) -> ChatCompletionMessageToolCallChunk {
        ChatCompletionMessageToolCallChunk {
            index,
            id: id.map(|s| s.to_string()),
            r#type: None,
            function: Some(FunctionCallStream {
                name: name.map(|s| s.to_string()),
                arguments: arguments.map(|s| s.to_string()),
            }),
        }
    }

    #[test]
    fn tool_call_arguments_buffer_until_id_arrives() {
        let mut aggregator = ToolCallAggregator::default();

        // Model streams arguments before providing the tool_call_id
        assert!(aggregator
            .process_chunk(&make_chunk(0, None, Some("demo_tool"), Some("{\"foo\":")))
            .is_empty());
        assert!(aggregator
            .process_chunk(&make_chunk(0, None, Some("demo_tool"), Some("\"bar\"")))
            .is_empty());

        let events = aggregator.process_chunk(&make_chunk(0, Some("call_1"), Some("demo_tool"), None));
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], ProviderEvent::ToolCallStart { .. }));
        assert!(matches!(events[1], ProviderEvent::ToolCallDelta { .. }));
        if let ProviderEvent::ToolCallDelta { id, delta } = &events[1] {
            assert_eq!(id, "call_1");
            assert_eq!(delta, "{\"foo\":\"bar\"");
        } else {
            panic!("Expected ToolCallDelta event");
        }

        let events = aggregator.process_chunk(&make_chunk(0, None, None, Some("\"baz\"}")));
        assert_eq!(events.len(), 1);
        if let ProviderEvent::ToolCallDelta { id, delta } = &events[0] {
            assert_eq!(id, "call_1");
            assert_eq!(delta, "\"baz\"}");
        } else {
            panic!("Expected ToolCallDelta event");
        }
    }

    #[test]
    fn duplicate_arguments_after_completion_are_ignored() {
        let mut aggregator = ToolCallAggregator::default();

        assert!(aggregator
            .process_chunk(&make_chunk(0, None, Some("glob"), Some("{\"pattern\":\"*\"}")))
            .is_empty());

        let events = aggregator.process_chunk(&make_chunk(0, Some("call_42"), Some("glob"), None));
        assert_eq!(events.len(), 2);

        let events = aggregator.process_chunk(&make_chunk(0, Some("call_42"), Some("glob"), Some("{\"pattern\": \"*\"}")));
        assert!(events.is_empty(), "Duplicate arguments should not emit new deltas");
    }
}
