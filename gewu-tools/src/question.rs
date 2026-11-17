//! Question tool.
//!
//! Lets the agent ask the user structured, multiple-choice questions
//! mid-run. The tool registers the request with a [`QuestionBroker`],
//! announces it on the ask stream, and blocks until the user answers or
//! dismisses the form.

use std::error::Error;
use std::sync::Arc;

use async_trait::async_trait;
use gewu_core::question::{
    Question, QuestionBroker, QuestionOutcome, QuestionPending, QuestionRequest,
};
use gewu_core::traits::Tool;
use gewu_core::types::{ToolContext, ToolResult};
use schemars::JsonSchema;
use schemars::schema::RootSchema;
use serde::Deserialize;
use uuid::Uuid;

/// The tool description shown to the model.
const DESCRIPTION: &str = "Use this tool when you need to ask the user questions during execution. This allows you to:
1. Gather user preferences or requirements
2. Clarify ambiguous instructions
3. Get decisions on implementation choices as you work
4. Offer choices to the user about what direction to take.

Usage notes:
- When `custom` is enabled (default), a \"Type your own answer\" option is added automatically; don't include \"Other\" or catch-all options
- Answers are returned as arrays of labels; set `multiple: true` to allow selecting more than one
- If you recommend a specific option, make that the first option in the list and add \"(Recommended)\" at the end of the label";

/// Input arguments accepted by the tool.
#[derive(Debug, Deserialize, JsonSchema)]
struct QuestionArgs {
    /// Questions to ask, in display order.
    questions: Vec<Question>,
}

/// The `question` tool, backed by the process-wide question broker.
pub struct QuestionTool {
    broker: Arc<QuestionBroker>,
    schema: RootSchema,
}

impl QuestionTool {
    pub fn new(broker: Arc<QuestionBroker>) -> Self {
        Self {
            broker,
            schema: schemars::schema_for!(QuestionArgs),
        }
    }
}

/// Model-facing summary of the user's answers.
fn answered_output(questions: &[Question], answers: &[Vec<String>]) -> String {
    let formatted = questions
        .iter()
        .enumerate()
        .map(|(index, question)| {
            let answer = answers.get(index).cloned().unwrap_or_default();
            let answer = if answer.is_empty() {
                "Unanswered".to_string()
            } else {
                answer.join(", ")
            };
            format!("\"{}\"=\"{}\"", question.question, answer)
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "User has answered your questions: {formatted}. You can now continue with the user's answers in mind."
    )
}

fn request_id() -> String {
    format!("que_{}", Uuid::new_v4().simple())
}

#[async_trait]
impl Tool for QuestionTool {
    fn name(&self) -> &str {
        "question"
    }

    fn description(&self) -> &str {
        DESCRIPTION
    }

    fn schema(&self) -> RootSchema {
        self.schema.clone()
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, Box<dyn Error + Send + Sync>> {
        if ctx.progress.is_none() {
            return Err("question is unavailable: this front-end cannot display questions".into());
        }
        let args: QuestionArgs = serde_json::from_value(args)?;
        let pending: QuestionPending = self
            .broker
            .register(QuestionRequest {
                request_id: request_id(),
                session_id: ctx.session_id.clone(),
                questions: args.questions.clone(),
            })
            .map_err(|error| -> Box<dyn Error + Send + Sync> { error.to_string().into() })?;
        let request_id = pending.request_id().to_string();

        // Registered first so an immediate reply cannot race the event.
        ctx.report_question_asked(&request_id, args.questions.clone());
        let outcome = pending
            .wait()
            .await
            .map_err(|error| -> Box<dyn Error + Send + Sync> { error.to_string().into() })?;
        match outcome {
            QuestionOutcome::Answered(answers) => {
                ctx.report_question_resolved(&request_id, Some(answers.clone()));
                Ok(ToolResult::new(
                    "question",
                    answered_output(&args.questions, &answers),
                ))
            }
            QuestionOutcome::Dismissed => {
                ctx.report_question_resolved(&request_id, None);
                Ok(ToolResult::new(
                    "question",
                    "The user dismissed this question without answering.",
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gewu_core::question::QuestionOption;
    use gewu_core::types::ProgressSink;
    use std::time::Duration;

    fn pick_question() -> Question {
        Question {
            question: "Which database?".into(),
            header: "Database".into(),
            options: vec![
                QuestionOption {
                    label: "Postgres".into(),
                    description: "Relational".into(),
                },
                QuestionOption {
                    label: "SQLite".into(),
                    description: "Embedded".into(),
                },
            ],
            multiple: false,
            custom: true,
        }
    }

    #[test]
    fn answer_output_lists_every_question() {
        let questions = vec![pick_question()];
        assert_eq!(
            answered_output(&questions, &[vec!["Postgres".into()]]),
            "User has answered your questions: \"Which database?\"=\"Postgres\". \
             You can now continue with the user's answers in mind."
        );
        assert_eq!(
            answered_output(&questions, &[vec![]]),
            "User has answered your questions: \"Which database?\"=\"Unanswered\". \
             You can now continue with the user's answers in mind."
        );
    }

    #[tokio::test]
    async fn tool_announces_and_returns_the_answer() {
        let broker = Arc::new(QuestionBroker::new());
        let tool = QuestionTool::new(broker.clone());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut ctx = ToolContext::new("session-1", "message-1");
        ctx.progress = Some(ProgressSink::new("question", tx));
        let args = serde_json::json!({ "questions": [pick_question()] });

        let task = tokio::spawn(async move { tool.execute(args, &ctx).await });
        let asked = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        let request_id = match asked {
            gewu_core::types::AgentEvent::QuestionAsked {
                request_id,
                questions,
            } => {
                assert_eq!(questions, vec![pick_question()]);
                request_id
            }
            other => panic!("unexpected event: {other:?}"),
        };
        broker
            .reply("session-1", &request_id, vec![vec!["SQLite".into()]])
            .unwrap();
        let result = task.await.unwrap().unwrap();
        assert!(result.output.contains("\"Which database?\"=\"SQLite\""));
        let resolved = rx.recv().await.unwrap();
        assert!(matches!(
            resolved,
            gewu_core::types::AgentEvent::QuestionResolved {
                answers: Some(answers),
                ..
            } if answers == vec![vec!["SQLite".to_string()]]
        ));
    }

    #[tokio::test]
    async fn dismissal_returns_a_normal_tool_result() {
        let broker = Arc::new(QuestionBroker::new());
        let tool = QuestionTool::new(broker.clone());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut ctx = ToolContext::new("session-1", "message-1");
        ctx.progress = Some(ProgressSink::new("question", tx));
        let args = serde_json::json!({ "questions": [pick_question()] });

        let task = tokio::spawn(async move { tool.execute(args, &ctx).await });
        let request_id = match rx.recv().await.unwrap() {
            gewu_core::types::AgentEvent::QuestionAsked { request_id, .. } => request_id,
            other => panic!("unexpected event: {other:?}"),
        };
        broker.reject("session-1", &request_id).unwrap();
        let result = task.await.unwrap().unwrap();
        assert!(result.output.contains("dismissed"));
        assert!(matches!(
            rx.recv().await.unwrap(),
            gewu_core::types::AgentEvent::QuestionResolved { answers: None, .. }
        ));
    }
}
