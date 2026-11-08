//! Shared startup logic used by both `run` and `serve` subcommands.
//!
//! Resolves configuration, initializes the LLM provider, opens the SQLite
//! pool, discovers skills, and opens the optional LLM request log.

use anyhow::{Context, Result};
use gewu_core::traits::{LlmProvider, RequestLog};
use gewu_core::types::PathEnforcement;
use gewu_providers::{AnthropicProvider, DeepSeekProvider, GlmProvider, MoonshotProvider, OpenAIProvider, ProviderProfile};
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;
use std::env;
use std::sync::{Arc, Mutex};
use tracing::info;
use uuid::Uuid;

use crate::config::LoadedConfig;

/// CLI flags shared by the `run` and `serve` subcommands.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct CommonArgs {
    /// Custom OpenAI Base URL
    #[arg(long, env = "OPENAI_BASE_URL")]
    pub base_url: Option<String>,

    /// OpenAI API Key
    #[arg(long, env = "OPENAI_API_KEY")]
    pub api_key: Option<String>,

    /// OpenAI Endpoint URL
    #[arg(long, env = "OPENAI_ENDPOINT")]
    pub endpoint: Option<String>,

    /// Custom Model Name
    #[arg(long, short, env = "OPENAI_MODEL")]
    pub model: Option<String>,

    /// Provider profile for direct CLI use (configured models own their provider)
    #[arg(long, env = "GEWU_PROVIDER")]
    pub provider: Option<String>,

    /// Initial Agent (default: build)
    #[arg(long, default_value = "build")]
    pub agent: String,

    /// Log level
    #[arg(long, default_value = "info")]
    pub log_level: String,

    /// Path to SQLite database file
    #[arg(long)]
    pub db_path: Option<String>,

    /// Path to LLM configuration file
    #[arg(long)]
    pub llm_config: Option<String>,
}

/// Everything needed to build agents, resolved from CLI args + config + env.
pub struct Bootstrap {
    pub provider: Arc<dyn LlmProvider>,
    pub pool: SqlitePool,
    pub mcp_tools: Vec<Arc<dyn gewu_core::traits::Tool>>,
    pub skills: Vec<gewu_tools::SkillInfo>,
    pub log_writer: Option<RequestLog>,
    pub model: String,
    pub provider_name: String,
    /// Working directory all tools resolve paths against; captured once at
    /// startup so the UI can show it.
    pub working_directory: String,
    /// File-path confinement level for tools and file-tree operations.
    pub enforcement: PathEnforcement,
    /// Model context window (tokens) forwarded to agents for compaction;
    /// `None` keeps the agent's built-in default.
    pub context_limit: Option<usize>,
}

/// Initialize tracing from the `--log-level` flag.
///
/// Idempotent: safe to call from both a subcommand handler and the shared
/// `bootstrap()` path. When a global dispatcher is already installed (e.g.
/// `run` initialized it before calling `bootstrap()`), the call is a no-op
/// instead of failing with "a global default trace dispatcher has already
/// been set".
pub fn init_tracing(log_level: &str) -> Result<()> {
    let level = match log_level.to_lowercase().as_str() {
        "error" => tracing::Level::ERROR,
        "warn" => tracing::Level::WARN,
        "info" => tracing::Level::INFO,
        "debug" => tracing::Level::DEBUG,
        "trace" => tracing::Level::TRACE,
        _ => tracing::Level::INFO,
    };
    let subscriber = tracing_subscriber::fmt().with_max_level(level).finish();
    if tracing::subscriber::set_global_default(subscriber).is_err() {
        tracing::debug!("tracing already initialized; keeping the existing dispatcher");
    }
    Ok(())
}

/// Resolve the SQLite database path from CLI args or the default location.
pub fn resolve_db_path(db_path: Option<String>) -> Result<String> {
    if let Some(path) = db_path {
        return Ok(path);
    }
    let home_dir = dirs::home_dir()
        .context("Failed to determine home directory. Please provide --db-path explicitly.")?;
    let gewu_dir = home_dir.join(".gewu");

    if !gewu_dir.exists() {
        std::fs::create_dir_all(&gewu_dir).context("Failed to create .gewu directory")?;
    }

    Ok(gewu_dir.join("gewu.db").to_string_lossy().to_string())
}

/// Resolve the list of skill directories from config, making relative paths
/// absolute against the current working directory.
pub fn resolve_skill_paths(config: &crate::config::Config) -> Vec<std::path::PathBuf> {
    config
        .skills
        .iter()
        .flatten()
        .map(|p| {
            let path = std::path::PathBuf::from(p);
            if path.is_absolute() {
                path
            } else {
                env::current_dir()
                    .map(|dir| dir.join(path))
                    .unwrap_or_else(|_| std::path::PathBuf::from(p))
            }
        })
        .collect()
}

/// Resolve provider/model settings and build the LLM provider.
///
/// Priority: CLI args > environment variables > config file > defaults.
pub fn build_provider(
    args: &CommonArgs,
    file_config: &crate::config::Config,
) -> Result<(Arc<dyn LlmProvider>, String, String)> {
    file_config.validate()?;
    let selected = args.model.clone()
        .or_else(|| env::var("OPENAI_MODEL").ok())
        .or_else(|| file_config.model.clone())
        .context("select a configured model with --model or the config's model field")?;
    let entry = file_config.models.get(&selected);
    anyhow::ensure!(entry.is_some() || file_config.models.is_empty(), "unknown configured model '{selected}'");
    let override_provider = args.provider.clone().or_else(|| env::var("GEWU_PROVIDER").ok());
    let profile = if let Some(entry) = entry {
        if let Some(name) = &override_provider {
            anyhow::ensure!(name.parse::<ProviderProfile>().map_err(anyhow::Error::msg)? == entry.provider,
                "provider override conflicts with provider assigned to model '{selected}'");
        }
        entry.provider
    } else {
        override_provider.context("--provider is required for direct CLI model selection")?
            .parse::<ProviderProfile>().map_err(anyhow::Error::msg)?
    };
    let settings = file_config.providers.get(profile.id());
    let model = entry.and_then(|entry| entry.model.clone()).unwrap_or(selected);
    let api_key = args
        .api_key
        .clone()
        .or_else(|| env::var(profile.api_key_env()).ok())
        .or_else(|| settings.and_then(|settings| settings.api_key.clone()))
        .with_context(|| format!("API key required: use --api-key, {}, or providers.{}.api_key", profile.api_key_env(), profile.id()))?;

    let base_url = args
        .base_url
        .clone()
        .or_else(|| env::var("OPENAI_BASE_URL").ok())
        .or(args.endpoint.clone())
        .or_else(|| env::var("OPENAI_ENDPOINT").ok())
        .or_else(|| settings.and_then(|settings| settings.base_url.clone()))
        .or_else(|| profile.base_url().map(str::to_string))
        .with_context(|| format!("providers.{}.base_url is required", profile.id()))?;

    let provider_name = profile.id().to_string();

    let provider: Arc<dyn LlmProvider> = match profile {
        ProviderProfile::Anthropic => {
            let mut builder = AnthropicProvider::new_with_endpoint(&api_key, &base_url, &model);
            if let Some(max_tokens) = file_config.max_tokens {
                builder = builder.with_max_tokens(max_tokens);
            }
            info!(
                "Anthropic max_tokens: {} (override with `max_tokens` in the config file)",
                builder.max_tokens()
            );
            Arc::new(builder)
        }
        ProviderProfile::Deepseek => Arc::new(DeepSeekProvider::new_with_endpoint(&api_key, &base_url, &model)),
        ProviderProfile::Glm => Arc::new(GlmProvider::new_with_endpoint(&api_key, &base_url, &model)),
        ProviderProfile::Moonshot => Arc::new(MoonshotProvider::new_with_endpoint(&api_key, &base_url, &model)),
        ProviderProfile::Openai | ProviderProfile::OpenaiCompatible => Arc::new(OpenAIProvider::new_with_endpoint(&api_key, &base_url, &model)),
    };

    Ok((provider, provider_name, model))
}

/// Open the optional LLM request log file when configured.
pub fn open_request_log(file_config: &crate::config::Config) -> Result<Option<RequestLog>> {
    let Some(path) = &file_config.log else {
        return Ok(None);
    };
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("Failed to open LLM request log file: {}", path))?;
    info!("Logging LLM requests to: {}", path);
    Ok(Some(Arc::new(Mutex::new(Box::new(file)))))
}

/// Perform the full startup sequence shared by `run` and `serve`.
pub async fn bootstrap(args: &CommonArgs) -> Result<Bootstrap> {
    init_tracing(&args.log_level)?;

    let loaded_config = LoadedConfig::load(args.llm_config.as_deref())?;
    if let Some(path) = &loaded_config.path {
        info!("Loaded config from: {}", path.display());
    } else {
        info!("No config file found");
    }
    let file_config = &loaded_config.config;

    let (provider, provider_name, model) = build_provider(args, file_config)?;
    info!("Using model: {}", model);
    info!("Using provider: {}", provider_name);

    let db_path = resolve_db_path(args.db_path.clone())?;
    info!("Using database: {}", db_path);
    let db_url = format!("sqlite://{}?mode=rwc", db_path);
    let pool = SqlitePoolOptions::new()
        .connect(&db_url)
        .await
        .context("Failed to connect to SQLite database")?;

    let mcp_tools =
        crate::mcp_loader::load_mcp_tools(loaded_config.config.mcp_servers.clone()).await?;
    let log_writer = open_request_log(file_config)?;

    let skill_paths = resolve_skill_paths(file_config);
    let skills = if skill_paths.is_empty() {
        Vec::new()
    } else {
        let found = gewu_tools::skill::discover(&skill_paths);
        info!("Discovered {} skills", found.len());
        found
    };

    Ok(Bootstrap {
        provider,
        pool,
        mcp_tools,
        skills,
        log_writer,
        model,
        provider_name,
        working_directory: env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("/"))
            .to_string_lossy()
            .to_string(),
        enforcement: loaded_config.config.sandbox,
        context_limit: loaded_config.config.context_limit,
    })
}

/// Default session ID generation shared by subcommands.
pub fn new_session_id() -> String {
    Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_model_selects_provider_and_api_model_id() {
        let config = serde_json::from_str(r#"{"model":"coding","models":{"coding":{"provider":"deepseek","model":"deepseek-reasoner"}},"providers":{"deepseek":{"api_key":"test"}}}"#).unwrap();
        let args = CommonArgs { api_key: Some("test".into()), model: Some("coding".into()), provider: Some("deepseek".into()), ..Default::default() };
        let (_, provider, model) = build_provider(&args, &config).unwrap();
        assert_eq!(provider, "deepseek");
        assert_eq!(model, "deepseek-reasoner");
        let conflict = CommonArgs { provider: Some("openai".into()), ..args.clone() };
        assert!(build_provider(&conflict, &config).is_err());
        let unknown = CommonArgs { model: Some("missing".into()), ..args };
        assert!(build_provider(&unknown, &config).is_err());
    }

    #[test]
    fn startup_constructs_service_specific_providers() {
        for (name, field) in [("deepseek", "reasoning_content"), ("moonshot", "reasoning_content"), ("glm", "reasoning")] {
            let config = serde_json::from_value(serde_json::json!({
                "models": {"coding": {"provider": name, "model": "arbitrary-model"}},
                "providers": {(name): {"base_url": "https://example.com/v1"}}
            })).unwrap();
            let args = CommonArgs {
                api_key: Some("key".into()), model: Some("coding".into()),
                provider: Some(name.into()), base_url: Some("https://example.com/v1".into()),
                ..Default::default()
            };
            let (provider, selected, model) = build_provider(&args, &config).unwrap();
            assert_eq!(selected, name);
            assert_eq!(model, "arbitrary-model");
            let payload = provider.build(&[gewu_core::types::Message::Assistant {
                content: Some("answer".into()), reasoning: None, replay: None, tool_calls: None,
            }], &[]).unwrap();
            assert_eq!(payload["messages"][0][field], "");
        }
    }

    #[test]
    fn init_tracing_is_idempotent() {
        // Regression: `run` called init_tracing and then bootstrap() called
        // it again, which failed with "a global default trace dispatcher has
        // already been set" and killed every `run` invocation.
        init_tracing("info").unwrap();
        init_tracing("debug").unwrap();
    }
}
