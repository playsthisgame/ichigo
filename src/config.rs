use anyhow::{bail, Context, Result};
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

/// User preferences, in the same directory as the global requests.
///
/// TOML rather than YAML, and that extension is load-bearing rather than a
/// matter of taste: requests are `<name>.yaml`, so a `.toml` file cannot be
/// mistaken for one. `collect_yaml_entries` skips it without being told to, and
/// a request legitimately named `config` writes `config.yaml` and cannot
/// collide with it. Teach the request loader to read `.toml` and both of those
/// stop being true at once.
pub const USER_CONFIG_FILE: &str = "config.toml";

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
    pub profiles: Option<Vec<Profile>>,
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
        serde_yaml_ng::from_str(&content)
            .with_context(|| format!("Invalid YAML in request config '{}'", name))
    }
}

impl ChainConfig {
    pub fn load(name: &str) -> Result<Self> {
        let path = resolve_config_path(name)
            .with_context(|| format!("Request '{}' not found", name))?;
        let content = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        serde_yaml_ng::from_str(&content)
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

// ─── User config ──────────────────────────────────────────────────────────────
//
// Preferences, as opposed to requests. Read once at launch from
// `~/.config/ichigo/config.toml`; there is no project-local override, because
// these are preferences about how *you* type rather than about a project.
//
// A missing file is not an error. A malformed one is, and the TUI opens on the
// message rather than silently running on defaults — a keymap that quietly does
// nothing is worse than one that says why.

/// The file exactly as written. Every field is optional so adding one never
/// invalidates an existing file, and `deny_unknown_fields` means a misspelling
/// is refused instead of ignored, which is the difference between a typo you
/// find in a second and one you edit around forever.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct UserConfigFile {
    #[serde(default)]
    keys: KeysFile,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeysFile {
    insert_escape: Option<String>,
}

/// Key preferences, resolved and validated.
///
/// `Copy` on purpose: the TUI's form handlers need it while holding a mutable
/// borrow of `App::mode`, and copying it out first is what lets both happen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Keys {
    /// A two-key sequence that leaves insert mode — vim's `inoremap jk <Esc>`.
    /// `None` when unset, in which case only `Esc` does it.
    pub insert_escape: Option<(char, char)>,
}

impl Keys {
    /// Reads `~/.config/ichigo/config.toml`, or the defaults when there is none.
    pub fn load() -> Result<Self> {
        let path = global_dir().join(USER_CONFIG_FILE);
        let Ok(content) = fs::read_to_string(&path) else {
            return Ok(Self::default());
        };
        let file: UserConfigFile = basic_toml::from_str(&content)
            .with_context(|| format!("Invalid TOML in {}", path.display()))?;
        Self::from_file(file.keys)
    }

    fn from_file(keys: KeysFile) -> Result<Self> {
        let insert_escape = match keys.insert_escape {
            None => None,
            Some(seq) => {
                // Exactly two, because the pending first key is one `char` in
                // `Edit` — which stays `Copy` only as long as that is true.
                let mut chars = seq.chars();
                match (chars.next(), chars.next(), chars.next()) {
                    (Some(a), Some(b), None) => Some((a, b)),
                    _ => bail!(
                        "keys.insert_escape must be exactly two characters (got {seq:?}); \
                         omit the key to leave Esc as the only way out of insert mode"
                    ),
                }
            }
        };
        Ok(Self { insert_escape })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(toml: &str) -> Result<Keys> {
        let file: UserConfigFile = basic_toml::from_str(toml)?;
        Keys::from_file(file.keys)
    }

    #[test]
    fn an_empty_user_config_gives_the_defaults() {
        assert_eq!(parse("").unwrap(), Keys::default());
        assert_eq!(parse("[keys]\n").unwrap(), Keys::default());
    }

    #[test]
    fn a_two_key_sequence_is_accepted() {
        let keys = parse("[keys]\ninsert_escape = \"jk\"\n").unwrap();
        assert_eq!(keys.insert_escape, Some(('j', 'k')));
    }

    #[test]
    fn a_sequence_of_the_wrong_length_is_refused_by_name() {
        for seq in ["j", "jkl", ""] {
            let err = parse(&format!("[keys]\ninsert_escape = {seq:?}\n")).unwrap_err();
            assert!(
                err.to_string().contains("exactly two characters"),
                "{seq:?} gave: {err}"
            );
        }
    }

    #[test]
    fn multibyte_sequences_count_characters_not_bytes() {
        let keys = parse("[keys]\ninsert_escape = \"éø\"\n").unwrap();
        assert_eq!(keys.insert_escape, Some(('é', 'ø')));
    }

    #[test]
    fn a_misspelled_key_is_refused_rather_than_ignored() {
        assert!(parse("[keys]\ninsert_esc = \"jk\"\n").is_err());
        assert!(parse("[kyes]\ninsert_escape = \"jk\"\n").is_err());
    }

    /// The whole reason the file is TOML: it cannot be mistaken for a request.
    #[test]
    fn the_user_config_is_not_a_request_file() {
        assert!(!USER_CONFIG_FILE.ends_with(".yaml"));
        // A request actually named `config` writes beside it, not over it.
        assert_ne!(global_config_path("config").file_name().unwrap(), USER_CONFIG_FILE);
    }
}
