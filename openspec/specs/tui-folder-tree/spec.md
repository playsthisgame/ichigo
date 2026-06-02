## ADDED Requirements

### Requirement: Browse mode displays configs as a folder tree
The TUI Browse mode SHALL render request configs grouped into a collapsible tree that mirrors the `/`-delimited folder hierarchy encoded in config names. Each distinct path prefix SHALL appear as a folder node; each config SHALL appear as a leaf node indented beneath its parent folder.

#### Scenario: Flat configs render without folders
- **WHEN** all loaded configs have names with no `/` delimiter (e.g. `login`, `ping`)
- **THEN** the list SHALL display each config as a leaf at depth 0, with no folder nodes

#### Scenario: Nested configs render under folder nodes
- **WHEN** configs include names like `auth/login` and `auth/refresh`
- **THEN** the list SHALL display an `auth` folder node, with `login` and `refresh` indented beneath it

#### Scenario: Global configs appear under a dedicated folder
- **WHEN** one or more configs are loaded from the global config directory
- **THEN** they SHALL be grouped under a top-level `[global]` folder node, separate from local configs

### Requirement: Folder nodes can be expanded and collapsed
Each folder node in the tree SHALL support expand and collapse toggling. When collapsed, its children SHALL be hidden from the visible list. When expanded, its children SHALL be visible.

#### Scenario: All folders start expanded
- **WHEN** the TUI is opened
- **THEN** all folder nodes SHALL be in the expanded state by default

#### Scenario: User collapses a folder with Enter
- **WHEN** the cursor is on an expanded folder node and the user presses Enter
- **THEN** the folder SHALL collapse and its children SHALL be removed from the visible list

#### Scenario: User expands a folder with Enter
- **WHEN** the cursor is on a collapsed folder node and the user presses Enter
- **THEN** the folder SHALL expand and its children SHALL appear beneath it

#### Scenario: Space also toggles folder collapse state
- **WHEN** the cursor is on a folder node and the user presses Space
- **THEN** the folder SHALL toggle between expanded and collapsed

### Requirement: Navigation skips hidden items
Keyboard navigation (j/k/↑/↓/g/G) in Browse mode SHALL move the cursor only through visible rows. Children of collapsed folders SHALL not be reachable by keyboard navigation while the folder is collapsed.

#### Scenario: Cursor jumps over collapsed folder children
- **WHEN** a folder is collapsed and the cursor is on the folder node, and the user presses j
- **THEN** the cursor SHALL move to the next visible item after the collapsed folder, skipping all its children

#### Scenario: Cursor moves to last visible row with G
- **WHEN** the user presses G
- **THEN** the cursor SHALL land on the last visible row (respecting collapsed folders)

### Requirement: Cursor moves to folder when its child is hidden by collapse
If the currently selected item is a leaf inside a folder that gets collapsed, the cursor SHALL automatically move to the folder node itself.

#### Scenario: Collapsing a folder containing the selected leaf
- **WHEN** the cursor is on a leaf inside folder `auth` and another mechanism collapses `auth`
- **THEN** the cursor SHALL move to the `auth` folder node

### Requirement: Filter mode bypasses tree structure
When the filter is active, Browse mode SHALL display a flat list of matching configs identical to pre-tree behavior. Tree folder nodes SHALL NOT appear in filter results. Folder expand/collapse keys SHALL have no effect while the filter is active.

#### Scenario: Filter shows matching leaves only
- **WHEN** the user activates the filter and types a query
- **THEN** the list SHALL display only leaf entries whose name, method, or URL match the query, with no folder nodes

#### Scenario: Clearing filter restores tree view
- **WHEN** the user clears the filter
- **THEN** the tree view SHALL be restored with collapse states unchanged from before the filter was activated
