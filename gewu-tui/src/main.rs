//! `gewu-tui` terminal UI: chat with a running `gewu-server serve` in a
//! full-screen terminal interface.
//!
//! ```text
//! cargo run -p gewu-server -- serve          # in another terminal
//! cargo run -p gewu-tui
//! ```

mod resume;
mod tui;
mod tui_view;

use anyhow::{Context, Result};
use clap::Parser;
use futures::StreamExt;
use gewu_client::Client;
use gewu_protocol::{AgentEvent, ServerMessage};
use ratatui::crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event as TermEvent, EventStream, KeyCode,
    KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::crossterm::execute;
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

    /// Start a fresh conversation instead of resuming the most recent session
    #[arg(long)]
    new: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let client = Client::tcp(&cli.host, cli.tcp_port)
        .await
        .context("failed to connect to the gewu server (is `gewu-server serve` running?)")?;

    // Replay the last conversation before entering the alternate screen so the
    // first frame already shows history. `--session-id` wins over `--new`.
    let mut app = App::new();
    resume::load_startup_session(&client, &mut app, cli.session_id.as_deref(), !cli.new).await;

    // Show which model the server runs. Non-fatal: the status line simply
    // omits it when unavailable.
    match client.info().await {
        Ok(info) => app.model_label = Some(format!("{}/{}", info.provider, info.model)),
        Err(e) => app.note(format!("could not read server info: {e}")),
    }

    let result = run(client, &mut app, &cli.host, cli.tcp_port).await;
    restore_terminal();
    result?;

    // Surface the session id after a normal exit so it can be resumed.
    if let Some(session) = &app.model.session_id {
        writeln!(std::io::stderr(), "[session: {session}]")?;
    }
    Ok(())
}

/// Enter the alternate screen and ask the terminal for mouse events.
///
/// Mouse capture is what makes touchpad/wheel scrolling reach the app instead
/// of scrolling the terminal's own (alternate-screen) history — important
/// over SSH, where there is no local scrollback to fall back on.
fn init_terminal() -> ratatui::DefaultTerminal {
    let terminal = tui_init();
    let _ = execute!(std::io::stdout(), EnableMouseCapture);

    // Chain onto ratatui's restore-before-panic hook so mouse mode is
    // released even if the app panics.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(std::io::stdout(), DisableMouseCapture);
        previous(info);
    }));
    terminal
}

/// Release mouse mode, then restore the terminal.
fn restore_terminal() {
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    tui_restore();
}

/// Terminal event loop: routes crossterm events and ask-stream frames into
/// the [`App`], redrawing on every change.
async fn run(mut client: Client, app: &mut App, host: &str, port: u16) -> Result<()> {
    let mut terminal = init_terminal();
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
            // -- keyboard / mouse --------------------------------------------
            maybe_event = events.next() => {
                let Some(event) = maybe_event else { break };
                match event? {
                    TermEvent::Key(key) => {
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
                            // The server pins a session per connection, so a
                            // new chat needs a fresh connection. Keep the old
                            // client until the new one is up.
                            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                if active_ask.is_some() {
                                    let _ = client.interrupt().await;
                                    active_ask = None;
                                }
                                match Client::tcp(host, port).await {
                                    Ok(fresh) => {
                                        client.close().await;
                                        client = fresh;
                                        app.new_chat();
                                    }
                                    Err(e) => {
                                        app.fail(format!("could not start a new chat: {e}"));
                                    }
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
                            // With an empty composer these scroll the transcript;
                            // otherwise they move the input cursor.
                            KeyCode::Home if app.input.is_empty() => app.scroll_to_top(),
                            KeyCode::End if app.input.is_empty() => app.scroll_to_bottom(),
                            KeyCode::Home => app.move_home(),
                            KeyCode::End => app.move_end(),
                            KeyCode::Up if app.input.is_empty() => {
                                app.scroll_up(SCROLL_STEP);
                            }
                            KeyCode::Down if app.input.is_empty() => {
                                app.scroll_down(SCROLL_STEP);
                            }
                            KeyCode::PageUp => app.scroll_up(10),
                            KeyCode::PageDown => app.scroll_down(10),
                            KeyCode::Char(c) => app.push_char(c),
                            _ => {}
                        }
                    }
                    // Touchpad/wheel scrolling arrives as mouse events once
                    // capture is on; route them into the transcript in any
                    // phase, including while a reply streams. A plain left
                    // click toggles tool output/disclosure; Shift+click is
                    // left to the terminal for text selection.
                    TermEvent::Mouse(mouse) => match mouse.kind {
                        MouseEventKind::ScrollUp => app.scroll_up(SCROLL_STEP),
                        MouseEventKind::ScrollDown => app.scroll_down(SCROLL_STEP),
                        MouseEventKind::Down(MouseButton::Left)
                            if !mouse.modifiers.contains(KeyModifiers::SHIFT) =>
                        {
                            app.click_at(mouse.column, mouse.row);
                        }
                        _ => {}
                    },
                    // Resize and focus events just trigger a redraw.
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
        assert!(!cli.new);
    }

    #[test]
    fn cli_accepts_new_flag() {
        let cli = Cli::try_parse_from(["gewu-tui", "--new"]).unwrap();
        assert!(cli.new);
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
