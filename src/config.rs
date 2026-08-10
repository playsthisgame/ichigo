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

/// The list pane's share of the TUI width when the file does not say. Also what
/// a file that fails to load falls back to.
pub const DEFAULT_SPLIT_PCT: u16 = 35;

/// Bounds on that share. Neither pane may be dragged — or configured — narrower
/// than its two border columns plus something to put between them. An
/// out-of-range value in the file is refused rather than clamped, for the same
/// reason `deny_unknown_fields` refuses a misspelling: a config that silently
/// means something other than what it says is the thing this file avoids.
pub const MIN_SPLIT_PCT: u16 = 15;
pub const MAX_SPLIT_PCT: u16 = 85;

/// The file exactly as written. Every field is optional so adding one never
/// invalidates an existing file, and `deny_unknown_fields` means a misspelling
/// is refused instead of ignored, which is the difference between a typo you
/// find in a second and one you edit around forever.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct UserConfigFile {
    #[serde(default)]
    keys: KeysFile,
    #[serde(default)]
    layout: LayoutFile,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeysFile {
    insert_escape: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct LayoutFile {
    split_pct: Option<u16>,
}

/// The user config, resolved and validated: the keymap plus everything that is
/// not one. `Keys` stays its own type because the TUI's form handlers take it
/// by copy while holding a `&mut` borrow of `App::mode`; the rest of the file
/// is read once at startup and never needed in that position.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UserConfig {
    pub keys: Keys,
    /// The list pane's share of the TUI width, as a percentage. A percentage
    /// rather than a column count so the ratio survives a terminal resize —
    /// and so a config written on one terminal means the same on another.
    pub split_pct: u16,
}

impl UserConfig {
    /// Reads `~/.config/ichigo/config.toml`, or the defaults when there is none.
    pub fn load() -> Result<Self> {
        let path = global_dir().join(USER_CONFIG_FILE);
        let Ok(content) = fs::read_to_string(&path) else {
            return Ok(Self::defaults());
        };
        let file: UserConfigFile = basic_toml::from_str(&content)
            .with_context(|| format!("Invalid TOML in {}", path.display()))?;
        Self::from_file(file)
    }

    /// What every field means when the file does not say. Not `Default`, which
    /// derives a `split_pct` of 0 — a value the validator would reject.
    pub fn defaults() -> Self {
        Self { keys: Keys::default(), split_pct: DEFAULT_SPLIT_PCT }
    }

    fn from_file(file: UserConfigFile) -> Result<Self> {
        let split_pct = match file.layout.split_pct {
            None => DEFAULT_SPLIT_PCT,
            Some(pct) if (MIN_SPLIT_PCT..=MAX_SPLIT_PCT).contains(&pct) => pct,
            Some(pct) => bail!(
                "layout.split_pct must be between {MIN_SPLIT_PCT} and {MAX_SPLIT_PCT} \
                 (got {pct}); it is the request list's share of the width as a percentage"
            ),
        };
        Ok(Self { keys: Keys::from_file(file.keys)?, split_pct })
    }
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

    fn parse_config(toml: &str) -> Result<UserConfig> {
        let file: UserConfigFile = basic_toml::from_str(toml)?;
        UserConfig::from_file(file)
    }

    fn parse(toml: &str) -> Result<Keys> {
        parse_config(toml).map(|c| c.keys)
    }

    #[test]
    fn an_empty_user_config_gives_the_defaults() {
        assert_eq!(parse_config("").unwrap(), UserConfig::defaults());
        assert_eq!(parse_config("[keys]\n").unwrap(), UserConfig::defaults());
        assert_eq!(parse_config("[layout]\n").unwrap(), UserConfig::defaults());
    }

    /// `Default` derives a `split_pct` of 0, which the validator rejects — the
    /// defaults have to come from `defaults()`, not from deriving them.
    #[test]
    fn the_default_split_is_within_its_own_bounds() {
        let pct = UserConfig::defaults().split_pct;
        assert!((MIN_SPLIT_PCT..=MAX_SPLIT_PCT).contains(&pct));
        assert_eq!(pct, DEFAULT_SPLIT_PCT);
    }

    #[test]
    fn a_split_percentage_is_read_from_the_file() {
        assert_eq!(parse_config("[layout]\nsplit_pct = 50\n").unwrap().split_pct, 50);
        // The bounds themselves are accepted.
        for pct in [MIN_SPLIT_PCT, MAX_SPLIT_PCT] {
            let toml = format!("[layout]\nsplit_pct = {pct}\n");
            assert_eq!(parse_config(&toml).unwrap().split_pct, pct);
        }
    }

    /// Refused rather than clamped: a file that silently means something other
    /// than what it says is what `deny_unknown_fields` exists to prevent.
    #[test]
    fn an_out_of_range_split_is_refused_by_name() {
        for pct in [0, 1, MIN_SPLIT_PCT - 1, MAX_SPLIT_PCT + 1, 100, 1000] {
            let toml = format!("[layout]\nsplit_pct = {pct}\n");
            let err = parse_config(&toml).unwrap_err();
            assert!(
                err.to_string().contains("layout.split_pct must be between"),
                "{pct} gave: {err}"
            );
        }
    }

    #[test]
    fn a_misspelled_layout_key_is_refused_rather_than_ignored() {
        assert!(parse_config("[layout]\nsplit_pc = 50\n").is_err());
        assert!(parse_config("[laoyut]\nsplit_pct = 50\n").is_err());
    }

    /// The two sections are independent: setting one leaves the other's default.
    #[test]
    fn the_sections_do_not_disturb_each_other() {
        let c = parse_config("[keys]\ninsert_escape = \"jk\"\n").unwrap();
        assert_eq!(c.split_pct, DEFAULT_SPLIT_PCT);
        let c = parse_config("[layout]\nsplit_pct = 60\n").unwrap();
        assert_eq!(c.keys, Keys::default());
        let c = parse_config("[keys]\ninsert_escape = \"jk\"\n[layout]\nsplit_pct = 60\n").unwrap();
        assert_eq!(c.keys.insert_escape, Some(('j', 'k')));
        assert_eq!(c.split_pct, 60);
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
