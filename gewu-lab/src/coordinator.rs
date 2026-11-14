use crate::{ClaimedTask, ExecutionResult, LabStore, Result, TaskResult};
use async_trait::async_trait;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

#[async_trait]
pub trait Executor: Send + Sync {
    async fn execute(&self, task: ClaimedTask) -> ExecutionResult;

    async fn try_execute(&self, task: ClaimedTask) -> std::result::Result<ExecutionResult, String> {
        Ok(self.execute(task).await)
    }
}

pub trait Clock: Send + Sync {
    fn now(&self) -> i64;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum RunOutcome {
    Idle,
    Completed(TaskResult),
    Failed(TaskResult),
}

pub struct Coordinator<E, C = SystemClock> {
    store: LabStore,
    executor: Arc<E>,
    clock: C,
    worker_id: String,
    lease_seconds: i64,
    lab_id: Option<String>,
    timeout: std::time::Duration,
}

impl<E: Executor> Coordinator<E, SystemClock> {
    pub fn new(
        store: LabStore,
        executor: Arc<E>,
        worker_id: impl Into<String>,
        lease_seconds: i64,
    ) -> Self {
        Self::with_clock(store, executor, SystemClock, worker_id, lease_seconds)
    }
}

impl<E: Executor, C: Clock> Coordinator<E, C> {
    pub fn scoped_to_lab(mut self, lab_id: impl Into<String>) -> Self {
        self.lab_id = Some(lab_id.into());
        self
    }

    pub fn with_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_clock(
        store: LabStore,
        executor: Arc<E>,
        clock: C,
        worker_id: impl Into<String>,
        lease_seconds: i64,
    ) -> Self {
        Self {
            store,
            executor,
            clock,
            worker_id: worker_id.into(),
            lease_seconds,
            lab_id: None,
            timeout: std::time::Duration::from_secs(120),
        }
    }

    pub async fn run_once(&self) -> Result<RunOutcome> {
        self.run_once_at(self.clock.now()).await
    }

    pub async fn run_once_at(&self, now: i64) -> Result<RunOutcome> {
        let Some(claimed) = self
            .store
            .claim_next_for_lab(
                self.lab_id.as_deref(),
                &self.worker_id,
                now,
                self.lease_seconds,
            )
            .await?
        else {
            return Ok(RunOutcome::Idle);
        };
        let result =
            match tokio::time::timeout(self.timeout, self.executor.try_execute(claimed.clone()))
                .await
            {
                Ok(Ok(result)) => result.validate_complete().map(|()| result),
                Ok(Err(error)) => Err(error),
                Err(_) => Err("execution timed out".into()),
            };
        let completed_at = self.clock.now();
        let failed = result.is_err();
        let task_result = match result {
            Ok(result) => {
                self.store
                    .complete_attempt(
                        &claimed.attempt_id,
                        &claimed.lease_token,
                        &result,
                        completed_at,
                    )
                    .await?
            }
            Err(error) => {
                self.store
                    .fail_attempt(
                        &claimed.attempt_id,
                        &claimed.lease_token,
                        &error,
                        completed_at,
                    )
                    .await?
            }
        };
        Ok(if failed {
            RunOutcome::Failed(task_result)
        } else {
            RunOutcome::Completed(task_result)
        })
    }
}
