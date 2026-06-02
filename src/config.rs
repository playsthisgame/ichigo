use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

pub fn local_dir() -> PathBuf {
    PathBuf::from(".ichigo")
}

pub fn global_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".config").join("ichigo")
}

pub(crate) fn dir_for(global: bool) -> PathBuf {
    if global { global_dir() } else { local_dir() }
}

pub struct RequestEntry {
    pub name: String,
    pub global: bool,
}

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
    pub extract: Option<HashMap<String,String>>,
    pub profiles: Option<Vec<Profile>>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Body {
    pub content_type: String,
    pub data: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ChainConfig {
    pub name: String,
    pub steps: Vec<RequestConfig>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Profile {
    pub name: String,
    pub params: HashMap<String,String>,
}

impl RequestConfig {
    pub fn load(name: &str) -> Result<Self> {
        let path = resolve_config_path(name)
            .with_context(|| format!("Request '{}' not found", name))?;
        let content = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        serde_yaml::from_str(&content)
            .with_context(|| format!("Invalid YAML in request config '{}'", name))
    }
}

impl ChainConfig {
    pub fn load(name: &str) -> Result<Self> {
        let path = resolve_config_path(name)
            .with_context(|| format!("Request '{}' not found", name))?;
        let content = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        serde_yaml::from_str(&content)
            .with_context(|| format!("Invalid YAML in chain config '{}'", name))
    }
}

pub fn local_config_path(name: &str) -> PathBuf {
    local_dir().join(format!("{}.yaml", name))
}

pub fn global_config_path(name: &str) -> PathBuf {
    global_dir().join(format!("{}.yaml", name))
}

pub fn resolve_config_path(name: &str) -> Option<PathBuf> {
    let local = local_config_path(name);
    if local.exists() {
        return Some(local);
    }
    let global = global_config_path(name);
    if global.exists() {
        return Some(global);
    }
    None
}

pub fn list_requests() -> Result<Vec<RequestEntry>> {
    let mut entries: Vec<RequestEntry> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    let local = local_dir();
    if local.exists() {
        collect_yaml_entries(&local, &local, false, &mut seen, &mut entries);
    }

    let global = global_dir();
    if global.exists() {
        collect_yaml_entries(&global, &global, true, &mut seen, &mut entries);
    }

    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

fn collect_yaml_entries(
    dir: &std::path::Path,
    base: &std::path::Path,
    global: bool,
    seen: &mut HashSet<String>,
    entries: &mut Vec<RequestEntry>,
) {
    let Ok(read) = fs::read_dir(dir) else { return };
    for entry in read.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_yaml_entries(&path, base, global, seen, entries);
        } else if path.extension().is_some_and(|e| e == "yaml")
            && let Ok(rel) = path.strip_prefix(base) {
                let name = rel.with_extension("").to_string_lossy().into_owned();
                if seen.insert(name.clone()) {
                    entries.push(RequestEntry { name, global });
                }
            }
    }
}

pub(crate) fn prune_empty_parents(path: &std::path::Path, base: &std::path::Path) {
    for dir in path.ancestors().skip(1) {
        if dir == base {
            break;
        }
        let is_empty = fs::read_dir(dir).is_ok_and(|mut d| d.next().is_none());
        if is_empty {
            fs::remove_dir(dir).ok();
        } else {
            break;
        }
    }
}
