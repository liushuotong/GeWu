//! Ratatui rendering for the [`crate::tui::App`] state. Pure functions from
//! state to widgets; no I/O, so views are testable with `TestBackend`.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::tui::{App, Block as Msg, Phase};

/// Colors for each transcript speaker.
fn label_style(label: &str) -> Style {
    match label {
        "you" => Style::new().cyan().bold(),
        "gewu" => Style::new().green().bold(),
        "tool" => Style::new().yellow(),
        "error" => Style::new().red().bold(),
        _ => Style::new().dim(),
    }
}

/// Render one transcript block as styled lines (label + wrapped body).
fn block_lines(block: &Msg, width: u16) -> Vec<Line<'static>> {
    let label = block.label();
    let mut lines = vec![Line::from(Span::styled(
        label.to_string(),
        label_style(label),
    ))];

    let body_width = width.saturating_sub(3).max(1) as usize;
    match block {
        // Tool blocks show the invocation first (`$ cmd`), then the output —
        // mirroring gewu-server's `[Tool Start: bash] $ cmd` convention.
        Msg::Tool { input, output, .. } => {
            for raw in wrap_text(&format!("$ {input}"), body_width) {
                lines.push(Line::from(Span::raw(format!("  {raw}"))));
            }
            let text = output.as_deref().unwrap_or("running…");
            for raw in wrap_text(text, body_width) {
                lines.push(Line::from(Span::styled(
                    format!("  {raw}"),
                    Style::new().dim(),
                )));
            }
        }
        _ => {
            let text = block.text();
            let style = if label == "you" {
                Style::new()
            } else {
                Style::new().dim()
            };
            for raw in wrap_text(text, body_width) {
                lines.push(Line::from(Span::styled(format!("  {raw}"), style)));
            }
        }
    }
    lines.push(Line::from(""));
    lines
}

/// Greedy word wrap for plain text (no styling inside the body).
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for paragraph in text.split('\n') {
        if paragraph.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut line = String::new();
        let mut rest = paragraph;
        while !rest.is_empty() {
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            let (word, remainder) = rest.split_at(end);
            let remainder = remainder.trim_start();

            // Hard-break words longer than the width.
            if word.chars().count() > width {
                if !line.is_empty() {
                    out.push(std::mem::take(&mut line));
                }
                let mut chunk: &str = word;
                while chunk.chars().count() > width {
                    let take = chunk
                        .char_indices()
                        .nth(width)
                        .map_or(chunk.len(), |(i, _)| i);
                    out.push(chunk[..take].to_string());
                    chunk = &chunk[take..];
                }
                line.push_str(chunk);
                rest = remainder;
                continue;
            }

            if line.is_empty() {
                line.push_str(word);
            } else if line.chars().count() + 1 + word.chars().count() <= width {
                line.push(' ');
                line.push_str(word);
            } else {
                out.push(std::mem::take(&mut line));
                line.push_str(word);
            }
            rest = remainder;
        }
        if !line.is_empty() || out.is_empty() {
            out.push(line);
        }
    }
    out
}

/// The bottom status line.
fn status_line(app: &App) -> Line<'static> {
    let phase = match app.model.phase {
        Phase::Idle => Span::styled("idle", Style::new().dim()),
        Phase::Busy => Span::styled("thinking…", Style::new().cyan()),
    };
    let session = match &app.model.session_id {
        Some(id) => {
            let short: String = id.chars().take(8).collect();
            Span::styled(format!(" session {short}"), Style::new().dim())
        }
        None => Span::raw(""),
    };
    let keys = match app.model.phase {
        Phase::Busy => Span::styled("  Esc interrupt", Style::new().dim()),
        Phase::Idle => Span::styled("  Enter send  Ctrl+D quit", Style::new().dim()),
    };
    // The scroll hint precedes the key hints so it stays visible on narrow
    // terminals (the status line is clipped, not wrapped).
    let scroll_hint = if app.follow {
        Span::raw("")
    } else {
        Span::styled(format!("  ↑{} more", app.scroll), Style::new().yellow())
    };
    Line::from(vec![phase, session, scroll_hint, keys])
}

/// Draw the whole UI for one frame.
pub fn draw(frame: &mut Frame, app: &mut App) {
    let root = frame.area();
    let [transcript_area, input_area, status_area] = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .areas(root);

    // -- transcript ----------------------------------------------------------
    let width = transcript_area.width;
    let mut lines: Vec<Line<'static>> = Vec::new();
    for block in &app.model.blocks {
        lines.extend(block_lines(block, width));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "connected — type a message and press Enter",
            Style::new().dim(),
        )));
    }

    let total = lines.len();
    let visible = transcript_area.height.saturating_sub(2) as usize; // borders
    let hidden = total.saturating_sub(visible);
    let offset = if app.follow {
        hidden
    } else {
        hidden.saturating_sub(app.scroll)
    };
    let show: Vec<Line<'static>> = lines.into_iter().skip(offset).take(visible).collect();

    let transcript = Paragraph::new(show).block(Block::default().borders(Borders::ALL).title(
        Span::styled(" gewu ", Style::new().bold().add_modifier(Modifier::DIM)),
    ));
    frame.render_widget(transcript, transcript_area);

    // -- input ---------------------------------------------------------------
    let input_title = match app.model.phase {
        Phase::Idle => " prompt ",
        Phase::Busy => " prompt (busy — Esc interrupts) ",
    };
    let input =
        Paragraph::new(app.input.as_str()).block(Block::default().borders(Borders::ALL).title(
            Span::styled(input_title, Style::new().bold().add_modifier(Modifier::DIM)),
        ));
    frame.render_widget(input, input_area);

    // Place the cursor inside the input box on the first line.
    if app.model.phase == Phase::Idle {
        let first_line = app.input.lines().next().unwrap_or("");
        let cursor_col = app.cursor.min(first_line.chars().count());
        let x = (input_area.x + 1 + cursor_col as u16)
            .min(input_area.x + input_area.width.saturating_sub(2));
        frame.set_cursor_position((x, input_area.y + 1));
    }

    // -- status --------------------------------------------------------------
    frame.render_widget(Paragraph::new(status_line(app)), status_area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn render(app: &mut App, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn draws_transcript_input_status() {
        let mut app = App::new();
        app.model.blocks.push(Msg::User("hi there".into()));
        app.model.blocks.push(Msg::Assistant("hello!".into()));

        let screen = render(&mut app, 40, 12);
        assert!(screen.contains("gewu"), "title: {screen}");
        assert!(screen.contains("you"), "user label: {screen}");
        assert!(screen.contains("hi there"));
        assert!(screen.contains("hello!"));
        assert!(screen.contains("prompt"), "input box: {screen}");
        assert!(screen.contains("Enter send"), "status: {screen}");
    }

    #[test]
    fn busy_phase_shows_interrupt_hint() {
        let mut app = App::new();
        app.submit("go".into());
        let screen = render(&mut app, 40, 10);
        assert!(screen.contains("thinking"));
        assert!(screen.contains("Esc interrupt"));
    }

    #[test]
    fn scroll_up_pins_history() {
        let mut app = App::new();
        for i in 0..20 {
            app.model.blocks.push(Msg::Assistant(format!("line {i}")));
        }
        app.scroll = 200;
        app.follow = false;
        let screen = render(&mut app, 30, 8);
        assert!(
            screen.contains("line 0"),
            "top visible when scrolled: {screen}"
        );
        assert!(screen.contains("more"), "scroll hint: {screen}");
    }

    #[test]
    fn tool_block_renders_command() {
        let mut app = App::new();
        app.model.blocks.push(Msg::Tool {
            tool: "bash".into(),
            input: "ls -la".into(),
            output: Some("total 0".into()),
        });
        let screen = render(&mut app, 40, 10);
        assert!(screen.contains("tool"));
        assert!(screen.contains("ls -la"));
    }

    #[test]
    fn wrap_text_handles_long_words_and_blank_lines() {
        assert_eq!(wrap_text("abc", 10), vec!["abc"]);
        assert_eq!(wrap_text("a b c", 3), vec!["a b", "c"]);
        assert_eq!(wrap_text("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(wrap_text("x\n\ny", 10), vec!["x", "", "y"]);
        assert_eq!(wrap_text("", 5), vec![""]);
    }

    #[test]
    fn empty_state_shows_hint() {
        let mut app = App::new();
        let screen = render(&mut app, 40, 10);
        assert!(screen.contains("connected"));
    }
}
