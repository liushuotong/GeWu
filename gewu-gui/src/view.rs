//! HTML rendering for the web GUI.
//!
//! Mirrors `gewu-tui`'s `tui_view`: transcript blocks, Markdown, tool runs,
//! and the status line. Everything user- or model-authored is escaped, and
//! raw HTML in Markdown is neutralized.

use std::fmt::Write as _;

use gewu_client::session::{Block, Phase};
use pulldown_cmark::{CowStr, Event, Options, Parser, Tag, html};

use crate::workspace::Tab;

/// Lines of tool output shown before collapsing.
const TOOL_PREVIEW_LINES: usize = 6;
/// Defensive cap on expanded tool output.
const TOOL_MAX_LINES: usize = 500;

/// Escape text for use in HTML text or a double-quoted attribute.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Render the active tab's transcript.
pub fn transcript_html(tab: &Tab) -> String {
    if tab.model.blocks.is_empty() {
        return "<div class=\"empty\">connected — type a message and press Enter</div>".to_string();
    }

    let mut out = String::new();
    let last = tab.model.blocks.len() - 1;
    let mut index = 0;
    while index < tab.model.blocks.len() {
        if matches!(tab.model.blocks[index], Block::Tool { .. }) {
            let run_len = tab.model.blocks[index..]
                .iter()
                .take_while(|block| matches!(block, Block::Tool { .. }))
                .count();
            render_tool_run(
                &mut out,
                tab,
                &tab.model.blocks[index..index + run_len],
                index,
            );
            index += run_len;
        } else {
            let streaming = tab.busy() && index == last;
            render_block(&mut out, &tab.model.blocks[index], index, streaming);
            index += 1;
        }
    }
    out
}

/// Render one non-tool transcript block.
fn render_block(out: &mut String, block: &Block, index: usize, streaming: bool) {
    let class = match block {
        Block::User(_) => "user",
        Block::Assistant(_) => "assistant",
        Block::Error(_) => "error",
        Block::Info(_) => "info",
        Block::Tool { .. } => return,
    };
    let _ = writeln!(out, "<div class=\"block {class}\" data-block=\"{index}\">");
    write_block_body(out, block, streaming);
    out.push_str("</div>\n");
}

/// Render the inner `.body` div of one block (between the `.block` wrapper).
fn write_block_body(out: &mut String, block: &Block, streaming: bool) {
    match block {
        // Streaming and final assistant bodies are both Markdown; streaming
        // only adds a class (the blinking cursor) so the text formats in
        // real time and settles into the identical form once done.
        Block::Assistant(text) => {
            let _ = writeln!(
                out,
                "<div class=\"body markdown{}\">{}</div>",
                if streaming { " streaming" } else { "" },
                markdown_to_html(text)
            );
        }
        _ => {
            let _ = writeln!(
                out,
                "<div class=\"body plain\">{}</div>",
                escape(block.text())
            );
        }
    }
}

/// Render one block as a standalone `.block` div, for targeted appends.
pub fn block_html(tab: &Tab, index: usize) -> Option<String> {
    let block = tab.model.blocks.get(index)?;
    if matches!(block, Block::Tool { .. }) {
        return None;
    }
    let streaming = tab.busy() && index == tab.model.blocks.len() - 1;
    let mut out = String::new();
    render_block(&mut out, block, index, streaming);
    Some(out)
}

/// Render the Markdown body of an assistant block, either in its streaming
/// form (with the cursor class) or final form, for in-place swaps.
pub fn assistant_body_html(tab: &Tab, index: usize, streaming: bool) -> Option<String> {
    match tab.model.blocks.get(index) {
        Some(Block::Assistant(text)) => Some(format!(
            "<div class=\"body markdown{}\">{}</div>",
            if streaming { " streaming" } else { "" },
            markdown_to_html(text)
        )),
        _ => None,
    }
}

/// Render just the tool card that starts at block `first`, for targeted
/// updates after a toggle. `None` when `first` is not a tool block.
pub fn tool_card_html(tab: &Tab, first: usize) -> Option<String> {
    if !matches!(tab.model.blocks.get(first), Some(Block::Tool { .. })) {
        return None;
    }
    let run_len = tab.model.blocks[first..]
        .iter()
        .take_while(|block| matches!(block, Block::Tool { .. }))
        .count();
    let mut out = String::new();
    render_tool_run(
        &mut out,
        tab,
        &tab.model.blocks[first..first + run_len],
        first,
    );
    Some(out)
}

/// Render a run of consecutive tool blocks inside one card.
fn render_tool_run(out: &mut String, tab: &Tab, run: &[Block], first: usize) {
    let tools: Vec<(&str, &str, Option<&str>)> = run
        .iter()
        .filter_map(|block| match block {
            Block::Tool {
                tool,
                input,
                output,
            } => Some((tool.as_str(), input.as_str(), output.as_deref())),
            _ => None,
        })
        .collect();

    let _ = writeln!(out, "<div class=\"tool-card\" data-card=\"{first}\">");
    if tools.len() == 1 {
        let (tool, input, output) = tools[0];
        render_tool(out, tab, tool, input, output, first, 0);
    } else {
        let expanded = tab.expanded_groups.contains(&first);
        let glyph = if expanded { "−" } else { "+" };
        let running = run
            .iter()
            .any(|block| matches!(block, Block::Tool { output: None, .. }));
        let _ = writeln!(
            out,
            "<div class=\"tool-group{}\" data-block=\"{first}\" data-action=\"toggle_group\" data-tab=\"{}\">\
             <button class=\"tool-summary\" data-action=\"toggle_group\" data-tab=\"{}\" data-block=\"{first}\">{glyph} {}</button>",
            if running { " running" } else { "" },
            tab.id,
            tab.id,
            escape(&group_label(run))
        );
        if expanded {
            for (offset, (tool, input, output)) in tools.iter().enumerate() {
                render_tool(out, tab, tool, input, *output, first + offset, 1);
            }
        }
        out.push_str("</div>\n");
    }
    out.push_str("</div>\n");
}

/// One tool row plus its output.
#[allow(clippy::too_many_arguments)]
fn render_tool(
    out: &mut String,
    tab: &Tab,
    tool: &str,
    input: &str,
    output: Option<&str>,
    index: usize,
    depth: usize,
) {
    let (name, title) = tool_row(tool, input);
    let running = output.is_none();
    let expanded = tab.expanded_outputs.contains(&index);
    let _ = writeln!(
        out,
        "<div class=\"tool{} depth-{depth}\" data-action=\"toggle_tool\" data-tab=\"{}\" data-block=\"{index}\">\
         <button class=\"tool-row\" data-action=\"toggle_tool\" data-tab=\"{}\" data-block=\"{index}\">\
         <span class=\"tool-name\">{}</span>{} <span class=\"tool-title\">{}</span></button>",
        if running { " running" } else { "" },
        tab.id,
        tab.id,
        escape(name),
        if running {
            "<span class=\"tool-spinner\" aria-hidden=\"true\"></span>"
        } else {
            ""
        },
        escape(&title)
    );

    let Some(output) = output else {
        out.push_str("</div>\n");
        return;
    };
    let lines: Vec<&str> = output.trim_end().lines().collect();
    if lines.is_empty() {
        out.push_str("</div>\n");
        return;
    }
    let shown = if expanded {
        lines.len().min(TOOL_MAX_LINES)
    } else {
        lines.len().min(TOOL_PREVIEW_LINES)
    };
    let _ = writeln!(
        out,
        "<pre class=\"tool-output\">{}</pre>",
        escape(&lines[..shown].join("\n"))
    );
    if lines.len() > shown {
        let hidden = lines.len() - shown;
        let _ = writeln!(
            out,
            "<button class=\"tool-more\" data-action=\"toggle_tool\" data-tab=\"{}\" data-block=\"{index}\">… +{hidden} line{}</button>",
            tab.id,
            if hidden == 1 { "" } else { "s" }
        );
    }
    out.push_str("</div>\n");
}

/// Readable display name for a tool.
fn tool_name(tool: &str) -> &str {
    match tool {
        "read_file" | "read" => "read",
        "write_file" | "write" => "write",
        "edit_file" | "edit" => "edit",
        "apply_patch" | "patch" => "patch",
        "webfetch" | "web_fetch" => "fetch",
        "websearch" | "web_search" => "search",
        "shell" => "bash",
        other => other,
    }
}

fn tool_row<'a>(tool: &'a str, input: &str) -> (&'a str, String) {
    let title = if input.is_empty() {
        tool.to_string()
    } else {
        input.to_string()
    };
    (tool_name(tool), title)
}

/// `Explored: 2 bash, 1 read` (or `Exploring: …` while a tool runs).
fn group_label(run: &[Block]) -> String {
    let mut counts: Vec<(&str, usize)> = Vec::new();
    let mut complete = true;
    for block in run {
        let Block::Tool { tool, output, .. } = block else {
            continue;
        };
        if output.is_none() {
            complete = false;
        }
        let name = tool_name(tool);
        match counts.iter_mut().find(|(n, _)| *n == name) {
            Some((_, count)) => *count += 1,
            None => counts.push((name, 1)),
        }
    }
    let parts: Vec<String> = counts
        .iter()
        .map(|(name, count)| format!("{count} {}", plural(name, *count)))
        .collect();
    format!(
        "{}: {}",
        if complete { "Explored" } else { "Exploring" },
        parts.join(", ")
    )
}

fn plural(name: &str, count: usize) -> String {
    if count == 1 || name == "bash" {
        name.to_string()
    } else if name.ends_with('s') || name.ends_with("ch") || name.ends_with("sh") {
        format!("{name}es")
    } else {
        format!("{name}s")
    }
}

/// Markdown to safe HTML.
///
/// Raw HTML is converted to escaped text, and dangerous link/image schemes
/// are replaced with `#`, so model-authored content cannot inject script.
pub fn markdown_to_html(text: &str) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_FOOTNOTES);

    let parser = Parser::new_ext(text, options).map(|event| match event {
        Event::Html(html) | Event::InlineHtml(html) => Event::Text(html),
        Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Link {
            link_type,
            dest_url: safe_dest(dest_url),
            title,
            id,
        }),
        Event::Start(Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Image {
            link_type,
            dest_url: safe_dest(dest_url),
            title,
            id,
        }),
        other => other,
    });

    let mut out = String::new();
    html::push_html(&mut out, parser);
    out
}

fn safe_dest(dest: CowStr<'_>) -> CowStr<'_> {
    let compact: String = dest
        .chars()
        .filter(|c| !c.is_whitespace() && !c.is_control())
        .collect();
    let lower = compact.to_ascii_lowercase();
    if lower.starts_with("javascript:")
        || lower.starts_with("vbscript:")
        || lower.starts_with("data:")
    {
        CowStr::Borrowed("#")
    } else {
        dest
    }
}

/// `812` → `812`, `34_567` → `35k`.
fn format_k(tokens: u64) -> String {
    if tokens >= 1000 {
        format!("{}k", (tokens + 500) / 1000)
    } else {
        tokens.to_string()
    }
}

/// The status line: a telemetry strip with phase, model, key hints,
/// and a context gauge on the right.
pub fn status_html(tab: &Tab) -> String {
    let mut out = String::from("<div class=\"status-inner\">");
    match tab.model.phase {
        Phase::Idle => out.push_str("<span class=\"phase\"><i class=\"dot\"></i>idle</span>"),
        Phase::Busy => {
            out.push_str("<span class=\"phase busy\"><i class=\"dot\"></i>thinking…</span>");
        }
    }
    if let Some(label) = &tab.model_label {
        let _ = write!(
            out,
            "<span class=\"kv\"><span class=\"k\">model</span><span class=\"v model\">{}</span></span>",
            escape(label)
        );
    }
    let keys = if tab.model.phase == Phase::Busy {
        "Esc interrupt"
    } else {
        "Enter send · Shift+Enter newline"
    };
    let _ = write!(out, "<span class=\"hint\">{keys}</span>");

    if let Some((used, limit)) = tab.context.filter(|(used, limit)| *used > 0 && *limit > 0) {
        let pct = (used * 100 / limit).min(100);
        let class = if pct >= 80 { "usage warn" } else { "usage" };
        // The CSP forbids inline styles, so the gauge width snaps to 5% steps.
        let step = pct / 5 * 5;
        let _ = write!(
            out,
            "<span class=\"{class}\" title=\"{used} / {limit} tokens\">\
             <span class=\"k\">ctx</span>\
             <span class=\"gauge\" aria-hidden=\"true\"><span class=\"gauge-fill g{step}\"></span></span>\
             <span class=\"v\">{} / {} ({}%)</span></span>",
            format_k(used),
            format_k(limit),
            pct
        );
    }
    out.push_str("</div>");
    out
}

/// The right sidebar: the tools available to the agent, or why they are not,
/// and the working directory pinned to the bottom.
pub fn sidebar_html(tools: &Result<Vec<String>, String>, working_directory: Option<&str>) -> String {
    let mut out = String::from("<section class=\"side-section\">");
    match tools {
        Ok(tools) => {
            let _ = write!(
                out,
                "<h2 class=\"side-title\">tools<span class=\"side-count\">{}</span></h2>",
                tools.len()
            );
            if tools.is_empty() {
                out.push_str("<p class=\"side-note\">no tools</p>");
            } else {
                out.push_str("<ul class=\"side-list\">");
                for name in tools {
                    let _ = write!(out, "<li>{}</li>", escape(name));
                }
                out.push_str("</ul>");
            }
        }
        Err(e) => {
            out.push_str("<h2 class=\"side-title\">tools</h2>");
            let _ = write!(
                out,
                "<p class=\"side-note error\">tools unavailable: {}</p>",
                escape(e)
            );
        }
    }
    out.push_str("</section>");
    if let Some(dir) = working_directory {
        let _ = write!(
            out,
            "<section class=\"side-section side-cwd\" title=\"{}\">
             <h2 class=\"side-title\">cwd</h2>
             <p class=\"side-note side-path\">{}</p>
             </section>",
            escape(dir),
            escape(dir)
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;

    fn tab_with_blocks(blocks: Vec<Block>) -> (Workspace, u64) {
        let mut ws = Workspace::new();
        let id = ws.open_blank();
        let tab = ws.tab_mut(id).unwrap();
        tab.model.blocks = blocks;
        (ws, id)
    }

    #[test]
    fn escapes_html_sensitive_text() {
        assert_eq!(
            escape(r#"<script>&"x" '</script>"#),
            "&lt;script&gt;&amp;&quot;x&quot; &#39;&lt;/script&gt;"
        );
    }

    #[test]
    fn sidebar_lists_escaped_tool_names() {
        let html = sidebar_html(&Ok(vec!["read".into(), "<bad>".into()]), None);
        assert!(html.contains("<span class=\"side-count\">2</span>"), "{html}");
        assert!(html.contains("<li>read</li>"), "{html}");
        assert!(html.contains("<li>&lt;bad&gt;</li>"), "{html}");
        assert!(!html.contains("side-cwd"), "{html}");
    }

    #[test]
    fn sidebar_reports_unavailable_tools() {
        let html = sidebar_html(&Err("boom <x>".into()), None);
        assert!(html.contains("tools unavailable: boom &lt;x&gt;"), "{html}");
        assert!(!html.contains("<ul"), "{html}");
    }

    #[test]
    fn sidebar_shows_working_directory_last() {
        let html = sidebar_html(&Ok(vec!["read".into()]), Some("/home/kzhang/dev/GeWu"));
        assert!(html.contains("side-cwd"), "{html}");
        assert!(html.contains("side-path\">/home/kzhang/dev/GeWu</p>"), "{html}");
        assert!(html.contains("title=\"/home/kzhang/dev/GeWu\""), "{html}");
        // The cwd section renders after the tools section.
        let tools_pos = html.find("tools").unwrap();
        let cwd_pos = html.find("side-cwd").unwrap();
        assert!(tools_pos < cwd_pos, "{html}");
    }

    #[test]
    fn sidebar_escapes_working_directory() {
        let html = sidebar_html(&Ok(vec![]), Some("/tmp/<bad>&dir"));
        assert!(html.contains("/tmp/&lt;bad&gt;&amp;dir"), "{html}");
        assert!(!html.contains("<bad>"), "{html}");
    }

    #[test]
    fn renders_user_and_assistant_blocks() {
        let (ws, id) = tab_with_blocks(vec![
            Block::User("<b>hi</b>".into()),
            Block::Assistant("**bold**".into()),
        ]);
        let html = transcript_html(ws.tab(id).unwrap());
        assert!(html.contains("class=\"block user\""), "{html}");
        assert!(!html.contains("class=\"label\""), "{html}");
        assert!(!html.contains(">gewu<"), "{html}");
        assert!(!html.contains(">you<"), "{html}");
        assert!(html.contains("&lt;b&gt;hi&lt;/b&gt;"), "{html}");
        assert!(html.contains("<strong>bold</strong>"), "{html}");
        assert!(!html.contains("<b>hi</b>"), "user HTML must be escaped");
    }

    #[test]
    fn streaming_assistant_is_markdown_and_escaped() {
        let (mut ws, id) = tab_with_blocks(vec![Block::Assistant("<b>partial".into())]);
        ws.tab_mut(id).unwrap().model.phase = Phase::Busy;
        let html = transcript_html(ws.tab(id).unwrap());
        assert!(html.contains("class=\"body markdown streaming\""), "{html}");
        assert!(!html.contains("class=\"label\""), "{html}");
        assert!(!html.contains(">gewu<"), "{html}");
        assert!(html.contains("&lt;b&gt;partial"), "{html}");
        assert!(!html.contains("<b>partial"), "raw HTML must stay escaped");
    }

    #[test]
    fn markdown_raw_html_is_neutralized() {
        let html = markdown_to_html("<script>alert(1)</script>\n\n**ok**");
        assert!(!html.contains("<script>"), "{html}");
        assert!(html.contains("&lt;script&gt;"), "{html}");
        assert!(html.contains("<strong>ok</strong>"), "{html}");
    }

    #[test]
    fn markdown_dangerous_links_are_replaced() {
        let html = markdown_to_html("[x](javascript:alert(1))");
        assert!(!html.contains("javascript:"), "{html}");
        assert!(html.contains("href=\"#\""), "{html}");

        let html = markdown_to_html("[ok](https://example.org)");
        assert!(html.contains("href=\"https://example.org\""), "{html}");
    }

    #[test]
    fn tool_rows_use_readable_names() {
        assert_eq!(tool_row("bash", "ls -la"), ("bash", "ls -la".to_string()));
        assert_eq!(
            tool_row("read_file", "src/a.rs"),
            ("read", "src/a.rs".to_string())
        );
        assert_eq!(tool_row("apply_patch", "p.diff").0, "patch");
        assert_eq!(tool_row("custom_tool", "{}").0, "custom_tool");
    }

    #[test]
    fn single_tool_renders_row_and_output() {
        let (ws, id) = tab_with_blocks(vec![Block::Tool {
            tool: "bash".into(),
            input: "ls".into(),
            output: Some("a.rs\nb.rs".into()),
        }]);
        let html = transcript_html(ws.tab(id).unwrap());
        assert!(html.contains("class=\"tool-card\" data-card=\"0\""), "{html}");
        assert!(html.contains("tool-name\">bash"), "{html}");
        assert!(html.contains("tool-title\">ls"), "{html}");
        assert!(html.contains("a.rs\nb.rs"), "{html}");
        assert!(
            html.contains("class=\"tool depth-0\" data-action=\"toggle_tool\""),
            "the whole card body should be clickable: {html}"
        );
        assert!(html.contains("data-action=\"toggle_tool\""), "{html}");
    }

    #[test]
    fn tool_card_html_renders_one_run() {
        let (mut ws, id) = tab_with_blocks(vec![
            Block::Tool {
                tool: "bash".into(),
                input: "first".into(),
                output: Some("one".into()),
            },
            Block::Assistant("between".into()),
            Block::Tool {
                tool: "bash".into(),
                input: "second".into(),
                output: Some((1..=10).map(|n| format!("line {n}")).collect::<Vec<_>>().join("\n")),
            },
        ]);
        ws.tab_mut(id).unwrap().expanded_outputs.insert(2);
        let html = tool_card_html(ws.tab(id).unwrap(), 2).expect("tool card");
        assert!(html.contains("class=\"tool-card\" data-card=\"2\""), "{html}");
        assert!(html.contains("line 10"), "expanded lines: {html}");
        assert!(!html.contains("first"), "only the requested run: {html}");
        assert!(!html.contains("between"), "{html}");
    }

    #[test]
    fn tool_card_html_needs_a_tool_block() {
        let (ws, id) = tab_with_blocks(vec![Block::Assistant("hi".into())]);
        assert!(tool_card_html(ws.tab(id).unwrap(), 0).is_none());
        assert!(tool_card_html(ws.tab(id).unwrap(), 9).is_none());
    }

    #[test]
    fn block_html_renders_one_block() {
        let (mut ws, id) = tab_with_blocks(vec![
            Block::User("hi".into()),
            Block::Assistant("streaming".into()),
        ]);
        let tab = {
            let tab = ws.tab_mut(id).unwrap();
            tab.model.phase = Phase::Busy;
            tab
        };
        let html = block_html(tab, 0).expect("user block");
        assert!(html.contains("class=\"block user\" data-block=\"0\""), "{html}");
        assert!(!html.contains("streaming"), "{html}");

        let html = block_html(tab, 1).expect("assistant block");
        assert!(html.contains("class=\"block assistant\" data-block=\"1\""), "{html}");
        assert!(html.contains("body markdown streaming"), "{html}");

        assert!(block_html(tab, 9).is_none());
    }

    #[test]
    fn assistant_body_html_renders_final_markdown() {
        let (ws, id) = tab_with_blocks(vec![
            Block::User("hi".into()),
            Block::Assistant("**done**".into()),
        ]);
        let html = assistant_body_html(ws.tab(id).unwrap(), 1, false).expect("body");
        assert!(html.contains("class=\"body markdown\""), "{html}");
        assert!(html.contains("<strong>done</strong>"), "{html}");
        assert!(!html.contains("streaming"), "{html}");

        // Only assistant blocks have a final body.
        assert!(assistant_body_html(ws.tab(id).unwrap(), 0, false).is_none());
        assert!(assistant_body_html(ws.tab(id).unwrap(), 9, false).is_none());
    }
    #[test]
    fn tool_output_collapses_with_expand_button() {
        let output = (1..=10)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let (mut ws, id) = tab_with_blocks(vec![Block::Tool {
            tool: "bash".into(),
            input: "seq 10".into(),
            output: Some(output),
        }]);
        let html = transcript_html(ws.tab(id).unwrap());
        assert!(html.contains("line 6"), "{html}");
        assert!(!html.contains("line 10"), "{html}");
        assert!(html.contains("… +4 lines"), "{html}");

        ws.tab_mut(id).unwrap().expanded_outputs.insert(0);
        let html = transcript_html(ws.tab(id).unwrap());
        assert!(html.contains("line 10"), "{html}");
        assert!(!html.contains("+4 lines"), "{html}");
    }

    #[test]
    fn consecutive_tools_group_and_expand() {
        let (mut ws, id) = tab_with_blocks(vec![
            Block::Tool {
                tool: "bash".into(),
                input: "ls".into(),
                output: Some("a".into()),
            },
            Block::Tool {
                tool: "read_file".into(),
                input: "src/main.rs".into(),
                output: Some("b".into()),
            },
        ]);
        let html = transcript_html(ws.tab(id).unwrap());
        assert!(html.contains("class=\"tool-card\""), "{html}");
        assert!(html.contains("+ Explored: 1 bash, 1 read"), "{html}");
        assert!(!html.contains("src/main.rs"), "{html}");
        assert!(html.contains("data-action=\"toggle_group\""), "{html}");
        assert!(
            html.contains("class=\"tool-group\" data-block=\"0\" data-action=\"toggle_group\""),
            "the whole group card should be clickable: {html}"
        );

        ws.tab_mut(id).unwrap().expanded_groups.insert(0);
        let html = transcript_html(ws.tab(id).unwrap());
        assert!(html.contains("− Explored: 1 bash, 1 read"), "{html}");
        assert!(html.contains("src/main.rs"), "{html}");
    }

    #[test]
    fn running_tool_shows_spinner() {
        let (ws, id) = tab_with_blocks(vec![Block::Tool {
            tool: "bash".into(),
            input: "sleep 30".into(),
            output: None,
        }]);
        let html = transcript_html(ws.tab(id).unwrap());
        assert!(html.contains("class=\"tool running depth-0\""), "{html}");
        assert!(
            html.contains("<span class=\"tool-spinner\" aria-hidden=\"true\"></span>"),
            "{html}"
        );
    }

    #[test]
    fn finished_tool_has_no_running_indicator() {
        let (ws, id) = tab_with_blocks(vec![Block::Tool {
            tool: "bash".into(),
            input: "ls".into(),
            output: Some("a".into()),
        }]);
        let html = transcript_html(ws.tab(id).unwrap());
        assert!(html.contains("class=\"tool depth-0\""), "{html}");
        assert!(!html.contains("tool-spinner"), "{html}");
        assert!(!html.contains("running"), "{html}");
    }

    #[test]
    fn running_group_says_exploring() {
        let (ws, id) = tab_with_blocks(vec![
            Block::Tool {
                tool: "bash".into(),
                input: "ls".into(),
                output: None,
            },
            Block::Tool {
                tool: "grep".into(),
                input: "TODO".into(),
                output: None,
            },
        ]);
        let html = transcript_html(ws.tab(id).unwrap());
        assert!(html.contains("+ Exploring: 1 bash, 1 grep"), "{html}");
        assert!(html.contains("class=\"tool-group running\""), "{html}");
    }

    #[test]
    fn finished_group_has_no_running_class() {
        let (ws, id) = tab_with_blocks(vec![
            Block::Tool {
                tool: "bash".into(),
                input: "ls".into(),
                output: Some("a".into()),
            },
            Block::Tool {
                tool: "grep".into(),
                input: "TODO".into(),
                output: Some("b".into()),
            },
        ]);
        let html = transcript_html(ws.tab(id).unwrap());
        assert!(html.contains("+ Explored: 1 bash, 1 grep"), "{html}");
        assert!(
            html.contains("class=\"tool-group\" data-block=\"0\""),
            "{html}"
        );
        assert!(!html.contains("tool-spinner"), "{html}");
    }

    #[test]
    fn partial_group_marks_remaining_tool_running() {
        // A mixed run: first tool finished, second still running. The group
        // label pulses and the expanded rows show only the pending spinner.
        let (mut ws, id) = tab_with_blocks(vec![
            Block::Tool {
                tool: "bash".into(),
                input: "ls".into(),
                output: Some("a".into()),
            },
            Block::Tool {
                tool: "grep".into(),
                input: "TODO".into(),
                output: None,
            },
        ]);
        ws.tab_mut(id).unwrap().expanded_groups.insert(0);
        let html = transcript_html(ws.tab(id).unwrap());
        assert!(html.contains("class=\"tool-group running\""), "{html}");
        assert!(html.contains("Exploring: 1 bash, 1 grep"), "{html}");
        assert!(html.contains("class=\"tool depth-1\""), "{html}");
        assert!(html.contains("tool-spinner"), "{html}");
    }

    #[test]
    fn status_shows_model_and_usage() {
        let (mut ws, id) = tab_with_blocks(vec![]);
        let tab = ws.tab_mut(id).unwrap();
        tab.model_label = Some("gpt-4o".into());
        tab.context = Some((34_000, 128_000));
        let html = status_html(ws.tab(id).unwrap());
        assert!(html.contains("gpt-4o"), "{html}");
        assert!(!html.contains("openai"), "{html}");
        assert!(!html.contains("session"), "{html}");
        assert!(html.contains("34k / 128k (26%)"), "{html}");
        assert!(html.contains("gauge-fill g25"), "{html}");
        assert!(html.contains("class=\"usage\""), "{html}");
    }

    #[test]
    fn status_usage_warns_near_limit() {
        let (mut ws, id) = tab_with_blocks(vec![]);
        ws.tab_mut(id).unwrap().context = Some((110_000, 128_000));
        let html = status_html(ws.tab(id).unwrap());
        assert!(html.contains("class=\"usage warn\""), "{html}");
        assert!(html.contains("(85%)"), "{html}");
        assert!(html.contains("gauge-fill g85"), "{html}");
    }

    #[test]
    fn busy_status_shows_interrupt_hint() {
        let (mut ws, id) = tab_with_blocks(vec![]);
        ws.tab_mut(id).unwrap().model.submit("go".into());
        let html = status_html(ws.tab(id).unwrap());
        assert!(html.contains("thinking…"), "{html}");
        assert!(html.contains("Esc interrupt"), "{html}");
    }
}
