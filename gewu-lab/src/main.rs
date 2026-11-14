mod app;
mod cli;
mod gui;

use clap::Parser;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let cli = cli::Cli::parse();
    let json = cli.json;
    match app::run(cli).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            if json {
                eprintln!("{}", serde_json::json!({"error": format!("{error:#}")}));
            } else {
                eprintln!("error: {error:#}");
            }
            std::process::ExitCode::FAILURE
        }
    }
}
