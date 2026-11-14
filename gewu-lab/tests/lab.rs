use async_trait::async_trait;
use gewu_lab::{
    ArtifactInput, ClaimedTask, Coordinator, DependencySpec, ExecutionResult, Executor, LabStore,
    PlanningProposalSpec, ProjectSpec, RunOutcome, TaskSpec,
};
use std::sync::Arc;
use tempfile::NamedTempFile;
use tokio::task::JoinSet;

struct FakeProvider {
    events: Vec<gewu_core::ProviderEvent>,
}

#[async_trait]
impl gewu_core::LlmProvider for FakeProvider {
    fn build(
        &self,
        messages: &[gewu_core::Message],
        tools: &[Arc<dyn gewu_core::Tool>],
    ) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
        assert!(tools.is_empty());
        let text = serde_json::to_string(messages).unwrap();
        assert!(text.contains("criteria"));
        assert!(text.contains("interest"));
        assert!(!text.contains("lease_token"));
        Ok(serde_json::json!({}))
    }

    async fn send<'a>(
        &'a self,
        _: serde_json::Value,
    ) -> Result<
        futures::stream::BoxStream<
            'a,
            Result<gewu_core::ProviderEvent, Box<dyn std::error::Error + Send + Sync>>,
        >,
        Box<dyn std::error::Error + Send + Sync>,
    > {
        Ok(Box::pin(futures::stream::iter(
            self.events.clone().into_iter().map(Ok),
        )))
    }

    async fn list_models(&self) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
        Ok(vec![])
    }
}

#[tokio::test]
async fn enabled_research_executes_real_agent_tools() {
    struct ResearchProvider;
    #[async_trait]
    impl gewu_core::LlmProvider for ResearchProvider {
        fn build(
            &self,
            messages: &[gewu_core::Message],
            tools: &[Arc<dyn gewu_core::Tool>],
        ) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
            assert_eq!(
                tools.iter().map(|tool| tool.name()).collect::<Vec<_>>(),
                ["read_file", "list", "glob", "grep"]
            );
            let observed = messages.iter().any(|message| matches!(message, gewu_core::Message::Tool { content, .. } if content == "observed evidence"));
            Ok(serde_json::json!({"observed": observed}))
        }
        async fn send<'a>(
            &'a self,
            request: serde_json::Value,
        ) -> Result<
            futures::stream::BoxStream<
                'a,
                Result<gewu_core::ProviderEvent, Box<dyn std::error::Error + Send + Sync>>,
            >,
            Box<dyn std::error::Error + Send + Sync>,
        > {
            let events = if request["observed"] == true {
                provider_events(
                    r#"{"output":"draft","evidence":"observed evidence","uncertainties":"limited","artifacts":[{"title":"source","kind":"source","content":"observed evidence","source":"paper.txt"}]}"#,
                    "stop",
                )
            } else {
                vec![
                    gewu_core::ProviderEvent::ToolCallStart {
                        id: "read-1".into(),
                        name: "read_file".into(),
                    },
                    gewu_core::ProviderEvent::ToolCallDelta {
                        id: "read-1".into(),
                        delta: r#"{"path":"paper.txt"}"#.into(),
                    },
                    gewu_core::ProviderEvent::Finish {
                        reason: Some("tool_calls".into()),
                    },
                ]
            };
            Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
        }
        async fn list_models(
            &self,
        ) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
            Ok(vec![])
        }
    }
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("paper.txt"), "observed evidence").unwrap();
    let (store, _file) = store().await;
    store.create_task(&task("research", 1), 1).await.unwrap();
    let executor = gewu_lab::AgentExecutor::new(Arc::new(ResearchProvider))
        .with_research(gewu_lab::ResearchConfig {
            enabled: true,
            project_root: Some(root.path().into()),
        })
        .unwrap();
    let coordinator = Coordinator::with_clock(
        store.clone(),
        Arc::new(executor),
        FixedClock(3),
        "worker",
        10,
    );
    assert!(matches!(
        coordinator.run_once_at(2).await.unwrap(),
        RunOutcome::Completed(_)
    ));
    let artifacts = store.artifacts_for_task("research").await.unwrap();
    assert_eq!(artifacts[0].source.as_deref(), Some("paper.txt"));
    assert_eq!(artifacts[0].provenance, "worker_reported_unverified");
}

struct FixedClock(i64);
impl gewu_lab::Clock for FixedClock {
    fn now(&self) -> i64 {
        self.0
    }
}

fn provider_events(text: &str, reason: &str) -> Vec<gewu_core::ProviderEvent> {
    vec![
        gewu_core::ProviderEvent::ContentDelta { delta: text.into() },
        gewu_core::ProviderEvent::Finish {
            reason: Some(reason.into()),
        },
    ]
}

#[tokio::test]
async fn real_executor_submits_only_valid_complete_drafts() {
    for (text, reason, success) in [
        (
            r#"{"output":"draft","evidence":"supplied","uncertainties":"unverified"}"#,
            "stop",
            true,
        ),
        ("not JSON", "stop", false),
        (
            r#"{"output":"draft","evidence":"supplied","uncertainties":"unverified"}"#,
            "length",
            false,
        ),
        (
            r#"{"output":"draft","evidence":"","uncertainties":"unverified"}"#,
            "stop",
            false,
        ),
    ] {
        let (store, _file) = store().await;
        store.create_task(&task("t", 1), 1).await.unwrap();
        let executor = gewu_lab::AgentExecutor::new(Arc::new(FakeProvider {
            events: provider_events(text, reason),
        }));
        let coordinator = Coordinator::with_clock(
            store.clone(),
            Arc::new(executor),
            FixedClock(5),
            "worker",
            10,
        )
        .scoped_to_lab("lab");
        let outcome = coordinator.run_once_at(2).await.unwrap();
        assert_eq!(matches!(outcome, RunOutcome::Completed(_)), success);
        let attempts = store.attempts_for_task("t").await.unwrap();
        assert_eq!(attempts[0].completed_at, Some(5));
        assert_eq!(
            attempts[0].status,
            if success { "submitted" } else { "failed" }
        );
        assert_eq!(
            store.get_task("t").await.unwrap().unwrap().status,
            if success {
                gewu_lab::TaskStatus::AwaitingReview
            } else {
                gewu_lab::TaskStatus::Blocked
            }
        );
    }
}

#[tokio::test]
async fn lab_scope_owner_and_expired_execution_are_safe() {
    let (store, _file) = store().await;
    store.create_task(&task("t", 1), 1).await.unwrap();
    assert!(
        store
            .claim_next_for_lab(Some("other"), "worker", 2, 10)
            .await
            .unwrap()
            .is_none()
    );
    let executor = gewu_lab::AgentExecutor::new(Arc::new(FakeProvider {
        events: provider_events(
            r#"{"output":"draft","evidence":"supplied","uncertainties":"unverified"}"#,
            "stop",
        ),
    }));
    let coordinator = Coordinator::with_clock(
        store.clone(),
        Arc::new(executor),
        FixedClock(12),
        "worker",
        10,
    )
    .scoped_to_lab("lab");
    assert!(matches!(
        coordinator.run_once_at(2).await,
        Err(gewu_lab::LabError::StaleAttempt)
    ));
    assert_eq!(store.recover_expired(12).await.unwrap(), 1);
    let replacement = store
        .claim_next("replacement", 13, 10)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        store
            .fail_attempt(&replacement.attempt_id, "wrong", "failure", 14)
            .await,
        Err(gewu_lab::LabError::StaleAttempt)
    ));
    store.cancel_task("t", "human", "stop", 14).await.unwrap();
    assert!(matches!(
        store
            .fail_attempt(
                &replacement.attempt_id,
                &replacement.lease_token,
                "failure",
                15
            )
            .await,
        Err(gewu_lab::LabError::StaleAttempt)
    ));
}

#[tokio::test]
async fn timeout_is_failed_and_blocked() {
    struct SlowExecutor;
    #[async_trait]
    impl Executor for SlowExecutor {
        async fn execute(&self, _: ClaimedTask) -> ExecutionResult {
            std::future::pending().await
        }
    }
    let (store, _file) = store().await;
    store.create_task(&task("t", 1), 1).await.unwrap();
    let coordinator = Coordinator::with_clock(
        store.clone(),
        Arc::new(SlowExecutor),
        FixedClock(3),
        "worker",
        10,
    )
    .with_timeout(std::time::Duration::from_millis(1));
    assert!(matches!(
        coordinator.run_once_at(2).await.unwrap(),
        RunOutcome::Failed(_)
    ));
    let attempts = store.attempts_for_task("t").await.unwrap();
    assert_eq!(attempts[0].feedback.as_deref(), Some("execution timed out"));
    assert_eq!(attempts[0].output, None);
}

async fn store() -> (LabStore, NamedTempFile) {
    let file = NamedTempFile::new().unwrap();
    let store = LabStore::connect(&format!("sqlite:{}", file.path().display()))
        .await
        .unwrap();
    store.create_lab("lab", "Lab", "interest", 1).await.unwrap();
    store
        .create_project("project", "lab", "Project", "question", 1)
        .await
        .unwrap();
    store
        .approve_project("project", "human", "approved", 2)
        .await
        .unwrap();
    (store, file)
}

fn task(id: &str, priority: i64) -> TaskSpec {
    TaskSpec {
        id: id.into(),
        project_id: "project".into(),
        objective: id.into(),
        context: "context".into(),
        deliverable: "deliverable".into(),
        completion_criteria: "criteria".into(),
        owner: None,
        priority,
    }
}

async fn claim(store: &LabStore, id: &str) -> gewu_lab::ClaimedTask {
    store.create_task(&task(id, 1), 1).await.unwrap();
    store.claim_next("worker", 2, 10).await.unwrap().unwrap()
}

#[tokio::test]
async fn dependency_eligibility_and_priority_override() {
    let (store, _file) = store().await;
    store.create_task(&task("first", 1), 1).await.unwrap();
    store.create_task(&task("second", 4), 1).await.unwrap();
    store.add_dependency("second", "first").await.unwrap();
    assert_eq!(
        store
            .claim_next("worker", 2, 10)
            .await
            .unwrap()
            .unwrap()
            .task
            .id,
        "first"
    );
    store
        .complete_attempt(
            "does-not-matter",
            "does-not-matter",
            &ExecutionResult {
                output: "".into(),
                evidence: "".into(),
                uncertainties: "unknown".into(),
                artifacts: vec![],
            },
            3,
        )
        .await
        .err();
    store
        .review_result("missing", true, "human", "missing", 3)
        .await
        .err();
    store.create_task(&task("third", 1), 1).await.unwrap();
    store
        .override_priority("third", 9, "human", "urgent", 4)
        .await
        .unwrap();
    let second = store.claim_next("worker", 5, 10).await.unwrap().unwrap();
    assert_eq!(second.task.id, "third");
}

#[tokio::test]
async fn atomic_duplicate_claim_prevention() {
    let (store, _file) = store().await;
    store.create_task(&task("only", 1), 1).await.unwrap();
    let mut jobs = JoinSet::new();
    for worker in ["a", "b"] {
        let store = store.clone();
        jobs.spawn(async move { store.claim_next(worker, 2, 10).await.unwrap() });
    }
    let mut claims = Vec::new();
    while let Some(result) = jobs.join_next().await {
        if let Some(claim) = result.unwrap() {
            claims.push(claim);
        }
    }
    assert_eq!(claims.len(), 1);
}

#[tokio::test]
async fn pause_cancel_and_result_review_are_durable() {
    let (store, _file) = store().await;
    store.create_task(&task("paused", 1), 1).await.unwrap();
    store
        .set_paused("lab", true, "human", "pause", 2)
        .await
        .unwrap();
    assert!(store.claim_next("worker", 3, 10).await.unwrap().is_none());
    store
        .set_paused("lab", false, "human", "resume", 4)
        .await
        .unwrap();
    let claim = store.claim_next("worker", 5, 10).await.unwrap().unwrap();
    store
        .complete_attempt(
            &claim.attempt_id,
            &claim.lease_token,
            &ExecutionResult {
                output: "draft".into(),
                evidence: "evidence".into(),
                uncertainties: "unknown".into(),
                artifacts: vec![],
            },
            6,
        )
        .await
        .unwrap();
    store
        .review_result(&claim.attempt_id, false, "human", "retry", 7)
        .await
        .unwrap();
    assert_eq!(
        store.get_task("paused").await.unwrap().unwrap().status,
        gewu_lab::TaskStatus::Ready
    );
    let retry = store.claim_next("worker", 8, 10).await.unwrap().unwrap();
    store
        .complete_attempt(
            &retry.attempt_id,
            &retry.lease_token,
            &ExecutionResult {
                output: "final".into(),
                evidence: "proof".into(),
                uncertainties: "none".into(),
                artifacts: vec![],
            },
            9,
        )
        .await
        .unwrap();
    store
        .review_result(&retry.attempt_id, true, "human", "accepted", 10)
        .await
        .unwrap();
    assert_eq!(
        store.get_task("paused").await.unwrap().unwrap().status,
        gewu_lab::TaskStatus::Completed
    );
    store.create_task(&task("cancelled", 1), 11).await.unwrap();
    store
        .cancel_task("cancelled", "human", "out of scope", 12)
        .await
        .unwrap();
    assert_eq!(
        store.get_task("cancelled").await.unwrap().unwrap().status,
        gewu_lab::TaskStatus::Cancelled
    );
}

#[tokio::test]
async fn restart_expiry_and_stale_completion_are_safe() {
    let (store, _file) = store().await;
    let claim = claim(&store, "recover").await;
    assert_eq!(store.recover_expired(13).await.unwrap(), 1);
    assert_eq!(store.recover_expired(20).await.unwrap(), 0);
    let replacement = store.claim_next("worker-2", 21, 10).await.unwrap().unwrap();
    assert!(
        store
            .complete_attempt(
                &claim.attempt_id,
                &claim.lease_token,
                &ExecutionResult {
                    output: "stale".into(),
                    evidence: "".into(),
                    uncertainties: "unknown".into(),
                    artifacts: vec![]
                },
                22
            )
            .await
            .is_err()
    );
    store
        .complete_attempt(
            &replacement.attempt_id,
            &replacement.lease_token,
            &ExecutionResult {
                output: "fresh".into(),
                evidence: "".into(),
                uncertainties: "unknown".into(),
                artifacts: vec![],
            },
            22,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn interest_revisions_and_single_worker_coordinator() {
    let (store, _file) = store().await;
    assert_eq!(
        store
            .revise_interest("lab", "updated", "new direction", 2)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        store
            .interest_revision("lab", 1)
            .await
            .unwrap()
            .unwrap()
            .interest,
        "interest"
    );
    store.create_task(&task("coordinated", 1), 3).await.unwrap();
    struct TestExecutor;
    #[async_trait]
    impl Executor for TestExecutor {
        async fn execute(&self, _task: ClaimedTask) -> ExecutionResult {
            ExecutionResult {
                output: "output".into(),
                evidence: "evidence".into(),
                uncertainties: "none".into(),
                artifacts: vec![],
            }
        }
    }
    struct TestClock;
    impl gewu_lab::Clock for TestClock {
        fn now(&self) -> i64 {
            5
        }
    }
    let coordinator =
        Coordinator::with_clock(store, Arc::new(TestExecutor), TestClock, "worker", 10);
    assert!(matches!(
        coordinator.run_once_at(4).await.unwrap(),
        RunOutcome::Completed(_)
    ));
    assert!(matches!(
        coordinator.run_once_at(5).await.unwrap(),
        RunOutcome::Idle
    ));
}
#[tokio::test]
async fn artifacts_are_atomic_and_survive_reopen() {
    let (store, file) = store().await;
    let claim = claim(&store, "artifact").await;
    let mut result = ExecutionResult {
        output: "output".into(),
        evidence: "evidence".into(),
        uncertainties: "none".into(),
        artifacts: vec![ArtifactInput {
            title: "result".into(),
            kind: "note".into(),
            content: "finding".into(),
            source: Some("notes/result.txt".into()),
        }],
    };
    assert!(
        store
            .complete_attempt(&claim.attempt_id, "wrong", &result, 3)
            .await
            .is_err()
    );
    assert!(
        store
            .artifacts_for_task("artifact")
            .await
            .unwrap()
            .is_empty()
    );
    result.artifacts[0].content = "x".repeat(65_537);
    assert!(
        store
            .complete_attempt(&claim.attempt_id, &claim.lease_token, &result, 3)
            .await
            .is_err()
    );
    assert_eq!(
        store.attempts_for_task("artifact").await.unwrap()[0].status,
        "running"
    );
    result.artifacts[0].content = "finding".into();
    store
        .complete_attempt(&claim.attempt_id, &claim.lease_token, &result, 3)
        .await
        .unwrap();
    assert!(
        store
            .complete_attempt(&claim.attempt_id, &claim.lease_token, &result, 4)
            .await
            .is_err()
    );
    store.pool().close().await;
    let reopened = LabStore::connect(&format!("sqlite:{}", file.path().display()))
        .await
        .unwrap();
    let artifacts = reopened.artifacts_for_task("artifact").await.unwrap();
    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].provenance, "worker_reported_unverified");
    assert_eq!(artifacts[0].attempt_id, claim.attempt_id);
    assert_eq!(artifacts[0].task_id, "artifact");
    assert_eq!(artifacts[0].created_at, 3);
    assert_eq!(
        reopened.get_artifact(&artifacts[0].id).await.unwrap(),
        Some(artifacts[0].clone())
    );
    assert!(
        !serde_json::to_string(&artifacts)
            .unwrap()
            .contains(&claim.lease_token)
    );
}

#[test]
fn artifact_limits_and_sources_are_validated() {
    let mut artifact = ArtifactInput {
        title: "x".into(),
        kind: "note".into(),
        content: "x".repeat(65_536),
        source: None,
    };
    assert!(artifact.validate().is_ok());
    artifact.content.push('x');
    assert!(artifact.validate().is_err());
    artifact.content = "api_key=secret".into();
    assert!(artifact.validate().is_err());
    artifact.content = "finding".into();
    for source in [
        "../outside",
        "/etc/passwd",
        ".env",
        "https://user:secret@example.org",
        "https://example.org?token=secret",
    ] {
        artifact.source = Some(source.into());
        assert!(artifact.validate().is_err());
    }
    artifact.source = None;
    let mut result = ExecutionResult {
        output: "o".into(),
        evidence: "e".into(),
        uncertainties: "u".into(),
        artifacts: vec![artifact; 17],
    };
    assert!(result.validate().is_err());
    result.artifacts.clear();
    result.output = "x".repeat(1_048_576);
    assert!(result.validate().is_err());
    assert!(serde_json::from_str::<ExecutionResult>(r#"{"output":"o","evidence":"e","uncertainties":"u","artifacts":[{"title":"t","kind":"note","content":"c","unexpected":1}]}"#).is_err());
}

fn proposal() -> PlanningProposalSpec {
    let mut first = task("first", 1);
    first.project_id = "planned".into();
    let mut second = task("second", 9);
    second.project_id = "planned".into();
    PlanningProposalSpec {
        projects: vec![ProjectSpec {
            id: "planned".into(),
            name: "Planned".into(),
            question: "Why?".into(),
        }],
        tasks: vec![first, second],
        dependencies: vec![DependencySpec {
            task_id: "second".into(),
            depends_on: "first".into(),
        }],
    }
}

#[test]
fn proposal_cli_accepts_stdin_and_files_and_audits_decisions() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("lab.db");
    let run = |args: &[&str], input: Option<&str>| {
        let mut child = Command::new(env!("CARGO_BIN_EXE_gewu-lab"))
            .arg("--db-path")
            .arg(&db)
            .arg("--json")
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if let Some(input) = input {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.as_bytes())
                .unwrap();
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };
    run(
        &[
            "init",
            "--id",
            "lab",
            "--name",
            "Lab",
            "--interest",
            "science",
        ],
        None,
    );
    let json = serde_json::to_string(&proposal()).unwrap();
    assert_eq!(
        run(
            &["proposal", "create", "--lab-id", "lab", "--id", "stdin"],
            Some(&json)
        )["proposal"]["status"],
        "proposed"
    );
    assert_eq!(
        run(&["proposal", "list", "--lab-id", "lab"], None)["proposals"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    run(
        &[
            "proposal",
            "reject",
            "--id",
            "stdin",
            "--actor",
            "reviewer",
            "--rationale",
            "not yet",
        ],
        None,
    );
    assert_eq!(
        run(&["proposal", "inspect", "--id", "stdin"], None)["decisions"][0]["actor"],
        "reviewer"
    );
    let file = dir.path().join("plan with spaces.json");
    std::fs::write(&file, json).unwrap();
    run(
        &[
            "proposal",
            "create",
            "--lab-id",
            "lab",
            "--id",
            "file",
            "--file",
            file.to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(
        run(
            &[
                "proposal",
                "approve",
                "--id",
                "file",
                "--rationale",
                "reviewed"
            ],
            None
        )["proposal"]["status"],
        "approved"
    );
}

#[tokio::test]
async fn proposals_are_durable_and_never_queued_before_approval() {
    let (store, file) = store().await;
    let spec = proposal();
    store
        .create_proposal("plan", "lab", &spec, 3)
        .await
        .unwrap();
    assert!(store.get_task("first").await.unwrap().is_none());
    assert!(store.get_project("planned").await.unwrap().is_none());
    assert!(store.claim_next("worker", 4, 10).await.unwrap().is_none());
    assert!(store.attempts("lab").await.unwrap().is_empty());
    store.pool().close().await;
    let store = LabStore::connect(&format!("sqlite:{}", file.path().display()))
        .await
        .unwrap();
    assert_eq!(store.proposals("lab").await.unwrap()[0].spec, spec);
    store
        .revise_interest("lab", "changed", "new focus", 4)
        .await
        .unwrap();
    store
        .approve_proposal("plan", "human", "reviewed", 5)
        .await
        .unwrap();
    let project = store.get_project("planned").await.unwrap().unwrap();
    assert_eq!(project.status, gewu_lab::ProjectStatus::Active);
    assert_eq!(project.interest_revision, 1);
    assert_eq!(
        store.get_proposal("plan").await.unwrap().unwrap().status,
        gewu_lab::ProposalStatus::Approved
    );
    assert_eq!(
        store
            .claim_next("worker", 6, 10)
            .await
            .unwrap()
            .unwrap()
            .task
            .id,
        "first"
    );
    assert!(store.claim_next("worker", 6, 10).await.unwrap().is_none());
    assert!(
        store
            .approve_proposal("plan", "human", "again", 7)
            .await
            .is_err()
    );
    assert!(
        store
            .reject_proposal("plan", "human", "again", 7)
            .await
            .is_err()
    );
    let audit = store.proposal_decisions("plan").await.unwrap();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].actor, "human");
    assert_eq!(audit[0].rationale, "reviewed");
    assert_eq!(audit[0].created_at, 5);
    assert!(
        store
            .decisions("lab")
            .await
            .unwrap()
            .iter()
            .any(|d| d.id == audit[0].id && d.kind == "approve_proposal")
    );
}

#[tokio::test]
async fn invalid_proposals_are_rejected_before_persistence() {
    let (store, _file) = store().await;
    store
        .create_lab("other", "Other", "other", 1)
        .await
        .unwrap();
    store
        .create_project("foreign", "other", "Foreign", "why", 1)
        .await
        .unwrap();
    store.create_task(&task("existing", 1), 1).await.unwrap();
    let mut cases = Vec::new();
    let base = proposal();
    let mut json = serde_json::to_value(&base).unwrap();
    for (section, fields) in [
        ("projects", vec!["id", "name", "question"]),
        (
            "tasks",
            vec![
                "id",
                "project_id",
                "objective",
                "context",
                "deliverable",
                "completion_criteria",
            ],
        ),
    ] {
        for field in fields {
            let mut invalid = json.clone();
            invalid[section][0][field] = serde_json::json!(" ");
            cases.push(serde_json::from_value(invalid).unwrap());
        }
    }
    for field in ["priority", "objective", "context", "project_id"] {
        let mut missing = json.clone();
        missing["tasks"][0].as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<PlanningProposalSpec>(missing).is_err());
    }
    json["tasks"][0]["unexpected"] = serde_json::json!(true);
    assert!(serde_json::from_value::<PlanningProposalSpec>(json).is_err());
    let mut invalid = base.clone();
    invalid.tasks[0].owner = Some(" ".into());
    cases.push(invalid);
    let mut invalid = base.clone();
    invalid.tasks.push(invalid.tasks[0].clone());
    cases.push(invalid);
    let mut invalid = base.clone();
    invalid.projects.push(invalid.projects[0].clone());
    cases.push(invalid);
    for project in ["unknown", "foreign"] {
        let mut invalid = base.clone();
        invalid.tasks[0].project_id = project.into();
        cases.push(invalid);
    }
    let mut invalid = base.clone();
    invalid.tasks[0].id = "existing".into();
    cases.push(invalid);
    let mut invalid = base.clone();
    invalid.projects[0].id = "project".into();
    cases.push(invalid);
    for (source, target) in [
        ("second", "missing"),
        ("missing", "first"),
        ("first", "first"),
    ] {
        let mut invalid = base.clone();
        invalid.dependencies = vec![DependencySpec {
            task_id: source.into(),
            depends_on: target.into(),
        }];
        cases.push(invalid);
    }
    let mut invalid = base.clone();
    invalid.dependencies.push(DependencySpec {
        task_id: "first".into(),
        depends_on: "second".into(),
    });
    cases.push(invalid);
    let mut invalid = base.clone();
    invalid.dependencies.push(invalid.dependencies[0].clone());
    cases.push(invalid);
    let mut invalid = base.clone();
    invalid.tasks[0].project_id = "project".into();
    cases.push(invalid);
    cases.push(PlanningProposalSpec {
        projects: vec![],
        tasks: vec![],
        dependencies: vec![],
    });
    for invalid in cases {
        assert!(
            store
                .create_proposal("bad", "lab", &invalid, 2)
                .await
                .is_err(),
            "accepted {invalid:?}"
        );
        assert!(store.get_proposal("bad").await.unwrap().is_none());
    }
    assert!(store.create_proposal(" ", "lab", &base, 2).await.is_err());
    assert!(
        store
            .create_proposal("bad", "unknown", &base, 2)
            .await
            .is_err()
    );
    assert!(store.get_task("first").await.unwrap().is_none());
}

#[tokio::test]
async fn approval_rolls_back_materialization_and_decisions_on_database_failure() {
    let (store, _file) = store().await;
    store
        .create_proposal("plan", "lab", &proposal(), 2)
        .await
        .unwrap();
    sqlx::raw_sql("CREATE TRIGGER fail_audit BEFORE INSERT ON lab_decisions WHEN NEW.kind = 'approve_proposal' BEGIN SELECT RAISE(ABORT, 'injected audit failure'); END;")
        .execute(store.pool()).await.unwrap();
    assert!(
        store
            .approve_proposal("plan", "human", "yes", 3)
            .await
            .is_err()
    );
    assert!(store.get_project("planned").await.unwrap().is_none());
    assert!(store.get_task("first").await.unwrap().is_none());
    assert!(store.get_task("second").await.unwrap().is_none());
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM lab_dependencies")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert_eq!(
        store.get_proposal("plan").await.unwrap().unwrap().status,
        gewu_lab::ProposalStatus::Proposed
    );
    assert!(store.proposal_decisions("plan").await.unwrap().is_empty());
    sqlx::raw_sql("DROP TRIGGER fail_audit")
        .execute(store.pool())
        .await
        .unwrap();
    store
        .approve_proposal("plan", "human", "yes", 4)
        .await
        .unwrap();
}

#[tokio::test]
async fn rejection_is_durable_audited_and_terminal_even_when_plan_becomes_stale() {
    let (store, file) = store().await;
    store
        .create_proposal("plan", "lab", &proposal(), 2)
        .await
        .unwrap();
    store.create_task(&task("first", 1), 3).await.unwrap();
    assert!(
        store
            .approve_proposal("plan", "human", "yes", 4)
            .await
            .is_err()
    );
    for (actor, rationale) in [(" ", "reason"), ("human", " ")] {
        assert!(
            store
                .reject_proposal("plan", actor, rationale, 4)
                .await
                .is_err()
        );
        assert!(
            store
                .approve_proposal("plan", actor, rationale, 4)
                .await
                .is_err()
        );
    }
    store
        .reject_proposal("plan", "reviewer", "not aligned", 5)
        .await
        .unwrap();
    store.pool().close().await;
    let store = LabStore::connect(&format!("sqlite:{}", file.path().display()))
        .await
        .unwrap();
    assert_eq!(
        store.get_proposal("plan").await.unwrap().unwrap().status,
        gewu_lab::ProposalStatus::Rejected
    );
    assert_eq!(
        store.proposal_decisions("plan").await.unwrap()[0].rationale,
        "not aligned"
    );
    assert_eq!(
        store.proposal_decisions("plan").await.unwrap()[0].actor,
        "reviewer"
    );
    assert!(store.get_project("planned").await.unwrap().is_none());
    assert!(store.get_task("second").await.unwrap().is_none());
    assert!(
        store
            .approve_proposal("plan", "human", "again", 6)
            .await
            .is_err()
    );
    assert!(
        store
            .reject_proposal("plan", "human", "again", 6)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM lab_proposal_decisions")
            .execute(store.pool())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn concurrent_approval_materializes_once_and_supports_existing_dependencies() {
    let (store, _file) = store().await;
    store.create_task(&task("existing", 1), 1).await.unwrap();
    let spec = PlanningProposalSpec {
        projects: vec![],
        tasks: vec![task("followup", 9)],
        dependencies: vec![DependencySpec {
            task_id: "followup".into(),
            depends_on: "existing".into(),
        }],
    };
    store
        .create_proposal("plan", "lab", &spec, 2)
        .await
        .unwrap();
    let (a, b) = tokio::join!(
        store.approve_proposal("plan", "a", "yes", 3),
        store.approve_proposal("plan", "b", "yes", 3)
    );
    assert_ne!(a.is_ok(), b.is_ok());
    assert_eq!(store.proposal_decisions("plan").await.unwrap().len(), 1);
    assert_eq!(
        store
            .claim_next("worker", 4, 10)
            .await
            .unwrap()
            .unwrap()
            .task
            .id,
        "existing"
    );
    assert!(store.claim_next("worker", 4, 10).await.unwrap().is_none());
}
