use async_openai::types::chat::{
    ChatCompletionRequestSystemMessageArgs, ChatCompletionRequestUserMessageArgs,
    ChatCompletionRequestAssistantMessageArgs, ChatCompletionRequestToolMessageArgs,
    CreateChatCompletionRequest, CreateChatCompletionRequestArgs, ChatCompletionTools,
    FunctionObjectArgs, ChatCompletionMessageToolCalls, FunctionCall as OpenAIFunctionCall,
    ChatCompletionRequestMessage, ChatCompletionTool, ChatCompletionMessageToolCall,
    ChatCompletionResponseStream,
};
use futures::{StreamExt};
use futures::stream::BoxStream;
use gewu_core::traits::{LlmProvider, Tool};
use gewu_core::types::{Message, ProviderEvent};
use async_trait::async_trait;
use async_openai::Client;
use std::error::Error;
use std::sync::Arc;

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

    fn convert_message(&self, msg: &Message) -> Result<ChatCompletionRequestMessage, Box<dyn Error + Send + Sync>> {
        match msg {
            Message::System { content } => {
                let m = ChatCompletionRequestSystemMessageArgs::default()
                    .content(content.clone())
                    .build()
                    .map_err(|e| Box::new(e) as Box<dyn Error + Send + Sync>)?;
                Ok(ChatCompletionRequestMessage::System(m))
            }
            Message::User { content } => {
                let m = ChatCompletionRequestUserMessageArgs::default()
                    .content(content.clone())
                    .build()
                    .map_err(|e| Box::new(e) as Box<dyn Error + Send + Sync>)?;
                Ok(ChatCompletionRequestMessage::User(m))
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
                Ok(ChatCompletionRequestMessage::Assistant(m))
            }
            Message::Tool { tool_call_id, content } => {
                let m = ChatCompletionRequestToolMessageArgs::default()
                    .tool_call_id(tool_call_id.clone())
                    .content(content.clone())
                    .build()
                    .map_err(|e| Box::new(e) as Box<dyn Error + Send + Sync>)?;
                Ok(ChatCompletionRequestMessage::Tool(m))
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
        let openai_messages: Vec<ChatCompletionRequestMessage> = messages
            .iter()
            .map(|msg| self.convert_message(msg))
            .collect::<Result<Vec<_>, _>>()?;

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

impl ResponseParser for OpenAIProvider {
    fn parse_stream(
        &self,
        stream: ChatCompletionResponseStream,
    ) -> BoxStream<'static, Result<ProviderEvent, Box<dyn Error + Send + Sync>>> {
        let s = async_stream::try_stream! {
            let mut tool_map: Vec<String> = Vec::new();

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
                            let index = tool_call.index as usize;

                            if let Some(id) = &tool_call.id {
                                if index >= tool_map.len() {
                                    tool_map.resize(index + 1, String::new());
                                }
                                tool_map[index] = id.clone();

                                let name = tool_call.function.as_ref().map(|f| f.name.clone().unwrap_or_default()).unwrap_or_default();

                                yield ProviderEvent::ToolCallStart {
                                    id: id.clone(),
                                    name,
                                };
                            }

                            if let Some(function) = &tool_call.function {
                                if let Some(args) = &function.arguments {
                                    if !args.is_empty() {
                                        let id = if index < tool_map.len() {
                                            tool_map[index].clone()
                                        } else {
                                            String::new()
                                        };

                                        yield ProviderEvent::ToolCallDelta {
                                            id,
                                            delta: args.clone(),
                                        };
                                    }
                                }
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
}
