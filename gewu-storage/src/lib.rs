use anyhow::{Context, Result};
use async_trait::async_trait;
use gewu_core::traits::Memory;
use gewu_core::types::Message;
use sqlx::{Row, sqlite::SqlitePool};
use tracing::{error, info};

pub struct SqliteMemory {
    pool: SqlitePool,
    session_id: String,
    messages: Vec<Message>,
}

impl SqliteMemory {
    /// Creates a new `SqliteMemory` instance.
    ///
    /// This function:
    /// 1. Ensures the `memory_messages` table exists.
    /// 2. Loads any existing messages for the given `session_id`.
    pub async fn new(pool: SqlitePool, session_id: String) -> Result<Self> {
        // Create table if not exists
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS memory_messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                content TEXT NOT NULL,
                created_at INTEGER DEFAULT (unixepoch())
            )
            "#,
        )
        .execute(&pool)
        .await
        .context("Failed to create memory_messages table")?;

        // Create index for session_id
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_memory_messages_session_id ON memory_messages(session_id)"
        )
        .execute(&pool)
        .await
        .context("Failed to create index on session_id")?;

        // Load existing messages
        let rows =
            sqlx::query("SELECT content FROM memory_messages WHERE session_id = ? ORDER BY id ASC")
                .bind(&session_id)
                .fetch_all(&pool)
                .await
                .context("Failed to load messages from DB")?;

        let mut messages = Vec::new();
        for row in rows {
            let content: String = row.try_get("content")?;
            let message: Message =
                serde_json::from_str(&content).context("Failed to deserialize message content")?;
            messages.push(message);
        }

        info!(
            "Loaded {} messages for session {}",
            messages.len(),
            session_id
        );

        Ok(Self {
            pool,
            session_id,
            messages,
        })
    }
}

#[async_trait]
impl Memory for SqliteMemory {
    fn messages(&self) -> &[Message] {
        &self.messages
    }

    fn messages_mut(&mut self) -> &mut [Message] {
        &mut self.messages
    }

    async fn add_message(&mut self, message: Message) {
        // 1. Update in-memory cache
        self.messages.push(message.clone());

        // 2. Async persist to DB
        let session_id = self.session_id.clone();
        let pool = self.pool.clone();

        // We spawn this task or await it?
        // The trait is async, so we can await it.
        // It's safer to await to ensure durability.
        let content = match serde_json::to_string(&message) {
            Ok(s) => s,
            Err(e) => {
                error!("Failed to serialize message for persistence: {}", e);
                return;
            }
        };

        if let Err(e) =
            sqlx::query("INSERT INTO memory_messages (session_id, content) VALUES (?, ?)")
                .bind(session_id)
                .bind(content)
                .execute(&pool)
                .await
        {
            error!("Failed to persist message to DB: {}", e);
        }
    }

    async fn clear(&mut self) {
        self.messages.clear();
        if let Err(e) = sqlx::query("DELETE FROM memory_messages WHERE session_id = ?")
            .bind(&self.session_id)
            .execute(&self.pool)
            .await
        {
            error!("Failed to clear messages from DB: {}", e);
        }
    }

    async fn replace(&mut self, messages: Vec<Message>) {
        self.messages = messages.clone();

        // Transactional replacement
        let mut tx = match self.pool.begin().await {
            Ok(tx) => tx,
            Err(e) => {
                error!("Failed to begin transaction for replace: {}", e);
                return;
            }
        };

        if let Err(e) = sqlx::query("DELETE FROM memory_messages WHERE session_id = ?")
            .bind(&self.session_id)
            .execute(&mut *tx)
            .await
        {
            error!("Failed to delete old messages in replace: {}", e);
            return;
        }

        for msg in messages {
            let content = match serde_json::to_string(&msg) {
                Ok(s) => s,
                Err(e) => {
                    error!("Failed to serialize message in replace: {}", e);
                    continue; // Or abort transaction?
                }
            };

            if let Err(e) =
                sqlx::query("INSERT INTO memory_messages (session_id, content) VALUES (?, ?)")
                    .bind(&self.session_id)
                    .bind(content)
                    .execute(&mut *tx)
                    .await
            {
                error!("Failed to insert message in replace: {}", e);
                // Should rollback here technically, but let's just log for now
                return;
            }
        }

        if let Err(e) = tx.commit().await {
            error!("Failed to commit replace transaction: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gewu_core::types::{ContentPart, Message};
    use sqlx::sqlite::SqlitePoolOptions;

    #[tokio::test]
    async fn test_persistence() -> Result<()> {
        let pool = SqlitePoolOptions::new().connect("sqlite::memory:").await?;

        let session_id = "test_1".to_string();

        // 1. Create and add
        let mut mem1 = SqliteMemory::new(pool.clone(), session_id.clone()).await?;
        let msg = Message::User {
            content: vec![ContentPart::Text {
                text: "Hello".to_string(),
            }],
        };
        mem1.add_message(msg).await;
        assert_eq!(mem1.messages().len(), 1);

        // 2. Load from same DB
        let mem2 = SqliteMemory::new(pool.clone(), session_id.clone()).await?;
        assert_eq!(mem2.messages().len(), 1);
        match &mem2.messages()[0] {
            Message::User { content } => {
                let text = content
                    .iter()
                    .find_map(|p| match p {
                        ContentPart::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .unwrap_or("");
                assert_eq!(text, "Hello");
            }
            _ => panic!("Wrong type"),
        }

        Ok(())
    }
}
