use gewu_core::traits::{LlmProvider, Tool};
use gewu_core::types::{Message, ProviderEvent};
use anyhow::Result;
use async_trait::async_trait;
use async_openai::{
    Client,
    types::chat::{
        CreateChatCompletionRequestArgs, ChatCompletionRequestMessage,
        ChatCompletionRequestSystemMessageArgs, ChatCompletionRequestUserMessageArgs,
        ChatCompletionRequestAssistantMessageArgs, ChatCompletionRequestToolMessageArgs,
        ChatCompletionTools,
        FunctionObjectArgs, ChatCompletionMessageToolCalls, FunctionCall as OpenAIFunctionCall,
    },
};
use std::error::Error;
use std::sync::Arc;
use futures::StreamExt;
use futures::stream::BoxStream;

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
}

#[async_trait]
impl LlmProvider for OpenAIProvider {
    async fn stream_response<'a>(
        &'a self,
        messages: &[Message],
        tools: &[Arc<dyn Tool>],
    ) -> Result<BoxStream<'a, Result<ProviderEvent, Box<dyn Error + Send + Sync>>>, Box<dyn Error + Send + Sync>> {
        let mut openai_messages: Vec<ChatCompletionRequestMessage> = Vec::new();
        
        for msg in messages {
            match msg {
                Message::System { content } => {
                    let m = ChatCompletionRequestSystemMessageArgs::default()
                        .content(content.clone())
                        .build()
                        .map_err(|e| Box::new(e) as Box<dyn Error + Send + Sync>)?;
                    openai_messages.push(ChatCompletionRequestMessage::System(m));
                }
                Message::User { content } => {
                    let m = ChatCompletionRequestUserMessageArgs::default()
                        .content(content.clone())
                        .build()
                        .map_err(|e| Box::new(e) as Box<dyn Error + Send + Sync>)?;
                    openai_messages.push(ChatCompletionRequestMessage::User(m));
                }
                Message::Assistant { content, tool_calls } => {
                    let mut builder = ChatCompletionRequestAssistantMessageArgs::default();
                    if let Some(c) = content {
                        builder.content(c.clone());
                    }
                    if let Some(calls) = tool_calls {
                        let openai_calls: Vec<ChatCompletionMessageToolCalls> = calls.iter().map(|tc| {
                            ChatCompletionMessageToolCalls::Function(async_openai::types::chat::ChatCompletionMessageToolCall {
                                id: tc.id.clone(),
                                function: OpenAIFunctionCall {
                                    name: tc.function.name.clone(),
                                    arguments: tc.function.arguments.clone(),
                                },
                            })
                        }).collect();
                        builder.tool_calls(openai_calls);
                    }
                    let m = builder.build().map_err(|e| Box::new(e) as Box<dyn Error + Send + Sync>)?;
                    openai_messages.push(ChatCompletionRequestMessage::Assistant(m));
                }
                Message::Tool { tool_call_id, content } => {
                    let m = ChatCompletionRequestToolMessageArgs::default()
                        .tool_call_id(tool_call_id.clone())
                        .content(content.clone())
                        .build()
                        .map_err(|e| Box::new(e) as Box<dyn Error + Send + Sync>)?;
                    openai_messages.push(ChatCompletionRequestMessage::Tool(m));
                }
            }
        }

        let mut openai_tools: Vec<ChatCompletionTools> = Vec::new();
        for tool in tools {
            let function = FunctionObjectArgs::default()
                .name(tool.name())
                .description(tool.description())
                .parameters(tool.parameters_schema())
                .build()
                .map_err(|e| Box::new(e) as Box<dyn Error + Send + Sync>)?;
                
            let t = ChatCompletionTools::Function(
                async_openai::types::chat::ChatCompletionTool {
                    function,
                }
            );
            openai_tools.push(t);
        }
        
        let mut request_builder = CreateChatCompletionRequestArgs::default();
        request_builder
            .model(&self.model)
            .messages(openai_messages);

        if !openai_tools.is_empty() {
            request_builder.tools(openai_tools);
        }

        let request = request_builder.build()
            .map_err(|e| Box::new(e) as Box<dyn Error + Send + Sync>)?;

        let mut stream = self.client.chat().create_stream(request).await
            .map_err(|e| Box::new(e) as Box<dyn Error + Send + Sync>)?;

        let s = async_stream::try_stream! {
            let mut tool_map: Vec<String> = Vec::new();

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
                                            delta: args.clone()
                                        };
                                    }
                                }
                            }
                        }
                    }
                    
                    if let Some(reason) = &choice.finish_reason {
                        yield ProviderEvent::Finish { reason: Some(format!("{:?}", reason)) };
                    }
                }
            }
        };

        Ok(Box::pin(s))
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

        assert_eq!(provider.model, model);
    }

    #[test]
    fn test_new_provider_with_endpoint() {
        let api_key = "test-key";
        let base_url = "http://localhost:8080/v1";
        let model = "gpt-3.5-turbo";
        let provider = OpenAIProvider::new_with_endpoint(api_key, base_url, model);

        assert_eq!(provider.model, model);
    }
}
