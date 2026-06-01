## Context

The TUI Browse mode currently holds `App.entries: Vec<Entry>` (a flat list loaded from `list_requests()`) and uses a ratatui `List` widget driven by `ListState`. Config names already encode folder paths using `/` as a delimiter (e.g. `auth/login`, `auth/refresh`), but the list renders them as opaque strings with no hierarchy.

The goal is to add a collapsible tree view that mirrors the `.ichigo/` folder structure while keeping all existing entry operations (run, test, delete, edit, copy) unchanged.

## Goals / Non-Goals

**Goals:**
- Group entries into a tree by their `/`-delimited name prefixes
- Render folder nodes with Nerd Font icons (`/`) when available, otherwise using box-drawing tree lines (`├──`/`└──`) with a trailing `/` suffix to identify folders
- Let users expand/collapse folders with Enter or Space in Browse mode
- Navigation (j/k/↑/↓/g/G) moves only through visible (non-collapsed) rows
- When filter is active, bypass the tree and show flat filtered results (same as today)
- Global configs show a top-level `[global]` folder node

**Non-Goals:**
- Persisting collapse state across TUI sessions
- Creating or renaming folders from within the TUI
- Changing the on-disk layout or config format in any way
- Mouse support

## Decisions

### 1. Tree model: `TreeNode` enum, built at load time

Build a `TreeNode` tree from the flat `Vec<Entry>` after loading, by splitting each entry's name on `/` and inserting into a recursive structure.

```
enum TreeNode {
    Folder { path: String, children: Vec<TreeNode> },
    Leaf   { entry_idx: usize },          // index into App.entries
}
```

**Why this over storing paths in the list widget**: ratatui's `List` has no built-in tree support. A custom model is cleaner than encoding indentation into `ListItem` strings and trying to reverse-map clicks/keys back to an entry.

**Alternative considered**: represent the tree as a `Vec<TreeRow>` flat projection and rebuild on every toggle. Rejected — the recursive `TreeNode` structure is easier to reason about for collapse/expand and auto-expand-on-load logic; the flat projection is then derived from it.

### 2. Collapse state: `HashSet<String>` of collapsed folder paths

`App` gains `collapsed_folders: HashSet<String>`. A folder is expanded by default (not in the set). Toggling inserts or removes. Paths use the same `/`-delimited format as entry names (e.g. `auth`).

**Why not a `HashMap<String, bool>`**: the set of collapsed folders is almost always small; a `HashSet` is simpler and avoids a default-value question.

### 3. Visible row projection: derive on demand

A `fn visible_rows(tree: &[TreeNode], collapsed: &HashSet<String>) -> Vec<VisibleRow>` walks the tree and emits one `VisibleRow` per visible node, skipping children of collapsed folders.

```
enum VisibleRow {
    Folder { path: String, depth: usize, expanded: bool },
    Leaf   { entry_idx: usize, depth: usize },
}
```

`ListState` continues to index into this projected slice. Every key event that changes the cursor or triggers an action re-derives `visible_rows` (cheap — the list is small).

**Why not cache**: entries and collapse state both change infrequently; recomputing on every render avoids cache invalidation bugs.

### 4. Enter/Space: toggle folder, run leaf

In Browse mode:
- If the selected visible row is a `Folder` → toggle it in `collapsed_folders`; adjust cursor if the selected item was scrolled away
- If the selected visible row is a `Leaf` → same as current Enter behavior (fire the request)
- Space on a `Leaf` → same as current Space behavior (if any); on a `Folder` → toggle

This keeps the existing leaf key bindings untouched.

### 5. Filter mode bypasses tree

When `App.filter_active` is true, `filtered_indices()` is used exactly as today, and the `List` widget renders the flat filtered results. Tree rendering and folder toggle keys are disabled while a filter is active. This avoids complexity and matches user expectation: filtering is a search, not a tree traversal.

### 6. Auto-expand on load

After building the tree, if the initial cursor lands on a leaf, walk its ancestor folders and ensure none of them are in `collapsed_folders`. Since the set starts empty (all expanded), this is a no-op by default but is the correct invariant to enforce after a future "collapse all" command.

### 7. Icon strategy: Nerd Font with tree-line fallback

At startup, `App` detects whether the terminal is likely rendering Nerd Fonts and stores the result in `use_nerd_fonts: bool`. The two rendering modes are:

**Nerd Font mode** (when detected):
```
 auth/         ← collapsed folder
 auth/         ← expanded folder
 ├── login      ← leaf, tree-line indented
 └── refresh
 utils/
 └── health
```
Icons used: `\u{f07b}` (closed folder) and `\u{f07c}` (open folder) from the Font Awesome set, which ship in every Nerd Font variant.

**Tree-line mode** (fallback):
```
 auth/         ← collapsed (trailing / marks it as a folder; children absent = closed)
 utils/        ← expanded
 ├── health
 └── status
```
No explicit expand/collapse glyph. The presence or absence of child rows communicates state; the trailing `/` distinguishes folders from leaves.

**Detection heuristic** (checked in order, first match wins):
1. `ICHIGO_ICONS=0` → force tree-line mode
2. `ICHIGO_ICONS=1` → force Nerd Font mode
3. `KITTY_WINDOW_ID` set → Nerd Font mode (kitty)
4. `WEZTERM_PANE` or `WEZTERM_EXECUTABLE` set → Nerd Font mode
5. `TERM_PROGRAM` ∈ `{ghostty, iTerm.app}` → Nerd Font mode
6. `TERM=xterm-kitty` → Nerd Font mode
7. Otherwise → tree-line mode

**Why not a deeper probe**: reliably detecting whether a font has Nerd Font glyphs at runtime requires raw terminal I/O before the TUI starts (print a PUA char, query cursor column). The env-var heuristic covers the common cases without that complexity. The `ICHIGO_ICONS` escape hatch handles everything else.

## Risks / Trade-offs

- **Re-deriving visible rows on every frame**: negligible cost for typical config counts (<1000). If performance degrades, cache `visible_rows` behind a dirty flag.
- **Cursor adjustment after collapse**: if the cursor is on a child of a folder that gets collapsed, the cursor must be moved to the folder node itself. Off-by-one bugs are likely here — cover with unit tests.
- **Filter + tree interaction**: bypassing the tree in filter mode is simpler but means toggling filter clears any tree navigation context. Acceptable for now.
