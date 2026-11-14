use crate::cli::ProposalCommand;
use crate::cli::{Cli, Command, LabCommand, ProjectCommand, ReviewCommand, RunArgs, TaskCommand};
use anyhow::{Context, Result, bail};
use gewu_lab::{AgentExecutor, Coordinator, ExecutionConfig, RunOutcome};
use gewu_lab::{Clock, Lab, LabStore, Project, SystemClock, Task, TaskSpec};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
use uuid::Uuid;

fn config_path(path: Option<PathBuf>, home: Option<PathBuf>) -> Result<PathBuf> {
    match path {
        Some(path) if !path.as_os_str().is_empty() => Ok(path),
        Some(_) => bail!("--config must not be empty"),
        None => Ok(home
            .context("cannot determine home directory; pass --config")?
            .join(".gewu-lab")
            .join("config.json")),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LabConfig {
    executor: Option<ExecutionConfig>,
}

fn load_executor(path: &Path, explicit: bool) -> Result<Option<AgentExecutor>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound && !explicit => return Ok(None),
        Err(error) => return Err(error).context("cannot read lab config"),
    };
    let config: LabConfig = serde_json::from_slice(&bytes).context("invalid lab config JSON")?;
    config.executor.map(|config| config.executor()).transpose()
}

async fn execute_once(
    store: &LabStore,
    args: &RunArgs,
    executor: Option<std::sync::Arc<AgentExecutor>>,
) -> Result<Value> {
    lab(store, &args.lab_id).await?;
    required(&args.worker_id, "worker-id")?;
    if args.timeout_seconds >= args.lease_seconds as u64 {
        bail!("timeout-seconds must be less than lease-seconds");
    }
    let Some(executor) = executor else {
        return poll(store, &args.lab_id).await;
    };
    let outcome = Coordinator::new(store.clone(), executor, &args.worker_id, args.lease_seconds)
        .scoped_to_lab(&args.lab_id)
        .with_timeout(Duration::from_secs(args.timeout_seconds))
        .run_once()
        .await?;
    Ok(match outcome {
        RunOutcome::Idle => json!({"lab_id": args.lab_id, "outcome": "idle"}),
        RunOutcome::Completed(result) => {
            json!({"lab_id": args.lab_id, "outcome": "submitted", "task_id": result.task_id, "attempt_id": result.attempt_id})
        }
        RunOutcome::Failed(result) => {
            json!({"lab_id": args.lab_id, "outcome": "failed", "task_id": result.task_id, "attempt_id": result.attempt_id})
        }
    })
}

pub fn database_path(path: Option<PathBuf>, home: Option<PathBuf>) -> Result<PathBuf> {
    match path {
        Some(path) if !path.as_os_str().is_empty() => Ok(path),
        Some(_) => bail!("--db-path must not be empty"),
        None => Ok(home
            .context("cannot determine home directory; pass --db-path")?
            .join(".gewu-lab")
            .join("lab.db")),
    }
}

async fn connect(path: &Path) -> Result<LabStore> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create database directory {}", parent.display()))?;
    }
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(10));
    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(options)
        .await
        .with_context(|| format!("cannot open database {}", path.display()))?;
    Ok(LabStore::from_pool(pool).await?)
}

fn validate_config(path: &Path) -> Result<Value> {
    let bytes = std::fs::read(path).context("cannot read lab config")?;
    let config: LabConfig =
        serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("invalid lab config JSON"))?;
    if let Some(executor) = config.executor {
        executor.validate()?;
    }
    Ok(json!({"valid": true}))
}

pub async fn run(cli: Cli) -> Result<()> {
    if matches!(cli.command, Command::ConfigValidate) {
        return emit(
            &validate_config(&config_path(cli.config, dirs::home_dir())?)?,
            cli.json,
        );
    }
    if let Command::Gui(args) = &cli.command {
        let path = database_path(cli.db_path, dirs::home_dir())?;
        let store = connect(&path).await?;
        return crate::gui::serve(store, args, cli.json).await;
    }
    let executor = if matches!(
        cli.command,
        Command::Run(_) | Command::RunOnce(_) | Command::Status(_)
    ) {
        load_executor(
            &config_path(cli.config.clone(), dirs::home_dir())?,
            cli.config.is_some(),
        )?
        .map(std::sync::Arc::new)
    } else {
        None
    };
    let path = database_path(cli.db_path, dirs::home_dir())?;
    let store = connect(&path).await?;
    if let Command::Run(args) = &cli.command {
        let shutdown = tokio::signal::ctrl_c();
        tokio::pin!(shutdown);
        loop {
            emit(
                &execute_once(&store, args, executor.clone()).await?,
                cli.json,
            )?;
            tokio::select! {
                result = &mut shutdown => { result?; break; }
                () = tokio::time::sleep(Duration::from_secs(args.interval)) => {}
            }
        }
    } else {
        let mut value = match &cli.command {
            Command::RunOnce(args) => execute_once(&store, args, executor.clone()).await?,
            _ => handle(&store, &cli.command).await?,
        };
        if matches!(cli.command, Command::Status(_)) && executor.is_some() {
            value["executor"] = json!("configured");
        }
        emit(&value, cli.json)?;
    }
    store.pool().close().await;
    Ok(())
}

fn emit(value: &Value, json_output: bool) -> Result<()> {
    let mut stdout = io::stdout().lock();
    if json_output {
        writeln!(stdout, "{value}")?;
    } else {
        write_text(&mut stdout, value, 0)?;
    }
    stdout.flush()?;
    Ok(())
}

fn write_text(output: &mut impl Write, value: &Value, indent: usize) -> io::Result<()> {
    let padding = " ".repeat(indent);
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                if value.is_object() || value.is_array() {
                    writeln!(output, "{padding}{key}:")?;
                    write_text(output, value, indent + 2)?;
                } else if let Some(text) = value.as_str() {
                    writeln!(output, "{padding}{key}: {text}")?;
                } else {
                    writeln!(output, "{padding}{key}: {value}")?;
                }
            }
        }
        Value::Array(items) => {
            if items.is_empty() {
                writeln!(output, "{padding}(none)")?;
            }
            for item in items {
                writeln!(output, "{padding}-")?;
                write_text(output, item, indent + 2)?;
            }
        }
        _ => writeln!(output, "{padding}{value}")?,
    }
    Ok(())
}

fn required(value: &str, field: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{field} must not be empty");
    }
    Ok(())
}

async fn lab(store: &LabStore, id: &str) -> Result<Lab> {
    store
        .get_lab(id)
        .await?
        .with_context(|| format!("unknown lab: {id}; use init first"))
}

async fn project(store: &LabStore, id: &str) -> Result<Project> {
    store
        .get_project(id)
        .await?
        .with_context(|| format!("unknown project: {id}"))
}

async fn task(store: &LabStore, id: &str) -> Result<Task> {
    store
        .get_task(id)
        .await?
        .with_context(|| format!("unknown task: {id}"))
}

async fn projects(store: &LabStore, lab_id: &str) -> Result<Vec<Project>> {
    Ok(
        sqlx::query_as("SELECT * FROM lab_projects WHERE lab_id = ? ORDER BY created_at, id")
            .bind(lab_id)
            .fetch_all(store.pool())
            .await?,
    )
}

async fn tasks(store: &LabStore, project_id: &str) -> Result<Vec<Task>> {
    Ok(sqlx::query_as("SELECT * FROM lab_tasks WHERE project_id = ? ORDER BY COALESCE(priority_override, priority) DESC, created_at, id")
        .bind(project_id).fetch_all(store.pool()).await?)
}

async fn poll(store: &LabStore, lab_id: &str) -> Result<Value> {
    let lab = lab(store, lab_id).await?;
    Ok(json!({
        "lab_id": lab.id,
        "outcome": if lab.paused { "idle" } else { "no_executor_configured" },
        "message": if lab.paused { "Lab is paused; no tasks claimed" } else { "No executor configured; no tasks claimed or executed" }
    }))
}

async fn handle(store: &LabStore, command: &Command) -> Result<Value> {
    let now = SystemClock.now();
    match command {
        Command::Init(args) => {
            for (value, field) in [
                (&args.id, "id"),
                (&args.name, "name"),
                (&args.interest, "interest"),
            ] {
                required(value, field)?;
            }
            let created = if let Some(existing) = store.get_lab(&args.id).await? {
                let initial = store
                    .interest_revision(&args.id, 1)
                    .await?
                    .context("missing initial interest")?;
                if existing.name != args.name || initial.interest != args.interest {
                    bail!(
                        "lab {} already exists with a different name or initial interest; no changes made",
                        args.id
                    );
                }
                false
            } else {
                store
                    .create_lab(&args.id, &args.name, &args.interest, now)
                    .await?;
                true
            };
            Ok(json!({"created": created, "lab": lab(store, &args.id).await?}))
        }
        Command::Status(args) => {
            let lab = lab(store, &args.lab_id).await?;
            let interest = store
                .interest_revision(&lab.id, lab.current_interest_revision)
                .await?;
            let counts: Vec<(String, i64)> = sqlx::query_as("SELECT t.status, COUNT(*) FROM lab_tasks t JOIN lab_projects p ON p.id = t.project_id WHERE p.lab_id = ? GROUP BY t.status ORDER BY t.status")
                .bind(&lab.id).fetch_all(store.pool()).await?;
            let counts: std::collections::BTreeMap<_, _> = counts.into_iter().collect();
            Ok(
                json!({"lab": lab, "interest": interest, "projects": projects(store, &args.lab_id).await?, "task_counts": counts, "executor": "not_configured"}),
            )
        }
        Command::Proposal { command } => match command {
            ProposalCommand::Create(args) => {
                let reader: Box<dyn Read> = match args.file.as_deref() {
                    None => Box::new(io::stdin()),
                    Some(path) if path == Path::new("-") => Box::new(io::stdin()),
                    Some(path) => {
                        Box::new(std::fs::File::open(path).context("cannot open proposal file")?)
                    }
                };
                let mut bytes = Vec::new();
                reader.take(1_048_577).read_to_end(&mut bytes)?;
                if bytes.len() > 1_048_576 {
                    bail!("proposal exceeds 1 MiB");
                }
                let spec = serde_json::from_slice(&bytes).context("invalid proposal JSON")?;
                let id = args
                    .id
                    .clone()
                    .unwrap_or_else(|| Uuid::new_v4().to_string());
                store.create_proposal(&id, &args.lab_id, &spec, now).await?;
                Ok(json!({"proposal": store.get_proposal(&id).await?}))
            }
            ProposalCommand::Inspect(args) => Ok(json!({
                "proposal": store.get_proposal(&args.id).await?.context("unknown proposal")?,
                "decisions": store.proposal_decisions(&args.id).await?
            })),
            ProposalCommand::List(args) => {
                lab(store, &args.lab_id).await?;
                Ok(json!({"proposals": store.proposals(&args.lab_id).await?}))
            }
            ProposalCommand::Approve(args) | ProposalCommand::Reject(args) => {
                if matches!(command, ProposalCommand::Approve(_)) {
                    store
                        .approve_proposal(&args.id, &args.actor, &args.rationale, now)
                        .await?;
                } else {
                    store
                        .reject_proposal(&args.id, &args.actor, &args.rationale, now)
                        .await?;
                }
                Ok(
                    json!({"proposal": store.get_proposal(&args.id).await?, "decisions": store.proposal_decisions(&args.id).await?}),
                )
            }
        },
        Command::Project { command } => match command {
            ProjectCommand::Create(args) => {
                lab(store, &args.lab_id).await?;
                required(&args.name, "name")?;
                let id = args
                    .id
                    .clone()
                    .unwrap_or_else(|| Uuid::new_v4().to_string());
                required(&id, "id")?;
                store
                    .create_project(&id, &args.lab_id, &args.name, &args.question, now)
                    .await?;
                Ok(json!({"project": project(store, &id).await?}))
            }
            ProjectCommand::List(args) => {
                lab(store, &args.lab_id).await?;
                Ok(json!({"projects": projects(store, &args.lab_id).await?}))
            }
            ProjectCommand::Approve(args) => {
                project(store, &args.id).await?;
                store
                    .approve_project(&args.id, &args.actor, &args.rationale, now)
                    .await?;
                Ok(json!({"project": project(store, &args.id).await?}))
            }
        },
        Command::Task { command } => match command {
            TaskCommand::Create(args) => {
                project(store, &args.project_id).await?;
                let id = args
                    .id
                    .clone()
                    .unwrap_or_else(|| Uuid::new_v4().to_string());
                required(&id, "id")?;
                if let Some(owner) = &args.owner {
                    required(owner, "owner")?;
                }
                store
                    .create_task(
                        &TaskSpec {
                            id: id.clone(),
                            project_id: args.project_id.clone(),
                            objective: args.objective.clone(),
                            context: args.context.clone(),
                            deliverable: args.deliverable.clone(),
                            completion_criteria: args.completion_criteria.clone(),
                            owner: args.owner.clone(),
                            priority: args.priority,
                        },
                        now,
                    )
                    .await?;
                Ok(json!({"task": task(store, &id).await?}))
            }
            TaskCommand::List(args) => {
                project(store, &args.project_id).await?;
                Ok(json!({"tasks": tasks(store, &args.project_id).await?}))
            }
            TaskCommand::Inspect(args) => Ok(
                json!({"task": task(store, &args.id).await?, "attempts": store.attempts_for_task(&args.id).await?}),
            ),
            TaskCommand::Priority(args) => {
                store
                    .override_priority(&args.id, args.priority, &args.actor, &args.rationale, now)
                    .await?;
                Ok(json!({"task": task(store, &args.id).await?}))
            }
            TaskCommand::Cancel(args) => {
                store
                    .cancel_task(&args.id, &args.actor, &args.rationale, now)
                    .await?;
                Ok(json!({"task": task(store, &args.id).await?}))
            }
        },
        Command::Lab { command } => {
            let (args, paused) = match command {
                LabCommand::Pause(args) => (args, true),
                LabCommand::Resume(args) => (args, false),
            };
            store
                .set_paused(&args.lab_id, paused, &args.actor, &args.rationale, now)
                .await?;
            Ok(json!({"lab": lab(store, &args.lab_id).await?}))
        }
        Command::Decisions(args) => {
            lab(store, &args.lab_id).await?;
            Ok(json!({"decisions": store.decisions(&args.lab_id).await?}))
        }
        Command::Attempts(args) => {
            lab(store, &args.lab_id).await?;
            Ok(json!({"attempts": store.attempts(&args.lab_id).await?}))
        }
        Command::Review { command } => {
            let (args, accepted) = match command {
                ReviewCommand::Accept(args) => (args, true),
                ReviewCommand::Reject(args) => (args, false),
            };
            store
                .review_result(
                    &args.attempt_id,
                    accepted,
                    &args.actor,
                    &args.rationale,
                    now,
                )
                .await?;
            Ok(json!({"attempt_id": args.attempt_id, "accepted": accepted}))
        }
        Command::Artifacts(args) => {
            task(store, &args.task_id).await?;
            Ok(json!({"artifacts": store.artifacts_for_task(&args.task_id).await?}))
        }
        Command::Artifact(args) => Ok(
            json!({"artifact": store.get_artifact(&args.id).await?.context("unknown artifact")?}),
        ),
        Command::ConfigValidate => bail!("config validation must run before database access"),
        Command::Gui(_) => bail!("gui must be handled before command dispatch"),
        Command::RunOnce(args) => poll(store, &args.lab_id).await,
        Command::Run(_) => bail!("run must use the polling loop"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn isolated_default_and_explicit_paths() {
        assert_eq!(
            database_path(None, Some(PathBuf::from("/home/test"))).unwrap(),
            PathBuf::from("/home/test/.gewu-lab/lab.db")
        );
        assert_eq!(
            database_path(Some(PathBuf::from("custom.db")), None).unwrap(),
            PathBuf::from("custom.db")
        );
        assert!(database_path(None, None).is_err());
        assert!(database_path(Some(PathBuf::new()), None).is_err());
    }

    #[test]
    fn text_output_displays_fields_and_empty_lists() {
        let mut output = Vec::new();
        write_text(
            &mut output,
            &json!({"message": "No executor configured", "tasks": []}),
            0,
        )
        .unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("message: No executor configured"));
        assert!(text.contains("tasks:\n  (none)"));
    }

    async fn execute(store: &LabStore, args: &[&str]) -> Result<Value> {
        let cli = Cli::try_parse_from(std::iter::once("gewu-lab").chain(args.iter().copied()))?;
        handle(store, &cli.command).await
    }

    #[tokio::test]
    async fn init_is_idempotent_and_conflicts_preserve_lab() {
        let dir = tempfile::tempdir().unwrap();
        let store = connect(&dir.path().join("lab.db")).await.unwrap();
        let args = [
            "init",
            "--id",
            "l",
            "--name",
            "Lab",
            "--interest",
            "science",
        ];
        assert_eq!(execute(&store, &args).await.unwrap()["created"], true);
        assert_eq!(execute(&store, &args).await.unwrap()["created"], false);
        assert!(
            execute(
                &store,
                &[
                    "init",
                    "--id",
                    "l",
                    "--name",
                    "Other",
                    "--interest",
                    "science"
                ]
            )
            .await
            .is_err()
        );
        assert_eq!(lab(&store, "l").await.unwrap().name, "Lab");
    }

    #[tokio::test]
    async fn handlers_manage_tasks_without_fake_execution() {
        let dir = tempfile::tempdir().unwrap();
        let store = connect(&dir.path().join("lab.db")).await.unwrap();
        execute(
            &store,
            &[
                "init",
                "--id",
                "l",
                "--name",
                "Lab",
                "--interest",
                "science",
            ],
        )
        .await
        .unwrap();
        execute(
            &store,
            &[
                "project",
                "create",
                "--lab-id",
                "l",
                "--id",
                "p",
                "--name",
                "P",
                "--question",
                "why",
            ],
        )
        .await
        .unwrap();
        execute(
            &store,
            &["project", "approve", "--id", "p", "--rationale", "yes"],
        )
        .await
        .unwrap();
        execute(
            &store,
            &[
                "task",
                "create",
                "--project-id",
                "p",
                "--id",
                "t",
                "--objective",
                "o",
                "--deliverable",
                "d",
                "--completion-criteria",
                "c",
            ],
        )
        .await
        .unwrap();
        assert_eq!(
            execute(&store, &["run-once", "--lab-id", "l"])
                .await
                .unwrap()["outcome"],
            "no_executor_configured"
        );
        assert_eq!(
            task(&store, "t").await.unwrap().status,
            gewu_lab::TaskStatus::Ready
        );
        assert!(store.attempts_for_task("t").await.unwrap().is_empty());
        execute(
            &store,
            &[
                "task",
                "priority",
                "--id",
                "t",
                "--priority",
                "9",
                "--rationale",
                "urgent",
            ],
        )
        .await
        .unwrap();
        assert_eq!(task(&store, "t").await.unwrap().effective_priority(), 9);
        execute(
            &store,
            &["lab", "pause", "--lab-id", "l", "--rationale", "break"],
        )
        .await
        .unwrap();
        assert_eq!(poll(&store, "l").await.unwrap()["outcome"], "idle");
        execute(
            &store,
            &["lab", "resume", "--lab-id", "l", "--rationale", "ready"],
        )
        .await
        .unwrap();
        execute(
            &store,
            &["task", "cancel", "--id", "t", "--rationale", "stop"],
        )
        .await
        .unwrap();
        assert_eq!(
            task(&store, "t").await.unwrap().status,
            gewu_lab::TaskStatus::Cancelled
        );
        assert!(
            execute(&store, &["task", "inspect", "--id", "missing"])
                .await
                .is_err()
        );
    }
}
