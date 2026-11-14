use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "gewu-lab",
    version,
    about = "Manage an isolated GeWu research lab"
)]
pub struct Cli {
    #[arg(
        long,
        global = true,
        help = "Database file (default: ~/.gewu-lab/lab.db)"
    )]
    pub db_path: Option<PathBuf>,
    #[arg(
        long,
        global = true,
        help = "Lab config file (default: ~/.gewu-lab/config.json)"
    )]
    pub config: Option<PathBuf>,
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    #[command(
        about = "Create a lab; identical id/name/initial interest is a no-op, conflicts fail"
    )]
    Init(InitArgs),
    Status(LabArgs),
    Proposal {
        #[command(subcommand)]
        command: ProposalCommand,
    },
    Project {
        #[command(subcommand)]
        command: ProjectCommand,
    },
    Task {
        #[command(subcommand)]
        command: TaskCommand,
    },
    Lab {
        #[command(subcommand)]
        command: LabCommand,
    },
    Decisions(LabArgs),
    Attempts(LabArgs),
    Artifacts(ArtifactArgs),
    Artifact(IdArgs),
    ConfigValidate,
    Gui(GuiArgs),
    Review {
        #[command(subcommand)]
        command: ReviewCommand,
    },
    #[command(about = "Execute one eligible task if lab LLM configuration is present")]
    RunOnce(RunArgs),
    #[command(about = "Poll for eligible tasks; stop with Ctrl-C")]
    Run(RunArgs),
}

#[derive(Debug, Args)]
pub struct GuiArgs {
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,
    #[arg(long, default_value_t = 8440)]
    pub port: u16,
    #[arg(long)]
    pub open: bool,
    #[arg(
        long,
        help = "GUI login secret (or GEWU_LAB_GUI_TOKEN); at least 32 bytes"
    )]
    pub token: Option<String>,
    #[arg(long)]
    pub secure_cookie: bool,
    #[arg(long = "allowed-origin")]
    pub allowed_origins: Vec<String>,
}

#[derive(Debug, Subcommand)]
pub enum ProposalCommand {
    Create(ProposalCreateArgs),
    Inspect(IdArgs),
    List(LabArgs),
    Approve(DecisionArgs),
    Reject(DecisionArgs),
}

#[derive(Debug, Args)]
pub struct ProposalCreateArgs {
    #[arg(long)]
    pub lab_id: String,
    #[arg(long)]
    pub id: Option<String>,
    #[arg(long, help = "Proposal JSON file; omit or use - for stdin")]
    pub file: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct InitArgs {
    #[arg(long)]
    pub id: String,
    #[arg(long)]
    pub name: String,
    #[arg(long)]
    pub interest: String,
}

#[derive(Debug, Args)]
pub struct LabArgs {
    #[arg(long)]
    pub lab_id: String,
}

#[derive(Debug, Subcommand)]
pub enum ProjectCommand {
    Create(ProjectCreateArgs),
    List(LabArgs),
    Approve(DecisionArgs),
}

#[derive(Debug, Args)]
pub struct ProjectCreateArgs {
    #[arg(long)]
    pub lab_id: String,
    #[arg(long)]
    pub id: Option<String>,
    #[arg(long)]
    pub name: String,
    #[arg(long)]
    pub question: String,
}

#[derive(Debug, Args)]
pub struct DecisionArgs {
    #[arg(long)]
    pub id: String,
    #[arg(long, default_value = "cli")]
    pub actor: String,
    #[arg(long)]
    pub rationale: String,
}

#[derive(Debug, Subcommand)]
pub enum TaskCommand {
    Create(TaskCreateArgs),
    List(ProjectArgs),
    Inspect(IdArgs),
    Priority(PriorityArgs),
    Cancel(DecisionArgs),
}

#[derive(Debug, Args)]
pub struct ProjectArgs {
    #[arg(long)]
    pub project_id: String,
}

#[derive(Debug, Args)]
pub struct ArtifactArgs {
    #[arg(long)]
    pub task_id: String,
}

#[derive(Debug, Args)]
pub struct TaskCreateArgs {
    #[arg(long)]
    pub project_id: String,
    #[arg(long)]
    pub id: Option<String>,
    #[arg(long)]
    pub objective: String,
    #[arg(long, default_value = "")]
    pub context: String,
    #[arg(long)]
    pub deliverable: String,
    #[arg(long)]
    pub completion_criteria: String,
    #[arg(long)]
    pub owner: Option<String>,
    #[arg(long, default_value_t = 0)]
    pub priority: i64,
}

#[derive(Debug, Args)]
pub struct IdArgs {
    #[arg(long)]
    pub id: String,
}

#[derive(Debug, Args)]
pub struct PriorityArgs {
    #[arg(long)]
    pub id: String,
    #[arg(long)]
    pub priority: i64,
    #[arg(long, default_value = "cli")]
    pub actor: String,
    #[arg(long)]
    pub rationale: String,
}

#[derive(Debug, Subcommand)]
pub enum LabCommand {
    Pause(LabDecisionArgs),
    Resume(LabDecisionArgs),
}

#[derive(Debug, Args)]
pub struct LabDecisionArgs {
    #[arg(long)]
    pub lab_id: String,
    #[arg(long, default_value = "cli")]
    pub actor: String,
    #[arg(long)]
    pub rationale: String,
}

#[derive(Debug, Args)]
pub struct RunArgs {
    #[arg(long)]
    pub lab_id: String,
    #[arg(long, default_value = "lab-worker")]
    pub worker_id: String,
    #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(i64).range(1..=86400))]
    pub lease_seconds: i64,
    #[arg(long, default_value_t = 120, value_parser = clap::value_parser!(u64).range(1..=86399))]
    pub timeout_seconds: u64,
    #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u64).range(1..=86400), help = "Polling interval in seconds (1-86400)")]
    pub interval: u64,
}

#[derive(Debug, Subcommand)]
pub enum ReviewCommand {
    Accept(ReviewArgs),
    Reject(ReviewArgs),
}

#[derive(Debug, Args)]
pub struct ReviewArgs {
    #[arg(long)]
    pub attempt_id: String,
    #[arg(long, default_value = "cli")]
    pub actor: String,
    #[arg(long)]
    pub rationale: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_nested_task_command() {
        let cli = Cli::try_parse_from([
            "gewu-lab",
            "task",
            "create",
            "--project-id",
            "p",
            "--objective",
            "o",
            "--deliverable",
            "d",
            "--completion-criteria",
            "c",
        ])
        .unwrap();
        assert!(matches!(cli.command, Command::Task { .. }));
    }

    #[test]
    fn global_options_and_poll_interval_are_validated() {
        let cli = Cli::try_parse_from([
            "gewu-lab",
            "run",
            "--lab-id",
            "l",
            "--interval",
            "2",
            "--json",
            "--db-path",
            "lab.db",
        ])
        .unwrap();
        assert!(cli.json);
        assert_eq!(cli.db_path, Some(PathBuf::from("lab.db")));
        assert!(matches!(
            cli.command,
            Command::Run(RunArgs { interval: 2, .. })
        ));
        for interval in ["0", "-1", "86401", "invalid"] {
            assert!(
                Cli::try_parse_from(["gewu-lab", "run", "--lab-id", "l", "--interval", interval])
                    .is_err()
            );
        }
        assert!(Cli::try_parse_from(["gewu-lab"]).is_err());
    }
}
