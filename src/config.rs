use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_yaml;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

pub const OXIDE_DIR: &str = ".oxide";

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct RequestConfig {
    pub name: String,
    pub method: String,
    pub url: String,
    pub description: Option<String>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub query: HashMap<String, String>,
    pub body: Option<Body>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Body {
    pub content_type: String,
    pub data: String,
}

impl RequestConfig {
    pub fn load(name: &str) -> Result<Self> {
        let path = config_path(name);
        let content = fs::read_to_string(&path)
            .with_context(|| format!("Request '{}' not found (looked in {})", name, path.display()))?;
        serde_yaml::from_str(&content)
            .with_context(|| format!("Invalid YAML in request config '{}'", name))
    }
}

pub fn config_path(name: &str) -> PathBuf {
    PathBuf::from(OXIDE_DIR).join(format!("{}.yaml", name))
}

pub fn list_requests() -> Result<Vec<String>> {
    let dir = PathBuf::from(OXIDE_DIR);
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut names = Vec::new();
    for entry in fs::read_dir(&dir).context("Failed to read .oxide directory")? {
        let path = entry?.path();
        if path.extension().map_or(false, |e| e == "yaml") {
            if let Some(stem) = path.file_stem() {
                names.push(stem.to_string_lossy().into_owned());
            }
        }
    }
    names.sort();
    Ok(names)
}
