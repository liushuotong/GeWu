use gewu_core::traits::{LlmProvider, Tool};
use gewu_core::types::{Message, ToolCall, FunctionCall};
use anyhow::Result;
use async_trait::async_trait;
use async_openai::{
    Client,
    types::{
        CreateChatCompletionRequestArgs, ChatCompletionRequestMessage,
        ChatCompletionRequestSystemMessageArgs, ChatCompletionRequestUserMessageArgs,
        ChatCompletionRequestAssistantMessageArgs, ChatCompletionRequestToolMessageArgs,
        ChatCompletionTool, ChatCompletionToolArgs, ChatCompletionToolType,
        FunctionObjectArgs, ChatCompletionMessageToolCall, FunctionCall as OpenAIFunctionCall,
    },
};
use std::error::Error;

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
    async fn generate_response(
        &self,
        messages: &[Message],
        tools: &[Box<dyn Tool>],
    ) -> Result<Message, Box<dyn Error + Send + Sync>> {
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
                        let openai_calls: Vec<ChatCompletionMessageToolCall> = calls.iter().map(|tc| {
                            ChatCompletionMessageToolCall {
                                id: tc.id.clone(),
                                r#type: ChatCompletionToolType::Function,
                                function: OpenAIFunctionCall {
                                    name: tc.function.name.clone(),
                                    arguments: tc.function.arguments.clone(),
                                },
                            }
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

        let mut openai_tools: Vec<ChatCompletionTool> = Vec::new();
        for tool in tools {
            let function = FunctionObjectArgs::default()
                .name(tool.name())
                .description(tool.description())
                .parameters(tool.parameters_schema())
                .build()
                .map_err(|e| Box::new(e) as Box<dyn Error + Send + Sync>)?;
                
            let t = ChatCompletionToolArgs::default()
                .r#type(ChatCompletionToolType::Function)
                .function(function)
                .build()
                .map_err(|e| Box::new(e) as Box<dyn Error + Send + Sync>)?;
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

        let response = self.client.chat().create(request).await
            .map_err(|e| Box::new(e) as Box<dyn Error + Send + Sync>)?;
            
        let choice = response.choices.first()
            .ok_or_else(|| Box::<dyn Error + Send + Sync>::from("No response choices"))?;
            
        let msg = &choice.message;
        
        let tool_calls = if let Some(calls) = &msg.tool_calls {
            Some(calls.iter().map(|c| ToolCall {
                id: c.id.clone(),
                r#type: "function".to_string(),
                function: FunctionCall {
                    name: c.function.name.clone(),
                    arguments: c.function.arguments.clone(),
                },
            }).collect())
        } else {
            None
        };

        Ok(Message::Assistant {
            content: msg.content.clone(),
            tool_calls,
        })
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
