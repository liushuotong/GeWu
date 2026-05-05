//! Evolution graph for iterative self-improvement.

use crate::critique::{CritiqueResult, SelfCritique};
use crate::Config;
use anyhow::Result;
use gewu_core::traits::LlmProvider;
use gewu_graph::state::State;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvolutionState {
    pub task: String,
    pub attempts: i32,
    pub max_attempts: i32,
    pub errors: Vec<String>,
    pub critique_history: Vec<CritiqueResult>,
    pub completed: bool,
    pub outcome: String,
}

impl Default for EvolutionState {
    fn default() -> Self {
        Self {
            task: String::new(),
            attempts: 0,
            max_attempts: 3,
            errors: Vec::new(),
            critique_history: Vec::new(),
            completed: false,
            outcome: String::new(),
        }
    }
}

impl EvolutionState {
    pub fn new(task: String, max_attempts: i32) -> Self {
        Self {
            task,
            max_attempts,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartialEvolutionState {
    pub task: Option<String>,
    pub attempts: Option<i32>,
    pub max_attempts: Option<i32>,
    pub errors: Option<Vec<String>>,
    pub critique_history: Option<Vec<CritiqueResult>>,
    pub completed: Option<bool>,
    pub outcome: Option<String>,
}

impl Default for PartialEvolutionState {
    fn default() -> Self {
        Self {
            task: None,
            attempts: None,
            max_attempts: None,
            errors: None,
            critique_history: None,
            completed: None,
            outcome: None,
        }
    }
}

impl State for EvolutionState {
    type Partial = PartialEvolutionState;

    fn merge(&self, update: Self::Partial) -> Self {
        Self {
            task: update.task.unwrap_or_else(|| self.task.clone()),
            attempts: update.attempts.unwrap_or(self.attempts),
            max_attempts: update.max_attempts.unwrap_or(self.max_attempts),
            errors: update.errors.unwrap_or_else(|| self.errors.clone()),
            critique_history: update
                .critique_history
                .unwrap_or_else(|| self.critique_history.clone()),
            completed: update.completed.unwrap_or(self.completed),
            outcome: update.outcome.unwrap_or_else(|| self.outcome.clone()),
        }
    }
}

pub struct EvolutionGraph {
    config: Config,
}

impl EvolutionGraph {
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    pub async fn run(
        &self,
        task: &str,
        llm: Arc<dyn LlmProvider>,
        mut execute_fn: impl FnMut(&str) -> Result<()>,
    ) -> Result<EvolutionState> {
        let mut state = EvolutionState::new(task.to_string(), self.config.max_attempts);

        while state.attempts < state.max_attempts && !state.completed {
            state.attempts += 1;
            tracing::info!(
                "Evolution attempt {}/{} for task: {}",
                state.attempts,
                state.max_attempts,
                task
            );

            match execute_fn(&state.task) {
                Ok(()) => {
                    state.completed = true;
                    state.outcome = "Task completed successfully".to_string();
                    tracing::info!("Task completed successfully on attempt {}", state.attempts);
                    break;
                }
                Err(e) => {
                    let error_msg = e.to_string();
                    tracing::warn!("Task failed on attempt {}: {}", state.attempts, error_msg);
                    state.errors.push(error_msg.clone());

                    if state.attempts >= state.max_attempts {
                        state.outcome = format!(
                            "Failed after {} attempts. Last error: {}",
                            state.attempts, error_msg
                        );
                        break;
                    }

                    let critique = SelfCritique::analyze(&state.task, &error_msg, &[], llm.as_ref()).await?;

                    tracing::info!(
                        "Critique for attempt {}: root_cause={}, should_retry={}",
                        state.attempts,
                        critique.root_cause,
                        critique.should_retry
                    );

                    state.critique_history.push(critique.clone());

                    if !critique.should_retry {
                        state.outcome = format!(
                            "Critique indicates not to retry. Root cause: {}. Suggested fix: {}",
                            critique.root_cause, critique.suggested_fix
                        );
                        break;
                    }

                    state.task = format!(
                        "{}\n\nPrevious attempt failed with: {}\n\nSuggested fix: {}\n\nPlease retry with this approach.",
                        task,
                        error_msg,
                        critique.suggested_fix
                    );
                }
            }
        }

        Ok(state)
    }

    pub fn should_evolve(&self, error_count: i32) -> bool {
        error_count > 0 && self.config.enabled
    }
}

impl Default for EvolutionGraph {
    fn default() -> Self {
        Self::new(Config::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_evolution_state_merge() {
        let state = EvolutionState::default();
        let update = PartialEvolutionState {
            task: Some("New task".to_string()),
            attempts: Some(2),
            ..Default::default()
        };

        let merged = state.merge(update);
        assert_eq!(merged.task, "New task");
        assert_eq!(merged.attempts, 2);
    }

    #[test]
    fn test_evolution_state_new() {
        let state = EvolutionState::new("Test task".to_string(), 5);
        assert_eq!(state.task, "Test task");
        assert_eq!(state.max_attempts, 5);
        assert_eq!(state.attempts, 0);
        assert!(!state.completed);
    }
}
