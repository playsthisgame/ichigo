mod tree;
mod render;
mod handlers;
mod edit;

use edit::Edit;
use tree::{EntryKind, Entry, TreeNode, VisibleRow, load_entries, load_entry, build_tree, collect_folder_paths, visible_rows, detect_nerd_fonts};

use anyhow::Result;
use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyEvent, MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, widgets::ListState, Terminal};
use ratatui_image::{
    picker::{Picker, ProtocolType},
    protocol::StatefulProtocol,
};
use std::{collections::{HashMap, HashSet}, fs, io, io::IsTerminal};
use crate::config::{dir_for, prune_empty_parents, global_config_path, local_config_path, ChainConfig, Keys, RequestConfig, UserConfig, MAX_SPLIT_PCT, MIN_SPLIT_PCT};

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
        edit: Edit,                  // the caret within that field
        action: PendingAction,
    },
    TestInput {
        entry_name: String,
        vars: Vec<(String, String)>,
        iterations: String,
        focused: usize, // 0..vars.len() = var fields, vars.len() = iterations field
        edit: Edit,
    },
    Response {
        kind: ResponseKind,
        body: String,
        scroll: u16,
        response_filter: String,
        response_filter_active: bool,
        // Index into the *filtered* lines, not the body's. Any change to the
        // filter has to reset it, or it points at a line that is no longer
        // shown and `y` copies something the pane never displayed.
        cursor: usize,
        // Where a visual selection started; None when not selecting. The
        // selection runs between it and `cursor` in either direction.
        anchor: Option<usize>,
        // Transient feedback ("copied 2 lines"), shown beside the status badge
        // and cleared by the next keypress.
        status: Option<String>,
        // A decoded image body, already handed to the terminal's graphics
        // protocol. `None` for every text response, and also for an image the
        // terminal cannot draw — in that case `body` (the summary line) is the
        // whole of what the pane shows.
        //
        // It lives here rather than on `App` so it dies with the pane, the way
        // every other mode's state does. Note this makes `Mode` un-`Clone`able:
        // `StatefulProtocol` owns the encoded image and is not `Clone`.
        image: Option<Box<StatefulProtocol>>,
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
    // The draft's headers *or* query params as an editable key/value list.
    // focused: 2i = pairs[i].name, 2i+1 = pairs[i].value
    //
    // One mode for both because they are the same screen: two fields per row,
    // the same Tab walk, the same add/remove chords, the same
    // edits-live-in-the-draft-until-saved contract. `kind` names which map the
    // rows come from and go back to, and is the only thing the handler and the
    // renderer branch on — see `PairKind`.
    EditPairs {
        kind: PairKind,
        draft: RequestDraft,
        pairs: Vec<(String, String)>,
        focused: usize,
        edit: Edit,
        error: Option<String>,
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
        edit: Edit,
        error: Option<String>,
    },
}

/// Constructors for the modes that open on a text field.
///
/// They exist for the caret: each of these modes carries one `Edit` for
/// whichever field has focus, and a form can open on a value that arrived
/// prefilled — from a profile, the environment, or the config on disk. Building
/// them by hand meant repeating that anchoring at seven call sites and getting
/// a caret of 0 on a filled field wherever it was forgotten.
impl Mode {
    /// A response pane showing `body`. Reach it through `App::show_message` /
    /// `show_error` unless there is no `App` yet, as at startup.
    fn message(kind: ResponseKind, body: String) -> Self {
        Mode::message_with_image(kind, body, None)
    }

    /// The same pane with a decoded image drawn under the body text.
    ///
    /// `image` is `None` whenever the response was text, the terminal has no
    /// graphics protocol, or the decode failed — in each case `body` already
    /// says what happened, so this is the one constructor and not two.
    fn message_with_image(
        kind: ResponseKind,
        body: String,
        image: Option<Box<StatefulProtocol>>,
    ) -> Self {
        Mode::Response {
            kind,
            body,
            scroll: 0,
            response_filter: String::new(),
            response_filter_active: false,
            cursor: 0,
            anchor: None,
            status: None,
            image,
        }
    }

    fn var_input(entry_name: String, vars: Vec<(String, String)>, action: PendingAction) -> Self {
        let edit = Edit::at_end(vars.first().map_or("", |(_, v)| v.as_str()));
        Mode::VarInput { entry_name, vars, focused: 0, edit, action }
    }

    /// Opens on the first variable, or on the iteration count when the request
    /// has none.
    fn test_input(entry_name: String, vars: Vec<(String, String)>) -> Self {
        let iterations = "10".to_string();
        let edit = Edit::at_end(vars.first().map_or(iterations.as_str(), |(_, v)| v.as_str()));
        Mode::TestInput { entry_name, vars, iterations, focused: 0, edit }
    }

    fn edit_pairs(kind: PairKind, draft: RequestDraft) -> Self {
        let pairs = kind.pairs(&draft);
        let edit = Edit::at_end(pairs.first().map_or("", |(name, _)| name.as_str()));
        Mode::EditPairs { kind, draft, pairs, focused: 0, edit, error: None }
    }

    fn new_profile(
        draft: RequestDraft,
        editing: Option<usize>,
        name: String,
        params: Vec<(String, String)>,
    ) -> Self {
        let edit = Edit::at_end(&name);
        Mode::NewProfile { draft, editing, name, params, focused: 0, edit, error: None }
    }
}

/// Which of the draft's two name/value maps a `Mode::EditPairs` is editing.
///
/// The pane itself is one screen serving both. Everything that genuinely
/// differs hangs off this enum rather than off a duplicated mode, handler, and
/// renderer — which is the arrangement that would drift, since a fix to the
/// caret handling or the add/remove chords would have to be made twice.
///
/// What differs is only: the words on screen, and the rules in `apply`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PairKind {
    Headers,
    Query,
}

impl PairKind {
    /// The pane title.
    pub(crate) fn title(self) -> &'static str {
        match self {
            PairKind::Headers => " Headers ",
            PairKind::Query => " Query params ",
        }
    }

    /// The singular noun for a row, used in labels, hints, and errors.
    pub(crate) fn noun(self) -> &'static str {
        match self {
            PairKind::Headers => "header",
            PairKind::Query => "param",
        }
    }

    /// What the pane says when there is nothing in it.
    pub(crate) fn empty_hint(self) -> &'static str {
        match self {
            PairKind::Headers => "  No headers. Ctrl+a adds one.",
            PairKind::Query => "  No query params. Ctrl+a adds one.",
        }
    }

    /// The draft's current rows for this map, ordered.
    fn pairs(self, draft: &RequestDraft) -> Vec<(String, String)> {
        let map = match self {
            PairKind::Headers => &draft.headers,
            PairKind::Query => &draft.query,
        };
        // Sorted because a `HashMap` has no order of its own — without this the
        // rows would shuffle every time the editor is opened.
        let mut pairs: Vec<(String, String)> =
            map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
        pairs
    }

    /// Folds edited rows back into the draft, or refuses them by name.
    fn apply(self, draft: &mut RequestDraft, pairs: Vec<(String, String)>) -> Result<(), String> {
        match self {
            PairKind::Headers => apply_headers(draft, pairs),
            PairKind::Query => apply_query(draft, pairs),
        }
    }
}

/// What `RequestDraft::focused` is pointing at.
///
/// Tab walks the four text fields and then four rows that are *actions* — the
/// global flag, the headers pane, the query pane, the profiles pane — so that
/// everything the form can reach is reachable by walking it, with no chord to
/// know in advance. Text and actions have to stay distinguishable because only
/// the first has a caret: handing an action row to `edit::apply` would index
/// `fields` out of range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Focus {
    Field(usize),
    Global,
    Headers,
    Query,
    Profiles,
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
    // Indexes the text fields first, then the three action rows. Read it
    // through `focus_target` rather than comparing it to `fields.len()`.
    pub(crate) focused: usize,
    // The caret in `fields[focused]`, and stale while an action row has focus —
    // nothing reads it there, because no field's index can match. Travels with
    // the draft so a trip through the headers or profiles pane comes back to
    // the same spot in the same field.
    edit: Edit,
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
        let name = original_name.clone().unwrap_or_default();
        Self {
            edit: Edit::at_end(&name),
            fields: vec![
                ("name".to_string(), name),
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

    pub(crate) fn headers(&self) -> &HashMap<String, String> {
        &self.headers
    }

    pub(crate) fn query(&self) -> &HashMap<String, String> {
        &self.query
    }

    /// The number of Tab stops: the text fields, then the four action rows.
    pub(crate) fn rows(&self) -> usize {
        self.fields.len() + 4
    }

    /// What `focused` is pointing at. Anything past the last field is an action
    /// row; the order here is the order they are drawn in, and headers sit
    /// beside query because that is the order a request config lists them in.
    pub(crate) fn focus_target(&self) -> Focus {
        match self.focused.checked_sub(self.fields.len()) {
            None => Focus::Field(self.focused),
            Some(0) => Focus::Global,
            Some(1) => Focus::Headers,
            Some(2) => Focus::Query,
            _ => Focus::Profiles,
        }
    }

    /// Moves focus and puts the caret at the end of the field it lands on.
    ///
    /// The draft keeps one caret for whichever field has focus, so every focus
    /// change has to re-anchor it — otherwise Tab carries an offset from the
    /// previous field into a shorter one. An action row has no field to anchor
    /// to, so it leaves the caret alone; nothing reads it there, and Tabbing
    /// back onto a field re-anchors it anyway.
    fn focus(&mut self, idx: usize) {
        self.focused = idx;
        if let Some((_, value)) = self.fields.get(idx) {
            self.edit = Edit::at_end(value);
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
    // Read once at launch from `config.toml`. Not reloaded mid-session: a keymap
    // changing under a half-typed field would be worse than not reloading it.
    keys: Keys,
    mode: Mode,
    // ── The draggable split ──
    // The list pane's share of the width, seeded from `layout.split_pct` and
    // then moved by dragging. A drag is not written back to the file: the
    // config is the width a session *starts* at, and a drag to read one long
    // response should not silently become the permanent setting.
    split_pct: u16,
    // Where the divider was actually drawn on the last frame, and the width it
    // was drawn against. Recorded by `render::draw` rather than recomputed here,
    // because a click has to be tested against the geometry the user is looking
    // at — re-deriving it from `split_pct` would repeat the layout solver's
    // rounding and land a column off on some widths.
    split_x: u16,
    term_width: u16,
    dragging_split: bool,
    // How this terminal draws pixels, probed once at launch. `None` means it
    // cannot, and every image response falls back to its summary line.
    //
    // Not reloadable for the same reason `keys` isn't, and a stronger one: the
    // probe writes control sequences to stdout and reads the reply off stdin,
    // which is only safe before the alternate screen is up.
    picker: Option<Picker>,
}

/// How far either side of the divider still counts as grabbing it. The divider
/// is two adjacent border columns (the list's right, the detail's left), so a
/// pixel-exact hit test would make it a one-column target on a mouse that
/// reports whole cells.
const DIVIDER_GRAB: u16 = 1;

/// The `split_pct` that puts the divider under `column` on a terminal `total`
/// columns wide, clamped to the pane bounds. `None` when there is no width to
/// divide — before the first frame, `App::term_width` is still 0.
///
/// The divider is drawn at the right pane's first column, so the left pane is
/// exactly `column` columns wide: this is the inverse of the layout solver, and
/// rounds to nearest so the divider tracks the pointer rather than trailing it.
fn split_pct_at(column: u16, total: u16) -> Option<u16> {
    if total == 0 {
        return None;
    }
    let total = u32::from(total);
    let pct = (u32::from(column) * 100 + total / 2) / total;
    Some((pct as u16).clamp(MIN_SPLIT_PCT, MAX_SPLIT_PCT))
}

/// Probes the terminal for a graphics protocol, once, at launch.
///
/// Two-stage by way of `ratatui-image`, the same shape yazi uses: guess from
/// `$TERM` / `$TERM_PROGRAM`, and where that says nothing, write control
/// sequences and read the reply. The reply arrives on **stdin**, which is why
/// this has to run before the alternate screen and raw mode are set up — it is
/// called from `App::new`, and `run` builds the `App` first for that reason.
///
/// A failed probe is not an error: there is then nothing to draw an image on,
/// and the summary line is the honest answer.
///
/// The `is_terminal` guard is load-bearing, and not just an optimization.
/// `ratatui-image` runs the query on a detached thread and gives up on it after
/// one second, but the thread itself has no timeout — it blocks in
/// `stdin().read()` until something parses as a Device Status Report. Where
/// nothing ever answers, that abandoned thread goes on consuming stdin, so the
/// keys meant for the event loop are swallowed one by one, and it finally calls
/// `disable_raw_mode` on the way out — under a TUI that is by then running in
/// it. The two ways to get there are exactly the two this refuses:
///
/// * `ichigo | cat` — the query goes down the pipe, so the terminal never sees
///   it and never replies.
/// * `ichigo < /dev/null` — `read` returns `Ok(0)` forever and its loop spins.
///
/// With a terminal on both ends the trailing `\x1b[5n` is answered by
/// essentially everything, which is why it is in the query at all, so the
/// thread ends promptly and the one-second bound is the worst case.
///
/// `Halfblocks` is refused deliberately. It is `ratatui-image`'s universal
/// fallback and would render *something* in any terminal, but a quarter-scale
/// mosaic of colour blocks sitting in the response pane reads as what the
/// server sent, and it isn't.
fn detect_picker() -> Option<Picker> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return None;
    }
    let picker = Picker::from_query_stdio().ok()?;
    (picker.protocol_type() != ProtocolType::Halfblocks).then_some(picker)
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
        // A broken user config opens the TUI on the message and runs on
        // defaults, rather than either refusing to start over a preference or
        // silently ignoring a keymap someone is waiting to see work.
        let (config, mode) = match UserConfig::load() {
            Ok(config) => (config, Mode::Browse),
            Err(e) => (
                UserConfig::defaults(),
                Mode::message(ResponseKind::Error, format!("Config: {e:#}")),
            ),
        };
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
            keys: config.keys,
            mode,
            split_pct: config.split_pct,
            split_x: 0,
            term_width: 0,
            dragging_split: false,
            picker: detect_picker(),
        })
    }

    // ─── Mouse ────────────────────────────────────────────────────────────────

    /// The mouse does one thing: drag the divider between the two panes.
    ///
    /// It is deliberately mode-independent — both panes are drawn in every mode,
    /// so widening the detail pane to read a response works the same as widening
    /// it to fill in a form, and a drag never has to be a keystroke some mode
    /// would rather have as text.
    fn handle_mouse(&mut self, ev: MouseEvent) {
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.dragging_split = self.split_x.abs_diff(ev.column) <= DIVIDER_GRAB;
            }
            // The button state in a drag event is the button that started it, so
            // this cannot fire from a right-drag that began elsewhere.
            MouseEventKind::Drag(MouseButton::Left) if self.dragging_split => {
                self.set_split_at(ev.column);
            }
            MouseEventKind::Up(MouseButton::Left) => self.dragging_split = false,
            _ => {}
        }
    }

    /// Puts the divider under `column`, leaving it where it is if the terminal
    /// width has not been recorded yet (no frame drawn, so no divider to grab).
    fn set_split_at(&mut self, column: u16) {
        if let Some(pct) = split_pct_at(column, self.term_width) {
            self.split_pct = pct;
        }
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
        self.mode = Mode::message(kind, body);
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
            self.mode = Mode::var_input(entry_name, vars, PendingAction::Run);
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
            self.mode = Mode::var_input(entry_name.to_string(), vars, PendingAction::Run);
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

        self.mode = Mode::test_input(entry_name, vars);
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
            self.mode = Mode::var_input(entry_name, vars, PendingAction::Curl);
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
            self.mode = Mode::test_input(entry_name, vars);
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
            self.mode = Mode::var_input(entry_name, vars, action);
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

        let (status, body, image) = match result {
            Ok(response) => {
                let status = response.status().as_u16();
                // Read the type before the body: `text()` and `bytes()` both
                // consume the response, so the branch has to be decided first.
                // An image read as text is lossy UTF-8 over binary — the
                // mojibake this whole path exists to stop showing.
                let content_type = response
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();

                if crate::media::is_renderable_image(&content_type) {
                    let (body, image) = self.render_image_body(&content_type, response);
                    (status, body, image)
                } else {
                    let text = response.text().unwrap_or_else(|e| e.to_string());
                    // Pretty-print JSON if the body parses as such.
                    let body = serde_json::from_str::<serde_json::Value>(&text)
                        .ok()
                        .and_then(|v| serde_json::to_string_pretty(&v).ok())
                        .unwrap_or(text);
                    (status, body, None)
                }
            }
            Err(e) => (0, format!("Error: {e}"), None),
        };

        let kind = if status == 0 { ResponseKind::Error } else { ResponseKind::Http(status) };
        self.mode = Mode::message_with_image(kind, body, image);
    }

    /// Turns an image response into the pane's two halves: the summary line
    /// that is always shown, and the protocol handle that is shown when the
    /// terminal can draw one.
    ///
    /// Every failure below degrades to summary-plus-note rather than to an
    /// error pane. The request itself succeeded — a `200` that we could not
    /// decode is still a `200`, and showing it as an error would misreport the
    /// server.
    fn render_image_body(
        &self,
        content_type: &str,
        response: reqwest::blocking::Response,
    ) -> (String, Option<Box<StatefulProtocol>>) {
        let bytes = match response.bytes() {
            Ok(bytes) => bytes,
            Err(e) => return (format!("{content_type}\n\nCould not read body: {e}"), None),
        };

        let decoded = match crate::media::decode(&bytes) {
            Ok(image) => image,
            Err(e) => {
                let summary = crate::media::summarize(content_type, bytes.len(), None);
                return (format!("{summary}\n\nCould not decode: {e:#}"), None);
            }
        };

        let dims = (decoded.width(), decoded.height());
        let summary = crate::media::summarize(content_type, bytes.len(), Some(dims));

        match &self.picker {
            Some(picker) => {
                (summary, Some(Box::new(picker.new_resize_protocol(decoded))))
            }
            // No graphics protocol: the summary is the whole of the pane, and
            // it says so rather than leaving a blank space where an image was
            // expected.
            None => (
                format!("{summary}\n\nThis terminal has no image protocol ichigo can use."),
                None,
            ),
        }
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

                let content_type = response
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                let is_json = content_type.contains("application/json");

                // A chain renders as one combined text block, so an image step
                // contributes its summary line rather than an inline image —
                // but it must not contribute its *bytes*, which is what reading
                // it as text used to do. `text` stays empty so a step that also
                // declares `extract` fails with the honest "response is not
                // JSON" below instead of on mojibake.
                let (text, pretty) = if crate::media::is_renderable_image(&content_type) {
                    let len = response.bytes().map(|b| b.len()).unwrap_or(0);
                    (String::new(), crate::media::summarize(&content_type, len, None))
                } else {
                    let text = response.text().unwrap_or_default();
                    let pretty = serde_json::from_str::<serde_json::Value>(&text)
                        .ok()
                        .and_then(|v| serde_json::to_string_pretty(&v).ok())
                        .unwrap_or_else(|| text.clone());
                    (text, pretty)
                };

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

        match serde_yaml_ng::to_string(&config) {
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
                edit::insert_str(&mut draft.fields[focused].1, &mut draft.edit, &single_line());
                *error = None;
            }
            Mode::NewProfile { name, params, focused, edit, error, .. } => {
                *error = None;
                if let Some(value) = profile_field(name, params, *focused) {
                    edit::insert_str(value, edit, &single_line());
                }
            }
            Mode::EditPairs { pairs, focused, edit, error, .. } => {
                *error = None;
                if let Some(value) = pair_field(pairs, *focused) {
                    edit::insert_str(value, edit, &single_line());
                }
            }
            Mode::VarInput { vars, focused, edit, .. } => {
                if let Some((_, value)) = vars.get_mut(*focused) {
                    edit::insert_str(value, edit, &single_line());
                }
            }
            Mode::TestInput { vars, focused, iterations, edit, .. } => {
                if *focused < vars.len() {
                    edit::insert_str(&mut vars[*focused].1, edit, &single_line());
                } else {
                    // The iterations field only ever accepts digits.
                    let digits: String = text.chars().filter(|c| c.is_ascii_digit()).collect();
                    edit::insert_str(iterations, edit, &digits);
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
            // Browse binds bare letters and reads only the `KeyCode`, so a
            // `Ctrl+<letter>` — which crossterm reports as `Char` plus a
            // CONTROL modifier — would run the unmodified binding. That made
            // `Ctrl+q` quit the TUI, which stopped being merely odd once
            // `Ctrl+q` became "edit query params" one pane away: a chord worth
            // learning must not end the session where it is not bound. Browse
            // has no chords of its own, so refusing all of them here is the
            // same central refusal `edit::apply` performs for form fields.
            if key.modifiers.contains(event::KeyModifiers::CONTROL) {
                return false;
            }
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
        if matches!(self.mode, Mode::EditPairs { .. }) {
            return handlers::handle_key_edit_pairs(self, key);
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
    //
    // Mouse capture is what makes the pane divider draggable, and it costs the
    // terminal's own click-drag text selection while ichigo is up — most
    // terminals give that back under Shift (Option on macOS). The response
    // pane's `V`/`y` copy does not depend on it either way. It has the same
    // must-be-disabled-on-exit obligation as bracketed paste.
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = event_loop(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        DisableBracketedPaste,
        LeaveAlternateScreen
    )?;
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
            Event::Mouse(mouse) => app.handle_mouse(mouse),
            _ => {}
        }
    }
    Ok(())
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Folds edited header pairs back into a draft.
///
/// Rows with a blank name are dropped, so an added-then-abandoned row is not an
/// error. Two rows naming the same header are refused *case-insensitively* —
/// HTTP header names do not distinguish case, so `Accept` and `accept` are one
/// header, and a `HashMap` would silently keep whichever landed last.
///
/// A `Content-Type` row is moved into `body.content_type` when the draft has a
/// body, never left in `headers`. That is the same normalization `from_curl`
/// applies and for the same reason: `to_curl` re-derives the header from the
/// body, so a config holding both emits it twice.
fn apply_headers(draft: &mut RequestDraft, pairs: Vec<(String, String)>) -> Result<(), String> {
    let mut kept: Vec<(String, String)> = Vec::with_capacity(pairs.len());
    for (name, value) in pairs {
        let name = name.trim().to_string();
        if name.is_empty() {
            continue;
        }
        if kept.iter().any(|(k, _): &(String, String)| k.eq_ignore_ascii_case(&name)) {
            return Err(format!("Duplicate header '{name}'"));
        }
        kept.push((name, value.trim().to_string()));
    }

    if let (Some(body), Some(pos)) = (
        draft.body.as_mut(),
        kept.iter().position(|(k, _)| k.eq_ignore_ascii_case("content-type")),
    ) {
        let (_, value) = kept.remove(pos);
        if !value.is_empty() {
            body.content_type = value;
        }
    }

    draft.headers = kept.into_iter().collect();
    Ok(())
}

/// Folds edited query rows back into a draft.
///
/// Rows with a blank name are dropped, as in `apply_headers`, so an
/// added-then-abandoned row is not an error.
///
/// Duplicate names are refused **case-sensitively**, which is the one rule that
/// genuinely differs from headers: HTTP header names are case-insensitive, so
/// `Accept` and `accept` are one header, but a query string's keys are opaque
/// bytes to the server and `page` and `Page` are two different params. Refusing
/// case-insensitively here would reject a request that is perfectly legal to
/// send. The refusal itself is still needed, because `query` is a `HashMap`:
/// two rows with the same name cannot both survive, and silently keeping
/// whichever landed last is the outcome this prevents. It is also why
/// `from_curl` declines to lift a repeated key out of a URL.
///
/// Values are trimmed, as headers are. A query value gets URL-encoded on the
/// way out, so a leading or trailing space would be sent as `%20` rather than
/// ignored — which makes an accidental one a real, and invisible, bug.
fn apply_query(draft: &mut RequestDraft, pairs: Vec<(String, String)>) -> Result<(), String> {
    let mut kept: Vec<(String, String)> = Vec::with_capacity(pairs.len());
    for (name, value) in pairs {
        let name = name.trim().to_string();
        if name.is_empty() {
            continue;
        }
        if kept.iter().any(|(k, _): &(String, String)| k == &name) {
            return Err(format!("Duplicate query param '{name}'"));
        }
        kept.push((name, value.trim().to_string()));
    }

    draft.query = kept.into_iter().collect();
    Ok(())
}

/// The row a `Mode::EditPairs` focus index points at: `2i` is
/// `pairs[i]`'s name, `2i+1` its value. `None` once focus outruns the list,
/// which a deleted row can leave it doing for one keystroke.
fn pair_field(pairs: &mut [(String, String)], focused: usize) -> Option<&mut String> {
    let (name, value) = pairs.get_mut(focused / 2)?;
    Some(if focused % 2 == 1 { value } else { name })
}

/// The profile field a `Mode::NewProfile` focus index points at: `0` is the
/// name, then `1+2i` / `2+2i` are `params[i]`'s key and value.
fn profile_field<'a>(
    name: &'a mut String,
    params: &'a mut [(String, String)],
    focused: usize,
) -> Option<&'a mut String> {
    match focused.checked_sub(1) {
        None => Some(name),
        Some(offset) => pair_field(params, offset),
    }
}

/// The response lines the filter leaves visible.
///
/// What the pane draws, what `G` clamps the cursor against, and what a copy
/// puts on the clipboard are all this list. It exists because those three had
/// drifted: the predicate was spelled out separately at each site and the copy
/// path never got one, so filtering to two lines and pressing `c` handed you
/// the whole body.
pub(super) fn visible_response_lines<'a>(body: &'a str, filter: &str) -> Vec<&'a str> {
    let q = filter.to_lowercase();
    body.lines()
        .filter(|line| q.is_empty() || line.to_lowercase().contains(&q))
        .collect()
}

/// The inclusive line range a visual selection covers, in view order.
///
/// `anchor` may sit either side of the cursor — dragging a selection upward is
/// as ordinary as dragging it down — so the two are sorted rather than assumed.
/// With no anchor the range is the cursor line alone, which is what makes `y`
/// with nothing selected copy one line instead of nothing.
pub(super) fn selection_range(cursor: usize, anchor: Option<usize>) -> (usize, usize) {
    match anchor {
        Some(a) if a <= cursor => (a, cursor),
        Some(a) => (cursor, a),
        None => (cursor, cursor),
    }
}

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

    /// The percentage has to be the inverse of the layout solver, or the
    /// divider steps away from the pointer on the first drag event.
    #[test]
    fn a_drag_puts_the_divider_under_the_pointer() {
        assert_eq!(split_pct_at(40, 100), Some(40));
        assert_eq!(split_pct_at(60, 120), Some(50));
        // Rounds to nearest rather than truncating: 28/80 is 35%, not 34%.
        assert_eq!(split_pct_at(28, 80), Some(35));
    }

    #[test]
    fn a_drag_past_the_edge_stops_at_the_bounds() {
        assert_eq!(split_pct_at(0, 100), Some(MIN_SPLIT_PCT));
        assert_eq!(split_pct_at(100, 100), Some(MAX_SPLIT_PCT));
        // A drag off the right edge reports a column past the width.
        assert_eq!(split_pct_at(400, 100), Some(MAX_SPLIT_PCT));
    }

    /// Before the first frame there is no width recorded, and no divider drawn
    /// to have been grabbed — dividing by it would panic.
    #[test]
    fn a_drag_with_no_frame_drawn_yet_is_ignored() {
        assert_eq!(split_pct_at(10, 0), None);
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

    fn pairs(rows: &[(&str, &str)]) -> Vec<(String, String)> {
        rows.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn headers_are_replaced_wholesale_so_a_removed_row_is_gone() {
        let mut draft = RequestDraft::from_config(full_config(), None, false);
        assert!(draft.headers.contains_key("Accept"));

        apply_headers(&mut draft, pairs(&[("X-Only", "1")])).expect("valid");

        assert_eq!(draft.headers.len(), 1);
        assert!(!draft.headers.contains_key("Accept"), "removed row survived");
        assert_eq!(draft.headers.get("X-Only").unwrap(), "1");
    }

    #[test]
    fn a_blank_name_is_dropped_rather_than_stored() {
        let mut draft = RequestDraft::blank();
        apply_headers(&mut draft, pairs(&[("Accept", "a"), ("  ", "orphan"), ("", "")]))
            .expect("blank rows are not an error");
        assert_eq!(draft.headers.len(), 1);
    }

    #[test]
    fn names_and_values_are_trimmed() {
        let mut draft = RequestDraft::blank();
        apply_headers(&mut draft, pairs(&[("  Accept  ", "  application/json  ")])).unwrap();
        assert_eq!(draft.headers.get("Accept").unwrap(), "application/json");
    }

    /// HTTP header names are case-insensitive, so these are one header. A
    /// `HashMap` would keep both and send whichever it felt like.
    #[test]
    fn a_duplicate_name_is_refused_ignoring_case() {
        let mut draft = RequestDraft::blank();
        let err = apply_headers(&mut draft, pairs(&[("Accept", "a"), ("accept", "b")]))
            .expect_err("same header twice");
        assert!(err.contains("accept"), "the error names the header: {err}");
        assert!(draft.headers.is_empty(), "nothing applied on refusal");
    }

    /// CLAUDE.md's rule: a request with a body keeps its content type in
    /// `body.content_type` and never in `headers`, because `to_curl` re-derives
    /// the header from the body and would otherwise emit it twice.
    #[test]
    fn a_content_type_row_moves_into_the_body() {
        let mut draft = RequestDraft::from_config(full_config(), None, false);
        apply_headers(&mut draft, pairs(&[("Content-Type", "text/plain"), ("Accept", "a")]))
            .unwrap();

        assert_eq!(draft.body.as_ref().unwrap().content_type, "text/plain");
        assert!(
            !draft.headers.keys().any(|k| k.eq_ignore_ascii_case("content-type")),
            "content type left in headers: {:?}",
            draft.headers
        );
        assert_eq!(draft.headers.get("Accept").unwrap(), "a");
    }

    /// With no body there is nothing to re-derive it from, so it stays a header.
    #[test]
    fn a_content_type_row_stays_a_header_when_there_is_no_body() {
        let mut draft = RequestDraft::blank();
        apply_headers(&mut draft, pairs(&[("Content-Type", "text/plain")])).unwrap();
        assert_eq!(draft.headers.get("Content-Type").unwrap(), "text/plain");
    }

    #[test]
    fn query_params_are_replaced_wholesale_so_a_removed_row_is_gone() {
        let mut draft = RequestDraft::from_config(full_config(), None, false);
        assert!(draft.query.contains_key("page"));

        apply_query(&mut draft, pairs(&[("limit", "50")])).expect("valid");

        assert_eq!(draft.query.len(), 1);
        assert!(!draft.query.contains_key("page"), "removed row survived");
        assert_eq!(draft.query.get("limit").unwrap(), "50");
    }

    #[test]
    fn a_blank_query_name_is_dropped_rather_than_stored() {
        let mut draft = RequestDraft::blank();
        apply_query(&mut draft, pairs(&[("page", "2"), ("  ", "orphan"), ("", "")]))
            .expect("blank rows are not an error");
        assert_eq!(draft.query.len(), 1);
    }

    #[test]
    fn query_names_and_values_are_trimmed() {
        let mut draft = RequestDraft::blank();
        apply_query(&mut draft, pairs(&[("  page  ", "  2  ")])).unwrap();
        assert_eq!(draft.query.get("page").unwrap(), "2");
    }

    /// The one rule that differs from headers. A query key is opaque to the
    /// server, so `page` and `Page` are two params and refusing them as a pair
    /// would reject a request that is legal to send.
    #[test]
    fn query_names_differing_only_in_case_are_two_params() {
        let mut draft = RequestDraft::blank();
        apply_query(&mut draft, pairs(&[("page", "1"), ("Page", "2")]))
            .expect("case makes these distinct");
        assert_eq!(draft.query.len(), 2);
        assert_eq!(draft.query.get("page").unwrap(), "1");
        assert_eq!(draft.query.get("Page").unwrap(), "2");
    }

    /// An exact repeat still has to be refused: `query` is a `HashMap`, so one
    /// of the two would vanish silently.
    #[test]
    fn an_exactly_duplicated_query_name_is_refused() {
        let mut draft = RequestDraft::blank();
        let err = apply_query(&mut draft, pairs(&[("page", "1"), ("page", "2")]))
            .expect_err("same param twice");
        assert!(err.contains("page"), "the error names the param: {err}");
        assert!(draft.query.is_empty(), "nothing applied on refusal");
    }

    /// `Content-Type` is a header rule and must not leak into query handling —
    /// a param that happens to be named that is just a param.
    #[test]
    fn a_content_type_query_param_is_left_alone() {
        let mut draft = RequestDraft::from_config(full_config(), None, false);
        let before = draft.body.as_ref().unwrap().content_type.clone();

        apply_query(&mut draft, pairs(&[("Content-Type", "text/plain")])).unwrap();

        assert_eq!(draft.query.get("Content-Type").unwrap(), "text/plain");
        assert_eq!(draft.body.as_ref().unwrap().content_type, before, "body was touched");
    }

    /// Editing one map must not disturb the other; they are separate rows on
    /// the form and separate keys in the config.
    #[test]
    fn editing_query_leaves_headers_untouched_and_the_reverse() {
        let mut draft = RequestDraft::from_config(full_config(), None, false);

        apply_query(&mut draft, pairs(&[("limit", "50")])).unwrap();
        assert_eq!(draft.headers.get("Accept").unwrap(), "application/json");

        apply_headers(&mut draft, pairs(&[("Accept", "text/plain")])).unwrap();
        assert_eq!(draft.query.get("limit").unwrap(), "50");
    }

    /// Both kinds read and write their own map through `PairKind`, which is
    /// what lets one pane serve both.
    #[test]
    fn each_kind_reads_and_writes_its_own_map() {
        let draft = RequestDraft::from_config(full_config(), None, false);
        assert_eq!(PairKind::Headers.pairs(&draft), pairs(&[("Accept", "application/json")]));
        assert_eq!(PairKind::Query.pairs(&draft), pairs(&[("page", "2")]));

        let mut draft = draft;
        PairKind::Query.apply(&mut draft, pairs(&[("q", "x")])).unwrap();
        assert_eq!(draft.query.get("q").unwrap(), "x");
        assert_eq!(draft.headers.get("Accept").unwrap(), "application/json");
    }

    /// Rows are sorted, because a `HashMap` has none of its own and unsorted
    /// rows would shuffle between openings of the pane.
    #[test]
    fn rows_come_back_in_a_stable_order() {
        let mut draft = RequestDraft::blank();
        apply_query(&mut draft, pairs(&[("zeta", "1"), ("alpha", "2"), ("mid", "3")])).unwrap();
        let names: Vec<String> =
            PairKind::Query.pairs(&draft).into_iter().map(|(k, _)| k).collect();
        assert_eq!(names, vec!["alpha", "mid", "zeta"]);
    }

    /// Tab has to reach every action row. Query was added between headers and
    /// profiles, so the walk is four fields then four rows.
    #[test]
    fn tab_walks_every_field_and_action_row() {
        let draft = RequestDraft::blank();
        assert_eq!(draft.rows(), draft.fields.len() + 4);

        let targets: Vec<Focus> = (0..draft.rows())
            .map(|i| {
                let mut d = draft.clone();
                d.focused = i;
                d.focus_target()
            })
            .collect();

        assert_eq!(
            targets,
            vec![
                Focus::Field(0),
                Focus::Field(1),
                Focus::Field(2),
                Focus::Field(3),
                Focus::Global,
                Focus::Headers,
                Focus::Query,
                Focus::Profiles,
            ]
        );
    }

    const BODY: &str = "alpha\nBETA line\ngamma\nbeta again\ndelta";

    #[test]
    fn an_empty_filter_shows_every_line() {
        assert_eq!(visible_response_lines(BODY, "").len(), 5);
    }

    #[test]
    fn filtering_is_case_insensitive_and_by_substring() {
        assert_eq!(
            visible_response_lines(BODY, "beta"),
            vec!["BETA line", "beta again"]
        );
    }

    /// The pane, `G`, and copy all read this one list. Copy used to take the
    /// whole body instead, so filtering to two lines and pressing `c` handed
    /// over all five.
    #[test]
    fn a_filtered_copy_is_only_the_visible_lines() {
        let copied = visible_response_lines(BODY, "beta").join("\n");
        assert_eq!(copied, "BETA line\nbeta again");
        assert!(!copied.contains("alpha"));
    }

    #[test]
    fn with_no_anchor_the_range_is_the_cursor_line_alone() {
        assert_eq!(selection_range(3, None), (3, 3));
    }

    #[test]
    fn a_selection_dragged_upward_covers_the_same_lines_as_one_dragged_down() {
        assert_eq!(selection_range(1, Some(4)), (1, 4));
        assert_eq!(selection_range(4, Some(1)), (1, 4));
    }

    #[test]
    fn a_selection_is_inclusive_of_both_ends() {
        let lines = visible_response_lines(BODY, "");
        let (from, to) = selection_range(2, Some(1));
        assert_eq!(lines[from..=to], ["BETA line", "gamma"]);
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
