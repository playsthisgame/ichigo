use anyhow::Result;
use std::{collections::{HashMap, HashSet}, fs};
use crate::config::{list_requests, resolve_config_path, ChainConfig, RequestConfig};

// ─── Data ─────────────────────────────────────────────────────────────────────

pub(crate) enum EntryKind {
    Request {
        method: String,
        url: String,
        description: Option<String>,
        headers: HashMap<String, String>,
        query: HashMap<String, String>,
        body_data: Option<String>,
        profiles: Vec<crate::config::Profile>,
    },
    Chain {
        steps: Vec<String>,
    },
}

pub(crate) struct Entry {
    pub(crate) name: String,
    pub(crate) global: bool,
    pub(crate) kind: EntryKind,
}

pub(crate) fn load_entries() -> Result<Vec<Entry>> {
    let raw = list_requests()?;
    let mut entries = Vec::new();
    for r in raw {
        let path = resolve_config_path(&r.name).unwrap();
        let content = fs::read_to_string(&path)?;
        let kind = if content.contains("steps:") {
            let chain: ChainConfig = serde_yaml::from_str(&content)
                .unwrap_or(ChainConfig { name: r.name.clone(), steps: vec![], profiles: None });
            EntryKind::Chain {
                steps: chain.steps.iter().map(|s| s.name.clone()).collect(),
            }
        } else {
            let config: RequestConfig = serde_yaml::from_str(&content)?;
            EntryKind::Request {
                method: config.method,
                url: config.url,
                description: config.description,
                headers: config.headers,
                query: config.query,
                body_data: config.body.map(|b| b.data),
                profiles: config.profiles.unwrap_or_default(),
            }
        };
        entries.push(Entry { name: r.name, global: r.global, kind });
    }
    Ok(entries)
}

// ─── Tree model ───────────────────────────────────────────────────────────────

pub(crate) enum TreeNode {
    Folder { path: String, children: Vec<TreeNode> },
    Leaf { entry_idx: usize },
}

pub(crate) enum VisibleRow {
    Folder { path: String, expanded: bool, prefix: String },
    Leaf { entry_idx: usize, prefix: String },
}

pub(crate) fn build_tree(entries: &[Entry]) -> Vec<TreeNode> {
    let (globals, locals): (Vec<_>, Vec<_>) =
        entries.iter().enumerate().partition(|(_, e)| e.global);

    let mut root: Vec<TreeNode> = Vec::new();
    for (idx, entry) in &locals {
        tree_insert(&mut root, &entry.name, *idx, "");
    }
    if !globals.is_empty() {
        let mut global_children: Vec<TreeNode> = Vec::new();
        for (idx, entry) in &globals {
            tree_insert(&mut global_children, &entry.name, *idx, "[global]");
        }
        root.push(TreeNode::Folder { path: "[global]".to_string(), children: global_children });
    }
    root
}

fn tree_insert(nodes: &mut Vec<TreeNode>, name: &str, entry_idx: usize, path_prefix: &str) {
    match name.find('/') {
        None => nodes.push(TreeNode::Leaf { entry_idx }),
        Some(slash) => {
            let segment = &name[..slash];
            let rest = &name[slash + 1..];
            let full_path = if path_prefix.is_empty() {
                segment.to_string()
            } else {
                format!("{}/{}", path_prefix, segment)
            };
            if let Some(pos) = nodes.iter().position(|n| {
                matches!(n, TreeNode::Folder { path, .. } if path == &full_path)
            }) {
                if let TreeNode::Folder { children, .. } = &mut nodes[pos] {
                    tree_insert(children, rest, entry_idx, &full_path);
                }
            } else {
                let mut children = Vec::new();
                tree_insert(&mut children, rest, entry_idx, &full_path);
                nodes.push(TreeNode::Folder { path: full_path, children });
            }
        }
    }
}

pub(crate) fn collect_folder_paths(nodes: &[TreeNode], paths: &mut HashSet<String>) {
    for node in nodes {
        if let TreeNode::Folder { path, children } = node {
            paths.insert(path.clone());
            collect_folder_paths(children, paths);
        }
    }
}

pub(crate) fn visible_rows(tree: &[TreeNode], collapsed: &HashSet<String>) -> Vec<VisibleRow> {
    let mut rows = Vec::new();
    emit_rows(tree, collapsed, true, "", &mut rows);
    rows
}

fn emit_rows(
    nodes: &[TreeNode],
    collapsed: &HashSet<String>,
    is_root: bool,
    parent_prefix: &str,
    rows: &mut Vec<VisibleRow>,
) {
    let count = nodes.len();
    for (i, node) in nodes.iter().enumerate() {
        let is_last = i == count - 1;
        match node {
            TreeNode::Folder { path, children } => {
                let expanded = !collapsed.contains(path);
                let (my_prefix, child_prefix) = if is_root {
                    (String::new(), String::new())
                } else {
                    let conn = if is_last { "└── " } else { "├── " };
                    let cont = if is_last { "    " } else { "│   " };
                    (format!("{}{}", parent_prefix, conn), format!("{}{}", parent_prefix, cont))
                };
                rows.push(VisibleRow::Folder { path: path.clone(), expanded, prefix: my_prefix });
                if expanded {
                    emit_rows(children, collapsed, false, &child_prefix, rows);
                }
            }
            TreeNode::Leaf { entry_idx } => {
                let my_prefix = if is_root {
                    String::new()
                } else {
                    let conn = if is_last { "└── " } else { "├── " };
                    format!("{}{}", parent_prefix, conn)
                };
                rows.push(VisibleRow::Leaf { entry_idx: *entry_idx, prefix: my_prefix });
            }
        }
    }
}

pub(crate) fn detect_nerd_fonts() -> bool {
    if let Ok(val) = std::env::var("ICHIGO_ICONS") {
        return val == "1";
    }
    if std::env::var("KITTY_WINDOW_ID").is_ok() { return true; }
    if std::env::var("WEZTERM_PANE").is_ok() || std::env::var("WEZTERM_EXECUTABLE").is_ok() { return true; }
    if let Ok(p) = std::env::var("TERM_PROGRAM")
        && matches!(p.as_str(), "ghostty" | "iTerm.app") { return true; }
    if let Ok(t) = std::env::var("TERM")
        && matches!(t.as_str(), "xterm-kitty" | "xterm-ghostty") { return true; }
    // Inside tmux, the outer terminal's env vars are hidden. Ask tmux directly.
    if std::env::var("TMUX").is_ok() {
        if tmux_env_matches("TERM_PROGRAM", &["ghostty", "iTerm.app"]) { return true; }
        if tmux_env_matches("TERM", &["xterm-kitty", "xterm-ghostty"]) { return true; }
        if tmux_env_matches("KITTY_WINDOW_ID", &[]) { return true; }
    }
    false
}

fn tmux_env_matches(var: &str, values: &[&str]) -> bool {
    let Ok(out) = std::process::Command::new("tmux")
        .args(["show-environment", "-g", var])
        .output() else { return false };
    let raw = String::from_utf8_lossy(&out.stdout);
    let line = raw.trim();
    // tmux prefixes unset vars with '-'; skip those.
    if line.starts_with('-') { return false; }
    if values.is_empty() {
        // Caller just wants to know if the var is set at all (e.g. KITTY_WINDOW_ID).
        return line.contains('=');
    }
    let val = line.trim_start_matches(&format!("{}=", var));
    values.contains(&val)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf_entry(name: &str, global: bool) -> Entry {
        Entry {
            name: name.to_string(),
            global,
            kind: EntryKind::Request {
                method: "GET".to_string(),
                url: "http://example.com".to_string(),
                description: None,
                headers: HashMap::new(),
                query: HashMap::new(),
                body_data: None,
                profiles: vec![],
            },
        }
    }

    #[test]
    fn build_tree_flat_list() {
        let entries = vec![leaf_entry("login", false), leaf_entry("ping", false)];
        let tree = build_tree(&entries);
        assert_eq!(tree.len(), 2);
        assert!(matches!(tree[0], TreeNode::Leaf { entry_idx: 0 }));
        assert!(matches!(tree[1], TreeNode::Leaf { entry_idx: 1 }));
    }

    #[test]
    fn build_tree_nested() {
        let entries = vec![
            leaf_entry("auth/login", false),
            leaf_entry("auth/refresh", false),
            leaf_entry("ping", false),
        ];
        let tree = build_tree(&entries);
        assert_eq!(tree.len(), 2);
        match &tree[0] {
            TreeNode::Folder { path, children } => {
                assert_eq!(path, "auth");
                assert_eq!(children.len(), 2);
                assert!(matches!(children[0], TreeNode::Leaf { entry_idx: 0 }));
                assert!(matches!(children[1], TreeNode::Leaf { entry_idx: 1 }));
            }
            _ => panic!("expected folder node"),
        }
        assert!(matches!(tree[1], TreeNode::Leaf { entry_idx: 2 }));
    }

    #[test]
    fn build_tree_global_folder() {
        let entries = vec![leaf_entry("login", false), leaf_entry("global_req", true)];
        let tree = build_tree(&entries);
        assert_eq!(tree.len(), 2);
        assert!(matches!(tree[0], TreeNode::Leaf { entry_idx: 0 }));
        match &tree[1] {
            TreeNode::Folder { path, children } => {
                assert_eq!(path, "[global]");
                assert_eq!(children.len(), 1);
            }
            _ => panic!("expected [global] folder"),
        }
    }

    #[test]
    fn visible_rows_all_expanded() {
        let entries = vec![leaf_entry("auth/login", false), leaf_entry("auth/refresh", false)];
        let tree = build_tree(&entries);
        let collapsed = HashSet::new();
        let rows = visible_rows(&tree, &collapsed);
        assert_eq!(rows.len(), 3);
        assert!(matches!(&rows[0], VisibleRow::Folder { path, expanded: true, .. } if path == "auth"));
        assert!(matches!(&rows[1], VisibleRow::Leaf { entry_idx: 0, .. }));
        assert!(matches!(&rows[2], VisibleRow::Leaf { entry_idx: 1, .. }));
    }

    #[test]
    fn visible_rows_collapsed_hides_children() {
        let entries = vec![
            leaf_entry("auth/login", false),
            leaf_entry("auth/refresh", false),
            leaf_entry("ping", false),
        ];
        let tree = build_tree(&entries);
        let mut collapsed = HashSet::new();
        collapsed.insert("auth".to_string());
        let rows = visible_rows(&tree, &collapsed);
        assert_eq!(rows.len(), 2);
        assert!(matches!(&rows[0], VisibleRow::Folder { path, expanded: false, .. } if path == "auth"));
        assert!(matches!(&rows[1], VisibleRow::Leaf { entry_idx: 2, .. }));
    }

    #[test]
    fn visible_rows_tree_prefixes() {
        let entries = vec![leaf_entry("auth/login", false), leaf_entry("auth/refresh", false)];
        let tree = build_tree(&entries);
        let collapsed = HashSet::new();
        let rows = visible_rows(&tree, &collapsed);
        match &rows[0] {
            VisibleRow::Folder { prefix, .. } => assert!(prefix.is_empty()),
            _ => panic!(),
        }
        match &rows[1] {
            VisibleRow::Leaf { prefix, .. } => assert!(prefix.contains("├──")),
            _ => panic!(),
        }
        match &rows[2] {
            VisibleRow::Leaf { prefix, .. } => assert!(prefix.contains("└──")),
            _ => panic!(),
        }
    }
}
