use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize, Default)]
pub struct Config {
    pub endpoint: Option<String>,
    pub api_key: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug)]
pub struct LoadedConfig {
    pub config: Config,
    pub path: Option<PathBuf>,
}

impl LoadedConfig {
    pub fn load(config_path: Option<&str>) -> Result<Self> {
        let path = resolve_config_path(config_path)?;

        if !path.exists() {
            return Ok(LoadedConfig {
                config: Config::default(),
                path: None,
            });
        }

        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read config file: {}", path.display()))?;

        let config: Config = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse config file: {}", path.display()))?;

        Ok(LoadedConfig {
            config,
            path: Some(path),
        })
    }
}

fn resolve_config_path(config_path: Option<&str>) -> Result<PathBuf> {
    if let Some(path) = config_path {
        return Ok(PathBuf::from(path));
    }

    let project_config = PathBuf::from("config.json");
    if project_config.exists() {
        return Ok(project_config);
    }

    let home_dir = dirs::home_dir().context("Failed to determine home directory")?;
    let home_config = home_dir.join(".gewu").join("config.json");
    if home_config.exists() {
        return Ok(home_config);
    }

    Ok(home_config)
}
