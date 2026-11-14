use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, sqlx::Type)]
#[sqlx(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Ready,
    InProgress,
    AwaitingReview,
    Blocked,
    Completed,
    Cancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, sqlx::Type)]
#[sqlx(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum ProjectStatus {
    Proposed,
    Active,
    Cancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, sqlx::FromRow)]
pub struct Lab {
    pub id: String,
    pub name: String,
    pub paused: bool,
    pub current_interest_revision: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, sqlx::FromRow)]
pub struct InterestRevision {
    pub lab_id: String,
    pub revision: i64,
    pub interest: String,
    pub rationale: String,
    pub created_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, sqlx::FromRow)]
pub struct Project {
    pub id: String,
    pub lab_id: String,
    pub name: String,
    pub question: String,
    pub status: ProjectStatus,
    pub interest_revision: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskSpec {
    pub id: String,
    pub project_id: String,
    pub objective: String,
    pub context: String,
    pub deliverable: String,
    pub completion_criteria: String,
    pub owner: Option<String>,
    pub priority: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, sqlx::FromRow)]
pub struct Task {
    pub id: String,
    pub project_id: String,
    pub objective: String,
    pub context: String,
    pub deliverable: String,
    pub completion_criteria: String,
    pub owner: Option<String>,
    pub priority: i64,
    pub priority_override: Option<i64>,
    pub status: TaskStatus,
}

impl Task {
    pub fn effective_priority(&self) -> i64 {
        self.priority_override.unwrap_or(self.priority)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClaimedTask {
    pub attempt_id: String,
    pub lease_token: String,
    pub task: Task,
    pub project: Project,
    pub interest: InterestRevision,
    pub expires_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExecutionResult {
    pub output: String,
    pub evidence: String,
    pub uncertainties: String,
    #[serde(default)]
    pub artifacts: Vec<ArtifactInput>,
}

impl ExecutionResult {
    pub fn validate(&self) -> std::result::Result<(), String> {
        if serde_json::to_vec(self)
            .map_err(|_| "invalid result")?
            .len()
            > 1_048_576
        {
            return Err("result exceeds 1 MiB".into());
        }
        if [&self.output, &self.evidence, &self.uncertainties]
            .iter()
            .any(|text| crate::research::sensitive_text(text))
        {
            return Err("result contains potential credentials".into());
        }
        if self.artifacts.len() > 16 {
            return Err("at most 16 artifacts are allowed".into());
        }
        for artifact in &self.artifacts {
            artifact.validate()?;
        }
        Ok(())
    }

    pub fn validate_complete(&self) -> std::result::Result<(), String> {
        self.validate()?;
        if [&self.output, &self.evidence, &self.uncertainties]
            .iter()
            .any(|value| value.trim().is_empty())
        {
            return Err("output, evidence and uncertainties must all be nonempty".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArtifactInput {
    pub title: String,
    pub kind: String,
    pub content: String,
    pub source: Option<String>,
}

impl ArtifactInput {
    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.title.trim().is_empty()
            || self.title.len() > 256
            || self.kind.is_empty()
            || self.kind.len() > 64
            || !self
                .kind
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            || self.content.trim().is_empty()
            || self.content.len() > 65_536
        {
            return Err("invalid artifact title, kind or content (content limit 64 KiB)".into());
        }
        if let Some(source) = &self.source
            && (source.is_empty()
                || source.len() > 2048
                || source.chars().any(char::is_control)
                || source.contains(['@', '?', '#'])
                || (!(source.starts_with("https://") || source.starts_with("http://"))
                    && !crate::research::safe_relative(std::path::Path::new(source))))
        {
            return Err("invalid artifact source".into());
        }
        if [
            &self.title,
            &self.kind,
            &self.content,
            self.source.as_deref().unwrap_or(""),
        ]
        .iter()
        .any(|text| crate::research::sensitive_text(text))
        {
            return Err("artifact contains potential credentials".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, sqlx::FromRow)]
pub struct Artifact {
    pub id: String,
    pub task_id: String,
    pub attempt_id: String,
    pub title: String,
    pub kind: String,
    pub content: String,
    pub source: Option<String>,
    pub provenance: String,
    pub created_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, sqlx::FromRow)]
pub struct Attempt {
    pub id: String,
    pub task_id: String,
    pub worker_id: String,
    pub status: String,
    pub claimed_at: i64,
    pub expires_at: i64,
    pub completed_at: Option<i64>,
    pub output: Option<String>,
    pub evidence: Option<String>,
    pub uncertainties: Option<String>,
    pub accepted: Option<bool>,
    pub feedback: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, sqlx::FromRow)]
pub struct Decision {
    pub id: String,
    pub lab_id: String,
    pub project_id: Option<String>,
    pub task_id: Option<String>,
    pub kind: String,
    pub actor: String,
    pub rationale: String,
    pub priority: Option<i64>,
    pub created_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskResult {
    pub task_id: String,
    pub attempt_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProjectSpec {
    pub id: String,
    pub name: String,
    pub question: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DependencySpec {
    pub task_id: String,
    pub depends_on: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PlanningProposalSpec {
    pub projects: Vec<ProjectSpec>,
    pub tasks: Vec<TaskSpec>,
    pub dependencies: Vec<DependencySpec>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, sqlx::Type)]
#[sqlx(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum ProposalStatus {
    Proposed,
    Approved,
    Rejected,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanningProposal {
    pub id: String,
    pub lab_id: String,
    pub interest_revision: i64,
    pub spec: PlanningProposalSpec,
    pub status: ProposalStatus,
    pub created_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, sqlx::FromRow)]
pub struct ProposalDecision {
    pub id: String,
    pub proposal_id: String,
    pub kind: String,
    pub actor: String,
    pub rationale: String,
    pub created_at: i64,
}
