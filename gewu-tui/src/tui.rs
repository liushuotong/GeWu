//! TUI application state: pure logic with no terminal I/O, so behavior is
//! unit-testable. Rendering lives in [`crate::tui_view`].
//!
//! The conversation itself (transcript blocks, busy/idle phase, session id)
//! lives in [`crate::session::SessionModel`], shared with graphical clients.
//! [`App`] adds the terminal-specific input box and scroll state on top.

use gewu_protocol::AgentEvent;

pub use gewu_client::session::{Block, Phase, SessionModel};

/// Terminal UI state for the chat client.
#[derive(Debug)]
pub struct App {
    /// Conversation state shared with other front-ends.
    pub model: SessionModel,
    pub input: String,
    /// Cursor position (in chars) within `input`.
    pub cursor: usize,
    /// Scroll offset from the bottom of the transcript (0 = follow).
    pub scroll: usize,
    /// Follow new output by keeping the transcript scrolled to the end.
    pub follow: bool,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        Self {
            model: SessionModel::new(),
            input: String::new(),
            cursor: 0,
            scroll: 0,
            follow: true,
        }
    }

    // -- input editing ------------------------------------------------------

    /// Insert a character at the cursor.
    pub fn push_char(&mut self, c: char) {
        self.input.insert(self.cursor, c);
        self.cursor += 1;
    }

    /// Remove the character before the cursor.
    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.input.remove(self.cursor);
        }
    }

    /// Remove the character under the cursor.
    pub fn delete(&mut self) {
        if self.cursor < self.input.chars().count() {
            self.input.remove(self.cursor);
        }
    }

    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.input.chars().count());
    }

    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor = self.input.chars().count();
    }

    // -- transcript ---------------------------------------------------------

    /// Apply one decoded agent event to the transcript.
    pub fn apply_event(&mut self, event: &AgentEvent) {
        self.model.apply_event(event);
    }

    /// Record a user prompt, reset the input box, and enter the busy phase.
    pub fn submit(&mut self, prompt: String) {
        self.model.submit(prompt);
        self.input.clear();
        self.cursor = 0;
        self.follow = true;
    }

    /// An ask finished (success or terminal error).
    pub fn finish_ask(&mut self) {
        self.model.finish_ask();
    }

    pub fn set_session(&mut self, session_id: Option<String>) {
        self.model.set_session(session_id);
    }

    pub fn note(&mut self, message: impl Into<String>) {
        self.model.note(message);
    }

    pub fn fail(&mut self, message: impl Into<String>) {
        self.model.fail(message);
    }

    // -- scrolling ----------------------------------------------------------

    pub fn scroll_up(&mut self, amount: usize) {
        self.follow = false;
        self.scroll = self.scroll.saturating_add(amount);
    }

    pub fn scroll_down(&mut self, amount: usize) {
        self.scroll = self.scroll.saturating_sub(amount);
        if self.scroll == 0 {
            self.follow = true;
        }
    }

    /// The prompt to send, if the input is non-empty.
    pub fn take_prompt(&mut self) -> Option<String> {
        let prompt = self.input.trim().to_string();
        if prompt.is_empty() {
            None
        } else {
            Some(prompt)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        App::new()
    }

    #[test]
    fn input_editing_basics() {
        let mut a = app();
        a.push_char('h');
        a.push_char('i');
        assert_eq!(a.input, "hi");
        assert_eq!(a.cursor, 2);
        a.move_left();
        a.push_char('!');
        assert_eq!(a.input, "h!i");
        a.backspace();
        assert_eq!(a.input, "hi");
        a.move_end();
        a.delete();
        assert_eq!(a.input, "hi");
        a.move_home();
        a.delete();
        assert_eq!(a.input, "i");
    }

    #[test]
    fn submit_clears_input_and_sets_busy() {
        let mut a = app();
        a.push_char('h');
        a.push_char('i');
        let prompt = a.take_prompt().unwrap();
        a.submit(prompt);
        assert_eq!(a.model.blocks.len(), 1);
        assert_eq!(a.model.blocks[0], Block::User("hi".into()));
        assert_eq!(a.model.phase, Phase::Busy);
        assert_eq!(a.input, "");
    }

    #[test]
    fn take_prompt_rejects_blank_input() {
        let mut a = app();
        a.push_char(' ');
        assert!(a.take_prompt().is_none());
    }

    #[test]
    fn events_during_busy_then_finish() {
        let mut a = app();
        a.submit("hi".into());
        a.apply_event(&AgentEvent::ContentDelta {
            delta: "hello".into(),
        });
        a.finish_ask();
        assert_eq!(a.model.phase, Phase::Idle);
    }

    #[test]
    fn scroll_follow_semantics() {
        let mut a = app();
        assert!(a.follow);
        a.scroll_up(5);
        assert_eq!(a.scroll, 5);
        assert!(!a.follow);
        a.scroll_down(3);
        assert_eq!(a.scroll, 2);
        assert!(!a.follow);
        a.scroll_down(10);
        assert_eq!(a.scroll, 0);
        assert!(a.follow);
    }

    #[test]
    fn session_only_set_when_present() {
        let mut a = app();
        a.set_session(None);
        assert_eq!(a.model.session_id, None);
        a.set_session(Some("abc".into()));
        assert_eq!(a.model.session_id.as_deref(), Some("abc"));
        a.set_session(None);
        assert_eq!(a.model.session_id.as_deref(), Some("abc"));
    }
}
