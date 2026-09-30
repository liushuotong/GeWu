use anyhow::{Context, Result, anyhow};
use gewu_core::types::{ContentPart, Message};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq)]
struct SessionSummary {
    session_id: String,
    message_count: i64,
    updated_at: String,
}

#[derive(Debug)]
struct StoredMessage {
    id: i64,
    created_at: String,
    message: Message,
}

pub async fn execute(session_id: Option<&str>, db_path: Option<&str>) -> Result<()> {
    let path = resolve_db_path(db_path)?;
    let pool = open_read_only(&path).await?;

    match session_id {
        Some(session_id) => print_session(&pool, session_id).await,
        None => print_sessions(&pool).await,
    }
}

fn resolve_db_path(db_path: Option<&str>) -> Result<PathBuf> {
    let path = match db_path {
        Some(path) => PathBuf::from(path),
        None => dirs::home_dir()
            .context("Failed to determine home directory. Please provide --db-path explicitly.")?
            .join(".gewu")
            .join("gewu.db"),
    };

    if !path.exists() {
        return Err(anyhow!("Memory database not found: {}", path.display()));
    }

    Ok(path)
}

async fn open_read_only(path: &Path) -> Result<SqlitePool> {
    let options = SqliteConnectOptions::new().filename(path).read_only(true);
    SqlitePoolOptions::new()
        .connect_with(options)
        .await
        .with_context(|| format!("Failed to open memory database: {}", path.display()))
}

async fn list_sessions(pool: &SqlitePool) -> Result<Vec<SessionSummary>> {
    let rows = sqlx::query(
        r#"
        SELECT
            session_id,
            COUNT(*) AS message_count,
            datetime(MAX(created_at), 'unixepoch', 'localtime') AS updated_at
        FROM memory_messages
        GROUP BY session_id
        ORDER BY MAX(created_at) DESC, session_id ASC
        "#,
    )
    .fetch_all(pool)
    .await
    .context("Failed to list memory sessions; the database may not contain agent memory")?;

    rows.into_iter()
        .map(|row| {
            Ok(SessionSummary {
                session_id: row.try_get("session_id")?,
                message_count: row.try_get("message_count")?,
                updated_at: row
                    .try_get::<Option<String>, _>("updated_at")?
                    .unwrap_or_else(|| "unknown".to_string()),
            })
        })
        .collect()
}

async fn load_session(pool: &SqlitePool, session_id: &str) -> Result<Vec<StoredMessage>> {
    let rows = sqlx::query(
        r#"
        SELECT
            id,
            content,
            datetime(created_at, 'unixepoch', 'localtime') AS created_at
        FROM memory_messages
        WHERE session_id = ?
        ORDER BY id ASC
        "#,
    )
    .bind(session_id)
    .fetch_all(pool)
    .await
    .with_context(|| format!("Failed to load memory for session '{}'", session_id))?;

    rows.into_iter()
        .map(|row| {
            let id: i64 = row.try_get("id")?;
            let content: String = row.try_get("content")?;
            let message = serde_json::from_str(&content)
                .with_context(|| format!("Failed to deserialize memory message {}", id))?;
            Ok(StoredMessage {
                id,
                created_at: row
                    .try_get::<Option<String>, _>("created_at")?
                    .unwrap_or_else(|| "unknown".to_string()),
                message,
            })
        })
        .collect()
}

async fn print_sessions(pool: &SqlitePool) -> Result<()> {
    let sessions = list_sessions(pool).await?;
    if sessions.is_empty() {
        println!("No memory sessions found.");
        return Ok(());
    }

    println!("{:<38} {:>8}  {}", "SESSION ID", "MESSAGES", "UPDATED");
    for session in sessions {
        println!(
            "{:<38} {:>8}  {}",
            session.session_id, session.message_count, session.updated_at
        );
    }
    Ok(())
}

async fn print_session(pool: &SqlitePool, session_id: &str) -> Result<()> {
    let messages = load_session(pool, session_id).await?;
    if messages.is_empty() {
        return Err(anyhow!("No memory found for session '{}'", session_id));
    }

    println!("Session: {}", session_id);
    for stored in messages {
        let (role, content) = format_message(&stored.message);
        println!(
            "\n[{}] {} {}\n{}",
            stored.id, stored.created_at, role, content
        );
    }
    Ok(())
}

fn format_message(message: &Message) -> (&'static str, String) {
    match message {
        Message::System { content } => ("SYSTEM", content.clone()),
        Message::User { content } => ("USER", format_content_parts(content)),
        Message::Assistant {
            content,
            tool_calls,
        } => {
            let mut output = content.clone().unwrap_or_default();
            if let Some(tool_calls) = tool_calls {
                for call in tool_calls {
                    if !output.is_empty() {
                        output.push('\n');
                    }
                    output.push_str(&format!(
                        "Tool call: {} {} [id: {}]",
                        call.function.name, call.function.arguments, call.id
                    ));
                }
            }
            ("ASSISTANT", output)
        }
        Message::Tool {
            tool_call_id,
            tool_name,
            content,
            media,
        } => {
            let mut output = format!(
                "{} [call: {}]\n{}",
                tool_name.as_deref().unwrap_or("tool"),
                tool_call_id,
                content
            );
            if !media.is_empty() {
                output.push('\n');
                output.push_str(&format_content_parts(media));
            }
            ("TOOL", output)
        }
    }
}

fn format_content_parts(parts: &[ContentPart]) -> String {
    parts
        .iter()
        .map(|part| match part {
            ContentPart::Text { text } => text.clone(),
            ContentPart::Image { mime_type, .. } => format!("[image: {}]", mime_type),
            ContentPart::Audio { mime_type, .. } => format!("[audio: {}]", mime_type),
            ContentPart::Video { mime_type, .. } => format!("[video: {}]", mime_type),
            ContentPart::File { mime_type, .. } => format!("[file: {}]", mime_type),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use gewu_core::types::{FunctionCall, ToolCall};
    use sqlx::sqlite::SqlitePoolOptions;
    use tempfile::tempdir;

    async fn test_pool() -> Result<SqlitePool> {
        let pool = SqlitePoolOptions::new().connect("sqlite::memory:").await?;
        sqlx::query(
            r#"
            CREATE TABLE memory_messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                content TEXT NOT NULL,
                created_at INTEGER NOT NULL
            )
            "#,
        )
        .execute(&pool)
        .await?;
        Ok(pool)
    }

    async fn insert_message(
        pool: &SqlitePool,
        session_id: &str,
        message: &Message,
        created_at: i64,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO memory_messages (session_id, content, created_at) VALUES (?, ?, ?)",
        )
        .bind(session_id)
        .bind(serde_json::to_string(message)?)
        .bind(created_at)
        .execute(pool)
        .await?;
        Ok(())
    }

    #[tokio::test]
    async fn lists_sessions_by_latest_message() -> Result<()> {
        let pool = test_pool().await?;
        insert_message(
            &pool,
            "older",
            &Message::System {
                content: "system".to_string(),
            },
            1,
        )
        .await?;
        insert_message(
            &pool,
            "newer",
            &Message::User {
                content: vec![ContentPart::Text {
                    text: "hello".to_string(),
                }],
            },
            2,
        )
        .await?;

        let sessions = list_sessions(&pool).await?;
        assert_eq!(sessions[0].session_id, "newer");
        assert_eq!(sessions[1].session_id, "older");
        Ok(())
    }

    #[tokio::test]
    async fn loads_messages_in_database_order() -> Result<()> {
        let pool = test_pool().await?;
        insert_message(
            &pool,
            "session",
            &Message::User {
                content: vec![ContentPart::Text {
                    text: "first".to_string(),
                }],
            },
            2,
        )
        .await?;
        insert_message(
            &pool,
            "session",
            &Message::Assistant {
                content: Some("second".to_string()),
                tool_calls: None,
            },
            1,
        )
        .await?;

        let messages = load_session(&pool, "session").await?;
        assert_eq!(format_message(&messages[0].message).1, "first");
        assert_eq!(format_message(&messages[1].message).1, "second");
        Ok(())
    }

    #[tokio::test]
    async fn reports_unknown_session() -> Result<()> {
        let pool = test_pool().await?;
        let error = print_session(&pool, "missing").await.unwrap_err();
        assert!(error.to_string().contains("No memory found"));
        Ok(())
    }

    #[test]
    fn formats_tool_calls_and_results() {
        let assistant = Message::Assistant {
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "call-1".to_string(),
                r#type: "function".to_string(),
                function: FunctionCall {
                    name: "websearch".to_string(),
                    arguments: r#"{"query":"rust"}"#.to_string(),
                },
            }]),
        };
        let tool = Message::Tool {
            tool_call_id: "call-1".to_string(),
            tool_name: Some("websearch".to_string()),
            content: "result".to_string(),
            media: vec![],
        };

        assert!(
            format_message(&assistant)
                .1
                .contains("Tool call: websearch")
        );
        assert!(format_message(&tool).1.contains("websearch [call: call-1]"));
    }

    #[tokio::test]
    async fn opens_database_read_only() -> Result<()> {
        let dir = tempdir()?;
        let path = dir.path().join("memory.db");
        let options = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true);
        let writable = SqlitePoolOptions::new().connect_with(options).await?;
        sqlx::query("CREATE TABLE memory_messages (id INTEGER)")
            .execute(&writable)
            .await?;
        writable.close().await;

        let readonly = open_read_only(&path).await?;
        let result = sqlx::query("INSERT INTO memory_messages (id) VALUES (1)")
            .execute(&readonly)
            .await;
        assert!(result.is_err());
        Ok(())
    }
}
