use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    symbols,
    widgets::{Axis, Block, Borders, Chart, Dataset, GraphType, List, ListItem, ListState, Paragraph},
    Frame, Terminal,
};
use std::{collections::{HashMap, HashSet}, fs, io};

use crate::config::{
    dir_for, list_requests, prune_empty_parents, resolve_config_path, ChainConfig, RequestConfig,
};

// ─── Data ─────────────────────────────────────────────────────────────────────

enum EntryKind {
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

struct Entry {
    name: String,
    global: bool,
    kind: EntryKind,
}

fn load_entries() -> Result<Vec<Entry>> {
    let raw = list_requests()?;
    let mut entries = Vec::new();
    for r in raw {
        let path = resolve_config_path(&r.name).unwrap();
        let content = fs::read_to_string(&path)?;
        let kind = if content.contains("steps:") {
            let chain: ChainConfig = serde_yaml::from_str(&content)
                .unwrap_or(ChainConfig { name: r.name.clone(), steps: vec![] });
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

enum TreeNode {
    Folder { path: String, children: Vec<TreeNode> },
    Leaf { entry_idx: usize },
}

enum VisibleRow {
    Folder { path: String, expanded: bool, prefix: String },
    Leaf { entry_idx: usize, prefix: String },
}

fn build_tree(entries: &[Entry]) -> Vec<TreeNode> {
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

fn visible_rows(tree: &[TreeNode], collapsed: &HashSet<String>) -> Vec<VisibleRow> {
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

fn detect_nerd_fonts() -> bool {
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

// ─── Modes ────────────────────────────────────────────────────────────────────

enum Mode {
    Browse,
    ProfileSelect {
        entry_name: String,
        profiles: Vec<crate::config::Profile>,
        selected: usize, // 0 = no profile, 1..=n = profiles[selected-1]
        for_test: bool,
    },
    VarInput {
        entry_name: String,
        vars: Vec<(String, String)>, // (placeholder name, value being typed)
        focused: usize,              // which field the cursor is in
    },
    TestInput {
        entry_name: String,
        vars: Vec<(String, String)>,
        iterations: String,
        focused: usize, // 0..vars.len() = var fields, vars.len() = iterations field
    },
    Response {
        status: u16,
        body: String,
        scroll: u16,
        response_filter: String,
        response_filter_active: bool,
    },
    TestResponse {
        results: crate::tester::TestResults,
    },
    NewRequest {
        fields: Vec<(String, String)>, // (label, value): name, method, url, description
        focused: usize,
        profiles: Vec<crate::config::Profile>,
        original_name: Option<String>, // Some(name) when editing an existing request
        global: bool,
        error: Option<String>,
    },
    ConfirmDelete {
        entry_name: String,
        global: bool,
    },
    // Sub-mode for adding a profile while creating/editing a request.
    // focused: 0 = profile name, 1+2i = params[i].key, 2+2i = params[i].value
    NewProfile {
        request_fields: Vec<(String, String)>,
        request_focused: usize,
        request_profiles: Vec<crate::config::Profile>,
        request_original_name: Option<String>,
        request_global: bool,
        name: String,
        params: Vec<(String, String)>,
        focused: usize,
        error: Option<String>,
    },
}

// ─── App state ────────────────────────────────────────────────────────────────

struct App {
    entries: Vec<Entry>,
    tree: Vec<TreeNode>,
    collapsed_folders: HashSet<String>,
    use_nerd_fonts: bool,
    list_state: ListState,
    pending_g: bool,
    filter: String,
    filter_active: bool,
    mode: Mode,
}

impl App {
    fn new() -> Result<Self> {
        let entries = load_entries()?;
        let tree = build_tree(&entries);
        let use_nerd_fonts = detect_nerd_fonts();
        let collapsed_folders = HashSet::new();
        let mut list_state = ListState::default();
        let initial_count = visible_rows(&tree, &collapsed_folders).len();
        if initial_count > 0 {
            list_state.select(Some(0));
        }
        Ok(Self {
            entries,
            tree,
            collapsed_folders,
            use_nerd_fonts,
            list_state,
            pending_g: false,
            filter: String::new(),
            filter_active: false,
            mode: Mode::Browse,
        })
    }

    fn using_tree(&self) -> bool {
        self.filter.is_empty()
    }

    fn filtered_indices(&self) -> Vec<usize> {
        if self.filter.is_empty() {
            return (0..self.entries.len()).collect();
        }
        let q = self.filter.to_lowercase();
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                if e.name.to_lowercase().contains(&q) {
                    return true;
                }
                if let EntryKind::Request { method, url, .. } = &e.kind {
                    return method.to_lowercase().contains(&q) || url.to_lowercase().contains(&q);
                }
                false
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn visible_count(&self) -> usize {
        if self.using_tree() {
            visible_rows(&self.tree, &self.collapsed_folders).len()
        } else {
            self.filtered_indices().len()
        }
    }

    fn move_down(&mut self) {
        let count = self.visible_count();
        if count == 0 { return; }
        let next = match self.list_state.selected() {
            Some(i) => (i + 1).min(count - 1),
            None => 0,
        };
        self.list_state.select(Some(next));
    }

    fn move_up(&mut self) {
        let count = self.visible_count();
        if count == 0 { return; }
        let next = match self.list_state.selected() {
            Some(i) => i.saturating_sub(1),
            None => 0,
        };
        self.list_state.select(Some(next));
    }

    fn move_top(&mut self) {
        if self.visible_count() > 0 {
            self.list_state.select(Some(0));
        }
    }

    fn move_bottom(&mut self) {
        let count = self.visible_count();
        if count > 0 {
            self.list_state.select(Some(count - 1));
        }
    }

    fn selected_entry_index(&self) -> Option<usize> {
        if self.using_tree() {
            let rows = visible_rows(&self.tree, &self.collapsed_folders);
            self.list_state.selected().and_then(|pos| {
                rows.get(pos).and_then(|row| match row {
                    VisibleRow::Leaf { entry_idx, .. } => Some(*entry_idx),
                    VisibleRow::Folder { .. } => None,
                })
            })
        } else {
            let filtered = self.filtered_indices();
            self.list_state.selected().and_then(|pos| filtered.get(pos).copied())
        }
    }

    fn toggle_folder(&mut self, path: String) {
        if self.collapsed_folders.contains(&path) {
            self.collapsed_folders.remove(&path);
        } else {
            self.collapsed_folders.insert(path.clone());
            // After collapse, move cursor to the folder node if it went out of bounds.
            let rows = visible_rows(&self.tree, &self.collapsed_folders);
            let count = rows.len();
            if let Some(pos) = self.list_state.selected()
                && pos >= count {
                    let folder_pos = rows.iter().position(|row| {
                        matches!(row, VisibleRow::Folder { path: p, .. } if *p == path)
                    });
                    self.list_state.select(Some(folder_pos.unwrap_or(count.saturating_sub(1))));
                }
        }
    }

    fn try_run_selected(&mut self) {
        let Some(idx) = self.selected_entry_index() else { return };
        let entry = &self.entries[idx];

        let entry_name = entry.name.clone();
        let (var_names, is_chain, profiles) = match &entry.kind {
            EntryKind::Chain { .. } => (vec![], true, vec![]),
            EntryKind::Request { url, headers, query, body_data, profiles, .. } => {
                (extract_var_names(url, headers, query, body_data.as_deref()), false, profiles.clone())
            }
        };

        if is_chain {
            self.mode = Mode::Response {
                status: 0,
                body: "Chain execution is not supported in the TUI yet.".to_string(),
                scroll: 0,
                response_filter: String::new(),
                response_filter_active: false,
            };
            return;
        }

        if !profiles.is_empty() {
            self.mode = Mode::ProfileSelect { entry_name, profiles, selected: 0, for_test: false };
            return;
        }

        let vars: Vec<(String, String)> = var_names
            .into_iter()
            .map(|name| {
                let value = std::env::var(&name).unwrap_or_default();
                (name, value)
            })
            .collect();

        if vars.is_empty() {
            self.execute_request(&entry_name, &HashMap::new());
        } else {
            self.mode = Mode::VarInput { entry_name, vars, focused: 0 };
        }
    }

    fn try_test_selected(&mut self) {
        let Some(idx) = self.selected_entry_index() else { return };
        let entry = &self.entries[idx];

        let entry_name = entry.name.clone();
        let is_chain = matches!(&entry.kind, EntryKind::Chain { .. });

        if is_chain {
            self.mode = Mode::Response {
                status: 0,
                body: "Testing chains is not supported.".to_string(),
                scroll: 0,
                response_filter: String::new(),
                response_filter_active: false,
            };
            return;
        }

        let (var_names, profiles) = match &entry.kind {
            EntryKind::Request { url, headers, query, body_data, profiles, .. } => {
                (extract_var_names(url, headers, query, body_data.as_deref()), profiles.clone())
            }
            _ => (vec![], vec![]),
        };

        if !profiles.is_empty() {
            self.mode = Mode::ProfileSelect { entry_name, profiles, selected: 0, for_test: true };
            return;
        }

        let vars: Vec<(String, String)> = var_names
            .into_iter()
            .map(|name| {
                let value = std::env::var(&name).unwrap_or_default();
                (name, value)
            })
            .collect();

        self.mode = Mode::TestInput { entry_name, vars, iterations: "10".to_string(), focused: 0 };
    }

    fn confirm_profile_select(&mut self) {
        let (entry_name, profile_params, for_test) = match &self.mode {
            Mode::ProfileSelect { entry_name, profiles, selected, for_test } => {
                let params: HashMap<String, String> = if *selected == 0 {
                    HashMap::new()
                } else {
                    profiles[*selected - 1].params.clone()
                };
                (entry_name.clone(), params, *for_test)
            }
            _ => return,
        };

        let (var_names, all_covered) = {
            let entry = self.entries.iter().find(|e| e.name == entry_name);
            match entry.map(|e| &e.kind) {
                Some(EntryKind::Request { url, headers, query, body_data, .. }) => {
                    let names = extract_var_names(url, headers, query, body_data.as_deref());
                    let covered = names.iter().all(|n| profile_params.contains_key(n));
                    (names, covered)
                }
                _ => (vec![], true),
            }
        };

        let vars: Vec<(String, String)> = var_names
            .into_iter()
            .map(|name| {
                let value = profile_params.get(&name)
                    .cloned()
                    .or_else(|| std::env::var(&name).ok())
                    .unwrap_or_default();
                (name, value)
            })
            .collect();

        if for_test {
            self.mode = Mode::TestInput { entry_name, vars, iterations: "10".to_string(), focused: 0 };
        } else if all_covered && vars.iter().all(|(_, v)| !v.is_empty()) {
            let var_map = vars.into_iter().collect();
            self.execute_request(&entry_name, &var_map);
        } else {
            self.mode = Mode::VarInput { entry_name, vars, focused: 0 };
        }
    }

    fn execute_test(&mut self, entry_name: &str, vars: &HashMap<String, String>, iterations: usize) {
        match RequestConfig::load(entry_name)
            .and_then(|config| crate::tester::collect_test_results(&config, vars, iterations))
        {
            Ok(results) => self.mode = Mode::TestResponse { results },
            Err(e) => self.mode = Mode::Response { status: 0, body: format!("Error: {e}"), scroll: 0, response_filter: String::new(), response_filter_active: false},
        }
    }

    fn execute_request(&mut self, entry_name: &str, vars: &HashMap<String, String>) {
        let result = RequestConfig::load(entry_name)
            .and_then(|config| crate::utils::send_request(&config, vars, false));

        let (status, body) = match result {
            Ok(response) => {
                let status = response.status().as_u16();
                let text = response.text().unwrap_or_else(|e| e.to_string());
                // Pretty-print JSON if the body parses as such.
                let body = serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|v| serde_json::to_string_pretty(&v).ok())
                    .unwrap_or(text);
                (status, body)
            }
            Err(e) => (0, format!("Error: {e}")),
        };

        self.mode = Mode::Response { status, body, scroll: 0 , response_filter: String::new(), response_filter_active: false };
    }

    // Returns true when the event loop should exit.
    fn handle_key(&mut self, key: KeyEvent) -> bool {
        if self.filter_active && matches!(self.mode, Mode::Browse) {
            return self.handle_key_filter(key);
        }
        if matches!(self.mode, Mode::Browse) {
            return self.handle_key_browse(key.code);
        }
        if matches!(self.mode, Mode::ProfileSelect { .. }) {
            return self.handle_key_profile_select(key.code);
        }
        if matches!(self.mode, Mode::VarInput { .. }) {
            return self.handle_key_var_input(key);
        }
        if matches!(self.mode, Mode::TestInput { .. }) {
            return self.handle_key_test_input(key);
        }
        if matches!(self.mode, Mode::NewRequest { .. }) {
            return self.handle_key_new_request(key);
        }
        if matches!(self.mode, Mode::NewProfile { .. }) {
            return self.handle_key_new_profile(key);
        }
        if matches!(self.mode, Mode::ConfirmDelete { .. }) {
            return self.handle_key_confirm_delete(key.code);
        }
        if matches!(self.mode, Mode::Response { response_filter_active: true, .. }) {
            return self.handle_key_response_filter(key);
        }
        self.handle_key_response(key.code)
    }

    fn handle_key_new_request(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Browse;
            }
            KeyCode::Tab => {
                if let Mode::NewRequest { fields, focused, .. } = &mut self.mode {
                    *focused = (*focused + 1) % fields.len();
                }
            }
            KeyCode::BackTab => {
                if let Mode::NewRequest { fields, focused, .. } = &mut self.mode {
                    let len = fields.len();
                    *focused = focused.checked_sub(1).unwrap_or(len - 1);
                }
            }
            KeyCode::Char('g') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Mode::NewRequest { global, .. } = &mut self.mode {
                    *global = !*global;
                }
            }
            KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let snapshot = match &self.mode {
                    Mode::NewRequest { fields, focused, profiles, original_name, global, .. } => {
                        Some((fields.clone(), *focused, profiles.clone(), original_name.clone(), *global))
                    }
                    _ => None,
                };
                if let Some((fields, focused, profiles, original_name, global)) = snapshot {
                    self.mode = Mode::NewProfile {
                        request_fields: fields,
                        request_focused: focused,
                        request_profiles: profiles,
                        request_original_name: original_name,
                        request_global: global,
                        name: String::new(),
                        params: Vec::new(),
                        focused: 0,
                        error: None,
                    };
                }
            }
            KeyCode::Char(c) => {
                if let Mode::NewRequest { fields, focused, error, .. } = &mut self.mode {
                    fields[*focused].1.push(c);
                    *error = None;
                }
            }
            KeyCode::Backspace => {
                if let Mode::NewRequest { fields, focused, error, .. } = &mut self.mode {
                    fields[*focused].1.pop();
                    *error = None;
                }
            }
            KeyCode::Enter => {
                self.save_new_request();
            }
            _ => {}
        }
        false
    }

    fn handle_key_new_profile(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc => {
                // Return to NewRequest without saving the profile
                if let Mode::NewProfile { request_fields, request_focused, request_profiles, request_original_name, request_global, .. } = &self.mode {
                    self.mode = Mode::NewRequest {
                        fields: request_fields.clone(),
                        focused: *request_focused,
                        profiles: request_profiles.clone(),
                        original_name: request_original_name.clone(),
                        global: *request_global,
                        error: None,
                    };
                }
            }
            KeyCode::Tab => {
                if let Mode::NewProfile { params, focused, .. } = &mut self.mode {
                    let total = 1 + 2 * params.len();
                    *focused = (*focused + 1) % total.max(1);
                }
            }
            KeyCode::BackTab => {
                if let Mode::NewProfile { params, focused, .. } = &mut self.mode {
                    let total = 1 + 2 * params.len();
                    *focused = focused.checked_sub(1).unwrap_or(total.saturating_sub(1));
                }
            }
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Mode::NewProfile { params, focused, .. } = &mut self.mode {
                    let new_idx = 1 + 2 * params.len();
                    params.push((String::new(), String::new()));
                    *focused = new_idx;
                }
            }
            KeyCode::Char(c) => {
                if let Mode::NewProfile { name, params, focused, error, .. } = &mut self.mode {
                    *error = None;
                    if *focused == 0 {
                        name.push(c);
                    } else {
                        let param_idx = (*focused - 1) / 2;
                        let is_value = (*focused - 1) % 2 == 1;
                        if let Some((k, v)) = params.get_mut(param_idx) {
                            if is_value { v.push(c) } else { k.push(c) }
                        }
                    }
                }
            }
            KeyCode::Backspace => {
                if let Mode::NewProfile { name, params, focused, .. } = &mut self.mode {
                    if *focused == 0 {
                        name.pop();
                    } else {
                        let param_idx = (*focused - 1) / 2;
                        let is_value = (*focused - 1) % 2 == 1;
                        if let Some((k, v)) = params.get_mut(param_idx) {
                            if is_value { v.pop() } else { k.pop() };
                        }
                    }
                }
            }
            KeyCode::Enter => {
                self.save_new_profile();
            }
            _ => {}
        }
        false
    }

    fn save_new_profile(&mut self) {
        let (profile_name, params, req_fields, req_focused, mut req_profiles, req_original_name, req_global) = match &self.mode {
            Mode::NewProfile { name, params, request_fields, request_focused, request_profiles, request_original_name, request_global, .. } => (
                name.trim().to_string(),
                params.clone(),
                request_fields.clone(),
                *request_focused,
                request_profiles.clone(),
                request_original_name.clone(),
                *request_global,
            ),
            _ => return,
        };

        if profile_name.is_empty() {
            if let Mode::NewProfile { error, .. } = &mut self.mode {
                *error = Some("Profile name is required".to_string());
            }
            return;
        }

        let filtered_params: HashMap<String, String> = params
            .into_iter()
            .filter(|(k, _)| !k.trim().is_empty())
            .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            .collect();

        req_profiles.push(crate::config::Profile { name: profile_name, params: filtered_params });

        self.mode = Mode::NewRequest {
            fields: req_fields,
            focused: req_focused,
            profiles: req_profiles,
            original_name: req_original_name,
            global: req_global,
            error: None,
        };
    }

    fn save_new_request(&mut self) {
        let (name, method, url, description, profiles, original_name, global) = match &self.mode {
            Mode::NewRequest { fields, profiles, original_name, global, .. } => (
                fields[0].1.trim().to_string(),
                fields[1].1.trim().to_uppercase(),
                fields[2].1.trim().to_string(),
                fields[3].1.trim().to_string(),
                profiles.clone(),
                original_name.clone(),
                *global,
            ),
            _ => return,
        };

        let set_error = |mode: &mut Mode, msg: &str| {
            if let Mode::NewRequest { error, .. } = mode {
                *error = Some(msg.to_string());
            }
        };

        if name.is_empty() {
            set_error(&mut self.mode, "Name is required");
            return;
        }
        if name.chars().any(|c| c == '\\' || c == '.')
            || name.starts_with('/')
            || name.ends_with('/')
            || name.contains("//")
        {
            set_error(&mut self.mode, "Invalid name (use folder/name for subfolders)");
            return;
        }
        if method.is_empty() {
            set_error(&mut self.mode, "Method is required");
            return;
        }
        if url.is_empty() {
            set_error(&mut self.mode, "URL is required");
            return;
        }

        let path = if global {
            crate::config::global_config_path(&name)
        } else {
            crate::config::local_config_path(&name)
        };
        let is_rename = original_name.as_deref() != Some(name.as_str());
        if path.exists() && (original_name.is_none() || is_rename) {
            set_error(&mut self.mode, &format!("'{}' already exists", name));
            return;
        }

        // When editing, preserve headers/query/body/extract from the original file.
        let (headers, query, body, extract) = original_name
            .as_deref()
            .and_then(|orig| crate::config::RequestConfig::load(orig).ok())
            .map(|c| (c.headers, c.query, c.body, c.extract))
            .unwrap_or_default();

        let config = crate::config::RequestConfig {
            name: name.clone(),
            method,
            url,
            description: if description.is_empty() { None } else { Some(description) },
            headers,
            query,
            body,
            extract,
            profiles: if profiles.is_empty() { None } else { Some(profiles) },
        };

        let dir = path.parent().expect("config path has no parent");
        if let Err(e) = fs::create_dir_all(dir) {
            set_error(&mut self.mode, &format!("Failed to create directory: {e}"));
            return;
        }

        match serde_yaml::to_string(&config) {
            Ok(yaml) => {
                if let Err(e) = fs::write(&path, yaml) {
                    set_error(&mut self.mode, &format!("Failed to write file: {e}"));
                    return;
                }
            }
            Err(e) => {
                set_error(&mut self.mode, &format!("Serialization error: {e}"));
                return;
            }
        }

        if let Some(ref old_name) = original_name
            && is_rename {
                let old_path = if global {
                    crate::config::global_config_path(old_name)
                } else {
                    crate::config::local_config_path(old_name)
                };
                let _ = fs::remove_file(&old_path);
                prune_empty_parents(&old_path, &dir_for(global));
            }

        if let Ok(entries) = load_entries() {
            let idx = entries.iter().position(|e| e.name == name);
            self.tree = build_tree(&entries);
            self.entries = entries;
            if let Some(entry_idx) = idx {
                let rows = visible_rows(&self.tree, &self.collapsed_folders);
                let pos = rows.iter().position(|row| {
                    matches!(row, VisibleRow::Leaf { entry_idx: i, .. } if *i == entry_idx)
                });
                self.list_state.select(pos.or(if rows.is_empty() { None } else { Some(0) }));
            }
        }
        self.mode = Mode::Browse;
    }

    fn handle_key_profile_select(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Esc => {
                self.mode = Mode::Browse;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if let Mode::ProfileSelect { profiles, selected, .. } = &mut self.mode {
                    *selected = (*selected + 1).min(profiles.len());
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if let Mode::ProfileSelect { selected, .. } = &mut self.mode {
                    *selected = selected.saturating_sub(1);
                }
            }
            KeyCode::Enter => {
                self.confirm_profile_select();
            }
            _ => {}
        }
        false
    }

    fn handle_key_browse(&mut self, code: KeyCode) -> bool {
        let was_pending_g = self.pending_g;
        self.pending_g = false;
        match code {
            KeyCode::Char('q') => return true,
            KeyCode::Char('j') | KeyCode::Down => self.move_down(),
            KeyCode::Char('k') | KeyCode::Up => self.move_up(),
            KeyCode::Char('G') => self.move_bottom(),
            KeyCode::Char('g') => {
                if was_pending_g {
                    self.move_top();
                } else {
                    self.pending_g = true;
                }
            }
            KeyCode::Char(' ') if self.using_tree() => {
                if let Some(pos) = self.list_state.selected() {
                    let rows = visible_rows(&self.tree, &self.collapsed_folders);
                    if let Some(VisibleRow::Folder { path, .. }) = rows.get(pos) {
                        self.toggle_folder(path.clone());
                    }
                }
            }
            KeyCode::Char(' ') => {}
            KeyCode::Char('r') | KeyCode::Enter => {
                if self.using_tree()
                    && let Some(pos) = self.list_state.selected() {
                    let rows = visible_rows(&self.tree, &self.collapsed_folders);
                    if let Some(VisibleRow::Folder { path, .. }) = rows.get(pos) {
                        self.toggle_folder(path.clone());
                        return false;
                    }
                }
                self.try_run_selected()
            }
            KeyCode::Char('t') => self.try_test_selected(),
            KeyCode::Char('n') => {
                self.mode = Mode::NewRequest {
                    fields: vec![
                        ("name".to_string(), String::new()),
                        ("method".to_string(), "GET".to_string()),
                        ("url".to_string(), String::new()),
                        ("description".to_string(), String::new()),
                    ],
                    focused: 0,
                    profiles: Vec::new(),
                    original_name: None,
                    global: false,
                    error: None,
                };
            }
            KeyCode::Char('e') => {
                let Some(idx) = self.selected_entry_index() else { return false };
                let entry = &self.entries[idx];
                let entry_global = entry.global;
                if let EntryKind::Request { method, url, description, profiles, .. } = &entry.kind {
                    let name = entry.name.clone();
                    self.mode = Mode::NewRequest {
                        fields: vec![
                            ("name".to_string(), name.clone()),
                            ("method".to_string(), method.clone()),
                            ("url".to_string(), url.clone()),
                            ("description".to_string(), description.clone().unwrap_or_default()),
                        ],
                        focused: 0,
                        profiles: profiles.clone(),
                        original_name: Some(name),
                        global: entry_global,
                        error: None,
                    };
                }
            }
            KeyCode::Char('c') => {
                let Some(idx) = self.selected_entry_index() else { return false };
                let entry = &self.entries[idx];
                if let EntryKind::Request { method, url, description, profiles, .. } = &entry.kind {
                    self.mode = Mode::NewRequest {
                        fields: vec![
                            ("name".to_string(), String::new()),
                            ("method".to_string(), method.clone()),
                            ("url".to_string(), url.clone()),
                            ("description".to_string(), description.clone().unwrap_or_default()),
                        ],
                        focused: 0,
                        profiles: profiles.clone(),
                        original_name: None,
                        global: false,
                        error: None,
                    };
                }
            }
            KeyCode::Char('d') => {
                let Some(idx) = self.selected_entry_index() else { return false };
                let entry = &self.entries[idx];
                self.mode = Mode::ConfirmDelete { entry_name: entry.name.clone(), global: entry.global };
            }
            KeyCode::Char('f') => {
                self.filter_active = true;
                let count = self.filtered_indices().len();
                self.list_state.select(if count == 0 { None } else { Some(0) });
            }
            _ => {}
        }
        false
    }

    fn handle_key_filter(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc => {
                self.filter_active = false;
                self.filter.clear();
                // Esc returns to tree mode; restore cursor at top of tree.
                let count = self.visible_count();
                self.list_state.select(if count == 0 { None } else { Some(0) });
            }
            KeyCode::Enter => {
                self.filter_active = false;
            }
            KeyCode::Char(c) => {
                self.filter.push(c);
                let count = self.visible_count();
                self.list_state.select(if count == 0 { None } else { Some(0) });
            }
            KeyCode::Backspace => {
                self.filter.pop();
                let count = self.visible_count();
                self.list_state.select(if count == 0 { None } else { Some(0) });
            }
            _ => {}
        }
        false
    }

    fn handle_key_response_filter(&mut self, key: KeyEvent) -> bool {
        if let Mode::Response { response_filter, response_filter_active, scroll, .. } = &mut self.mode {
            match key.code {
                KeyCode::Esc => {
                    response_filter.clear();
                    *response_filter_active = false;
                    *scroll = 0;
                }
                KeyCode::Enter => {
                    *response_filter_active = false;
                }
                KeyCode::Char(c) => {
                    response_filter.push(c);
                    *scroll = 0;
                }
                KeyCode::Backspace => {
                    response_filter.pop();
                    *scroll = 0;
                }
                _ => {}
            }
        }
        false
    }

    fn handle_key_confirm_delete(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Char('y') | KeyCode::Enter => {
                let (entry_name, global) = match &self.mode {
                    Mode::ConfirmDelete { entry_name , global} => (entry_name.clone(), *global),
                    _ => return false,
                };

                let path = if global {
                    crate::config::global_config_path(&entry_name)
                } else {
                    crate::config::local_config_path(&entry_name)
                };
                let _ = fs::remove_file(&path);
                prune_empty_parents(&path, &dir_for(global));
                if let Ok(entries) = load_entries() {
                    self.tree = build_tree(&entries);
                    self.entries = entries;
                    let count = self.visible_count();
                    let new_pos = self.list_state.selected()
                        .map(|i| i.min(count.saturating_sub(1)));
                    self.list_state.select(if count == 0 { None } else { new_pos });
                }
                self.mode = Mode::Browse;
            }
            KeyCode::Char('n') | KeyCode::Esc => {
                self.mode = Mode::Browse;
            }
            _ => {}
        }
        false
    }

    fn handle_key_var_input(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Browse;
            }
            KeyCode::Enter => {
                // Clone out the data we need; the borrow of self.mode ends
                // when this block exits, letting execute_request take &mut self.
                let (entry_name, var_map) = match &self.mode {
                    Mode::VarInput { entry_name, vars, .. } => (
                        entry_name.clone(),
                        vars.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<HashMap<_, _>>(),
                    ),
                    _ => return false,
                };
                self.execute_request(&entry_name, &var_map);
            }
            KeyCode::Tab => {
                if let Mode::VarInput { vars, focused, .. } = &mut self.mode {
                    let len = vars.len();
                    *focused = (*focused + 1) % len;
                }
            }
            KeyCode::BackTab => {
                if let Mode::VarInput { vars, focused, .. } = &mut self.mode {
                    let len = vars.len();
                    *focused = focused.checked_sub(1).unwrap_or(len - 1);
                }
            }
            KeyCode::Char(c) => {
                if let Mode::VarInput { vars, focused, .. } = &mut self.mode {
                    vars[*focused].1.push(c);
                }
            }
            KeyCode::Backspace => {
                if let Mode::VarInput { vars, focused, .. } = &mut self.mode {
                    vars[*focused].1.pop();
                }
            }
            _ => {}
        }
        false
    }

    fn handle_key_test_input(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Browse;
            }
            KeyCode::Enter => {
                let (entry_name, var_map, iterations) = match &self.mode {
                    Mode::TestInput { entry_name, vars, iterations, .. } => (
                        entry_name.clone(),
                        vars.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<HashMap<_, _>>(),
                        iterations.parse::<usize>().unwrap_or(10),
                    ),
                    _ => return false,
                };
                self.execute_test(&entry_name, &var_map, iterations);
            }
            KeyCode::Tab => {
                if let Mode::TestInput { vars, focused, .. } = &mut self.mode {
                    let len = vars.len() + 1;
                    *focused = (*focused + 1) % len;
                }
            }
            KeyCode::BackTab => {
                if let Mode::TestInput { vars, focused, .. } = &mut self.mode {
                    let len = vars.len() + 1;
                    *focused = focused.checked_sub(1).unwrap_or(len - 1);
                }
            }
            KeyCode::Char(c) => {
                if let Mode::TestInput { vars, focused, iterations, .. } = &mut self.mode {
                    if *focused < vars.len() {
                        vars[*focused].1.push(c);
                    } else if c.is_ascii_digit() {
                        iterations.push(c);
                    }
                }
            }
            KeyCode::Backspace => {
                if let Mode::TestInput { vars, focused, iterations, .. } = &mut self.mode {
                    if *focused < vars.len() {
                        vars[*focused].1.pop();
                    } else {
                        iterations.pop();
                    }
                }
            }
            _ => {}
        }
        false
    }

    fn handle_key_response(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Char('q') => return true,
            KeyCode::Esc => {
                self.mode = Mode::Browse;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if let Mode::Response { scroll, .. } = &mut self.mode {
                    *scroll = scroll.saturating_add(1);
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if let Mode::Response { scroll, .. } = &mut self.mode {
                    *scroll = scroll.saturating_sub(1);
                }
            }
            KeyCode::Char('c') => {
                let text = match &self.mode {
                    Mode::Response { body, .. } => body.clone(),
                    Mode::TestResponse { results } => format_test_results_text(results),
                    _ => return false,
                };
                copy_to_clipboard(&text);
            }
            KeyCode::Char('f') => {
                if let Mode::Response { response_filter_active, .. } = &mut self.mode {
                    *response_filter_active = true;
                }
            }
            _ => {}
        }
        false
    }
}

// ─── Entry point ──────────────────────────────────────────────────────────────

pub fn run() -> Result<()> {
    let mut app = App::new()?;

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = event_loop(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> Result<()> {
    loop {
        terminal.draw(|frame| draw(frame, app))?;
        if let Event::Key(key) = event::read()?
            && app.handle_key(key)
        {
            break;
        }
    }
    Ok(())
}

// ─── Drawing ──────────────────────────────────────────────────────────────────

fn draw(frame: &mut Frame, app: &mut App) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(frame.area());

    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(outer[0]);

    let filtered = app.filtered_indices();
    let tree_rows_storage: Option<Vec<VisibleRow>> = if app.using_tree() {
        Some(visible_rows(&app.tree, &app.collapsed_folders))
    } else {
        None
    };
    let tree_rows = tree_rows_storage.as_deref();
    draw_list(frame, panes[0], &app.entries, tree_rows, &filtered, &mut app.list_state, &app.filter, app.filter_active, app.use_nerd_fonts);

    match &app.mode {
        Mode::Browse => {
            // Accessing app.list_state and app.entries here is fine: they are
            // different fields from app.mode, and Rust can split-borrow them.
            let selected = app.selected_entry_index().map(|i| &app.entries[i]);
            draw_detail_browse(frame, panes[1], selected);
        }
        Mode::ProfileSelect { profiles, selected, entry_name, .. } => {
            draw_profile_select(frame, panes[1], profiles, *selected, entry_name);
        }
        Mode::VarInput { vars, focused, entry_name } => {
            draw_var_input(frame, panes[1], vars, *focused, entry_name);
        }
        Mode::TestInput { vars, focused, iterations, entry_name } => {
            draw_test_input(frame, panes[1], vars, iterations, *focused, entry_name);
        }
        Mode::Response { status, body, scroll , response_filter, response_filter_active} => {
            draw_response(frame, panes[1], *status, body, *scroll, response_filter, *response_filter_active);
        }
        Mode::TestResponse { results } => {
            draw_test_results(frame, panes[1], results);
        }
        Mode::NewRequest { fields, focused, profiles, original_name, global, error } => {
            draw_new_request(frame, panes[1], fields, *focused, profiles, original_name.is_some(), *global, error.as_deref());
        }
        Mode::NewProfile { name, params, focused, error, .. } => {
            draw_new_profile(frame, panes[1], name, params, *focused, error.as_deref());
        }
        Mode::ConfirmDelete { entry_name, ..} => {
            draw_confirm_delete(frame, panes[1], entry_name);
        }
    }

    draw_help(frame, outer[1], &app.mode);
}

#[allow(clippy::too_many_arguments)]
fn draw_list(
    frame: &mut Frame,
    area: Rect,
    entries: &[Entry],
    tree_rows: Option<&[VisibleRow]>,
    filtered: &[usize],
    list_state: &mut ListState,
    filter: &str,
    filter_active: bool,
    use_nerd_fonts: bool,
) {
    let show_filter = filter_active || !filter.is_empty();
    let chunks = if show_filter {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(0)])
            .split(area)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(0), Constraint::Min(0)])
            .split(area)
    };

    if show_filter {
        let display = if filter_active { format!("{}█", filter) } else { filter.to_string() };
        let filter_widget = Paragraph::new(Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled(display, Style::default().fg(Color::White)),
        ]))
        .block(
            Block::default()
                .title(" Filter ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(if filter_active { Color::Yellow } else { Color::DarkGray })),
        );
        frame.render_widget(filter_widget, chunks[0]);
    }

    let (items, highlight_sym): (Vec<ListItem>, &str) = if let Some(rows) = tree_rows {
        let items = rows.iter().map(|row| match row {
            VisibleRow::Folder { path, expanded, prefix, .. } => {
                make_tree_folder_item(path, *expanded, prefix, use_nerd_fonts)
            }
            VisibleRow::Leaf { entry_idx, prefix, .. } => {
                make_tree_leaf_item(&entries[*entry_idx], prefix, use_nerd_fonts)
            }
        }).collect();
        (items, "▶ ")
    } else {
        let items = filtered.iter().map(|&i| make_list_item(&entries[i])).collect();
        (items, "▶ ")
    };

    let list = List::new(items)
        .block(
            Block::default()
                .title(" Requests ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Blue)),
        )
        .highlight_style(
            Style::default()
                .bg(Color::Blue)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(highlight_sym);
    frame.render_stateful_widget(list, chunks[1], list_state);
}

fn draw_detail_browse(frame: &mut Frame, area: Rect, selected: Option<&Entry>) {
    let lines = selected
        .map(make_detail_lines)
        .unwrap_or_else(|| vec![Line::from(" No request selected.")]);
    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .title(" Details ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Blue)),
    );
    frame.render_widget(paragraph, area);
}

fn draw_profile_select(
    frame: &mut Frame,
    area: Rect,
    profiles: &[crate::config::Profile],
    selected: usize,
    entry_name: &str,
) {
    let title = format!(" Profile — {} ", entry_name);
    let mut lines: Vec<Line<'static>> = vec![Line::raw("")];

    let is_none_selected = selected == 0;
    let none_style = if is_none_selected {
        Style::default().fg(Color::White).bg(Color::Blue).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    lines.push(Line::from(Span::styled("  (no profile)", none_style)));
    lines.push(Line::raw(""));

    for (i, profile) in profiles.iter().enumerate() {
        let is_selected = selected == i + 1;
        let name_style = if is_selected {
            Style::default().fg(Color::White).bg(Color::Blue).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        lines.push(Line::from(Span::styled(
            format!("  {}", profile.name),
            name_style,
        )));
        for (k, v) in &profile.params {
            lines.push(Line::from(vec![
                Span::styled(format!("    {}: ", k), Style::default().fg(Color::DarkGray)),
                Span::styled(v.clone(), Style::default().fg(Color::DarkGray)),
            ]));
        }
        lines.push(Line::raw(""));
    }

    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Magenta)),
    );
    frame.render_widget(paragraph, area);
}

#[allow(clippy::too_many_arguments)]
fn draw_new_request(
    frame: &mut Frame,
    area: Rect,
    fields: &[(String, String)],
    focused: usize,
    profiles: &[crate::config::Profile],
    is_edit: bool,
    global: bool,
    error: Option<&str>,
) {
    let mut lines: Vec<Line<'static>> = vec![Line::raw("")];

    for (i, (label, value)) in fields.iter().enumerate() {
        let is_focused = i == focused;
        let is_required = i < 3; // name, method, url are required

        let mut label_spans = vec![Span::styled(
            format!("  {}", label),
            Style::default()
                .fg(if is_focused { Color::Green } else { Color::DarkGray })
                .add_modifier(Modifier::BOLD),
        )];
        if is_required {
            label_spans.push(Span::styled(" *", Style::default().fg(Color::Red)));
        }
        lines.push(Line::from(label_spans));

        let prompt = Span::styled("  > ", Style::default().fg(Color::DarkGray));
        let display = if is_focused { format!("{}█", value) } else { value.clone() };
        let input = Span::styled(
            display,
            Style::default().fg(if is_focused { Color::White } else { Color::DarkGray }),
        );
        lines.push(Line::from(vec![prompt, input]));
        lines.push(Line::raw(""));
    }

    let (global_check, global_color) = if global {
        ("[x] global", Color::Cyan)
    } else {
        ("[ ] global", Color::DarkGray)
    };
    lines.push(Line::from(vec![
        Span::styled("  ", Style::default()),
        Span::styled(global_check, Style::default().fg(global_color).add_modifier(Modifier::BOLD)),
        Span::styled("  Ctrl+g to toggle", Style::default().fg(Color::DarkGray)),
    ]));
    lines.push(Line::raw(""));

    if !profiles.is_empty() {
        lines.push(Line::from(Span::styled(
            "  profiles",
            Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD),
        )));
        for profile in profiles {
            lines.push(Line::from(vec![
                Span::styled("    ", Style::default()),
                Span::styled(profile.name.clone(), Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)),
            ]));
            for (k, v) in &profile.params {
                lines.push(Line::from(vec![
                    Span::styled(format!("      {}: ", k), Style::default().fg(Color::DarkGray)),
                    Span::styled(v.clone(), Style::default().fg(Color::White)),
                ]));
            }
        }
        lines.push(Line::raw(""));
    }

    if let Some(err) = error {
        lines.push(Line::from(Span::styled(
            format!("  error: {}", err),
            Style::default().fg(Color::Red),
        )));
    }

    let title = if is_edit { " Edit Request " } else { " New Request " };
    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Green)),
    );
    frame.render_widget(paragraph, area);
}

fn draw_new_profile(
    frame: &mut Frame,
    area: Rect,
    name: &str,
    params: &[(String, String)],
    focused: usize,
    error: Option<&str>,
) {
    let mut lines: Vec<Line<'static>> = vec![Line::raw("")];

    // Profile name field
    let name_focused = focused == 0;
    lines.push(Line::from(vec![
        Span::styled(
            "  profile name",
            Style::default()
                .fg(if name_focused { Color::Green } else { Color::DarkGray })
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" *", Style::default().fg(Color::Red)),
    ]));
    let display = if name_focused { format!("{}█", name) } else { name.to_string() };
    lines.push(Line::from(vec![
        Span::styled("  > ", Style::default().fg(Color::DarkGray)),
        Span::styled(display, Style::default().fg(if name_focused { Color::White } else { Color::DarkGray })),
    ]));
    lines.push(Line::raw(""));

    // Param pairs: focused index 1+2i = key, 2+2i = value
    for (i, (key, value)) in params.iter().enumerate() {
        let key_focused = focused == 1 + 2 * i;
        let val_focused = focused == 2 + 2 * i;

        lines.push(Line::from(Span::styled(
            format!("  param {} key", i + 1),
            Style::default()
                .fg(if key_focused { Color::Green } else { Color::DarkGray })
                .add_modifier(Modifier::BOLD),
        )));
        let display = if key_focused { format!("{}█", key) } else { key.clone() };
        lines.push(Line::from(vec![
            Span::styled("  > ", Style::default().fg(Color::DarkGray)),
            Span::styled(display, Style::default().fg(if key_focused { Color::White } else { Color::DarkGray })),
        ]));

        lines.push(Line::from(Span::styled(
            format!("  param {} value", i + 1),
            Style::default()
                .fg(if val_focused { Color::Green } else { Color::DarkGray })
                .add_modifier(Modifier::BOLD),
        )));
        let display = if val_focused { format!("{}█", value) } else { value.clone() };
        lines.push(Line::from(vec![
            Span::styled("  > ", Style::default().fg(Color::DarkGray)),
            Span::styled(display, Style::default().fg(if val_focused { Color::White } else { Color::DarkGray })),
        ]));
        lines.push(Line::raw(""));
    }

    if let Some(err) = error {
        lines.push(Line::from(Span::styled(
            format!("  error: {}", err),
            Style::default().fg(Color::Red),
        )));
    }

    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .title(" New Profile ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Magenta)),
    );
    frame.render_widget(paragraph, area);
}

fn draw_var_input(
    frame: &mut Frame,
    area: Rect,
    vars: &[(String, String)],
    focused: usize,
    entry_name: &str,
) {
    let title = format!(" Variables — {} ", entry_name);
    let mut lines: Vec<Line<'static>> = vec![Line::raw("")];

    for (i, (name, value)) in vars.iter().enumerate() {
        let is_focused = i == focused;

        lines.push(Line::from(Span::styled(
            format!("  {}", name),
            Style::default()
                .fg(if is_focused { Color::Yellow } else { Color::DarkGray })
                .add_modifier(Modifier::BOLD),
        )));

        let prompt = Span::styled("  > ", Style::default().fg(Color::DarkGray));
        // Append a block cursor character to the focused field's text.
        let display = if is_focused { format!("{}█", value) } else { value.clone() };
        let input = Span::styled(
            display,
            Style::default().fg(if is_focused { Color::White } else { Color::DarkGray }),
        );
        lines.push(Line::from(vec![prompt, input]));
        lines.push(Line::raw(""));
    }

    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow)),
    );
    frame.render_widget(paragraph, area);
}

fn draw_test_input(
    frame: &mut Frame,
    area: Rect,
    vars: &[(String, String)],
    iterations: &str,
    focused: usize,
    entry_name: &str,
) {
    let title = format!(" Test — {} ", entry_name);
    let mut lines: Vec<Line<'static>> = vec![Line::raw("")];

    for (i, (name, value)) in vars.iter().enumerate() {
        let is_focused = i == focused;
        lines.push(Line::from(Span::styled(
            format!("  {}", name),
            Style::default()
                .fg(if is_focused { Color::Yellow } else { Color::DarkGray })
                .add_modifier(Modifier::BOLD),
        )));
        let prompt = Span::styled("  > ", Style::default().fg(Color::DarkGray));
        let display = if is_focused { format!("{}█", value) } else { value.clone() };
        let input = Span::styled(
            display,
            Style::default().fg(if is_focused { Color::White } else { Color::DarkGray }),
        );
        lines.push(Line::from(vec![prompt, input]));
        lines.push(Line::raw(""));
    }

    let iter_focused = focused == vars.len();
    lines.push(Line::from(Span::styled(
        "  iterations",
        Style::default()
            .fg(if iter_focused { Color::Yellow } else { Color::DarkGray })
            .add_modifier(Modifier::BOLD),
    )));
    let prompt = Span::styled("  > ", Style::default().fg(Color::DarkGray));
    let display = if iter_focused { format!("{}█", iterations) } else { iterations.to_string() };
    let input = Span::styled(
        display,
        Style::default().fg(if iter_focused { Color::White } else { Color::DarkGray }),
    );
    lines.push(Line::from(vec![prompt, input]));

    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
    );
    frame.render_widget(paragraph, area);
}

fn draw_test_results(frame: &mut Frame, area: Rect, results: &crate::tester::TestResults) {
    let iterations = results.timings.len();
    let title = Line::from(vec![
        Span::raw(" Test Results "),
        Span::styled(
            format!(" {} iterations ", iterations),
            Style::default().fg(Color::Black).bg(Color::Cyan),
        ),
    ]);

    let outer = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(3), Constraint::Length(5)])
        .split(inner);

    // Status distribution horizontal gauges
    let max_count = results.statuses.iter().map(|s| s.count).max().unwrap_or(1);
    let bar_width = 30usize;
    let mut gauge_lines: Vec<Line<'static>> = vec![Line::raw("")];
    for sc in &results.statuses {
        let color = status_color(sc.status);
        let filled = ((sc.count * bar_width) / max_count).max(1);
        let empty = bar_width - filled;
        gauge_lines.push(Line::from(vec![
            Span::styled(format!("  {:>3}  ", sc.status), Style::default().fg(color).add_modifier(Modifier::BOLD)),
            Span::styled("█".repeat(filled), Style::default().fg(color)),
            Span::styled("░".repeat(empty), Style::default().fg(Color::DarkGray)),
            Span::styled(format!("  ×{}", sc.count), Style::default().fg(Color::White)),
        ]));
    }
    let gauge_widget = Paragraph::new(gauge_lines).block(
        Block::default()
            .title(" Status Distribution ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    frame.render_widget(gauge_widget, chunks[0]);

    // Timing stats
    let stats = Paragraph::new(Line::from(vec![
        Span::styled("  avg ", Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{:.2?}", results.avg), Style::default().fg(Color::Yellow)),
        Span::styled("   min ", Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{:.2?}", results.min), Style::default().fg(Color::Green)),
        Span::styled("   max ", Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{:.2?}", results.max), Style::default().fg(Color::Red)),
    ]));
    frame.render_widget(stats, chunks[1]);

    // Per-iteration latency line chart
    let max_ms = results.timings.iter().copied().max().unwrap_or(1);
    let latency_points: Vec<(f64, f64)> = results
        .timings
        .iter()
        .enumerate()
        .map(|(i, &ms)| (i as f64, ms as f64))
        .collect();
    let latency_dataset = Dataset::default()
        .marker(symbols::Marker::Braille)
        .graph_type(GraphType::Line)
        .style(Style::default().fg(Color::Cyan))
        .data(&latency_points);
    let latency_chart = Chart::new(vec![latency_dataset])
        .block(
            Block::default()
                .title(" Latency (ms) ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .x_axis(
            Axis::default()
                .bounds([0.0, results.timings.len().saturating_sub(1) as f64]),
        )
        .y_axis(
            Axis::default()
                .bounds([0.0, max_ms as f64])
                .labels(vec![Span::raw("0"), Span::raw(format!("{}ms", max_ms))]),
        );
    frame.render_widget(latency_chart, chunks[2]);
}

fn status_color(status: u16) -> Color {
    if status < 300 { Color::Green }
    else if status < 400 { Color::Cyan }
    else if status < 500 { Color::Yellow }
    else { Color::Red }
}

fn draw_response(frame: &mut Frame, area: Rect, status: u16, body: &str, scroll: u16, response_filter: &str, response_filter_active: bool) {
    let (status_color, status_label) = if status == 0 {
        (Color::Red, " Error ".to_string())
    } else if status < 300 {
        (Color::Green, format!(" {} ", status))
    } else if status < 400 {
        (Color::Cyan, format!(" {} ", status))
    } else if status < 500 {
        (Color::Yellow, format!(" {} ", status))
    } else {
        (Color::Red, format!(" {} ", status))
    };

    let title = Line::from(vec![
        Span::raw(" Response "),
        Span::styled(status_label, Style::default().fg(Color::Black).bg(status_color)),
    ]);

    let show_filter = response_filter_active || !response_filter.is_empty();
    let chunks = if show_filter {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(0)])
            .split(area)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(0), Constraint::Min(0)])
            .split(area)
    };

    if show_filter {
        let display = if response_filter_active { format!("{}█", response_filter) } else { response_filter.to_string() };
        let filter_widget = Paragraph::new(Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled(display, Style::default().fg(Color::White)),
        ]))
        .block(
            Block::default()
                .title(" Filter ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(if response_filter_active { Color::Yellow } else { Color::DarkGray })),
        );
        frame.render_widget(filter_widget, chunks[0]);
    }

    let q = response_filter.to_lowercase();
    let lines: Vec<Line<'static>> = body
        .lines()
        .filter(|line| q.is_empty() || line.to_lowercase().contains(&q))
        .map(colorize_json_line)
        .collect();

    let paragraph = Paragraph::new(lines)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(status_color)),
        )
        .scroll((scroll, 0));

    frame.render_widget(paragraph, chunks[1]);
}


fn draw_help(frame: &mut Frame, area: Rect, mode: &Mode) {
    let spans: Vec<Span<'static>> = match mode {
        Mode::Browse => vec![
            Span::styled(" q ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("quit", Style::default().fg(Color::DarkGray)),
            Span::styled("   j/k ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("navigate", Style::default().fg(Color::DarkGray)),
            Span::styled("   gg ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("top", Style::default().fg(Color::DarkGray)),
            Span::styled("   G ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("bottom", Style::default().fg(Color::DarkGray)),
            Span::styled("   r/Enter ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("run", Style::default().fg(Color::DarkGray)),
            Span::styled("   t ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("test", Style::default().fg(Color::DarkGray)),
            Span::styled("   n ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("new", Style::default().fg(Color::DarkGray)),
            Span::styled("   e ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("edit", Style::default().fg(Color::DarkGray)),
            Span::styled("   c ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("copy", Style::default().fg(Color::DarkGray)),
            Span::styled("   d ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("delete", Style::default().fg(Color::DarkGray)),
            Span::styled("   f ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("filter", Style::default().fg(Color::DarkGray)),
        ],
        Mode::ProfileSelect { .. } => vec![
            Span::styled(" Enter ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("select", Style::default().fg(Color::DarkGray)),
            Span::styled("   j/k ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("navigate", Style::default().fg(Color::DarkGray)),
            Span::styled("   Esc ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("cancel", Style::default().fg(Color::DarkGray)),
        ],
        Mode::VarInput { .. } => vec![
            Span::styled(" Enter ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("run", Style::default().fg(Color::DarkGray)),
            Span::styled("   Tab/S-Tab ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("switch field", Style::default().fg(Color::DarkGray)),
            Span::styled("   Esc ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("cancel", Style::default().fg(Color::DarkGray)),
        ],
        Mode::TestInput { .. } => vec![
            Span::styled(" Enter ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("run test", Style::default().fg(Color::DarkGray)),
            Span::styled("   Tab/S-Tab ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("switch field", Style::default().fg(Color::DarkGray)),
            Span::styled("   Esc ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("cancel", Style::default().fg(Color::DarkGray)),
        ],
        Mode::NewRequest { .. } => vec![
            Span::styled(" Enter ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("save", Style::default().fg(Color::DarkGray)),
            Span::styled("   Tab/S-Tab ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("next/prev field", Style::default().fg(Color::DarkGray)),
            Span::styled("   Ctrl+g ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("toggle global", Style::default().fg(Color::DarkGray)),
            Span::styled("   Ctrl+p ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("add profile", Style::default().fg(Color::DarkGray)),
            Span::styled("   Esc ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("cancel", Style::default().fg(Color::DarkGray)),
        ],
        Mode::NewProfile { .. } => vec![
            Span::styled(" Enter ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("save profile", Style::default().fg(Color::DarkGray)),
            Span::styled("   Tab/S-Tab ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("next/prev field", Style::default().fg(Color::DarkGray)),
            Span::styled("   Ctrl+a ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("add param", Style::default().fg(Color::DarkGray)),
            Span::styled("   Esc ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("cancel", Style::default().fg(Color::DarkGray)),
        ],
        Mode::ConfirmDelete { .. } => vec![
            Span::styled(" y/Enter ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("confirm delete", Style::default().fg(Color::DarkGray)),
            Span::styled("   n/Esc ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("cancel", Style::default().fg(Color::DarkGray)),
        ],
        Mode::Response { .. } | Mode::TestResponse { .. } => vec![
            Span::styled(" j/k ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("scroll", Style::default().fg(Color::DarkGray)),
            Span::styled("   f ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("filter", Style::default().fg(Color::DarkGray)),
            Span::styled("   c ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("copy", Style::default().fg(Color::DarkGray)),
            Span::styled("   Esc ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("back", Style::default().fg(Color::DarkGray)),
            Span::styled("   q ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("quit", Style::default().fg(Color::DarkGray)),
        ],
    };
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_confirm_delete(frame: &mut Frame, area: Rect, entry_name: &str) {
    let lines: Vec<Line<'static>> = vec![
        Line::raw(""),
        Line::from(vec![
            Span::styled("  Delete  ", Style::default().fg(Color::DarkGray)),
            Span::styled(entry_name.to_string(), Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
            Span::styled("?", Style::default().fg(Color::DarkGray)),
        ]),
        Line::raw(""),
        Line::from(Span::styled(
            "  This action cannot be undone.",
            Style::default().fg(Color::DarkGray),
        )),
        Line::raw(""),
        Line::from(vec![
            Span::styled("  y / Enter  ", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
            Span::styled("yes, delete", Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(vec![
            Span::styled("  n / Esc    ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
            Span::styled("cancel", Style::default().fg(Color::DarkGray)),
        ]),
    ];

    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .title(" Confirm Delete ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Red)),
    );
    frame.render_widget(paragraph, area);
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn colorize_json_value(val: &str) -> Vec<Span<'static>> {
    let (content, trailing) = val.strip_suffix(',').map_or((val, ""), |s| (s, ","));

    let color = if content.starts_with('"') {
        Color::Green
    } else if matches!(content, "true" | "false") {
        Color::Magenta
    } else if content == "null" {
        Color::Red
    } else if matches!(content, "{" | "}" | "[" | "]" | "{}" | "[]") {
        Color::DarkGray
    } else {
        Color::Cyan // numbers
    };

    let mut spans = vec![Span::styled(content.to_string(), Style::default().fg(color))];
    if !trailing.is_empty() {
        spans.push(Span::styled(",".to_string(), Style::default().fg(Color::DarkGray)));
    }
    spans
}

fn colorize_json_line(line: &str) -> Line<'static> {
    let indent_len = line.len() - line.trim_start().len();
    let indent = line[..indent_len].to_string();
    let trimmed = &line[indent_len..];

    if matches!(trimmed, "{" | "}" | "[" | "]" | "}," | "]," | "{}" | "[]" | "{}," | "[],") {
        return Line::from(vec![
            Span::raw(indent),
            Span::styled(trimmed.to_string(), Style::default().fg(Color::DarkGray)),
        ]);
    }

    // Key-value line: "key": value
    if trimmed.starts_with('"')
        && let Some(pos) = trimmed.find("\": ") {
            let key = trimmed[..pos + 1].to_string(); // includes closing "
            let value = &trimmed[pos + 3..];          // skip ": "
            let mut spans = vec![
                Span::raw(indent),
                Span::styled(key, Style::default().fg(Color::Yellow)),
                Span::styled(": ".to_string(), Style::default().fg(Color::DarkGray)),
            ];
            spans.extend(colorize_json_value(value));
            return Line::from(spans);
        }

    // Array value or plain text
    let mut spans = vec![Span::raw(indent)];
    spans.extend(colorize_json_value(trimmed));
    Line::from(spans)
}

fn copy_to_clipboard(text: &str) {
    use std::io::Write;
    use std::process::{Command, Stdio};
    if let Ok(mut child) = Command::new("pbcopy").stdin(Stdio::piped()).spawn() {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
    }
}

fn method_color(method: &str) -> Color {
    match method {
        "GET" => Color::Cyan,
        "POST" => Color::Green,
        "PUT" | "PATCH" => Color::Yellow,
        "DELETE" => Color::Red,
        _ => Color::White,
    }
}

// Scans all string fields of a config for {{NAME}} placeholders, preserving
// the order they first appear and deduplicating. Mirrors the logic in utils::interpolate.
fn extract_var_names(
    url: &str,
    headers: &HashMap<String, String>,
    query: &HashMap<String, String>,
    body_data: Option<&str>,
) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let sources = std::iter::once(url)
        .chain(headers.values().map(String::as_str))
        .chain(query.values().map(String::as_str))
        .chain(body_data);

    for s in sources {
        let mut rest = s;
        while let Some(start) = rest.find("{{") {
            rest = &rest[start + 2..];
            if let Some(end) = rest.find("}}") {
                let name = rest[..end].to_string();
                if seen.insert(name.clone()) {
                    names.push(name);
                }
                rest = &rest[end + 2..];
            }
        }
    }

    names
}

fn format_test_results_text(results: &crate::tester::TestResults) -> String {
    let mut lines = vec!["Results".to_string(), "".to_string()];
    for sc in &results.statuses {
        let reason = reqwest::StatusCode::from_u16(sc.status)
            .ok()
            .and_then(|s| s.canonical_reason())
            .unwrap_or("Unknown");
        lines.push(format!("  {} {}  x {}", sc.status, reason, sc.count));
    }
    lines.push("".to_string());
    lines.push("Timings".to_string());
    lines.push("".to_string());
    lines.push(format!("  avg:  {:.2?}", results.avg));
    lines.push(format!("  min:  {:.2?}", results.min));
    lines.push(format!("  max:  {:.2?}", results.max));
    lines.join("\n")
}

fn folder_name_spans(name: &str) -> Vec<Span<'static>> {
    match name.rfind('/') {
        Some(pos) => vec![
            Span::styled(
                format!("{}/ ", &name[..pos]),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                name[pos + 1..].to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ],
        None => vec![Span::styled(
            name.to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        )],
    }
}

fn make_list_item(entry: &Entry) -> ListItem<'static> {
    let scope = if entry.global { " global" } else { " local" };
    let line = match &entry.kind {
        EntryKind::Chain { .. } => {
            let mut spans = vec![
                Span::styled(
                    " CHAIN ",
                    Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
            ];
            spans.extend(folder_name_spans(&entry.name));
            spans.push(Span::styled(scope, Style::default().fg(Color::DarkGray)));
            Line::from(spans)
        }
        EntryKind::Request { method, description, .. } => {
            let color = method_color(method);
            let mut spans = vec![
                Span::styled(
                    format!(" {:<6}", method),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
            ];
            spans.extend(folder_name_spans(&entry.name));
            if let Some(desc) = description
                && !desc.is_empty()
            {
                spans.push(Span::styled(
                    format!(" — {}", desc),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            spans.push(Span::styled(scope, Style::default().fg(Color::DarkGray)));
            Line::from(spans)
        }
    };
    ListItem::new(line)
}

fn make_tree_folder_item(path: &str, expanded: bool, prefix: &str, use_nerd_fonts: bool) -> ListItem<'static> {
    let display_name = path.rsplit('/').next().unwrap_or(path).to_string();
    let line = if use_nerd_fonts {
        let icon = if expanded { "\u{f07c} " } else { "\u{f07b} " };
        Line::from(vec![
            Span::styled(prefix.to_string(), Style::default().fg(Color::DarkGray)),
            Span::styled(icon.to_string(), Style::default().fg(Color::Yellow)),
            Span::styled(display_name, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        ])
    } else {
        Line::from(vec![
            Span::styled(prefix.to_string(), Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{}/", display_name), Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        ])
    };
    ListItem::new(line)
}

fn make_tree_leaf_item(entry: &Entry, prefix: &str, _use_nerd_fonts: bool) -> ListItem<'static> {
    let leaf_name = entry.name.rsplit('/').next().unwrap_or(&entry.name).to_string();
    let line = match &entry.kind {
        EntryKind::Chain { .. } => Line::from(vec![
            Span::styled(prefix.to_string(), Style::default().fg(Color::DarkGray)),
            Span::styled(" CHAIN ", Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)),
            Span::raw(" "),
            Span::styled(leaf_name, Style::default().add_modifier(Modifier::BOLD)),
        ]),
        EntryKind::Request { method, description, .. } => {
            let color = method_color(method);
            let mut spans = vec![
                Span::styled(prefix.to_string(), Style::default().fg(Color::DarkGray)),
                Span::styled(format!(" {:<6}", method), Style::default().fg(color).add_modifier(Modifier::BOLD)),
                Span::raw(" "),
                Span::styled(leaf_name, Style::default().add_modifier(Modifier::BOLD)),
            ];
            if let Some(desc) = description.as_ref().filter(|d| !d.is_empty()) {
                spans.push(Span::styled(format!(" — {}", desc), Style::default().fg(Color::DarkGray)));
            }
            Line::from(spans)
        }
    };
    ListItem::new(line)
}

fn make_detail_lines(entry: &Entry) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = vec![];

    match &entry.kind {
        EntryKind::Chain { steps } => {
            let mut header = vec![
                Span::styled(
                    " CHAIN ",
                    Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
            ];
            header.extend(folder_name_spans(&entry.name));
            lines.push(Line::from(header));
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled(
                " Steps",
                Style::default().fg(Color::DarkGray),
            )));
            for (i, step) in steps.iter().enumerate() {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("  {}. ", i + 1),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::raw(step.clone()),
                ]));
            }
        }
        EntryKind::Request { method, url, description, headers, query, .. } => {
            let color = method_color(method);
            let mut header = vec![
                Span::styled(
                    format!(" {} ", method),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
            ];
            header.extend(folder_name_spans(&entry.name));
            lines.push(Line::from(header));
            lines.push(Line::raw(""));
            lines.push(Line::from(vec![
                Span::styled(" url    ", Style::default().fg(Color::DarkGray)),
                Span::styled(url.clone(), Style::default().fg(Color::Cyan)),
            ]));
            if let Some(desc) = description
                && !desc.is_empty()
            {
                lines.push(Line::from(vec![
                    Span::styled(" desc   ", Style::default().fg(Color::DarkGray)),
                    Span::raw(desc.clone()),
                ]));
            }
            if !headers.is_empty() {
                lines.push(Line::raw(""));
                lines.push(Line::from(Span::styled(
                    " Headers",
                    Style::default().fg(Color::DarkGray),
                )));
                for (k, v) in headers {
                    lines.push(Line::from(vec![
                        Span::styled(format!("  {}: ", k), Style::default().fg(Color::Yellow)),
                        Span::raw(v.clone()),
                    ]));
                }
            }
            if !query.is_empty() {
                lines.push(Line::raw(""));
                lines.push(Line::from(Span::styled(
                    " Query",
                    Style::default().fg(Color::DarkGray),
                )));
                for (k, v) in query {
                    lines.push(Line::from(vec![
                        Span::styled(format!("  {}: ", k), Style::default().fg(Color::Yellow)),
                        Span::raw(v.clone()),
                    ]));
                }
            }
        }
    }

    lines
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
