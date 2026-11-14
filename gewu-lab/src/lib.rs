mod coordinator;
mod executor;
mod model;
mod research;
mod store;

pub use coordinator::{Clock, Coordinator, Executor, RunOutcome, SystemClock};
pub use executor::{AgentExecutor, ExecutionConfig, ProviderKind};
pub use model::*;
pub use research::ResearchConfig;
pub use store::LabStore;

#[derive(Debug, thiserror::Error)]
pub enum LabError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("invalid operation: {0}")]
    Invalid(String),
    #[error("stale, expired, or cancelled attempt")]
    StaleAttempt,
    #[error("execution failed: {0}")]
    ExecutionFailed(String),
}

pub type Result<T> = std::result::Result<T, LabError>;
