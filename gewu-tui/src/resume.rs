//! Startup session resume: pick the most recent stored conversation and
//! replay it into the transcript.
//!
//! The server pins one session per connection when the first ask starts, so
//! setting the session id before the first prompt is enough to continue an
//! existing conversation — no reconnect is required on startup.

use gewu_client::Client;
use gewu_protocol::{Message, SessionSummary};

use crate::tui::App;

/// Choose which session to resume.
///
/// An explicit id (CLI flag or environment) always wins. Otherwise, when
/// `resume` is set, the newest stored session is used; `sessions` is expected
/// in `list_sessions` order (newest first).
pub fn choose_session<'a>(
    explicit: Option<&'a str>,
    resume: bool,
    sessions: &'a [SessionSummary],
) -> Option<&'a str> {
    if let Some(id) = explicit {
        return Some(id);
    }
    if !resume {
        return None;
    }
    sessions.first().map(|session| session.session_id.as_str())
}

/// Replay a stored session into `app`.
///
/// The session id is recorded even when the transcript is empty so the next
/// prompt continues that conversation.
pub fn apply_history(app: &mut App, session_id: &str, messages: &[Message]) {
    app.model.load_history(messages);
    app.rendered.clear();
    app.expanded_groups.clear();
    app.expanded_outputs.clear();
    app.click_map.clear();
    app.set_session(Some(session_id.to_string()));
    app.note(format!(
        "resumed session {} ({} stored message{})",
        short_id(session_id),
        messages.len(),
        if messages.len() == 1 { "" } else { "s" }
    ));
}

/// Resolve and load the startup session.
///
/// Failures are reported as notes instead of aborting: a TUI that cannot read
/// history should still let the user chat.
pub async fn load_startup_session(
    client: &Client,
    app: &mut App,
    explicit: Option<&str>,
    resume: bool,
) {
    let sessions = if explicit.is_none() && resume {
        match client.list_sessions().await {
            Ok(sessions) => sessions,
            Err(e) => {
                app.note(format!("could not list sessions: {e}"));
                return;
            }
        }
    } else {
        Vec::new()
    };

    let Some(session_id) = choose_session(explicit, resume, &sessions) else {
        return;
    };
    let session_id = session_id.to_string();

    match client.get_session(session_id.clone()).await {
        Ok(messages) => apply_history(app, &session_id, &messages),
        Err(e) => {
            // Still pin the id so new messages continue that conversation.
            app.set_session(Some(session_id));
            app.note(format!("could not load session: {e}"));
        }
    }
}

/// First eight characters of a session id, like the status line uses.
fn short_id(session_id: &str) -> String {
    let short: String = session_id.chars().take(8).collect();
    if session_id.chars().count() > 8 {
        format!("{short}…")
    } else {
        short
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gewu_protocol::ContentPart;

    fn summary(session_id: &str) -> SessionSummary {
        SessionSummary {
            session_id: session_id.to_string(),
            message_count: 1,
            updated_at: "now".to_string(),
        }
    }

    #[test]
    fn explicit_session_wins_over_resume() {
        let sessions = vec![summary("newest"), summary("older")];
        assert_eq!(
            choose_session(Some("chosen"), true, &sessions),
            Some("chosen")
        );
        // ...even when resume is disabled.
        assert_eq!(
            choose_session(Some("chosen"), false, &sessions),
            Some("chosen")
        );
    }

    #[test]
    fn resume_uses_newest_session() {
        let sessions = vec![summary("newest"), summary("older")];
        assert_eq!(choose_session(None, true, &sessions), Some("newest"));
    }

    #[test]
    fn resume_disabled_starts_fresh() {
        let sessions = vec![summary("newest")];
        assert_eq!(choose_session(None, false, &sessions), None);
    }

    #[test]
    fn empty_session_list_starts_fresh() {
        assert_eq!(choose_session(None, true, &[]), None);
    }

    #[test]
    fn apply_history_populates_transcript() {
        let mut app = App::new();
        let messages = vec![
            Message::User {
                content: vec![ContentPart::Text {
                    text: "old question".to_string(),
                }],
            },
            Message::Assistant {
                content: Some("old answer".to_string()),
                tool_calls: None,
            },
        ];

        apply_history(&mut app, "abcdef123456", &messages);

        assert_eq!(app.model.session_id.as_deref(), Some("abcdef123456"));
        assert_eq!(
            app.model.blocks[0],
            crate::tui::Block::User("old question".to_string())
        );
        assert_eq!(
            app.model.blocks[1],
            crate::tui::Block::Assistant("old answer".to_string())
        );
        assert!(
            app.model.blocks.iter().any(
                |block| matches!(block, crate::tui::Block::Info(text) if text.contains("abcdef12"))
            ),
            "a resume note is appended: {:?}",
            app.model.blocks
        );
    }

    #[test]
    fn apply_history_keeps_session_when_empty() {
        let mut app = App::new();
        apply_history(&mut app, "onlysession", &[]);
        assert_eq!(app.model.session_id.as_deref(), Some("onlysession"));
        assert!(
            app.model.blocks.iter().any(
                |block| matches!(block, crate::tui::Block::Info(text) if text.contains("0 stored"))
            ),
            "note records the empty history: {:?}",
            app.model.blocks
        );
    }

    /// Minimal newline-delimited JSON server for the resume flow.
    fn mock_server() -> (u16, std::thread::JoinHandle<()>) {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;
        use std::time::Duration;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut writer = stream.try_clone().unwrap();
            let mut reader = BufReader::new(stream);
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                let id = request.get("id").cloned().unwrap_or_default();
                let reply = match request["op"].as_str().unwrap() {
                    "list_sessions" => serde_json::json!({
                        "type": "sessions",
                        "id": id,
                        "sessions": [
                            {"session_id": "newest-session", "message_count": 2, "updated_at": "now"},
                            {"session_id": "older-session", "message_count": 1, "updated_at": "earlier"},
                        ],
                    }),
                    "get_session" => serde_json::json!({
                        "type": "session",
                        "id": id,
                        "session_id": request["session_id"],
                        "messages": [
                            {"role": "user", "content": [
                                {"type": "text", "text": "restored question"}
                            ]},
                            {"role": "assistant", "content": "restored answer", "tool_calls": null},
                        ],
                    }),
                    _ => serde_json::json!({"type": "pong", "id": id}),
                };
                writeln!(writer, "{reply}").unwrap();
                writer.flush().unwrap();
            }
        });
        (port, handle)
    }

    #[tokio::test]
    async fn startup_resume_loads_newest_session_over_tcp() {
        let (port, server) = mock_server();
        let client = Client::tcp("127.0.0.1", port).await.unwrap();

        let mut app = App::new();
        load_startup_session(&client, &mut app, None, true).await;

        client.close().await;
        server.join().unwrap();

        assert_eq!(app.model.session_id.as_deref(), Some("newest-session"));
        assert!(
            app.model.blocks.iter().any(
                |block| matches!(block, crate::tui::Block::User(text) if text == "restored question")
            ),
            "{:?}",
            app.model.blocks
        );
        assert!(
            app.model.blocks.iter().any(|block| matches!(
                block,
                crate::tui::Block::Assistant(text) if text == "restored answer"
            )),
            "{:?}",
            app.model.blocks
        );
    }
}
