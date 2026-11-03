//! `gewu-tui` terminal UI: chat with a running `gewu-server serve` in a
//! full-screen terminal interface.
//!
//! ```text
//! cargo run -p gewu-server -- serve          # in another terminal
//! cargo run -p gewu-tui
//! ```

mod tui;
mod tui_view;

use anyhow::{Context, Result};
use clap::Parser;
use futures::StreamExt;
use gewu_client::Client;
use gewu_protocol::{AgentEvent, ServerMessage};
use ratatui::crossterm::event::{
    Event as TermEvent, EventStream, KeyCode, KeyEventKind, KeyModifiers,
};
use ratatui::{init as tui_init, restore as tui_restore};
use std::io::Write;
use tui::{App, Phase};
use tui_view::draw;

const SCROLL_STEP: usize = 3;

#[derive(clap::Parser)]
#[command(name = "gewu-tui")]
#[command(version = "0.1.0")]
#[command(about = "Terminal UI for a running gewu server (start it with `gewu-server serve`)", long_about = None)]
struct Cli {
    /// Host running the gewu server
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// TCP port of the gewu server (newline-delimited JSON)
    #[arg(long, default_value_t = 8421)]
    tcp_port: u16,

    /// Resume a specific session (also via GEWU_SESSION_ID)
    #[arg(long, env = "GEWU_SESSION_ID")]
    session_id: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let client = Client::tcp(&cli.host, cli.tcp_port)
        .await
        .context("failed to connect to the gewu server (is `gewu-server serve` running?)")?;

    let mut app = App::new();
    if let Some(session) = &cli.session_id {
        app.set_session(Some(session.clone()));
        app.note(format!("resuming session {session}"));
    }

    let result = run(client.clone(), &mut app).await;
    tui_restore();
    result?;

    // Surface the session id after a normal exit so it can be resumed.
    if let Some(session) = &app.model.session_id {
        writeln!(std::io::stderr(), "[session: {session}]")?;
    }
    Ok(())
}

/// Terminal event loop: routes crossterm events and ask-stream frames into
/// the [`App`], redrawing on every change.
async fn run(client: Client, app: &mut App) -> Result<()> {
    let mut terminal = tui_init();
    let mut events = EventStream::new();

    // Liveness check; surfaces connection problems before the first prompt.
    match client.ping().await {
        Ok(()) => app.note("connected"),
        Err(e) => {
            app.fail(format!("server not responding: {e}"));
            draw_once(&mut terminal, app);
            tokio::time::sleep(std::time::Duration::from_millis(800)).await;
            return Ok(());
        }
    }
    draw_once(&mut terminal, app);

    // The in-flight ask's frame stream, when one is running.
    let mut active_ask: Option<gewu_client::AskStream> = None;

    loop {
        // Ensure the terminal reflects the latest state.
        draw_once(&mut terminal, app);

        tokio::select! {
            // -- keyboard ----------------------------------------------------
            maybe_event = events.next() => {
                let Some(event) = maybe_event else { break };
                let TermEvent::Key(key) = event? else { continue };
                // Only act on key presses (not terminal-reported releases).
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                    KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL)
                        && app.model.phase == Phase::Idle && app.input.is_empty() => break,
                    KeyCode::Esc => {
                        if app.model.phase == Phase::Busy {
                            let _ = client.interrupt().await;
                        }
                    }
                    KeyCode::Enter if app.model.phase == Phase::Idle => {
                        if let Some(prompt) = app.take_prompt() {
                            let mut builder = client.ask(prompt.clone());
                            if let Some(session) = app.model.session_id.clone() {
                                builder = builder.session_id(session);
                            }
                            app.submit(prompt);
                            active_ask = Some(builder.send().await?);
                        }
                    }
                    KeyCode::Backspace => app.backspace(),
                    KeyCode::Delete => app.delete(),
                    KeyCode::Left => app.move_left(),
                    KeyCode::Right => app.move_right(),
                    KeyCode::Home => app.move_home(),
                    KeyCode::End => app.move_end(),
                    KeyCode::Up if app.model.phase == Phase::Idle && app.input.is_empty() => {
                        app.scroll_up(SCROLL_STEP);
                    }
                    KeyCode::Down if app.model.phase == Phase::Idle && app.input.is_empty() => {
                        app.scroll_down(SCROLL_STEP);
                    }
                    KeyCode::PageUp => app.scroll_up(10),
                    KeyCode::PageDown => app.scroll_down(10),
                    KeyCode::Char(c) => app.push_char(c),
                    _ => {}
                }
            }
            // -- ask stream ---------------------------------------------------
            frame = async {
                match active_ask.as_mut() {
                    Some(ask) => ask.next_frame().await,
                    None => std::future::pending().await,
                }
            } => {
                let Some(frame) = frame else {
                    active_ask = None;
                    app.finish_ask();
                    continue;
                };
                match frame {
                    Ok(ServerMessage::Event { event, .. }) => {
                        match serde_json::from_value::<AgentEvent>(event) {
                            Ok(event) => app.apply_event(&event),
                            Err(e) => app.note(format!("unrecognized event: {e}")),
                        }
                    }
                    Ok(ServerMessage::Result { session_id, .. }) => {
                        app.set_session(Some(session_id));
                        active_ask = None;
                        app.finish_ask();
                    }
                    Ok(_) => {}
                    Err(e) => {
                        let interrupted = matches!(e, gewu_client::Error::Interrupted);
                        if interrupted {
                            app.note("interrupted");
                        } else {
                            app.fail(e.to_string());
                        }
                        active_ask = None;
                        app.finish_ask();
                    }
                }
            }
        }
    }

    // If an ask is still running, abort it politely before exiting.
    if active_ask.is_some() {
        let _ = client.interrupt().await;
    }
    client.close().await;
    Ok(())
}

/// Redraw immediately (used outside the main loop).
fn draw_once(terminal: &mut ratatui::DefaultTerminal, app: &mut App) {
    let _ = terminal.draw(|f| draw(f, app));
    let _ = std::io::stdout().flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_has_no_subcommands() {
        // The client is a TUI only: no ask/chat/tools/agents/ping subcommands.
        let mut cmd = Cli::command();
        cmd.build();
        assert!(cmd.get_subcommands().next().is_none());
    }

    #[test]
    fn cli_defaults() {
        let cli = Cli::try_parse_from(["gewu-tui"]).unwrap();
        assert_eq!(cli.host, "127.0.0.1");
        assert_eq!(cli.tcp_port, 8421);
        assert_eq!(cli.session_id, None);
    }

    #[test]
    fn cli_accepts_flags_and_env() {
        let cli = Cli::try_parse_from([
            "gewu-tui",
            "--host",
            "example.org",
            "--tcp-port",
            "9000",
            "--session-id",
            "abc",
        ])
        .unwrap();
        assert_eq!(cli.host, "example.org");
        assert_eq!(cli.tcp_port, 9000);
        assert_eq!(cli.session_id.as_deref(), Some("abc"));

        // GEWU_SESSION_ID feeds the same option.
        let cmd = Cli::command();
        let env = cmd
            .get_arguments()
            .find(|a| a.get_id() == "session_id")
            .and_then(|a| a.get_env())
            .is_some();
        assert!(env, "session_id reads GEWU_SESSION_ID");
    }
}
