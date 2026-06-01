## Why

The TUI currently displays all request configs as a flat list, which becomes hard to navigate as configs grow in number. Since ichigo already supports `/`-delimited subfolders for organizing configs, the list view should reflect that hierarchy visually — giving users a tree-based browser analogous to neo-tree in Neovim.

## What Changes

- Replace the flat list in Browse mode with a collapsible tree that mirrors the `.ichigo/` folder structure
- Folder nodes show a toggle indicator (`▶`/`▼`) and can be expanded/collapsed with Enter or Space
- Leaf nodes (individual request configs) behave exactly as they do today (select, run, delete, etc.)
- Keyboard navigation moves through visible (non-collapsed) items only
- Folders that contain the currently selected item expand automatically on load

## Capabilities

### New Capabilities

- `tui-folder-tree`: Tree-structured list view in Browse mode — renders folders as collapsible nodes and request configs as leaf nodes, with keyboard-driven expand/collapse

### Modified Capabilities

<!-- none -->

## Impact

- `src/tui.rs`: Primary change — `App` state gains a tree model; rendering and key-handling in Browse mode updated
- `src/config.rs`: `list_configs()` (or equivalent) may need to return path info to build the tree; no format changes
- No CLI, HTTP, or chain-execution logic is affected
