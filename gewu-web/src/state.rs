use anyhow::{Context, Result};
use futures::Stream;
use futures::stream::BoxStream;
use futures::StreamExt;
use gewu_providers::OpenAIProvider;
use gewu_cli::registry::create_agent;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tracing::{info, error};
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct SessionInfo {
    pub id: String,
    pub agent_type: String,
    pub created_at: i64,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub api_key: Option<String>,
    pub endpoint: Option<String>,
    pub model: Option<String>,
    pub db_path: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            endpoint: None,
            model: Some("gpt-5.3-codex".to_string()),
            db_path: None,
        }
    }
}

pub struct AgentHandle {
    pub agent: Arc<Mutex<gewu_core::Agent>>,
    pub session_id: String,
    pub agent_type: String,
}

#[derive(Clone)]
pub struct AppState {
    pub auth_token: Arc<RwLock<crate::auth::AuthState>>,
    pub sessions: Arc<RwLock<HashMap<String, SessionInfo>>>,
    pub agents: Arc<RwLock<HashMap<String, Arc<AgentHandle>>>>,
    pub config: Arc<RwLock<ServerConfig>>,
    pub db_pool: Arc<Mutex<Option<SqlitePool>>>,
}

impl AppState {
    pub async fn new(config: ServerConfig, auth_token: crate::auth::AuthState) -> Result<Self> {
        let db_pool = if let Some(ref path) = config.db_path {
            Some(create_db_pool(path).await?)
        } else {
            let db_path = get_default_db_path()?;
            Some(create_db_pool(&db_path).await?)
        };

        Ok(Self {
            auth_token: Arc::new(RwLock::new(auth_token)),
            sessions: Arc::new(RwLock::new(HashMap::new())),
            agents: Arc::new(RwLock::new(HashMap::new())),
            config: Arc::new(RwLock::new(config)),
            db_pool: Arc::new(Mutex::new(db_pool)),
        })
    }

    pub async fn create_session(&self, agent_type: String) -> Result<SessionInfo> {
        info!("Creating session with agent_type: {}", agent_type);
        let session_id = Uuid::new_v4().to_string();

        let config = self.config.read().await;
        let api_key = config.api_key.clone().context("API key not configured")?;
        let endpoint = config.endpoint.clone().context("Endpoint not configured")?;
        let model = config.model.clone().unwrap_or_else(|| "gpt-5.3-codex".to_string());
        drop(config);

        let pool_guard = self.db_pool.lock().await;
        let pool = pool_guard.as_ref().context("Database not initialized")?;

        info!("Creating provider and agent...");
        let provider = Arc::new(OpenAIProvider::new_with_endpoint(&api_key, &endpoint, &model));

        let (agent, _registry) = create_agent(&agent_type, provider, pool.clone(), session_id.clone(), vec![]).await?;
        info!("Agent created successfully");

        let created_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        let session_info = SessionInfo {
            id: session_id.clone(),
            agent_type: agent_type.clone(),
            created_at,
        };

        let handle = Arc::new(AgentHandle {
            agent: Arc::new(Mutex::new(agent)),
            session_id: session_id.clone(),
            agent_type: agent_type.clone(),
        });

        self.sessions.write().await.insert(session_id.clone(), session_info.clone());
        self.agents.write().await.insert(session_id.clone(), handle);
        info!("Session {} stored in state", session_id);

        Ok(session_info)
    }

    pub async fn get_session(&self, session_id: &str) -> Option<SessionInfo> {
        self.sessions.read().await.get(session_id).cloned()
    }

    pub async fn list_sessions(&self) -> Vec<SessionInfo> {
        self.sessions.read().await.values().cloned().collect()
    }

    pub async fn delete_session(&self, session_id: &str) -> Result<()> {
        self.sessions.write().await.remove(session_id);
        self.agents.write().await.remove(session_id);
        Ok(())
    }

    pub async fn get_agent(&self, session_id: &str) -> Option<Arc<AgentHandle>> {
        self.agents.read().await.get(session_id).cloned()
    }

    pub async fn update_config(&self, new_config: ServerConfig) -> Result<()> {
        *self.config.write().await = new_config;
        Ok(())
    }

    pub async fn get_config(&self) -> ServerConfig {
        self.config.read().await.clone()
    }
}

fn get_default_db_path() -> Result<String> {
    let home_dir = dirs::home_dir().context("Failed to determine home directory")?;
    let gewu_dir = home_dir.join(".gewu");
    if !gewu_dir.exists() {
        std::fs::create_dir_all(&gewu_dir).context("Failed to create .gewu directory")?;
    }
    Ok(gewu_dir.join("gewu.db").to_string_lossy().to_string())
}

async fn create_db_pool(db_path: &str) -> Result<SqlitePool> {
    let db_url = format!("sqlite://{}?mode=rwc", db_path);
    let pool = SqlitePoolOptions::new()
        .connect(&db_url)
        .await
        .context("Failed to connect to SQLite database")?;
    Ok(pool)
}
