use async_trait::async_trait;
use gewu_core::{Tool, ToolContext, ToolResult};
use serde::Deserialize;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

#[derive(Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchConfig {
    #[serde(default)]
    pub enabled: bool,
    pub project_root: Option<PathBuf>,
}

impl ResearchConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.enabled {
            let root = self
                .project_root
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("research requires project_root"))?;
            anyhow::ensure!(
                root.is_absolute() && root.is_dir(),
                "project_root must be an existing absolute directory"
            );
            anyhow::ensure!(
                root.canonicalize()?.parent().is_some(),
                "filesystem root is not a project root"
            );
        }
        Ok(())
    }

    pub(crate) fn tools(&self) -> anyhow::Result<Vec<Arc<dyn Tool>>> {
        self.validate()?;
        if !self.enabled {
            return Ok(vec![]);
        }
        let root = self.project_root.as_ref().unwrap().canonicalize()?;
        Ok(["read_file", "list", "glob", "grep"]
            .into_iter()
            .map(|name| {
                Arc::new(ResearchTool {
                    root: root.clone(),
                    name,
                }) as Arc<dyn Tool>
            })
            .collect())
    }
}

pub(crate) fn safe_relative(path: &Path) -> bool {
    path.components().all(|component| match component {
        Component::CurDir => true,
        Component::Normal(name) => {
            let name = name.to_string_lossy().to_ascii_lowercase();
            !name.starts_with('.')
                && ![
                    "secret",
                    "credential",
                    "token",
                    "password",
                    "config",
                    "id_rsa",
                    "id_ed25519",
                ]
                .iter()
                .any(|word| name.contains(word))
                && !["pem", "key", "p12", "pfx", "db", "sqlite", "sqlite3"]
                    .contains(&path_extension(&name))
        }
        _ => false,
    })
}

fn path_extension(name: &str) -> &str {
    name.rsplit_once('.').map_or("", |(_, extension)| extension)
}

pub(crate) fn sensitive_text(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "private key-----",
        "api_key",
        "api-key",
        "apikey",
        "access_token",
        "password=",
        "password:",
        "bearer ",
        "sk-",
        "ghp_",
        "github_pat_",
        "akia",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

struct ResearchTool {
    root: PathBuf,
    name: &'static str,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ResearchArgs {
    #[serde(default = "default_path")]
    path: String,
    pattern: Option<String>,
}

fn default_path() -> String {
    ".".into()
}

impl ResearchTool {
    fn resolve(&self, path: &Path) -> anyhow::Result<PathBuf> {
        anyhow::ensure!(safe_relative(path), "path denied");
        let mut resolved = self.root.clone();
        for part in path.components() {
            resolved.push(part);
            anyhow::ensure!(
                !std::fs::symlink_metadata(&resolved)?
                    .file_type()
                    .is_symlink(),
                "symlinks denied"
            );
        }
        let resolved = resolved.canonicalize()?;
        anyhow::ensure!(resolved.starts_with(&self.root), "path denied");
        Ok(resolved)
    }

    fn read(&self, relative: &Path) -> anyhow::Result<String> {
        let path = self.resolve(relative)?;
        let metadata = std::fs::metadata(&path)?;
        anyhow::ensure!(
            metadata.is_file() && metadata.len() <= 65_536,
            "only regular text files up to 64 KiB are allowed"
        );
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take(65_537)
            .read_to_end(&mut bytes)?;
        anyhow::ensure!(bytes.len() <= 65_536 && !bytes.contains(&0), "file denied");
        let text = String::from_utf8(bytes)?;
        anyhow::ensure!(!sensitive_text(&text), "potential credentials denied");
        Ok(text)
    }

    fn run(&self, args: ResearchArgs) -> anyhow::Result<String> {
        anyhow::ensure!(args.path.len() <= 2048, "path too long");
        let relative = Path::new(&args.path);
        let path = self.resolve(relative)?;
        if self.name == "read_file" {
            return self.read(relative);
        }
        let pattern = args.pattern.unwrap_or_default();
        anyhow::ensure!(pattern.len() <= 1024, "pattern too long");
        let glob = if self.name == "glob" {
            Some(glob::Pattern::new(&pattern)?)
        } else {
            None
        };
        let regex = if self.name == "grep" {
            Some(
                regex::RegexBuilder::new(&pattern)
                    .size_limit(1_048_576)
                    .build()?,
            )
        } else {
            None
        };
        let mut pending = vec![path];
        let mut output = String::new();
        let mut visited = 0;
        while let Some(path) = pending.pop() {
            visited += 1;
            if visited > 2000 || output.len() >= 32_768 {
                break;
            }
            let relative = path.strip_prefix(&self.root)?;
            if !safe_relative(relative) || self.resolve(relative).is_err() {
                continue;
            }
            let metadata = std::fs::symlink_metadata(&path)?;
            if metadata.is_dir() {
                if relative.components().count() < 16 {
                    for entry in std::fs::read_dir(path)?.take(2000).flatten() {
                        if pending.len() < 2000 {
                            pending.push(entry.path());
                        }
                    }
                }
            } else if metadata.is_file() {
                if let Some(regex) = &regex {
                    if let Ok(text) = self.read(relative) {
                        for (index, line) in text.lines().enumerate() {
                            if regex.is_match(line) {
                                output.push_str(&format!(
                                    "{}:{}:{}\n",
                                    relative.display(),
                                    index + 1,
                                    line.chars().take(500).collect::<String>()
                                ));
                                if output.len() >= 32_768 {
                                    break;
                                }
                            }
                        }
                    }
                } else if glob.as_ref().is_none_or(|glob| glob.matches_path(relative)) {
                    output.push_str(&format!("{}\n", relative.display()));
                }
            }
        }
        Ok(output)
    }
}

#[async_trait]
impl Tool for ResearchTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "Read-only project research. Relative paths only; hidden, sensitive and symlink paths denied. Bounded text results; listings and searches may be incomplete. read_file reads path; glob and grep use pattern."
    }
    fn schema(&self) -> schemars::schema::RootSchema {
        schemars::schema_for!(ResearchArgs)
    }
    async fn execute(
        &self,
        args: serde_json::Value,
        _: &ToolContext,
    ) -> Result<ToolResult, Box<dyn std::error::Error + Send + Sync>> {
        let input = serde_json::from_value(args)?;
        let output = self
            .run(input)
            .map_err(|_| "research request denied or unavailable")?;
        Ok(ToolResult::new(self.name, output))
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn default_is_empty_and_enabled_tools_are_read_only_and_confined() {
        assert!(ResearchConfig::default().tools().unwrap().is_empty());
        assert!(
            ResearchConfig {
                enabled: true,
                project_root: None
            }
            .validate()
            .is_err()
        );
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("paper.txt"), "research evidence").unwrap();
        std::fs::write(dir.path().join(".env"), "private").unwrap();
        std::fs::write(dir.path().join("credentials.json"), "private").unwrap();
        std::fs::write(dir.path().join("other.txt"), "api_key=private").unwrap();
        let tools = ResearchConfig {
            enabled: true,
            project_root: Some(dir.path().into()),
        }
        .tools()
        .unwrap();
        assert_eq!(
            tools.iter().map(|tool| tool.name()).collect::<Vec<_>>(),
            ["read_file", "list", "glob", "grep"]
        );
        let ctx = ToolContext::new("lab", "test");
        let read = &tools[0];
        assert!(
            read.execute(serde_json::json!({"path":"paper.txt"}), &ctx)
                .await
                .unwrap()
                .output
                .contains("evidence")
        );
        for path in [
            "../outside",
            "/etc/passwd",
            ".env",
            "credentials.json",
            "other.txt",
        ] {
            assert!(
                read.execute(serde_json::json!({"path":path}), &ctx)
                    .await
                    .is_err()
            );
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc", dir.path().join("escape")).unwrap();
            assert!(
                read.execute(serde_json::json!({"path":"escape/passwd"}), &ctx)
                    .await
                    .is_err()
            );
        }
        for tool in tools.iter().skip(1) {
            let result = tool.execute(serde_json::json!({"pattern":"*"}), &ctx).await;
            if tool.name() == "grep" {
                continue;
            }
            let output = result.unwrap().output;
            assert!(output.contains("paper.txt"));
            assert!(!output.contains("credentials"));
            assert!(!output.contains(".env"));
        }
        let output = tools[3]
            .execute(serde_json::json!({"pattern":"private|evidence"}), &ctx)
            .await
            .unwrap()
            .output;
        assert!(output.contains("evidence"));
        assert!(!output.contains("private"));
    }
}
