//! Structured questions the agent asks the user mid-run.
//!
//! The `question` tool registers a request with a [`QuestionBroker`], emits
//! [`AgentEvent::QuestionAsked`](crate::types::AgentEvent::QuestionAsked) so a
//! front-end can render a form, and waits until the user answers or dismisses
//! the request. The broker is transport-agnostic: the server exposes
//! `reply`/`reject` over its protocol, while the core keeps only the pending
//! request registry and answer validation.

use std::collections::HashMap;
use std::sync::{Mutex, Weak};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

/// Hard caps that keep a model-authored form (and its answers) bounded.
const MAX_QUESTIONS: usize = 20;
const MAX_OPTIONS: usize = 20;
const MAX_QUESTION_CHARS: usize = 2000;
const MAX_HEADER_CHARS: usize = 100;
const MAX_LABEL_CHARS: usize = 200;
const MAX_DESCRIPTION_CHARS: usize = 1000;
const MAX_ANSWER_CHARS: usize = 1000;

/// One selectable choice in a question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct QuestionOption {
    /// Display text (1-5 words, concise).
    pub label: String,
    /// Explanation of the choice.
    #[serde(default)]
    pub description: String,
}

/// One question presented to the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Question {
    /// The complete question.
    pub question: String,
    /// Very short label for tabs (the UI truncates to 30 characters).
    pub header: String,
    /// Available choices.
    #[serde(default)]
    pub options: Vec<QuestionOption>,
    /// Allow selecting more than one choice.
    #[serde(default, skip_serializing_if = "is_false")]
    pub multiple: bool,
    /// Allow typing an answer that is not in `options` (default: true).
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub custom: bool,
}

fn default_true() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// A registered question request, addressed by [`QuestionRequest::request_id`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionRequest {
    pub request_id: String,
    pub session_id: String,
    pub questions: Vec<Question>,
}

/// How a pending question settled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuestionOutcome {
    /// Selected labels per question, in question order.
    Answered(Vec<Vec<String>>),
    /// The user dismissed the question without answering.
    Dismissed,
}

/// Why a question could not be registered, answered, or rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuestionError {
    /// No pending request has that id (already answered, interrupted, ...).
    NotFound(String),
    /// The request belongs to a different session than the caller's.
    WrongSession,
    /// The request or its answers are malformed.
    Invalid(String),
}

impl std::fmt::Display for QuestionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QuestionError::NotFound(id) => write!(f, "unknown question request '{id}'"),
            QuestionError::WrongSession => {
                write!(f, "question belongs to a different session")
            }
            QuestionError::Invalid(message) => write!(f, "invalid question: {message}"),
        }
    }
}

impl std::error::Error for QuestionError {}

struct PendingQuestion {
    session_id: String,
    questions: Vec<Question>,
    reply: oneshot::Sender<QuestionOutcome>,
}

/// In-memory registry of questions waiting for an answer.
///
/// Pending entries are live-only: they disappear when answered, dismissed,
/// or when the asking tool's future is dropped (interrupt, disconnect of the
/// whole turn, ...).
#[derive(Default)]
pub struct QuestionBroker {
    pending: Mutex<HashMap<String, PendingQuestion>>,
}

impl QuestionBroker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a question and returns the handle the tool awaits.
    pub fn register(
        self: &std::sync::Arc<Self>,
        request: QuestionRequest,
    ) -> Result<QuestionPending, QuestionError> {
        validate_questions(&request.questions).map_err(QuestionError::Invalid)?;
        let (reply, receiver) = oneshot::channel();
        let request_id = request.request_id.clone();
        let mut pending = self.pending.lock().expect("question mutex poisoned");
        if pending.contains_key(&request_id) {
            return Err(QuestionError::Invalid(format!(
                "duplicate request id '{request_id}'"
            )));
        }
        pending.insert(
            request_id.clone(),
            PendingQuestion {
                session_id: request.session_id,
                questions: request.questions,
                reply,
            },
        );
        drop(pending);
        Ok(QuestionPending {
            request_id,
            broker: std::sync::Arc::downgrade(self),
            receiver: Some(receiver),
        })
    }

    /// Session that owns a pending request, if it is still registered.
    pub fn session_of(&self, request_id: &str) -> Option<String> {
        self.pending
            .lock()
            .expect("question mutex poisoned")
            .get(request_id)
            .map(|pending| pending.session_id.clone())
    }

    /// Validates and delivers a reply. Returns the accepted answers.
    pub fn reply(
        &self,
        session_id: &str,
        request_id: &str,
        answers: Vec<Vec<String>>,
    ) -> Result<Vec<Vec<String>>, QuestionError> {
        let mut pending = self.pending.lock().expect("question mutex poisoned");
        let entry = pending
            .get(request_id)
            .ok_or_else(|| QuestionError::NotFound(request_id.to_string()))?;
        if entry.session_id != session_id {
            return Err(QuestionError::WrongSession);
        }
        let answers =
            validate_answers(&entry.questions, answers).map_err(QuestionError::Invalid)?;
        let entry = pending
            .remove(request_id)
            .ok_or_else(|| QuestionError::NotFound(request_id.to_string()))?;
        drop(pending);
        let _ = entry.reply.send(QuestionOutcome::Answered(answers.clone()));
        Ok(answers)
    }

    /// Rejects a pending request; the asker sees [`QuestionOutcome::Dismissed`].
    pub fn reject(&self, session_id: &str, request_id: &str) -> Result<(), QuestionError> {
        let mut pending = self.pending.lock().expect("question mutex poisoned");
        let entry = pending
            .get(request_id)
            .ok_or_else(|| QuestionError::NotFound(request_id.to_string()))?;
        if entry.session_id != session_id {
            return Err(QuestionError::WrongSession);
        }
        let entry = pending
            .remove(request_id)
            .ok_or_else(|| QuestionError::NotFound(request_id.to_string()))?;
        drop(pending);
        let _ = entry.reply.send(QuestionOutcome::Dismissed);
        Ok(())
    }

    /// Number of questions currently waiting for an answer.
    pub fn pending_count(&self) -> usize {
        self.pending.lock().expect("question mutex poisoned").len()
    }

    /// Drops every pending question owned by `session_id`, returning how many
    /// were removed. Used when a turn is interrupted: aborting the tool task
    /// is asynchronous, so the turn finalizer unregisters its questions
    /// deterministically instead of waiting for the task to drop.
    pub fn cancel_session(&self, session_id: &str) -> usize {
        let mut pending = self.pending.lock().expect("question mutex poisoned");
        let ids: Vec<String> = pending
            .iter()
            .filter(|(_, question)| question.session_id == session_id)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &ids {
            pending.remove(id);
        }
        ids.len()
    }
}

/// Handle to one registered question. Dropping it unregisters the request so
/// an interrupted tool does not leave a stale entry behind.
pub struct QuestionPending {
    request_id: String,
    broker: Weak<QuestionBroker>,
    receiver: Option<oneshot::Receiver<QuestionOutcome>>,
}

impl QuestionPending {
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    /// Waits indefinitely for the user's answer or dismissal.
    pub async fn wait(mut self) -> Result<QuestionOutcome, QuestionError> {
        let receiver = self
            .receiver
            .take()
            .expect("a question is waited on only once");
        receiver
            .await
            .map_err(|_| QuestionError::NotFound(self.request_id.clone()))
    }
}

impl Drop for QuestionPending {
    fn drop(&mut self) {
        if let Some(broker) = self.broker.upgrade() {
            broker
                .pending
                .lock()
                .expect("question mutex poisoned")
                .remove(&self.request_id);
        }
    }
}

/// Validates a tool-authored question list.
pub fn validate_questions(questions: &[Question]) -> Result<(), String> {
    if questions.is_empty() {
        return Err("at least one question is required".into());
    }
    if questions.len() > MAX_QUESTIONS {
        return Err(format!("at most {MAX_QUESTIONS} questions are allowed"));
    }
    for (index, question) in questions.iter().enumerate() {
        let number = index + 1;
        if question.question.trim().is_empty() {
            return Err(format!("question {number} has no text"));
        }
        if question.question.chars().count() > MAX_QUESTION_CHARS {
            return Err(format!(
                "question {number} is longer than {MAX_QUESTION_CHARS} characters"
            ));
        }
        if question.header.trim().is_empty() {
            return Err(format!("question {number} has no header"));
        }
        if question.header.chars().count() > MAX_HEADER_CHARS {
            return Err(format!(
                "question {number} header is longer than {MAX_HEADER_CHARS} characters"
            ));
        }
        if question.options.len() > MAX_OPTIONS {
            return Err(format!(
                "question {number} has more than {MAX_OPTIONS} options"
            ));
        }
        let mut labels = std::collections::HashSet::new();
        for option in &question.options {
            let label = option.label.trim();
            if label.is_empty() {
                return Err(format!("question {number} has an empty option label"));
            }
            if label.chars().count() > MAX_LABEL_CHARS {
                return Err(format!(
                    "question {number} has an option label longer than {MAX_LABEL_CHARS} characters"
                ));
            }
            if option.description.chars().count() > MAX_DESCRIPTION_CHARS {
                return Err(format!(
                    "question {number} has an option description longer than {MAX_DESCRIPTION_CHARS} characters"
                ));
            }
            if !labels.insert(label) {
                return Err(format!(
                    "question {number} has a duplicate option label '{label}'"
                ));
            }
        }
        if question.options.is_empty() && !question.custom {
            return Err(format!(
                "question {number} has no options and does not allow a custom answer"
            ));
        }
    }
    Ok(())
}

/// Validates a reply against the question it answers.
pub fn validate_answers(
    questions: &[Question],
    answers: Vec<Vec<String>>,
) -> Result<Vec<Vec<String>>, String> {
    if answers.len() != questions.len() {
        return Err(format!(
            "expected {} answers, got {}",
            questions.len(),
            answers.len()
        ));
    }
    let mut validated = Vec::with_capacity(answers.len());
    for (index, (question, answer)) in questions.iter().zip(answers).enumerate() {
        let number = index + 1;
        let limit = if question.multiple {
            question.options.len() + usize::from(question.custom)
        } else {
            1
        };
        if answer.len() > limit {
            return Err(format!(
                "question {number} accepts at most {limit} answer(s)"
            ));
        }
        let mut seen = std::collections::HashSet::new();
        let mut clean = Vec::with_capacity(answer.len());
        for label in answer {
            let label = label.trim().to_string();
            if label.is_empty() {
                continue;
            }
            if label.chars().count() > MAX_ANSWER_CHARS {
                return Err(format!(
                    "question {number} has an answer longer than {MAX_ANSWER_CHARS} characters"
                ));
            }
            if !question.custom
                && !question
                    .options
                    .iter()
                    .any(|option| option.label.trim() == label)
            {
                return Err(format!(
                    "question {number} does not offer the answer '{label}'"
                ));
            }
            if !seen.insert(label.clone()) {
                return Err(format!("question {number} repeats the answer '{label}'"));
            }
            clean.push(label);
        }
        validated.push(clean);
    }
    Ok(validated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn question(options: &[&str], multiple: bool, custom: bool) -> Question {
        Question {
            question: "Which one?".into(),
            header: "Pick".into(),
            options: options
                .iter()
                .map(|label| QuestionOption {
                    label: (*label).into(),
                    description: String::new(),
                })
                .collect(),
            multiple,
            custom,
        }
    }

    #[test]
    fn answer_validation_enforces_single_select_and_known_labels() {
        let questions = vec![question(&["A", "B"], false, false)];
        assert_eq!(
            validate_answers(&questions, vec![vec!["A".into()]]).unwrap(),
            vec![vec!["A".to_string()]]
        );
        assert!(validate_answers(&questions, vec![vec!["A".into(), "B".into()]]).is_err());
        assert!(validate_answers(&questions, vec![vec!["C".into()]]).is_err());
        assert!(validate_answers(&questions, vec![]).is_err());
    }

    #[test]
    fn custom_answers_allow_unknown_labels() {
        let questions = vec![question(&["A", "B"], false, true)];
        assert_eq!(
            validate_answers(&questions, vec![vec!["something else".into()]]).unwrap(),
            vec![vec!["something else".to_string()]]
        );
    }

    #[test]
    fn multiple_answers_are_deduplicated_and_bounded() {
        let questions = vec![question(&["A", "B"], true, false)];
        assert_eq!(
            validate_answers(&questions, vec![vec!["A".into(), "B".into()]]).unwrap(),
            vec![vec!["A".to_string(), "B".to_string()]]
        );
        assert!(validate_answers(&questions, vec![vec!["A".into(), "A".into()]]).is_err());
        assert!(
            validate_answers(&questions, vec![vec!["A".into(), "B".into(), "A".into()]]).is_err()
        );
    }

    #[tokio::test]
    async fn broker_replies_by_session() {
        let broker = Arc::new(QuestionBroker::new());
        let pending = broker
            .register(QuestionRequest {
                request_id: "que_1".into(),
                session_id: "s1".into(),
                questions: vec![question(&["A", "B"], false, true)],
            })
            .unwrap();
        assert_eq!(broker.pending_count(), 1);
        assert_eq!(broker.session_of("que_1"), Some("s1".into()));
        assert_eq!(
            broker.reply("s2", "que_1", vec![vec!["A".into()]]),
            Err(QuestionError::WrongSession)
        );
        let answers = broker.reply("s1", "que_1", vec![vec!["A".into()]]).unwrap();
        assert_eq!(answers, vec![vec!["A".to_string()]]);
        assert_eq!(
            pending.wait().await.unwrap(),
            QuestionOutcome::Answered(vec![vec!["A".to_string()]])
        );
        assert_eq!(broker.pending_count(), 0);
        assert_eq!(
            broker.reject("s1", "que_1"),
            Err(QuestionError::NotFound("que_1".into()))
        );
    }

    #[tokio::test]
    async fn dropping_a_pending_question_unregisters_it() {
        let broker = Arc::new(QuestionBroker::new());
        let pending = broker
            .register(QuestionRequest {
                request_id: "que_2".into(),
                session_id: "s1".into(),
                questions: vec![question(&["A"], false, true)],
            })
            .unwrap();
        drop(pending);
        assert_eq!(broker.pending_count(), 0);
        assert_eq!(broker.session_of("que_2"), None);
    }

    #[tokio::test]
    async fn cancelling_a_session_leaves_other_sessions_alone() {
        let broker = Arc::new(QuestionBroker::new());
        let mut handles = Vec::new();
        for (id, session) in [("que_a", "s1"), ("que_b", "s1"), ("que_c", "s2")] {
            handles.push(
                broker
                    .register(QuestionRequest {
                        request_id: id.into(),
                        session_id: session.into(),
                        questions: vec![question(&["A"], false, true)],
                    })
                    .unwrap(),
            );
        }
        assert_eq!(broker.cancel_session("s1"), 2);
        assert_eq!(broker.pending_count(), 1);
        assert_eq!(broker.session_of("que_c"), Some("s2".into()));
    }

    #[tokio::test]
    async fn rejection_resolves_the_waiter() {
        let broker = Arc::new(QuestionBroker::new());
        let pending = broker
            .register(QuestionRequest {
                request_id: "que_3".into(),
                session_id: "s1".into(),
                questions: vec![question(&["A"], false, true)],
            })
            .unwrap();
        broker.reject("s1", "que_3").unwrap();
        assert_eq!(pending.wait().await.unwrap(), QuestionOutcome::Dismissed);
    }

    #[test]
    fn question_defaults_allow_custom_answers_and_single_selection() {
        let parsed: Question =
            serde_json::from_str(r#"{"question":"Q","header":"H","options":[]}"#).unwrap();
        assert!(!parsed.multiple);
        assert!(parsed.custom);
        assert!(validate_questions(&[parsed]).is_ok());
    }
}
