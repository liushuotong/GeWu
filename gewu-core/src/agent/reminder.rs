//! Conversation-turn-based mode reminders, independent of tool-call rounds.

use crate::types::{ContentPart, Message};

/// Full instructions on entry, sparse reminders every five user turns, and
/// full refreshes every twenty-five turns. A missing sparse reminder makes
/// this a one-shot notice (for example, when leaving plan mode).
#[derive(Debug, Default)]
pub struct Reminder {
    full: String,
    sparse: Option<String>,
    user_turns: usize,
    pending: Option<String>,
}

impl Reminder {
    /// Create a new mode reminder, resetting the conversation-turn cadence.
    pub fn switch(full: impl Into<String>, sparse: Option<String>) -> Self {
        let full = full.into();
        Self {
            pending: Some(full.clone()),
            full,
            sparse,
            user_turns: 0,
        }
    }

    /// Advance once per user prompt, never once per provider/tool iteration.
    pub fn on_user_turn(&mut self) {
        self.user_turns = self.user_turns.saturating_add(1);
        if self.pending.is_none()
            && self.user_turns.is_multiple_of(5)
            && let Some(sparse) = &self.sparse
        {
            self.pending = Some(if self.user_turns.is_multiple_of(25) {
                self.full.clone()
            } else {
                sparse.clone()
            });
        }
    }

    /// Append a due reminder to the request's last user message. If there is
    /// no user message, retain it for a later request. History is not modified.
    pub fn inject(&mut self, messages: &mut [Message]) {
        if self.pending.is_none() {
            return;
        }
        let Some(Message::User { content }) = messages
            .iter_mut()
            .rev()
            .find(|message| matches!(message, Message::User { .. }))
        else {
            return;
        };
        if let Some(text) = self.pending.take() {
            content.push(ContentPart::Text { text });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user() -> Message {
        Message::User { content: vec![] }
    }

    #[test]
    fn cadence_counts_user_prompts_not_injection_attempts() {
        let mut reminder = Reminder::switch("full", Some("sparse".into()));
        for turn in 1..=50 {
            reminder.on_user_turn();
            let mut messages = vec![user()];
            reminder.inject(&mut messages);
            let expected = if turn == 1 || turn % 25 == 0 {
                Some("full")
            } else if turn % 5 == 0 {
                Some("sparse")
            } else {
                None
            };
            let Message::User { content } = &messages[0] else {
                unreachable!()
            };
            assert_eq!(
                content.first(),
                expected
                    .map(|text| ContentPart::Text { text: text.into() })
                    .as_ref(),
                "turn {turn}"
            );
            let mut tool_round = vec![user()];
            reminder.inject(&mut tool_round);
            assert_eq!(tool_round, vec![user()]);
        }
    }

    #[test]
    fn absent_user_defers_and_targets_last_user() {
        let mut reminder = Reminder::switch("full", None);
        reminder.on_user_turn();
        reminder.inject(&mut []);
        let mut messages = vec![user(), user()];
        reminder.inject(&mut messages);
        assert_eq!(messages[0], user());
        assert_ne!(messages[1], user());
        for _ in 0..30 {
            reminder.on_user_turn();
            let mut messages = vec![user()];
            reminder.inject(&mut messages);
            assert_eq!(messages, vec![user()]);
        }
    }

    #[test]
    fn default_is_silent() {
        let mut reminder = Reminder::default();
        for _ in 0..30 {
            reminder.on_user_turn();
            let mut messages = vec![user()];
            reminder.inject(&mut messages);
            assert_eq!(messages, vec![user()]);
        }
    }
}
