use crate::{
    Attempt, ClaimedTask, Decision, ExecutionResult, InterestRevision, Lab, LabError, Project,
    Result, Task, TaskResult, TaskSpec, TaskStatus,
};
use crate::{PlanningProposal, PlanningProposalSpec, ProposalDecision};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

#[derive(Clone)]
pub struct LabStore {
    pool: SqlitePool,
}

impl LabStore {
    pub async fn connect(database_url: &str) -> Result<Self> {
        let options = database_url
            .parse::<sqlx::sqlite::SqliteConnectOptions>()?
            .create_if_missing(true)
            .foreign_keys(true)
            .busy_timeout(std::time::Duration::from_secs(10));
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(options)
            .await?;
        Self::from_pool(pool).await
    }

    pub async fn from_pool(pool: SqlitePool) -> Result<Self> {
        let store = Self { pool };
        store.migrate().await?;
        Ok(store)
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn migrate(&self) -> Result<()> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::raw_sql(include_str!("schema.sql"))
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn create_lab(&self, id: &str, name: &str, interest: &str, now: i64) -> Result<()> {
        nonempty(interest)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query("INSERT INTO lab_labs(id, name, created_at) VALUES (?, ?, ?)")
            .bind(id)
            .bind(name)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO lab_interest_revisions(lab_id, revision, interest, rationale, created_at) VALUES (?, 1, ?, 'initial interest', ?)").bind(id).bind(interest).bind(now).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn get_lab(&self, id: &str) -> Result<Option<Lab>> {
        Ok(sqlx::query_as(
            "SELECT id, name, paused, current_interest_revision FROM lab_labs WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn labs(&self) -> Result<Vec<Lab>> {
        Ok(sqlx::query_as("SELECT id, name, paused, current_interest_revision FROM lab_labs ORDER BY created_at, id")
            .fetch_all(&self.pool).await?)
    }

    pub async fn projects(&self, lab_id: &str) -> Result<Vec<Project>> {
        Ok(
            sqlx::query_as("SELECT * FROM lab_projects WHERE lab_id = ? ORDER BY created_at, id")
                .bind(lab_id)
                .fetch_all(&self.pool)
                .await?,
        )
    }

    pub async fn tasks(&self, project_id: &str) -> Result<Vec<Task>> {
        Ok(sqlx::query_as("SELECT * FROM lab_tasks WHERE project_id = ? ORDER BY COALESCE(priority_override, priority) DESC, created_at, id")
            .bind(project_id).fetch_all(&self.pool).await?)
    }

    pub async fn interest_revision(
        &self,
        lab_id: &str,
        revision: i64,
    ) -> Result<Option<InterestRevision>> {
        Ok(
            sqlx::query_as(
                "SELECT * FROM lab_interest_revisions WHERE lab_id = ? AND revision = ?",
            )
            .bind(lab_id)
            .bind(revision)
            .fetch_optional(&self.pool)
            .await?,
        )
    }

    pub async fn revise_interest(
        &self,
        lab_id: &str,
        interest: &str,
        rationale: &str,
        now: i64,
    ) -> Result<i64> {
        nonempty(interest)?;
        nonempty(rationale)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let revision: i64 =
            sqlx::query_scalar("SELECT current_interest_revision + 1 FROM lab_labs WHERE id = ?")
                .bind(lab_id)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or_else(|| LabError::Invalid(format!("unknown lab: {lab_id}")))?;
        sqlx::query("INSERT INTO lab_interest_revisions(lab_id, revision, interest, rationale, created_at) VALUES (?, ?, ?, ?, ?)").bind(lab_id).bind(revision).bind(interest).bind(rationale).bind(now).execute(&mut *tx).await?;
        sqlx::query("UPDATE lab_labs SET current_interest_revision = ? WHERE id = ?")
            .bind(revision)
            .bind(lab_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(revision)
    }

    pub async fn create_project(
        &self,
        id: &str,
        lab_id: &str,
        name: &str,
        question: &str,
        now: i64,
    ) -> Result<()> {
        nonempty(question)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let revision: i64 =
            sqlx::query_scalar("SELECT current_interest_revision FROM lab_labs WHERE id = ?")
                .bind(lab_id)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or_else(|| LabError::Invalid(format!("unknown lab: {lab_id}")))?;
        sqlx::query("INSERT INTO lab_projects(id, lab_id, name, question, interest_revision, created_at) VALUES (?, ?, ?, ?, ?, ?)").bind(id).bind(lab_id).bind(name).bind(question).bind(revision).bind(now).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn get_project(&self, id: &str) -> Result<Option<Project>> {
        Ok(sqlx::query_as("SELECT * FROM lab_projects WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?)
    }

    pub async fn create_proposal(
        &self,
        id: &str,
        lab_id: &str,
        spec: &PlanningProposalSpec,
        now: i64,
    ) -> Result<()> {
        nonempty(id)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let revision: i64 =
            sqlx::query_scalar("SELECT current_interest_revision FROM lab_labs WHERE id = ?")
                .bind(lab_id)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or_else(|| LabError::Invalid(format!("unknown lab: {lab_id}")))?;
        validate_proposal(&mut tx, lab_id, spec).await?;
        let json =
            serde_json::to_string(spec).map_err(|error| LabError::Invalid(error.to_string()))?;
        sqlx::query("INSERT INTO lab_planning_proposals(id, lab_id, interest_revision, spec_json, status, created_at) VALUES (?, ?, ?, ?, 'proposed', ?)")
            .bind(id).bind(lab_id).bind(revision).bind(json).bind(now).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn get_proposal(&self, id: &str) -> Result<Option<PlanningProposal>> {
        let row = sqlx::query("SELECT * FROM lab_planning_proposals WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(proposal_from_row).transpose()
    }

    pub async fn proposals(&self, lab_id: &str) -> Result<Vec<PlanningProposal>> {
        let rows = sqlx::query(
            "SELECT * FROM lab_planning_proposals WHERE lab_id = ? ORDER BY created_at, id",
        )
        .bind(lab_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(proposal_from_row).collect()
    }

    pub async fn proposal_decisions(&self, proposal_id: &str) -> Result<Vec<ProposalDecision>> {
        Ok(sqlx::query_as(
            "SELECT * FROM lab_proposal_decisions WHERE proposal_id = ? ORDER BY created_at, id",
        )
        .bind(proposal_id)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn approve_proposal(
        &self,
        id: &str,
        actor: &str,
        rationale: &str,
        now: i64,
    ) -> Result<()> {
        self.decide_proposal(id, true, actor, rationale, now).await
    }

    pub async fn reject_proposal(
        &self,
        id: &str,
        actor: &str,
        rationale: &str,
        now: i64,
    ) -> Result<()> {
        self.decide_proposal(id, false, actor, rationale, now).await
    }

    async fn decide_proposal(
        &self,
        id: &str,
        approve: bool,
        actor: &str,
        rationale: &str,
        now: i64,
    ) -> Result<()> {
        validate_decision(actor, rationale)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let row = sqlx::query("SELECT * FROM lab_planning_proposals WHERE id = ?")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| LabError::Invalid(format!("unknown proposal: {id}")))?;
        let proposal = proposal_from_row(row)?;
        if proposal.status != crate::ProposalStatus::Proposed {
            return Err(LabError::Invalid("proposal is not proposed".into()));
        }
        let decision_id = Uuid::new_v4().to_string();
        if approve {
            validate_proposal(&mut tx, &proposal.lab_id, &proposal.spec).await?;
            for project in &proposal.spec.projects {
                sqlx::query("INSERT INTO lab_projects(id, lab_id, name, question, status, interest_revision, created_at) VALUES (?, ?, ?, ?, 'active', ?, ?)")
                    .bind(&project.id).bind(&proposal.lab_id).bind(&project.name).bind(&project.question).bind(proposal.interest_revision).bind(now).execute(&mut *tx).await?;
            }
            for task in &proposal.spec.tasks {
                sqlx::query("INSERT INTO lab_tasks(id, project_id, objective, context, deliverable, completion_criteria, owner, priority, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
                    .bind(&task.id).bind(&task.project_id).bind(&task.objective).bind(&task.context).bind(&task.deliverable).bind(&task.completion_criteria).bind(&task.owner).bind(task.priority).bind(now).bind(now).execute(&mut *tx).await?;
            }
            for dependency in &proposal.spec.dependencies {
                sqlx::query("INSERT INTO lab_dependencies(task_id, depends_on) VALUES (?, ?)")
                    .bind(&dependency.task_id)
                    .bind(&dependency.depends_on)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        sqlx::query("UPDATE lab_planning_proposals SET status = ? WHERE id = ?")
            .bind(if approve { "approved" } else { "rejected" })
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO lab_proposal_decisions(id, proposal_id, kind, actor, rationale, created_at) VALUES (?, ?, ?, ?, ?, ?)")
            .bind(&decision_id).bind(id).bind(if approve { "approve" } else { "reject" }).bind(actor).bind(rationale).bind(now).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO lab_decisions(id, lab_id, kind, actor, rationale, created_at) VALUES (?, ?, ?, ?, ?, ?)")
            .bind(decision_id).bind(&proposal.lab_id).bind(if approve { "approve_proposal" } else { "reject_proposal" }).bind(actor).bind(rationale).bind(now).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn create_task(&self, task: &TaskSpec, now: i64) -> Result<()> {
        for text in [
            &task.objective,
            &task.deliverable,
            &task.completion_criteria,
        ] {
            nonempty(text)?;
        }
        sqlx::query("INSERT INTO lab_tasks(id, project_id, objective, context, deliverable, completion_criteria, owner, priority, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)").bind(&task.id).bind(&task.project_id).bind(&task.objective).bind(&task.context).bind(&task.deliverable).bind(&task.completion_criteria).bind(&task.owner).bind(task.priority).bind(now).bind(now).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn get_task(&self, id: &str) -> Result<Option<Task>> {
        Ok(sqlx::query_as("SELECT * FROM lab_tasks WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?)
    }

    pub async fn add_dependency(&self, task_id: &str, depends_on: &str) -> Result<()> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let valid: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM lab_tasks t JOIN lab_tasks d ON d.project_id = t.project_id WHERE t.id = ? AND d.id = ? AND t.status = 'ready'").bind(task_id).bind(depends_on).fetch_one(&mut *tx).await?;
        let cycle: i64 = sqlx::query_scalar("WITH RECURSIVE ancestors(id) AS (SELECT ? UNION SELECT d.depends_on FROM lab_dependencies d JOIN ancestors a ON d.task_id = a.id) SELECT COUNT(*) FROM ancestors WHERE id = ?").bind(depends_on).bind(task_id).fetch_one(&mut *tx).await?;
        if valid != 1 || cycle != 0 {
            return Err(LabError::Invalid(
                "dependency must be acyclic, same-project, and added before execution".into(),
            ));
        }
        sqlx::query("INSERT INTO lab_dependencies(task_id, depends_on) VALUES (?, ?)")
            .bind(task_id)
            .bind(depends_on)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn approve_project(
        &self,
        project_id: &str,
        actor: &str,
        rationale: &str,
        now: i64,
    ) -> Result<()> {
        validate_decision(actor, rationale)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let lab_id: String = sqlx::query_scalar("UPDATE lab_projects SET status = 'active' WHERE id = ? AND status = 'proposed' RETURNING lab_id").bind(project_id).fetch_optional(&mut *tx).await?.ok_or_else(|| LabError::Invalid("project is not proposed".into()))?;
        sqlx::query("INSERT INTO lab_decisions(id, lab_id, project_id, kind, actor, rationale, created_at) VALUES (?, ?, ?, 'approve_project', ?, ?, ?)").bind(Uuid::new_v4().to_string()).bind(lab_id).bind(project_id).bind(actor).bind(rationale).bind(now).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn decisions(&self, lab_id: &str) -> Result<Vec<Decision>> {
        Ok(
            sqlx::query_as("SELECT * FROM lab_decisions WHERE lab_id = ? ORDER BY created_at, id")
                .bind(lab_id)
                .fetch_all(&self.pool)
                .await?,
        )
    }

    pub async fn set_paused(
        &self,
        lab_id: &str,
        paused: bool,
        actor: &str,
        rationale: &str,
        now: i64,
    ) -> Result<()> {
        validate_decision(actor, rationale)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let changed = sqlx::query("UPDATE lab_labs SET paused = ? WHERE id = ?")
            .bind(paused)
            .bind(lab_id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        if changed == 0 {
            return Err(LabError::Invalid(format!("unknown lab: {lab_id}")));
        }
        sqlx::query("INSERT INTO lab_decisions(id, lab_id, kind, actor, rationale, created_at) VALUES (?, ?, ?, ?, ?, ?)").bind(Uuid::new_v4().to_string()).bind(lab_id).bind(if paused { "pause" } else { "resume" }).bind(actor).bind(rationale).bind(now).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn override_priority(
        &self,
        task_id: &str,
        priority: i64,
        actor: &str,
        rationale: &str,
        now: i64,
    ) -> Result<()> {
        validate_decision(actor, rationale)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let lab_id: String = sqlx::query_scalar("SELECT p.lab_id FROM lab_tasks t JOIN lab_projects p ON p.id = t.project_id WHERE t.id = ? AND t.status NOT IN ('completed', 'cancelled')").bind(task_id).fetch_optional(&mut *tx).await?.ok_or_else(|| LabError::Invalid(format!("unknown or closed task: {task_id}")))?;
        sqlx::query("UPDATE lab_tasks SET priority_override = ?, updated_at = ? WHERE id = ?")
            .bind(priority)
            .bind(now)
            .bind(task_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO lab_decisions(id, lab_id, task_id, kind, actor, rationale, priority, created_at) VALUES (?, ?, ?, 'priority_override', ?, ?, ?, ?)").bind(Uuid::new_v4().to_string()).bind(lab_id).bind(task_id).bind(actor).bind(rationale).bind(priority).bind(now).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn cancel_task(
        &self,
        task_id: &str,
        actor: &str,
        rationale: &str,
        now: i64,
    ) -> Result<()> {
        validate_decision(actor, rationale)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let lab_id: String = sqlx::query_scalar("SELECT p.lab_id FROM lab_tasks t JOIN lab_projects p ON p.id = t.project_id WHERE t.id = ? AND t.status NOT IN ('completed', 'cancelled')").bind(task_id).fetch_optional(&mut *tx).await?.ok_or_else(|| LabError::Invalid(format!("unknown or closed task: {task_id}")))?;
        sqlx::query("UPDATE lab_tasks SET status = 'cancelled', updated_at = ? WHERE id = ?")
            .bind(now)
            .bind(task_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE lab_attempts SET status = 'cancelled', completed_at = ? WHERE task_id = ? AND status IN ('running', 'submitted')").bind(now).bind(task_id).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO lab_decisions(id, lab_id, task_id, kind, actor, rationale, created_at) VALUES (?, ?, ?, 'cancel', ?, ?, ?)").bind(Uuid::new_v4().to_string()).bind(lab_id).bind(task_id).bind(actor).bind(rationale).bind(now).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn recover_expired(&self, now: i64) -> Result<u64> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let count = recover(&mut tx, now).await?;
        tx.commit().await?;
        Ok(count)
    }

    pub async fn claim_next(
        &self,
        worker_id: &str,
        now: i64,
        lease_seconds: i64,
    ) -> Result<Option<ClaimedTask>> {
        self.claim_next_for_lab(None, worker_id, now, lease_seconds)
            .await
    }

    pub async fn claim_next_for_lab(
        &self,
        lab_id: Option<&str>,
        worker_id: &str,
        now: i64,
        lease_seconds: i64,
    ) -> Result<Option<ClaimedTask>> {
        let expires_at = now
            .checked_add(lease_seconds)
            .filter(|_| lease_seconds > 0 && !worker_id.trim().is_empty())
            .ok_or_else(|| LabError::Invalid("positive lease and worker id required".into()))?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        recover(&mut tx, now).await?;
        let task: Option<Task> = sqlx::query_as("SELECT t.* FROM lab_tasks t JOIN lab_projects p ON p.id = t.project_id JOIN lab_labs l ON l.id = p.lab_id WHERE t.status = 'ready' AND p.status = 'active' AND l.paused = 0 AND (? IS NULL OR p.lab_id = ?) AND (t.owner IS NULL OR t.owner = ?) AND NOT EXISTS (SELECT 1 FROM lab_dependencies d JOIN lab_tasks dependency ON dependency.id = d.depends_on WHERE d.task_id = t.id AND dependency.status != 'completed') ORDER BY COALESCE(t.priority_override, t.priority) DESC, t.created_at, t.id LIMIT 1").bind(lab_id).bind(lab_id).bind(worker_id).fetch_optional(&mut *tx).await?;
        let Some(mut task) = task else {
            tx.commit().await?;
            return Ok(None);
        };
        let project: Project = sqlx::query_as("SELECT * FROM lab_projects WHERE id = ?")
            .bind(&task.project_id)
            .fetch_one(&mut *tx)
            .await?;
        let interest: InterestRevision = sqlx::query_as(
            "SELECT * FROM lab_interest_revisions WHERE lab_id = ? AND revision = ?",
        )
        .bind(&project.lab_id)
        .bind(project.interest_revision)
        .fetch_one(&mut *tx)
        .await?;
        let attempt_id = Uuid::new_v4().to_string();
        let lease_token = Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO lab_attempts(id, task_id, worker_id, lease_token, status, claimed_at, expires_at) VALUES (?, ?, ?, ?, 'running', ?, ?)").bind(&attempt_id).bind(&task.id).bind(worker_id).bind(&lease_token).bind(now).bind(expires_at).execute(&mut *tx).await?;
        sqlx::query("UPDATE lab_tasks SET status = 'in_progress', updated_at = ? WHERE id = ?")
            .bind(now)
            .bind(&task.id)
            .execute(&mut *tx)
            .await?;
        task.status = TaskStatus::InProgress;
        tx.commit().await?;
        Ok(Some(ClaimedTask {
            attempt_id,
            lease_token,
            task,
            project,
            interest,
            expires_at,
        }))
    }

    pub async fn complete_attempt(
        &self,
        attempt_id: &str,
        lease_token: &str,
        result: &ExecutionResult,
        now: i64,
    ) -> Result<TaskResult> {
        result.validate().map_err(LabError::Invalid)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let task_id: String = sqlx::query_scalar("SELECT a.task_id FROM lab_attempts a JOIN lab_tasks t ON t.id = a.task_id WHERE a.id = ? AND a.lease_token = ? AND a.status = 'running' AND a.expires_at > ? AND a.claimed_at <= ? AND t.status = 'in_progress'").bind(attempt_id).bind(lease_token).bind(now).bind(now).fetch_optional(&mut *tx).await?.ok_or(LabError::StaleAttempt)?;
        sqlx::query("UPDATE lab_attempts SET status = 'submitted', completed_at = ?, output = ?, evidence = ?, uncertainties = ? WHERE id = ?").bind(now).bind(&result.output).bind(&result.evidence).bind(&result.uncertainties).bind(attempt_id).execute(&mut *tx).await?;
        sqlx::query("UPDATE lab_tasks SET status = 'awaiting_review', updated_at = ? WHERE id = ?")
            .bind(now)
            .bind(&task_id)
            .execute(&mut *tx)
            .await?;
        for artifact in &result.artifacts {
            sqlx::query("INSERT INTO lab_artifacts(id, task_id, attempt_id, title, kind, content, source, provenance, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, 'worker_reported_unverified', ?)")
                .bind(Uuid::new_v4().to_string())
                .bind(&task_id)
                .bind(attempt_id)
                .bind(&artifact.title)
                .bind(&artifact.kind)
                .bind(&artifact.content)
                .bind(&artifact.source)
                .bind(now)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(TaskResult {
            task_id,
            attempt_id: attempt_id.to_owned(),
        })
    }

    pub async fn fail_attempt(
        &self,
        attempt_id: &str,
        lease_token: &str,
        error: &str,
        now: i64,
    ) -> Result<TaskResult> {
        nonempty(error)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let task_id: String = sqlx::query_scalar("SELECT a.task_id FROM lab_attempts a JOIN lab_tasks t ON t.id = a.task_id WHERE a.id = ? AND a.lease_token = ? AND a.status = 'running' AND a.expires_at > ? AND a.claimed_at <= ? AND t.status = 'in_progress'")
            .bind(attempt_id).bind(lease_token).bind(now).bind(now).fetch_optional(&mut *tx).await?.ok_or(LabError::StaleAttempt)?;
        sqlx::query("UPDATE lab_attempts SET status = 'failed', completed_at = ?, feedback = ? WHERE id = ?")
            .bind(now).bind(error).bind(attempt_id).execute(&mut *tx).await?;
        sqlx::query("UPDATE lab_tasks SET status = 'blocked', updated_at = ? WHERE id = ?")
            .bind(now)
            .bind(&task_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(TaskResult {
            task_id,
            attempt_id: attempt_id.to_owned(),
        })
    }

    pub async fn attempts(&self, lab_id: &str) -> Result<Vec<Attempt>> {
        Ok(sqlx::query_as("SELECT a.* FROM lab_attempts a JOIN lab_tasks t ON t.id = a.task_id JOIN lab_projects p ON p.id = t.project_id WHERE p.lab_id = ? ORDER BY a.claimed_at, a.id")
            .bind(lab_id).fetch_all(&self.pool).await?)
    }

    pub async fn review_result(
        &self,
        attempt_id: &str,
        accepted: bool,
        actor: &str,
        rationale: &str,
        now: i64,
    ) -> Result<()> {
        validate_decision(actor, rationale)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let row = sqlx::query("SELECT a.task_id, p.lab_id FROM lab_attempts a JOIN lab_tasks t ON t.id = a.task_id JOIN lab_projects p ON p.id = t.project_id WHERE a.id = ? AND a.status = 'submitted' AND t.status = 'awaiting_review'").bind(attempt_id).fetch_optional(&mut *tx).await?.ok_or(LabError::StaleAttempt)?;
        let task_id: String = row.try_get("task_id")?;
        let lab_id: String = row.try_get("lab_id")?;
        sqlx::query("UPDATE lab_attempts SET status = ?, accepted = ?, feedback = ? WHERE id = ?")
            .bind(if accepted { "accepted" } else { "rejected" })
            .bind(accepted)
            .bind(rationale)
            .bind(attempt_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE lab_tasks SET status = ?, updated_at = ? WHERE id = ?")
            .bind(if accepted { "completed" } else { "ready" })
            .bind(now)
            .bind(&task_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO lab_decisions(id, lab_id, task_id, kind, actor, rationale, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)").bind(Uuid::new_v4().to_string()).bind(lab_id).bind(task_id).bind(if accepted { "accept_result" } else { "reject_result" }).bind(actor).bind(rationale).bind(now).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn artifacts_for_task(&self, task_id: &str) -> Result<Vec<crate::Artifact>> {
        Ok(
            sqlx::query_as("SELECT * FROM lab_artifacts WHERE task_id = ? ORDER BY created_at, id")
                .bind(task_id)
                .fetch_all(&self.pool)
                .await?,
        )
    }

    pub async fn get_artifact(&self, id: &str) -> Result<Option<crate::Artifact>> {
        Ok(sqlx::query_as("SELECT * FROM lab_artifacts WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?)
    }

    pub async fn attempts_for_task(&self, task_id: &str) -> Result<Vec<Attempt>> {
        Ok(
            sqlx::query_as("SELECT * FROM lab_attempts WHERE task_id = ? ORDER BY claimed_at, id")
                .bind(task_id)
                .fetch_all(&self.pool)
                .await?,
        )
    }
}

async fn recover(tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>, now: i64) -> Result<u64> {
    let count = sqlx::query("UPDATE lab_attempts SET status = 'expired', completed_at = ? WHERE status = 'running' AND expires_at <= ?").bind(now).bind(now).execute(&mut **tx).await?.rows_affected();
    sqlx::query("UPDATE lab_tasks SET status = 'ready', updated_at = ? WHERE status = 'in_progress' AND NOT EXISTS (SELECT 1 FROM lab_attempts WHERE task_id = lab_tasks.id AND status = 'running')").bind(now).execute(&mut **tx).await?;
    Ok(count)
}

fn proposal_from_row(row: sqlx::sqlite::SqliteRow) -> Result<PlanningProposal> {
    Ok(PlanningProposal {
        id: row.try_get("id")?,
        lab_id: row.try_get("lab_id")?,
        interest_revision: row.try_get("interest_revision")?,
        spec: serde_json::from_str(row.try_get("spec_json")?)
            .map_err(|error| LabError::Invalid(error.to_string()))?,
        status: row.try_get("status")?,
        created_at: row.try_get("created_at")?,
    })
}

async fn validate_proposal(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    lab_id: &str,
    spec: &PlanningProposalSpec,
) -> Result<()> {
    use std::collections::{HashMap, HashSet, VecDeque};
    let invalid = |message: &str| LabError::Invalid(message.into());
    if spec.projects.is_empty() && spec.tasks.is_empty() {
        return Err(invalid("proposal must contain projects or tasks"));
    }
    if serde_json::to_vec(spec)
        .map_err(|error| invalid(&error.to_string()))?
        .len()
        > 1_048_576
    {
        return Err(invalid("proposal exceeds 1 MiB"));
    }
    let mut projects = HashSet::new();
    for project in &spec.projects {
        for text in [&project.id, &project.name, &project.question] {
            nonempty(text)?;
        }
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM lab_projects WHERE id = ?)")
                .bind(&project.id)
                .fetch_one(&mut **tx)
                .await?;
        if !projects.insert(project.id.clone()) || exists {
            return Err(invalid("duplicate or existing project id"));
        }
    }
    let mut tasks = HashMap::new();
    for task in &spec.tasks {
        for text in [
            &task.id,
            &task.project_id,
            &task.objective,
            &task.context,
            &task.deliverable,
            &task.completion_criteria,
        ] {
            nonempty(text)?;
        }
        if let Some(owner) = &task.owner {
            nonempty(owner)?;
        }
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM lab_tasks WHERE id = ?)")
                .bind(&task.id)
                .fetch_one(&mut **tx)
                .await?;
        if tasks
            .insert(task.id.clone(), task.project_id.clone())
            .is_some()
            || exists
        {
            return Err(invalid("duplicate or existing task id"));
        }
        if !projects.contains(&task.project_id) {
            let valid: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM lab_projects WHERE id = ? AND lab_id = ? AND status != 'cancelled')")
                .bind(&task.project_id).bind(lab_id).fetch_one(&mut **tx).await?;
            if !valid {
                return Err(invalid("unknown, foreign-lab or cancelled project"));
            }
        }
    }
    let mut edges = HashSet::new();
    let mut graph: HashMap<String, Vec<String>> = HashMap::new();
    let mut degrees: HashMap<String, usize> = tasks.keys().map(|id| (id.clone(), 0)).collect();
    for edge in &spec.dependencies {
        let project = tasks
            .get(&edge.task_id)
            .ok_or_else(|| invalid("dependency source must be a proposed task"))?;
        if !edges.insert((&edge.task_id, &edge.depends_on)) {
            return Err(invalid("duplicate dependency"));
        }
        let target_project = if let Some(project) = tasks.get(&edge.depends_on) {
            project.clone()
        } else {
            sqlx::query_scalar::<_, String>("SELECT t.project_id FROM lab_tasks t JOIN lab_projects p ON p.id = t.project_id WHERE t.id = ? AND p.lab_id = ?")
                .bind(&edge.depends_on).bind(lab_id).fetch_optional(&mut **tx).await?
                .ok_or_else(|| invalid("missing dependency reference"))?
        };
        if *project != target_project {
            return Err(invalid("cross-project dependency"));
        }
        if tasks.contains_key(&edge.depends_on) {
            graph
                .entry(edge.task_id.clone())
                .or_default()
                .push(edge.depends_on.clone());
            *degrees.get_mut(&edge.depends_on).unwrap() += 1;
        }
    }
    let mut ready: VecDeque<String> = degrees
        .iter()
        .filter(|(_, n)| **n == 0)
        .map(|(id, _)| id.clone())
        .collect();
    let mut visited = 0;
    while let Some(id) = ready.pop_front() {
        visited += 1;
        if let Some(targets) = graph.get(&id) {
            for target in targets {
                let degree = degrees.get_mut(target).unwrap();
                *degree -= 1;
                if *degree == 0 {
                    ready.push_back(target.clone());
                }
            }
        }
    }
    if visited != tasks.len() {
        return Err(invalid("dependency cycle"));
    }
    Ok(())
}

fn nonempty(text: &str) -> Result<()> {
    if text.trim().is_empty() {
        return Err(LabError::Invalid("required text is empty".into()));
    }
    Ok(())
}

fn validate_decision(actor: &str, rationale: &str) -> Result<()> {
    nonempty(actor)?;
    nonempty(rationale)
}
