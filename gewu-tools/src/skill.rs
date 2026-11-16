//! Skill tool.
//!
//! This module provides the [`SkillTool`] which loads specialized skill
//! instructions into the conversation. Skills are markdown files named
//! `SKILL.md` with YAML frontmatter (`name`, optional `description`)
//! discovered from config-declared directories.
//!
//! # Tool Definition
//!
//! - **Name**: `skill`
//! - **Parameters**:
//!   - `name` (required): The name of the skill from available_skills

use async_trait::async_trait;
use gewu_core::traits::Tool;
use gewu_core::types::{ToolContext, ToolResult};
use schemars::schema::RootSchema;
use serde::Deserialize;
use std::error::Error;
use std::path::{Path, PathBuf};
use tracing::warn;

/// Maximum number of companion files sampled into the skill output.
const SKILL_FILE_LIMIT: usize = 10;

/// A discovered skill.
#[derive(Debug, Clone)]
pub struct SkillInfo {
    pub name: String,
    pub description: Option<String>,
    /// Path to the skill's `SKILL.md`.
    pub location: PathBuf,
    /// Markdown body below the frontmatter.
    pub content: String,
}

/// Parsed YAML frontmatter. Unknown keys are ignored.
#[derive(Debug, Deserialize)]
struct SkillFrontmatter {
    name: String,
    #[serde(default)]
    description: Option<String>,
}

/// Splits markdown frontmatter (`key: value` lines between leading `---`
/// fences) from the body. Returns `None` when there is no frontmatter.
fn split_frontmatter(content: &str) -> Option<(&str, &str)> {
    let rest = content.strip_prefix("---\n")?;
    let end = rest.find("\n---")?;
    let (front, body) = rest.split_at(end);
    let body = body.strip_prefix("\n---")?;
    let body = body.strip_prefix('\n').unwrap_or(body);
    Some((front, body))
}

/// Scans directories for `SKILL.md` files and parses their frontmatter.
///
/// The `name` frontmatter key is required and files with invalid frontmatter
/// are skipped. Directories are processed in the given order and entries
/// within each directory are sorted, so duplicate names deterministically
/// let the later path win.
pub fn discover(directories: &[PathBuf]) -> Vec<SkillInfo> {
    let mut skills: Vec<SkillInfo> = Vec::new();
    for directory in directories {
        for entry in ignore::WalkBuilder::new(directory)
            .hidden(false)
            .follow_links(false)
            .sort_by_file_path(|a, b| a.cmp(b))
            .build()
        {
            let Ok(entry) = entry else { continue };
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            if entry.file_name() != "SKILL.md" {
                continue;
            }
            let location = entry.into_path();
            let Ok(content) = std::fs::read_to_string(&location) else {
                continue;
            };
            let Some((front, body)) = split_frontmatter(&content) else {
                continue;
            };
            let frontmatter: SkillFrontmatter = match yaml_serde::from_str(front) {
                Ok(frontmatter) => frontmatter,
                Err(error) => {
                    warn!(
                        "skipping skill {}: invalid frontmatter: {error}",
                        location.display()
                    );
                    continue;
                }
            };
            let skill = SkillInfo {
                name: frontmatter.name.clone(),
                description: frontmatter.description,
                location,
                content: body.to_string(),
            };
            if let Some(existing) = skills.iter_mut().find(|s| s.name == skill.name) {
                warn!(
                    "duplicate skill name {} ({} overrides {})",
                    skill.name,
                    skill.location.display(),
                    existing.location.display()
                );
                *existing = skill;
            } else {
                skills.push(skill);
            }
        }
    }
    skills
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Renders the system-prompt section describing available skills.
pub fn available_skills_section(skills: &[SkillInfo]) -> String {
    let mut described: Vec<&SkillInfo> = skills
        .iter()
        .filter(|skill| skill.description.is_some())
        .collect();
    if described.is_empty() {
        return "No skills are currently available.".to_string();
    }
    described.sort_by(|a, b| a.name.cmp(&b.name));

    let mut out = String::from(
        "Skills provide specialized instructions and workflows for specific tasks.\n\
         Use the skill tool to load a skill when a task matches its description.\n\
         <available_skills>",
    );
    for skill in described {
        out.push_str(&format!(
            "\n  <skill>\n    <name>{}</name>\n    <description>{}</description>\n    <location>{}</location>\n  </skill>",
            escape_html(&skill.name),
            escape_html(skill.description.as_deref().unwrap_or("")),
            escape_html(&skill.location.display().to_string()),
        ));
    }
    out.push_str("\n</available_skills>");
    out
}

/// Lists up to [`SKILL_FILE_LIMIT`] companion files in a skill's directory,
/// excluding the `SKILL.md` itself.
fn list_skill_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in ignore::WalkBuilder::new(dir)
        .hidden(false)
        .follow_links(false)
        .sort_by_file_path(|a, b| a.cmp(b))
        .build()
    {
        if files.len() >= SKILL_FILE_LIMIT {
            break;
        }
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        if entry.file_name() == "SKILL.md" {
            continue;
        }
        files.push(entry.into_path());
    }
    files
}

pub struct SkillTool {
    skills: Vec<SkillInfo>,
    /// Cached description built from the discovered skills: static text
    /// leaves the model unaware of what exists, and it skips the tool.
    description: String,
    schema: RootSchema,
}

#[derive(Deserialize)]
struct SkillArgs {
    name: String,
}

/// Builds the tool description from the discovered skills, so a model
/// choosing tools sees the available names and descriptions.
fn skill_description(skills: &[SkillInfo]) -> String {
    let mut out = String::from(
        "Load a specialized skill when the task at hand matches one of the skills \
         listed below.\n\n\
         Use this tool to inject the skill's instructions and resources into the \
         current conversation. The output may contain detailed workflow guidance as \
         well as references to scripts, files, etc in the same directory as the skill.\n",
    );
    let mut sorted: Vec<&SkillInfo> = skills.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    out.push_str("\nAvailable skills:\n");
    let mut listed = 0;
    for skill in sorted {
        let Some(summary) = skill
            .description
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        else {
            continue;
        };
        out.push_str(&format!("- {}: {}\n", skill.name, summary.trim()));
        listed += 1;
    }
    if listed == 0 {
        out.push_str("- (no described skills are available)\n");
    }
    out
}

/// The `name` argument schema: an enum of the discovered skill names when
/// any exist, so an invalid name is rejected before execution.
fn schema(skills: &[SkillInfo]) -> RootSchema {
    let mut names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
    names.sort_unstable();
    let name = if names.is_empty() {
        serde_json::json!({
            "type": "string",
            "description": "Skill name from the available skills list"
        })
    } else {
        serde_json::json!({
            "type": "string",
            "description": "Skill name from the available skills list",
            "enum": names
        })
    };
    serde_json::from_value(serde_json::json!({
        "type": "object",
        "properties": {
            "name": name
        },
        "required": ["name"]
    }))
    .unwrap()
}

impl SkillTool {
    pub fn new(skills: Vec<SkillInfo>) -> Self {
        let description = skill_description(&skills);
        let schema = schema(&skills);
        Self {
            skills,
            description,
            schema,
        }
    }
}

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str {
        "skill"
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn schema(&self) -> RootSchema {
        self.schema.clone()
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, Box<dyn Error + Send + Sync>> {
        let input: SkillArgs = serde_json::from_value(args)?;

        let info = self
            .skills
            .iter()
            .find(|s| s.name == input.name)
            .ok_or_else(|| {
                let mut available: Vec<&str> =
                    self.skills.iter().map(|s| s.name.as_str()).collect();
                available.sort_unstable();
                let available = if available.is_empty() {
                    "none".to_string()
                } else {
                    available.join(", ")
                };
                format!(
                    "Skill \"{}\" not found. Available skills: {}",
                    input.name, available
                )
            })?;

        let dir = info
            .location
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let files = list_skill_files(&dir);
        let file_list = files
            .iter()
            .map(|f| format!("<file>{}</file>", f.display()))
            .collect::<Vec<_>>()
            .join("\n");

        let output = format!(
            "<skill_content name=\"{name}\">\n\
             # Skill: {name}\n\
             \n\
             {content}\n\
             \n\
             Base directory for this skill: {dir}\n\
             Relative paths in this skill (e.g., scripts/, reference/) are relative to this base directory.\n\
             Note: file list is sampled.\n\
             \n\
             <skill_files>\n\
             {file_list}\n\
             </skill_files>\n\
             </skill_content>",
            name = info.name,
            content = info.content.trim(),
            dir = dir.display(),
            file_list = file_list,
        );

        let mut result = ToolResult::new(format!("Loaded skill: {}", info.name), output);
        result
            .metadata
            .insert("name".to_string(), serde_json::json!(info.name));
        result.metadata.insert(
            "dir".to_string(),
            serde_json::json!(dir.display().to_string()),
        );
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_skill(dir: &Path, subdir: &str, frontmatter: &str, body: &str) -> PathBuf {
        let skill_dir = dir.join(subdir);
        fs::create_dir_all(&skill_dir).unwrap();
        let location = skill_dir.join("SKILL.md");
        fs::write(&location, format!("---\n{frontmatter}\n---\n{body}")).unwrap();
        location
    }

    #[test]
    fn split_frontmatter_requires_fences() {
        assert!(split_frontmatter("---\nname: a\n---\nbody").is_some());
        assert!(split_frontmatter("no frontmatter").is_none());
        assert!(split_frontmatter("---\nname: a\nbody without close").is_none());
    }

    #[test]
    fn discover_parses_yaml_frontmatter() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        write_skill(
            &root,
            "demo",
            "name: \"my-skill\"\ndescription: 'Does things'\nversion: 2\n\
             references:\n  - workers\n  - pages\n# comment",
            "body",
        );

        let skills = discover(&[root]);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "my-skill");
        assert_eq!(skills[0].description.as_deref(), Some("Does things"));
    }

    #[test]
    fn discover_supports_literal_block_description() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        write_skill(
            &root,
            "multiline",
            "name: multiline\ndescription: |\n  First line.\n\n  Second paragraph.",
            "body",
        );

        let skills = discover(&[root]);
        assert_eq!(
            skills[0].description.as_deref(),
            Some("First line.\n\nSecond paragraph.")
        );
    }

    #[test]
    fn discover_supports_folded_block_description() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        write_skill(
            &root,
            "folded",
            "name: folded\ndescription: >\n  Folded text\n  across two lines.",
            "body",
        );

        let skills = discover(&[root]);
        assert_eq!(
            skills[0].description.as_deref(),
            Some("Folded text across two lines.")
        );
    }

    #[test]
    fn discover_skips_invalid_frontmatter() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        write_skill(&root, "broken", "name: [unclosed", "body");
        write_skill(&root, "valid", "name: valid", "body");

        let skills = discover(&[root]);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "valid");
    }

    #[test]
    fn discover_finds_nested_skills_and_requires_name() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        write_skill(
            &root,
            "alpha",
            "name: alpha\ndescription: First skill",
            "Alpha body",
        );
        write_skill(
            &root,
            "nested/beta",
            "name: \"beta\"\ndescription: \"Second skill\"",
            "Beta body",
        );
        write_skill(&root, "nameless", "description: no name", "skipped");
        write_skill(&root, "plain", "just markdown", "skipped");

        let skills = discover(&[root]);
        let mut names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, vec!["alpha", "beta"]);
        let alpha = skills.iter().find(|s| s.name == "alpha").unwrap();
        assert_eq!(alpha.description.as_deref(), Some("First skill"));
        assert_eq!(alpha.content.trim(), "Alpha body");
    }

    #[test]
    fn discover_duplicate_names_later_wins() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        write_skill(&root, "first", "name: dup\ndescription: first", "one");
        write_skill(&root, "second", "name: dup\ndescription: second", "two");

        let skills = discover(&[root]);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].description.as_deref(), Some("second"));
        assert!(skills[0].location.ends_with("second/SKILL.md"));
    }

    #[test]
    fn discover_duplicate_names_does_not_depend_on_creation_order() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        write_skill(&root, "second", "name: dup\ndescription: second", "two");
        write_skill(&root, "first", "name: dup\ndescription: first", "one");

        let skills = discover(&[root]);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].description.as_deref(), Some("second"));
        assert!(skills[0].location.ends_with("second/SKILL.md"));
    }

    #[tokio::test]
    async fn tool_executes_found_skill_with_sampled_files() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        write_skill(
            &root,
            "demo",
            "name: demo\ndescription: Demo skill",
            "Do the demo workflow",
        );
        fs::write(root.join("demo").join("run.sh"), "#!/bin/sh\necho hi").unwrap();
        fs::write(root.join("demo").join("notes.txt"), "notes").unwrap();

        let tool = SkillTool::new(discover(&[root]));
        let ctx = ToolContext::new("s", "m");
        let result = tool
            .execute(serde_json::json!({ "name": "demo" }), &ctx)
            .await
            .expect("skill should load");

        assert_eq!(result.title, "Loaded skill: demo");
        assert!(result.output.contains("<skill_content name=\"demo\">"));
        assert!(result.output.contains("Do the demo workflow"));
        assert!(result.output.contains("Base directory for this skill:"));
        assert!(result.output.contains("<file>"));
        assert!(result.metadata["name"] == "demo");
    }

    #[tokio::test]
    async fn tool_limits_sampled_files() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        write_skill(&root, "many", "name: many", "body");
        for i in 0..15 {
            fs::write(
                root.join("many").join(format!("file{i}.txt")),
                i.to_string(),
            )
            .unwrap();
        }

        let tool = SkillTool::new(discover(&[root]));
        let ctx = ToolContext::new("s", "m");
        let result = tool
            .execute(serde_json::json!({ "name": "many" }), &ctx)
            .await
            .expect("skill should load");

        assert_eq!(result.output.matches("<file>").count(), SKILL_FILE_LIMIT);
    }

    #[tokio::test]
    async fn tool_not_found_lists_available() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        write_skill(&root, "alpha", "name: alpha\ndescription: A", "body");

        let tool = SkillTool::new(discover(&[root]));
        let ctx = ToolContext::new("s", "m");
        let error = tool
            .execute(serde_json::json!({ "name": "missing" }), &ctx)
            .await
            .expect_err("unknown skill should fail");

        assert!(error.to_string().contains("Skill \"missing\" not found"));
        assert!(error.to_string().contains("alpha"));
    }

    #[test]
    fn prompt_section_renders_skills() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        write_skill(
            &root,
            "alpha",
            "name: alpha\ndescription: Does <things>",
            "body",
        );

        let skills = discover(&[root]);
        let section = available_skills_section(&skills);
        assert!(section.contains("<available_skills>"));
        assert!(section.contains("<name>alpha</name>"));
        assert!(section.contains("Does &lt;things&gt;"));
        assert!(section.contains("<location>"));
    }

    #[test]
    fn prompt_section_without_descriptions() {
        assert_eq!(
            available_skills_section(&[]),
            "No skills are currently available."
        );
    }

    #[test]
    fn tool_description_and_schema_name_the_discovered_skills() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        write_skill(&root, "beta", "name: beta\ndescription: Second", "b");
        write_skill(&root, "alpha", "name: alpha\ndescription: First", "a");

        let tool = SkillTool::new(discover(&[root]));
        let description = tool.description();
        assert!(description.contains("- alpha: First"), "{description}");
        assert!(description.contains("- beta: Second"), "{description}");
        // Sorted by name.
        let alpha = description.find("- alpha").unwrap();
        let beta = description.find("- beta").unwrap();
        assert!(alpha < beta, "{description}");

        let schema = serde_json::to_value(tool.schema()).unwrap();
        assert_eq!(
            schema["properties"]["name"]["enum"],
            serde_json::json!(["alpha", "beta"])
        );
    }

    #[test]
    fn tool_description_without_skills_has_no_enum() {
        let tool = SkillTool::new(Vec::new());
        assert!(
            tool.description()
                .contains("(no described skills are available)")
        );
        let schema = serde_json::to_value(tool.schema()).unwrap();
        assert!(schema["properties"]["name"].get("enum").is_none());
    }

    #[test]
    fn undescribed_skills_stay_out_of_the_description_but_in_the_enum() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        write_skill(&root, "beta", "name: beta", "b");
        let tool = SkillTool::new(discover(&[root]));
        assert!(
            tool.description()
                .contains("(no described skills are available)"),
            "{}",
            tool.description()
        );
        assert!(!tool.description().contains("- beta"));
        let schema = serde_json::to_value(tool.schema()).unwrap();
        assert_eq!(
            schema["properties"]["name"]["enum"],
            serde_json::json!(["beta"])
        );
    }
}
