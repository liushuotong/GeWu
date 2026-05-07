//! Planner agent for creating and revising execution plans.

use anyhow::Result;
use futures::StreamExt;
use gewu_core::traits::LlmProvider;
use std::sync::Arc;

pub struct Planner {
    llm: Arc<dyn LlmProvider>,
}

impl Planner {
    pub fn new(llm: Arc<dyn LlmProvider>) -> Self {
        Self { llm }
    }

    pub async fn create_plan(&self, task: &str) -> Result<String> {
        let prompt = Self::build_plan_prompt(task, None, None);

        let messages = vec![gewu_core::types::Message::User {
            content: vec![gewu_core::types::ContentPart::Text { text: prompt }],
        }];

        let mut stream = self.llm.stream_response(&messages, &[]).await
            .map_err(|e| anyhow::anyhow!("LLM stream error: {}", e))?;

        let mut response_text = String::new();
        while let Some(result) = stream.next().await {
            match result {
                Ok(event) => match event {
                    gewu_core::types::ProviderEvent::ContentDelta { delta } => {
                        response_text.push_str(&delta);
                    }
                    _ => {}
                },
                Err(e) => {
                    tracing::warn!("Error in planner stream: {}", e);
                    break;
                }
            }
        }

        Ok(response_text)
    }

    pub async fn revise_plan(
        &self,
        task: &str,
        existing_plan: &str,
        feedback: &str,
    ) -> Result<String> {
        let prompt = Self::build_plan_prompt(task, Some(existing_plan), Some(feedback));

        let messages = vec![gewu_core::types::Message::User {
            content: vec![gewu_core::types::ContentPart::Text { text: prompt }],
        }];

        let mut stream = self.llm.stream_response(&messages, &[]).await
            .map_err(|e| anyhow::anyhow!("LLM stream error: {}", e))?;

        let mut response_text = String::new();
        while let Some(result) = stream.next().await {
            match result {
                Ok(event) => match event {
                    gewu_core::types::ProviderEvent::ContentDelta { delta } => {
                        response_text.push_str(&delta);
                    }
                    _ => {}
                },
                Err(e) => {
                    tracing::warn!("Error in planner stream: {}", e);
                    break;
                }
            }
        }

        Ok(response_text)
    }

    fn build_plan_prompt(
        task: &str,
        existing_plan: Option<&str>,
        feedback: Option<&str>,
    ) -> String {
        let task_section = format!(
            r#"Task to accomplish:
{}"#,
            task
        );

        let revision_section = match (existing_plan, feedback) {
            (Some(plan), Some(review)) => format!(
                "\n\nPrevious Plan:\n---\n\n{}\n\n---\n\nReviewer Feedback:\n{}\n\nPlease revise the plan to address the feedback.",
                plan, review
            ),
            _ => String::new(),
        };

        format!(
            r#"You are a planner agent creating an execution plan.

Your task is to break down the user's request into clear, sequential steps.

{}
{}

Respond with:
1. A numbered list of steps (Step 1, Step 2, etc.)
2. Brief reasoning at the end explaining your approach

Format your response clearly so it can be understood by an executor agent."#,
            task_section, revision_section
        )
    }
}

impl Default for Planner {
    fn default() -> Self {
        Self {
            llm: Arc::new(gewu_providers::OpenAIProvider::new("dummy", "dummy")),
        }
    }
}