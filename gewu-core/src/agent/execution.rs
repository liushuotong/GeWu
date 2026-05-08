use crate::compaction;
use crate::types::{
    AgentEvent, ContentPart, FunctionCall, Message, ProviderEvent, ToolCall, ToolContext,
};
use anyhow::Result;
use futures::{Stream, StreamExt};
use std::collections::VecDeque;
use tracing::{info, warn, trace};
use uuid::Uuid;

use super::{execute_tool_with_panic_recovery, Agent};

impl Agent {
    pub fn ask<'a>(
        &'a mut self,
        user_message: &'a str,
    ) -> impl Stream<Item = Result<AgentEvent>> + 'a {
        async_stream::try_stream! {
            if self.memory.messages().is_empty() && !self.system_prompt.is_empty() {
                 self.memory.add_message(Message::System { content: self.system_prompt.clone() }).await;
            }

            self.memory.add_message(Message::User { content: vec![ContentPart::Text { text: user_message.to_string() }] }).await;

            let mut turn = 0;
            let mut recent_tool_calls: VecDeque<(String, String)> = VecDeque::new();
            let mut message_id = Uuid::new_v4().to_string();

            loop {
                if self.compact_if_needed().await? {
                    info!("History compacted.");
                }

                if let Some(max) = self.max_turns {
                    if turn >= max {
                        yield AgentEvent::Error { error: "Max turns reached without final answer.".to_string() };
                        break;
                    }
                }
                turn += 1;

                compaction::prune(self.memory.messages_mut());

                let mut current_content = String::new();
                let mut current_tool_calls: Vec<ToolCall> = Vec::new();

                let provider = self.llm.clone();
                let messages = self.memory.messages().to_vec();
                trace!("LLM request messages: {:?}", messages);

                let tools_clone = self.tools.clone();
                let mut stream = provider.stream_response(&messages, tools_clone.as_slice()).await
                    .map_err(|e| anyhow::anyhow!(e))?;

                while let Some(event_result) = stream.next().await {
                    let event = event_result.map_err(|e| anyhow::anyhow!(e))?;

                    match event {
                        ProviderEvent::ContentDelta { delta } => {
                            current_content.push_str(&delta);
                            yield AgentEvent::ContentDelta { delta };
                        }
                        ProviderEvent::ReasoningDelta { delta } => {
                            yield AgentEvent::ReasoningDelta { delta };
                        }
                        ProviderEvent::ToolCallStart { id, name } => {
                            current_tool_calls.push(ToolCall {
                                id,
                                r#type: "function".to_string(),
                                function: FunctionCall {
                                    name,
                                    arguments: String::new(),
                                },
                            });
                        }
                        ProviderEvent::ToolCallDelta { id, delta } => {
                            if let Some(tc) = current_tool_calls.iter_mut().find(|tc| tc.id == id) {
                                tc.function.arguments.push_str(&delta);
                            }
                        }
                        ProviderEvent::Finish { .. } => {
                        }
                        ProviderEvent::Error { error } => {
                            yield AgentEvent::Error { error };
                            return;
                        }
                    }
                }

                let assistant_msg = Message::Assistant {
                    content: if current_content.is_empty() { None } else { Some(current_content.clone()) },
                    tool_calls: if current_tool_calls.is_empty() { None } else { Some(current_tool_calls.clone()) },
                };
                self.memory.add_message(assistant_msg).await;

                if current_tool_calls.is_empty() {
                    yield AgentEvent::Finish { response: current_content };
                    break;
                }

                info!("Agent requested {} tool calls", current_tool_calls.len());

                for call in current_tool_calls {
                    let tool_name = call.function.name.clone();
                    let args = call.function.arguments.clone();
                    let call_id = call.id;

                    let current_call_sig = (tool_name.clone(), args.clone());
                    recent_tool_calls.push_back(current_call_sig.clone());
                    if recent_tool_calls.len() > 3 {
                        recent_tool_calls.pop_front();
                    }

                    if recent_tool_calls.len() == 3 && recent_tool_calls.iter().all(|x| x == &current_call_sig) {
                         let error_msg = format!("Doom loop detected: Tool '{}' called 3 times repeatedly with identical arguments. Stopping.", tool_name);
                         yield AgentEvent::Error { error: error_msg };
                         return;
                    }

                    if tool_name.is_empty() {
                         let error_msg = "Tool name is empty, skipping execution".to_string();
                         yield AgentEvent::Error { error: error_msg };
                         continue;
                    }

                    yield AgentEvent::ToolStart { tool: tool_name.clone(), input: args.clone() };

                    let ctx = ToolContext::new(&self.session_id, &message_id);
                    message_id = Uuid::new_v4().to_string();

                    let tool = self.tools.iter().find(|t| t.name() == tool_name);

                    let result_content = if let Some(t) = tool {
                        info!("Executing tool: {} with args: {}", tool_name, args);
                        let args_val: serde_json::Value = match serde_json::from_str(&args) {
                            Ok(v) => v,
                            Err(e) => {
                                yield AgentEvent::Error { error: format!("Failed to parse arguments: {}", e) };
                                continue;
                            }
                        };
                        match execute_tool_with_panic_recovery(t.clone(), args_val, ctx.clone()).await {
                            Ok(res) => {
                                let truncation_result = self.truncation.truncate(&res.output);
                                let output = if truncation_result.truncated {
                                    format!(
                                        "{}\n\n[Output truncated - {} bytes saved to {}]",
                                        truncation_result.content,
                                        truncation_result.content.len(),
                                        truncation_result.output_path.map(|p| p.display().to_string()).unwrap_or_default()
                                    )
                                } else {
                                    res.output
                                };
                                output
                            }
                            Err(e) => {
                                warn!("Tool '{}' failed: {}", tool_name, e);
                                format!("Error: {}", e)
                            }
                        }
                    } else {
                        format!("Tool not found: {}", tool_name)
                    };

                    yield AgentEvent::ToolFinish { tool: tool_name.clone(), output: result_content.clone() };

                    let media_parts = super::media::extract_media_parts(&result_content);
                    self.memory.add_message(Message::Tool {
                        tool_call_id: call_id,
                        tool_name: Some(tool_name.clone()),
                        content: result_content,
                        media: media_parts,
                    }).await;
                }
            }
        }
    }
}