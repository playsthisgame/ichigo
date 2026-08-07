mod tree;
mod render;
mod handlers;

use tree::{EntryKind, Entry, TreeNode, VisibleRow, load_entries, load_entry, build_tree, collect_folder_paths, visible_rows, detect_nerd_fonts};

use anyhow::Result;
use crossterm::{
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyEvent,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, widgets::ListState, Terminal};
use std::{collections::{HashMap, HashSet}, fs, io};
use crate::config::{dir_for, prune_empty_parents, global_config_path, local_config_path, ChainConfig, RequestConfig};

// ─── Modes ────────────────────────────────────────────────────────────────────

/// What the profile picker and variable form are collecting input *for*.
///
/// Both modes are shared by every action that needs a profile or variables, so
/// each has to carry its destination: without it, whatever the user typed would
/// fall through to the single hardcoded terminal step.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PendingAction {
    Run,
    Test,
    Curl,
}

/// What a `Mode::Response` pane is showing. Keeps a real HTTP response, an
/// error, and generated text distinguishable without overloading a status code.
pub(crate) enum ResponseKind {
    Http(u16),
    Error,
    /// A generated cURL command, plus the outcome of copying it.
    Curl { copied: Result<(), String> },
}

enum Mode {
    Browse,
    ProfileSelect {
        entry_name: String,
        profiles: Vec<crate::config::Profile>,
        selected: usize, // 0 = no profile, 1..=n = profiles[selected-1]
        action: PendingAction,
    },
    VarInput {
        entry_name: String,
        vars: Vec<(String, String)>, // (placeholder name, value being typed)
        focused: usize,              // which field the cursor is in
        action: PendingAction,
    },
    TestInput {
        entry_name: String,
        vars: Vec<(String, String)>,
        iterations: String,
        focused: usize, // 0..vars.len() = var fields, vars.len() = iterations field
    },
    Response {
        kind: ResponseKind,
        body: String,
        scroll: u16,
        response_filter: String,
        response_filter_active: bool,
    },
    TestResponse {
        results: crate::tester::TestResults,
    },
    NewRequest {
        draft: RequestDraft,
        error: Option<String>,
    },
    // Paste buffer for importing a cURL command. Confirming parses it and hands
    // the result to `NewRequest`; nothing reaches disk until that form is saved.
    ImportCurl {
        buffer: String,
        error: Option<String>,
    },
    ConfirmDelete {
        entry_name: String,
        global: bool,
    },
    // The draft's profiles, for picking one to edit or delete. The single way
    // into `NewProfile`, so every profile action starts from the same screen.
    // selected: 0..profiles.len() = a profile, profiles.len() = the "new" row —
    // which means an empty list opens with "new" already selected.
    ProfileList {
        draft: RequestDraft,
        selected: usize,
    },
    // Sub-mode for adding or editing one profile. Carries the whole draft so
    // nothing typed into the request form is lost on the way back.
    // focused: 0 = profile name, 1+2i = params[i].key, 2+2i = params[i].value
    NewProfile {
        draft: RequestDraft,
        // Which profile of the draft this edits; None when adding a new one.
        // Drives replace-vs-push at save time — without it, editing a profile
        // and keeping its name appends a second profile under the same name.
        editing: Option<usize>,
        name: String,
        params: Vec<(String, String)>,
        focused: usize,
        error: Option<String>,
    },
}

/// The request being created or edited in `NewRequest`.
///
/// The form edits four fields; a config has more. The rest live here rather
/// than being re-read from disk at save time, because an imported or cloned
/// request has no file to re-read — its headers and body exist only in this
/// draft, and recovering them from disk would silently drop them.
#[derive(Clone, Default)]
pub(crate) struct RequestDraft {
    // (label, value): name, method, url, description
    pub(crate) fields: Vec<(String, String)>,
    pub(crate) focused: usize,
    pub(crate) profiles: Vec<crate::config::Profile>,
    // Some(name) when editing an existing request; None when creating one.
    pub(crate) original_name: Option<String>,
    pub(crate) global: bool,
    // Carried through the form untouched.
    headers: HashMap<String, String>,
    query: HashMap<String, String>,
    body: Option<crate::config::Body>,
    extract: Option<HashMap<String, String>>,
}

impl RequestDraft {
    fn blank() -> Self {
        Self {
            fields: vec![
                ("name".to_string(), String::new()),
                ("method".to_string(), "GET".to_string()),
                ("url".to_string(), String::new()),
                ("description".to_string(), String::new()),
            ],
            ..Default::default()
        }
    }

    /// Builds a draft from a config — an existing request being edited or
    /// cloned, or one parsed out of a pasted cURL command.
    fn from_config(config: RequestConfig, original_name: Option<String>, global: bool) -> Self {
        Self {
            fields: vec![
                ("name".to_string(), original_name.clone().unwrap_or_default()),
                ("method".to_string(), config.method),
                ("url".to_string(), config.url),
                ("description".to_string(), config.description.unwrap_or_default()),
            ],
            focused: 0,
            profiles: config.profiles.unwrap_or_default(),
            original_name,
            global,
            headers: config.headers,
            query: config.query,
            body: config.body,
            extract: config.extract,
        }
    }
}

// ─── App state ────────────────────────────────────────────────────────────────

struct App {
    entries: Vec<Entry>,
    tree: Vec<TreeNode>,
    collapsed_folders: HashSet<String>,
    use_nerd_fonts: bool,
    list_state: ListState,
    pending_g: bool,
    // Height of the response body viewport, recorded each render so `G`
    // can clamp the scroll offset to the actual bottom of the content.
    response_view_height: u16,
    filter: String,
    filter_active: bool,
    // The `?` keymap overlay. A flag rather than a `Mode` variant because it
    // draws *over* whatever pane is up and dismisses back to it — as a mode it
    // would have to remember which of the nine it interrupted, and every action
    // path would gain a state that reaches none of them.
    show_help: bool,
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
            response_view_height: 0,
            filter: String::new(),
            filter_active: false,
            show_help: false,
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

    /// Shows a config-load failure in the Response pane. Actions call this and
    /// then abort — falling back to the cached entry would send the stale values
    /// this whole change exists to stop sending.
    fn show_error(&mut self, e: anyhow::Error) {
        self.show_message(ResponseKind::Error, format!("Error: {e:#}"));
    }

    /// Shows plain text in the Response pane — the single place that builds the
    /// mode, so its five fields aren't spelled out at a dozen call sites.
    fn show_message(&mut self, kind: ResponseKind, body: String) {
        self.mode = Mode::Response {
            kind,
            body,
            scroll: 0,
            response_filter: String::new(),
            response_filter_active: false,
        };
    }

    /// Re-reads entry `idx` from disk and replaces the cached copy, so profiles
    /// and `{{VAR}}` placeholders come from the file as it exists *now* rather
    /// than as it existed when the TUI started.
    ///
    /// Returns false when the config could not be loaded; the caller must not
    /// proceed. The entry keeps its position in `entries`, so `tree` and
    /// `list_state` stay valid and no rebuild is needed.
    fn reload_selected(&mut self, idx: usize) -> bool {
        match load_entry(&self.entries[idx].name) {
            Ok(entry) => {
                self.entries[idx] = entry;
                true
            }
            Err(e) => {
                self.show_error(e);
                false
            }
        }
    }

    /// Rebuilds the entry list and tree from disk, picking up configs created,
    /// renamed, or deleted while the TUI was open.
    ///
    /// The cursor is restored by *name*, since a refresh shifts entry indices.
    /// `collapsed_folders` and `filter` are deliberately left untouched: folders
    /// the user collapsed stay collapsed, and folders appearing for the first
    /// time are absent from the set and therefore start expanded.
    fn reload_entries(&mut self, select: Option<&str>) {
        // With no explicit target, keep the user's place across the refresh.
        let target = select
            .map(str::to_string)
            .or_else(|| self.selected_entry_index().map(|i| self.entries[i].name.clone()));

        let Ok(entries) = load_entries() else { return };
        self.tree = build_tree(&entries);
        self.entries = entries;

        let count = self.visible_count();
        if count == 0 {
            self.list_state.select(None);
            return;
        }
        let pos = target
            .and_then(|name| self.entries.iter().position(|e| e.name == name))
            .and_then(|idx| self.row_position(idx));
        // The selected config may have been deleted on disk; clamp to a valid
        // row rather than dropping the selection entirely.
        let fallback = self.list_state.selected().unwrap_or(0).min(count - 1);
        self.list_state.select(Some(pos.unwrap_or(fallback)));
    }

    /// Position of entry `idx` among the visible rows, through whichever
    /// addressing mode is active — the tree, or the flat filtered list.
    fn row_position(&self, entry_idx: usize) -> Option<usize> {
        if self.using_tree() {
            visible_rows(&self.tree, &self.collapsed_folders)
                .iter()
                .position(|row| matches!(row, VisibleRow::Leaf { entry_idx: i, .. } if *i == entry_idx))
        } else {
            self.filtered_indices().iter().position(|&i| i == entry_idx)
        }
    }

    fn try_run_selected(&mut self) {
        let Some(idx) = self.selected_entry_index() else { return };
        if !self.reload_selected(idx) {
            return;
        }
        let entry = &self.entries[idx];

        let entry_name = entry.name.clone();
        let (var_names, is_chain, profiles) = match &entry.kind {
            EntryKind::Chain { .. } => (vec![], true, vec![]),
            EntryKind::Request { url, headers, query, body_data, profiles, .. } => {
                (extract_var_names(url, headers, query, body_data.as_deref()), false, profiles.clone())
            }
        };

        if is_chain {
            self.start_chain(&entry_name);
            return;
        }

        if !profiles.is_empty() {
            self.mode = Mode::ProfileSelect { entry_name, profiles, selected: 0, action: PendingAction::Run };
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
            self.mode = Mode::VarInput { entry_name, vars, focused: 0, action: PendingAction::Run };
        }
    }

    fn start_chain(&mut self, entry_name: &str) {
        // Already fresh: profiles and step placeholders below both come from this
        // parse, not from the cached entry.
        let chain = match ChainConfig::load(entry_name) {
            Ok(c) => c,
            Err(e) => {
                self.show_error(e);
                return;
            }
        };

        // If the chain defines profiles, ask which one first (same as a request).
        let profiles = chain.profiles.clone().unwrap_or_default();
        if !profiles.is_empty() {
            self.mode = Mode::ProfileSelect {
                entry_name: entry_name.to_string(),
                profiles,
                selected: 0,
                action: PendingAction::Run,
            };
            return;
        }

        // No profiles: gather the placeholders the chain needs and prompt for them.
        let vars: Vec<(String, String)> = chain_var_names(&chain.steps)
            .into_iter()
            .map(|name| {
                let value = std::env::var(&name).unwrap_or_default();
                (name, value)
            })
            .collect();

        if vars.is_empty() {
            self.execute_chain(entry_name, &HashMap::new());
        } else {
            self.mode = Mode::VarInput {
                entry_name: entry_name.to_string(),
                vars,
                focused: 0,
                action: PendingAction::Run,
            };
        }
    }

    fn try_test_selected(&mut self) {
        let Some(idx) = self.selected_entry_index() else { return };
        if !self.reload_selected(idx) {
            return;
        }
        let entry = &self.entries[idx];

        let entry_name = entry.name.clone();
        let is_chain = matches!(&entry.kind, EntryKind::Chain { .. });

        if is_chain {
            self.show_message(ResponseKind::Error, "Testing chains is not supported.".to_string());
            return;
        }

        let (var_names, profiles) = match &entry.kind {
            EntryKind::Request { url, headers, query, body_data, profiles, .. } => {
                (extract_var_names(url, headers, query, body_data.as_deref()), profiles.clone())
            }
            _ => (vec![], vec![]),
        };

        if !profiles.is_empty() {
            self.mode = Mode::ProfileSelect { entry_name, profiles, selected: 0, action: PendingAction::Test };
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

    /// Entry point for `y`: generate a cURL command for the selected request.
    ///
    /// Reuses the profile picker and variable form by tagging them with
    /// `PendingAction::Curl`, so the terminal step renders instead of sending.
    fn try_copy_curl_selected(&mut self) {
        let Some(idx) = self.selected_entry_index() else { return };
        if !self.reload_selected(idx) {
            return;
        }
        let entry = &self.entries[idx];
        let entry_name = entry.name.clone();

        let (var_names, profiles) = match &entry.kind {
            // A chain threads values extracted from one step into the next;
            // a single cURL command has no way to express that.
            EntryKind::Chain { .. } => {
                self.show_message(
                    ResponseKind::Error,
                    "Copying chains as cURL is not supported.".to_string(),
                );
                return;
            }
            EntryKind::Request { url, headers, query, body_data, profiles, .. } => (
                extract_var_names(url, headers, query, body_data.as_deref()),
                profiles.clone(),
            ),
        };

        if !profiles.is_empty() {
            self.mode = Mode::ProfileSelect {
                entry_name,
                profiles,
                selected: 0,
                action: PendingAction::Curl,
            };
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
            self.render_curl(&entry_name, &HashMap::new());
        } else {
            self.mode = Mode::VarInput { entry_name, vars, focused: 0, action: PendingAction::Curl };
        }
    }

    /// Terminal step for `PendingAction::Curl`: build the command, copy it, and
    /// show it along with whether the copy landed.
    fn render_curl(&mut self, entry_name: &str, vars: &HashMap<String, String>) {
        let config = match RequestConfig::load(entry_name) {
            Ok(c) => c,
            Err(e) => {
                self.show_error(e);
                return;
            }
        };
        let command = crate::curl::to_curl(&config, vars);
        let copied = render::copy_to_clipboard(&command);
        self.show_message(ResponseKind::Curl { copied }, command);
    }

    fn is_chain_entry(&self, name: &str) -> bool {
        self.entries
            .iter()
            .any(|e| e.name == name && matches!(e.kind, EntryKind::Chain { .. }))
    }

    fn confirm_profile_select(&mut self) {
        let (entry_name, profile_params, action) = match &self.mode {
            Mode::ProfileSelect { entry_name, profiles, selected, action } => {
                let params: HashMap<String, String> = if *selected == 0 {
                    HashMap::new()
                } else {
                    profiles[*selected - 1].params.clone()
                };
                (entry_name.clone(), params, *action)
            }
            _ => return,
        };

        let is_chain = self.is_chain_entry(&entry_name);
        let (var_names, all_covered) = if is_chain {
            match ChainConfig::load(&entry_name) {
                Ok(chain) => {
                    let names = chain_var_names(&chain.steps);
                    let covered = names.iter().all(|n| profile_params.contains_key(n));
                    (names, covered)
                }
                Err(_) => (vec![], true),
            }
        } else {
            // Re-read rather than consulting `self.entries`: the placeholder list
            // has to match the file the request is about to be built from.
            match load_entry(&entry_name) {
                Ok(Entry { kind: EntryKind::Request { url, headers, query, body_data, .. }, .. }) => {
                    let names = extract_var_names(&url, &headers, &query, body_data.as_deref());
                    let covered = names.iter().all(|n| profile_params.contains_key(n));
                    (names, covered)
                }
                Ok(_) => (vec![], true),
                Err(e) => {
                    self.show_error(e);
                    return;
                }
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

        if action == PendingAction::Test {
            self.mode = Mode::TestInput { entry_name, vars, iterations: "10".to_string(), focused: 0 };
        } else if all_covered && vars.iter().all(|(_, v)| !v.is_empty()) {
            // The profile answered everything, so skip the variable form and go
            // straight to whatever this action's terminal step is.
            let var_map = vars.into_iter().collect();
            match action {
                PendingAction::Curl => self.render_curl(&entry_name, &var_map),
                _ if is_chain => self.execute_chain(&entry_name, &var_map),
                _ => self.execute_request(&entry_name, &var_map),
            }
        } else {
            self.mode = Mode::VarInput { entry_name, vars, focused: 0, action };
        }
    }

    fn execute_test(&mut self, entry_name: &str, vars: &HashMap<String, String>, iterations: usize) {
        match RequestConfig::load(entry_name)
            .and_then(|config| crate::tester::collect_test_results(&config, vars, iterations))
        {
            Ok(results) => self.mode = Mode::TestResponse { results },
            Err(e) => self.show_message(ResponseKind::Error, format!("Error: {e}")),
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

        let kind = if status == 0 { ResponseKind::Error } else { ResponseKind::Http(status) };
        self.show_message(kind, body);
    }

    fn execute_chain(&mut self, entry_name: &str, vars: &HashMap<String, String>) {
        // Run every step in order, threading extracted values forward, and
        // build one combined text block for the Response pane.
        let result = (|| -> Result<(u16, String)> {
            let chain = ChainConfig::load(entry_name)?;
            let mut current_vars = vars.clone();
            let mut out = String::new();
            let mut last_status = 0u16;
            let step_count = chain.steps.len();

            for (i, step) in chain.steps.iter().enumerate() {
                let response = crate::utils::send_request(step, &current_vars, false)?;
                let status = response.status().as_u16();
                last_status = status;

                let is_json = response
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .map(|v| v.contains("application/json"))
                    .unwrap_or(false);

                let text = response.text().unwrap_or_default();
                let pretty = serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|v| serde_json::to_string_pretty(&v).ok())
                    .unwrap_or_else(|| text.clone());

                out.push_str(&format!("▸ {}  [{}]\n", step.name, status));
                if pretty.is_empty() {
                    out.push_str("(empty body)\n");
                } else {
                    out.push_str(&pretty);
                    out.push('\n');
                }

                // Reuse the shared extractor from Layer 2.
                let extracted = crate::runner::extract_values(&step.extract, &text, is_json)?;
                for (k, v) in &extracted {
                    out.push_str(&format!("  ← {k}: {v}\n"));
                }

                if i + 1 < step_count {
                    out.push('\n'); // blank line between steps
                }
                current_vars.extend(extracted); // feed values into the next step
            }

            Ok((last_status, out))
        })();

        let (status, body) = match result {
            Ok((s, b)) => (s, b),
            Err(e) => (0, format!("Error: {e}")),
        };

        let kind = if status == 0 { ResponseKind::Error } else { ResponseKind::Http(status) };
        self.show_message(kind, body);
    }


    /// Parses the pasted command and hands the result to the new-request form.
    ///
    /// On failure the buffer is kept: re-pasting a long command because a flag
    /// was unsupported would be the worst part of the feature.
    fn confirm_import_curl(&mut self) {
        let Mode::ImportCurl { buffer, .. } = &self.mode else { return };
        match crate::curl::from_curl(buffer) {
            Ok(config) => {
                let draft = RequestDraft::from_config(config, None, false);
                self.mode = Mode::NewRequest { draft, error: None };
            }
            Err(e) => {
                if let Mode::ImportCurl { error, .. } = &mut self.mode {
                    *error = Some(format!("{e:#}"));
                }
            }
        }
    }

    /// Opens the selected request in the form for editing.
    ///
    /// Reads the config from disk rather than from the cached entry: the draft
    /// carries headers, query, body, and extract through to the save, and
    /// `Entry` does not hold all of them. Chains have no form and are ignored.
    fn edit_selected(&mut self) {
        let Some((name, global)) = self.selected_request() else { return };
        match RequestConfig::load(&name) {
            Ok(config) => {
                let draft = RequestDraft::from_config(config, Some(name), global);
                self.mode = Mode::NewRequest { draft, error: None };
            }
            Err(e) => self.show_error(e),
        }
    }

    /// Opens the selected request's profiles for editing.
    ///
    /// Re-reads from disk like every other action path: the profile params fed
    /// back into the draft are what gets written on save, so a stale snapshot
    /// here would quietly revert a token rotated in another terminal.
    fn edit_profiles_selected(&mut self) {
        let Some((name, global)) = self.selected_request() else { return };
        match RequestConfig::load(&name) {
            Ok(config) => {
                let draft = RequestDraft::from_config(config, Some(name), global);
                // 0 is the first profile, or the "new" row when there are none.
                self.mode = Mode::ProfileList { draft, selected: 0 };
            }
            Err(e) => self.show_error(e),
        }
    }

    /// Opens a copy of the selected request in the form, unnamed. Everything
    /// but the name is carried over, so a clone is a whole request rather than
    /// just its method and URL.
    fn clone_selected(&mut self) {
        let Some((name, _)) = self.selected_request() else { return };
        match RequestConfig::load(&name) {
            Ok(config) => {
                let draft = RequestDraft::from_config(config, None, false);
                self.mode = Mode::NewRequest { draft, error: None };
            }
            Err(e) => self.show_error(e),
        }
    }

    /// The selected entry's name and location, when it is a request rather than
    /// a chain or a folder row.
    fn selected_request(&self) -> Option<(String, bool)> {
        let idx = self.selected_entry_index()?;
        let entry = &self.entries[idx];
        match entry.kind {
            EntryKind::Request { .. } => Some((entry.name.clone(), entry.global)),
            EntryKind::Chain { .. } => None,
        }
    }

    fn save_new_profile(&mut self) {
        let (profile_name, params, editing, mut draft) = match &self.mode {
            Mode::NewProfile { name, params, editing, draft, .. } => {
                (name.trim().to_string(), params.clone(), *editing, draft.clone())
            }
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

        let profile = crate::config::Profile { name: profile_name, params: filtered_params };
        match upsert_profile(&mut draft.profiles, profile, editing) {
            Ok(selected) => self.mode = Mode::ProfileList { draft, selected },
            Err(message) => {
                if let Mode::NewProfile { error, .. } = &mut self.mode {
                    *error = Some(message);
                }
            }
        }
    }

    fn save_new_request(&mut self) {
        let draft = match &self.mode {
            Mode::NewRequest { draft, .. } => draft.clone(),
            _ => return,
        };
        let name = draft.fields[0].1.trim().to_string();
        let method = draft.fields[1].1.trim().to_uppercase();
        let url = draft.fields[2].1.trim().to_string();
        let description = draft.fields[3].1.trim().to_string();
        let RequestDraft { profiles, original_name, global, headers, query, body, extract, .. } = draft;

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

        self.reload_entries(Some(&name));
        self.mode = Mode::Browse;
    }

    /// Delivers a bracketed paste to whichever field has focus.
    ///
    /// Enabling bracketed paste means pasted text arrives here instead of as a
    /// run of `Char` events, so every text field has to be served — miss one and
    /// pasting into it becomes a silent no-op.
    fn handle_paste(&mut self, text: &str) {
        // Only the import buffer is multi-line; everywhere else a pasted newline
        // would be invisible at best.
        let single_line = || text.replace(['\r', '\n'], " ");

        if self.filter_active && matches!(self.mode, Mode::Browse) {
            self.filter.push_str(&single_line());
            let count = self.visible_count();
            self.list_state.select(if count == 0 { None } else { Some(0) });
            return;
        }

        match &mut self.mode {
            Mode::ImportCurl { buffer, error } => {
                buffer.push_str(text);
                *error = None;
            }
            Mode::NewRequest { draft, error } => {
                let focused = draft.focused;
                draft.fields[focused].1.push_str(&single_line());
                *error = None;
            }
            Mode::NewProfile { name, params, focused, error, .. } => {
                *error = None;
                if *focused == 0 {
                    name.push_str(&single_line());
                } else {
                    let idx = (*focused - 1) / 2;
                    let is_value = (*focused - 1) % 2 == 1;
                    if let Some((k, v)) = params.get_mut(idx) {
                        if is_value { v.push_str(&single_line()) } else { k.push_str(&single_line()) }
                    }
                }
            }
            Mode::VarInput { vars, focused, .. } => {
                if let Some((_, value)) = vars.get_mut(*focused) {
                    value.push_str(&single_line());
                }
            }
            Mode::TestInput { vars, focused, iterations, .. } => {
                if *focused < vars.len() {
                    vars[*focused].1.push_str(&single_line());
                } else {
                    // The iterations field only ever accepts digits.
                    iterations.extend(text.chars().filter(char::is_ascii_digit));
                }
            }
            Mode::Response { response_filter, response_filter_active: true, .. } => {
                response_filter.push_str(&single_line());
            }
            _ => {}
        }
    }

    // Returns true when the event loop should exit.
    fn handle_key(&mut self, key: KeyEvent) -> bool {
        // Any key dismisses the overlay and is swallowed doing so. Letting the
        // key through would mean `d` closes help *and* opens the delete
        // confirmation for whatever happened to be selected behind it.
        if self.show_help {
            self.show_help = false;
            return false;
        }
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
        if matches!(self.mode, Mode::ImportCurl { .. }) {
            return handlers::handle_key_import_curl(self, key);
        }
        if matches!(self.mode, Mode::ProfileList { .. }) {
            return handlers::handle_key_profile_list(self, key);
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
    // Bracketed paste makes a multi-line paste arrive as one event instead of a
    // stream of keystrokes with Enters in it. It must be turned back off on
    // every exit path, or the terminal keeps emitting paste markers.
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = event_loop(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), DisableBracketedPaste, LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> Result<()> {
    loop {
        terminal.draw(|frame| render::draw(frame, app))?;
        match event::read()? {
            Event::Key(key) => {
                if app.handle_key(key) {
                    break;
                }
            }
            Event::Paste(text) => app.handle_paste(&text),
            _ => {}
        }
    }
    Ok(())
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Places `profile` into `profiles` — replacing the one at `editing`, or
/// appending when that is `None` — and returns the index it landed at.
///
/// A name already taken by a *different* profile is refused. Two profiles under
/// one name would leave the picker ambiguous and the second one unreachable,
/// and `--profile <name>` on the CLI no better off. Editing a profile without
/// renaming it is not a clash, which is why `editing` is needed here at all.
fn upsert_profile(
    profiles: &mut Vec<crate::config::Profile>,
    profile: crate::config::Profile,
    editing: Option<usize>,
) -> Result<usize, String> {
    let clashes = profiles
        .iter()
        .enumerate()
        .any(|(i, p)| p.name == profile.name && Some(i) != editing);
    if clashes {
        return Err(format!("A profile named '{}' already exists", profile.name));
    }

    match editing {
        Some(i) if i < profiles.len() => {
            profiles[i] = profile;
            Ok(i)
        }
        _ => {
            profiles.push(profile);
            Ok(profiles.len() - 1)
        }
    }
}

// Scans all string fields of a config for {{NAME}} placeholders, preserving
// the order they first appear and deduplicating. Mirrors the logic in utils::interpolate.
// Collects the {{VAR}} placeholders a chain needs the user to supply.
// Skips names that an earlier step produces via `extract`, since those are
// filled in automatically as the chain runs.
fn chain_var_names(steps: &[RequestConfig]) -> Vec<String> {
    let mut produced: HashSet<String> = HashSet::new();
    for step in steps {
        if let Some(extract) = &step.extract {
            for key in extract.keys() {
                produced.insert(key.clone());
            }
        }
    }

    let mut names: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for step in steps {
        let body = step.body.as_ref().map(|b| b.data.as_str());
        for name in extract_var_names(&step.url, &step.headers, &step.query, body) {
            if !produced.contains(&name) && seen.insert(name.clone()) {
                names.push(name);
            }
        }
    }
    names
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Body;

    // Minimal RequestConfig builder for tests; callers tweak the fields they care about.
    fn step(url: &str) -> RequestConfig {
        RequestConfig {
            name: "step".to_string(),
            method: "GET".to_string(),
            url: url.to_string(),
            description: None,
            headers: HashMap::new(),
            query: HashMap::new(),
            body: None,
            extract: None,
            profiles: None,
        }
    }

    #[test]
    fn chain_var_names_collects_across_fields() {
        let mut s = step("https://api/{{USER_ID}}");
        s.query.insert("page".to_string(), "{{PAGE}}".to_string());
        s.body = Some(Body {
            content_type: "application/json".to_string(),
            data: "{\"q\": \"{{SEARCH}}\"}".to_string(),
        });
        let names = chain_var_names(&[s]);
        // url, query, and body placeholders are all picked up.
        assert!(names.contains(&"USER_ID".to_string()));
        assert!(names.contains(&"PAGE".to_string()));
        assert!(names.contains(&"SEARCH".to_string()));
    }

    #[test]
    fn chain_var_names_excludes_extracted_and_dedupes() {
        // Step 1 prompts for USER_ID and produces USERNAME via extract.
        let mut s1 = step("https://api/users/{{USER_ID}}");
        let mut extract = HashMap::new();
        extract.insert("USERNAME".to_string(), "$.username".to_string());
        s1.extract = Some(extract);

        // Step 2 reuses USER_ID (should dedupe) and consumes USERNAME (should be excluded).
        let mut s2 = step("https://api/posts");
        s2.query.insert("userId".to_string(), "{{USER_ID}}".to_string());
        s2.headers.insert("X-User".to_string(), "{{USERNAME}}".to_string());

        let names = chain_var_names(&[s1, s2]);
        // Only the truly user-supplied variable remains, listed once.
        assert_eq!(names, vec!["USER_ID".to_string()]);
    }

    fn full_config() -> RequestConfig {
        let mut c = step("https://api/x");
        c.method = "POST".to_string();
        c.description = Some("desc".to_string());
        c.headers.insert("Accept".to_string(), "application/json".to_string());
        c.query.insert("page".to_string(), "2".to_string());
        c.body = Some(Body {
            content_type: "application/json".to_string(),
            data: r#"{"a":1}"#.to_string(),
        });
        c.extract = Some(HashMap::from([("TOKEN".to_string(), "$.token".to_string())]));
        c.profiles = Some(vec![crate::config::Profile {
            name: "dev".to_string(),
            params: HashMap::new(),
        }]);
        c
    }

    /// The form edits four fields; everything else has to survive the trip
    /// through it. Saving used to recover these from the original file, which
    /// silently dropped them for a request that has no file yet.
    #[test]
    fn draft_carries_the_fields_the_form_does_not_edit() {
        let draft = RequestDraft::from_config(full_config(), Some("api/x".to_string()), false);

        assert_eq!(draft.fields[0].1, "api/x");
        assert_eq!(draft.fields[1].1, "POST");
        assert_eq!(draft.fields[2].1, "https://api/x");
        assert_eq!(draft.fields[3].1, "desc");
        assert_eq!(draft.headers.get("Accept").unwrap(), "application/json");
        assert_eq!(draft.query.get("page").unwrap(), "2");
        assert_eq!(draft.body.expect("body").data, r#"{"a":1}"#);
        assert_eq!(draft.extract.expect("extract").get("TOKEN").unwrap(), "$.token");
        assert_eq!(draft.profiles.len(), 1);
    }

    /// A clone is a whole request, not just its method and URL — and it has no
    /// file on disk to recover the rest from.
    #[test]
    fn a_clone_draft_keeps_everything_but_the_name() {
        let draft = RequestDraft::from_config(full_config(), None, false);

        assert_eq!(draft.fields[0].1, "", "a clone starts unnamed");
        assert!(draft.original_name.is_none());
        assert_eq!(draft.headers.len(), 1);
        assert!(draft.body.is_some());
        assert!(draft.extract.is_some());
    }

    /// A draft built from a parsed cURL command reaches the form intact.
    #[test]
    fn an_imported_draft_carries_the_parsed_request() {
        let config = crate::curl::from_curl(
            r#"curl -X POST 'https://api/x?page=2' -H 'Accept: application/json' --data-raw '{"a":1}'"#,
        )
        .expect("parses");
        let draft = RequestDraft::from_config(config, None, false);

        assert_eq!(draft.fields[0].1, "", "the user names an imported request");
        assert_eq!(draft.fields[1].1, "POST");
        assert_eq!(draft.fields[2].1, "https://api/x");
        assert_eq!(draft.query.get("page").unwrap(), "2");
        assert_eq!(draft.headers.get("Accept").unwrap(), "application/json");
        assert_eq!(draft.body.expect("body").data, r#"{"a":1}"#);
    }

    fn profile(name: &str, params: &[(&str, &str)]) -> crate::config::Profile {
        crate::config::Profile {
            name: name.to_string(),
            params: params
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    #[test]
    fn a_new_profile_is_appended() {
        let mut profiles = vec![profile("dev", &[])];
        let at = upsert_profile(&mut profiles, profile("prod", &[("HOST", "api")]), None)
            .expect("no clash");
        assert_eq!(at, 1);
        assert_eq!(profiles.len(), 2);
        assert_eq!(profiles[1].name, "prod");
    }

    /// The bug this feature exists to fix: editing a profile used to push a
    /// second one, so a request ended up with two `dev` profiles and the picker
    /// could only ever reach the first.
    #[test]
    fn editing_a_profile_replaces_it_rather_than_appending() {
        let mut profiles = vec![profile("dev", &[("TOKEN", "old")]), profile("prod", &[])];
        let at = upsert_profile(
            &mut profiles,
            profile("dev", &[("TOKEN", "new")]),
            Some(0),
        )
        .expect("keeping its own name is not a clash");

        assert_eq!(at, 0);
        assert_eq!(profiles.len(), 2, "no duplicate appended");
        assert_eq!(profiles[0].params.get("TOKEN").unwrap(), "new");
        assert_eq!(profiles[1].name, "prod", "the others are undisturbed");
    }

    #[test]
    fn a_profile_can_be_renamed() {
        let mut profiles = vec![profile("dev", &[("HOST", "localhost")])];
        let at = upsert_profile(&mut profiles, profile("local", &[("HOST", "localhost")]), Some(0))
            .expect("no clash");
        assert_eq!(at, 0);
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].name, "local");
    }

    #[test]
    fn a_name_another_profile_already_has_is_refused() {
        let mut profiles = vec![profile("dev", &[]), profile("prod", &[])];

        let err = upsert_profile(&mut profiles, profile("prod", &[]), None)
            .expect_err("adding a second `prod`");
        assert!(err.contains("prod"), "the error names the profile: {err}");

        let err = upsert_profile(&mut profiles, profile("prod", &[]), Some(0))
            .expect_err("renaming `dev` onto `prod`");
        assert!(err.contains("prod"));

        assert_eq!(profiles.len(), 2, "nothing was written on either refusal");
        assert_eq!(profiles[0].name, "dev");
    }

    /// `editing` indexes a list that a delete may have shortened. Falling back
    /// to append keeps a stale index from panicking or overwriting a neighbour.
    #[test]
    fn an_out_of_range_edit_index_appends() {
        let mut profiles = vec![profile("dev", &[])];
        let at = upsert_profile(&mut profiles, profile("prod", &[]), Some(9)).expect("no clash");
        assert_eq!(at, 1);
        assert_eq!(profiles.len(), 2);
    }
}
