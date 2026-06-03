mod tree;
mod render;
mod handlers;

use tree::{EntryKind, Entry, TreeNode, VisibleRow, load_entries, build_tree, collect_folder_paths, visible_rows, detect_nerd_fonts};

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyEvent},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, widgets::ListState, Terminal};
use std::{collections::{HashMap, HashSet}, fs, io};
use crate::config::{dir_for, prune_empty_parents, global_config_path, local_config_path, RequestConfig};

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
        let mut collapsed_folders = HashSet::new();
        collect_folder_paths(&tree, &mut collapsed_folders);
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
            global_config_path(&name)
        } else {
            local_config_path(&name)
        };
        let is_rename = original_name.as_deref() != Some(name.as_str());
        if path.exists() && (original_name.is_none() || is_rename) {
            set_error(&mut self.mode, &format!("'{}' already exists", name));
            return;
        }

        // When editing, preserve headers/query/body/extract from the original file.
        let (headers, query, body, extract) = original_name
            .as_deref()
            .and_then(|orig| RequestConfig::load(orig).ok())
            .map(|c| (c.headers, c.query, c.body, c.extract))
            .unwrap_or_default();

        let config = RequestConfig {
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
                    global_config_path(old_name)
                } else {
                    local_config_path(old_name)
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

    // Returns true when the event loop should exit.
    fn handle_key(&mut self, key: KeyEvent) -> bool {
        if self.filter_active && matches!(self.mode, Mode::Browse) {
            return handlers::handle_key_filter(self, key);
        }
        if matches!(self.mode, Mode::Browse) {
            return handlers::handle_key_browse(self, key.code);
        }
        if matches!(self.mode, Mode::ProfileSelect { .. }) {
            return handlers::handle_key_profile_select(self, key.code);
        }
        if matches!(self.mode, Mode::VarInput { .. }) {
            return handlers::handle_key_var_input(self, key);
        }
        if matches!(self.mode, Mode::TestInput { .. }) {
            return handlers::handle_key_test_input(self, key);
        }
        if matches!(self.mode, Mode::NewRequest { .. }) {
            return handlers::handle_key_new_request(self, key);
        }
        if matches!(self.mode, Mode::NewProfile { .. }) {
            return handlers::handle_key_new_profile(self, key);
        }
        if matches!(self.mode, Mode::ConfirmDelete { .. }) {
            return handlers::handle_key_confirm_delete(self, key.code);
        }
        if matches!(self.mode, Mode::Response { response_filter_active: true, .. }) {
            return handlers::handle_key_response_filter(self, key);
        }
        handlers::handle_key_response(self, key.code)
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
        terminal.draw(|frame| render::draw(frame, app))?;
        if let Event::Key(key) = event::read()?
            && app.handle_key(key)
        {
            break;
        }
    }
    Ok(())
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

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
