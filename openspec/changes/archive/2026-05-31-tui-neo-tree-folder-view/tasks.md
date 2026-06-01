## 1. Tree Data Model

- [x] 1.1 Define `TreeNode` enum (`Folder { path, children }` / `Leaf { entry_idx }`) in `tui.rs`
- [x] 1.2 Define `VisibleRow` enum (`Folder { path, depth, expanded }` / `Leaf { entry_idx, depth }`) in `tui.rs`
- [x] 1.3 Implement `build_tree(entries: &[Entry]) -> Vec<TreeNode>` that groups entries by `/`-delimited name prefixes, placing global entries under a synthetic `[global]` folder
- [x] 1.4 Implement `visible_rows(tree: &[TreeNode], collapsed: &HashSet<String>) -> Vec<VisibleRow>` that walks the tree and emits one row per visible node, skipping children of collapsed folders
- [x] 1.5 Write unit tests for `build_tree` (flat list, nested, global mixing) and `visible_rows` (collapsed subtree hidden, depth values correct)

## 2. App State Changes

- [x] 2.1 Add `tree: Vec<TreeNode>` and `collapsed_folders: HashSet<String>` fields to `App`
- [x] 2.2 Update `App::new()` to call `build_tree` after `load_entries` and initialise `collapsed_folders` as empty
- [x] 2.3 Replace `list_state: ListState` selection logic so it indexes into `visible_rows(...)` rather than the flat `entries` vec

## 3. Browse Mode: Expand/Collapse

- [x] 3.1 In the Browse key handler, detect when Enter or Space is pressed on a `VisibleRow::Folder` and toggle its path in `collapsed_folders`
- [x] 3.2 After collapsing a folder, check if the current cursor points to a now-hidden row; if so, move the cursor to the folder node that was just collapsed
- [x] 3.3 Keep Enter on `VisibleRow::Leaf` triggering the existing run/select flow unchanged

## 4. Rendering

- [x] 4.1 Replace the `List` widget population in Browse mode so it iterates `visible_rows(...)` instead of `filtered_indices()`
- [x] 4.2 Add `detect_nerd_fonts() -> bool` that checks `ICHIGO_ICONS` env var first, then terminal env vars (`KITTY_WINDOW_ID`, `WEZTERM_PANE`, `WEZTERM_EXECUTABLE`, `TERM_PROGRAM`, `TERM`) per the heuristic in the design doc; store result as `App.use_nerd_fonts: bool`
- [x] 4.3 Render folder rows: in Nerd Font mode use `\u{f07b}`/`\u{f07c}` icons + bold style; in tree-line mode append a trailing `/` to the folder name with bold style and no icon
- [x] 4.4 Render leaf rows: prefix with the appropriate box-drawing connector (`├── ` or `└── `) based on whether the leaf is last among its siblings; indent proportional to `depth`
- [x] 4.5 Ensure the detail pane on the right still resolves via `entry_idx` from the selected `VisibleRow::Leaf` (or is blank when a folder is selected)

## 5. Filter Mode Compatibility

- [x] 5.1 When `filter_active` is true, populate the `List` from `filtered_indices()` exactly as before (flat results, no folder nodes)
- [x] 5.2 Disable folder toggle key handling (Enter/Space on folder) while filter is active
- [x] 5.3 Verify that clearing the filter restores the tree view with `collapsed_folders` unchanged

## 6. Verification

- [x] 6.1 Run `cargo clippy` and resolve all warnings
- [x] 6.2 Run `cargo test` and ensure all existing tests pass alongside the new unit tests
- [x] 6.3 Manually open the TUI with a config set containing nested folders and verify expand/collapse, navigation, and filter behaviour match the spec
