## 1. Single-entry loading in `tree.rs`

- [x] 1.1 Extract the per-file body of `load_entries` (tree.rs:27–53) into `pub(crate) fn load_entry(name: &str, global: bool) -> Result<Entry>`, keeping the `content.contains("steps:")` chain detection and the `EntryKind` flattening in this one place.
- [x] 1.2 Replace `resolve_config_path(&r.name).unwrap()` with a proper error (`with_context`), so a config that vanishes between listing and reading returns `Err` instead of panicking.
- [x] 1.3 Rewrite `load_entries` as a loop over `load_entry`, skipping (not aborting on) entries that fail to parse so one malformed file cannot prevent TUI startup.
- [x] 1.4 Add unit tests in `tree.rs`: `load_entry` parses a request config, parses a chain config as `EntryKind::Chain`, and returns `Err` for invalid YAML and for a missing file.

## 2. Reload-on-action in `mod.rs`

- [x] 2.1 Add `fn reload_selected(&mut self, idx: usize) -> Option<&Entry>` (or equivalent) that calls `tree::load_entry` for `self.entries[idx]`, writes the result back into `self.entries[idx]` on success, and on failure sets `Mode::Response { status: 0, body: format!("Error: {e}"), scroll: 0, response_filter: String::new(), response_filter_active: false }` and returns `None`.
- [x] 2.2 Rewrite `try_run_selected` (mod.rs:217) to call the reload helper first and read `profiles` / `extract_var_names` inputs from the refreshed entry; return early when the reload failed.
- [x] 2.3 Rewrite `try_test_selected` (mod.rs:301) the same way, so the profile list shown before a load test comes from disk.
- [x] 2.4 Rewrite `confirm_profile_select` (mod.rs:348) so the request branch derives `var_names` from a fresh `load_entry` rather than `self.entries.iter().find(..)`; leave the chain branch's existing `ChainConfig::load` as-is (already fresh).
- [x] 2.5 Verify `start_chain` (mod.rs:254) already reloads and needs no change; if it reads any cached profile data, route it through the same helper.

## 3. Full-list refresh

- [x] 3.1 Add `fn reload_entries(&mut self, select: Option<&str>)` to `App`: rebuild `entries` and `tree` via `load_entries`, then restore the cursor by entry name — through `visible_rows` when `using_tree()`, through `filtered_indices` otherwise — falling back to a clamped valid position when the name is gone, and leaving `collapsed_folders` and `filter` untouched.
- [x] 3.2 Replace the inlined reload block in `save_new_request` (mod.rs:644–655) with a call to `reload_entries(Some(&name))`.
- [x] 3.3 Bind `KeyCode::Char('R')` in `handlers::handle_key_browse` to capture the selected entry's name and call `app.reload_entries(name.as_deref())`. Place it so it does not shadow the existing `Char('r') | Enter` run binding.
- [x] 3.4 Confirm `R` typed while the filter is active is still consumed by `handle_key_filter` (mod.rs:661 dispatches to it before the Browse handler) and appends to the query.

## 4. Discoverability and docs

- [x] 4.1 Add an `R refresh` (or `R reload`) hint to the Browse-mode key hints in `render.rs`.
- [x] 4.2 Update the README key table with `R`, and add a short note that `{{VAR}}` values resolved from environment variables are fixed at launch — put values that rotate (tokens) in a profile so they reload.
- [x] 4.3 Update `CLAUDE.md`'s `src/tui.rs` architecture notes to mention that action paths reload configs from disk and that `R` refreshes the list. (Note: the file layout described there is stale — the TUI is now `src/tui/{mod,handlers,render,tree}.rs`; fix that while editing.)

## 5. Verification

- [x] 5.1 `cargo test` and `cargo clippy` pass clean.
- [x] 5.2 Manual: open the TUI, edit a profile's `params` in the YAML from another terminal, run the config, and confirm the new value is sent (verify against a request-echo endpoint such as `httpbin.org/headers`).
- [x] 5.3 Manual: add a `{{NEW_VAR}}` to a config on disk while the TUI is open, run it, and confirm the var prompt includes `NEW_VAR`.
- [x] 5.4 Manual: break a config's YAML on disk, run it, and confirm an error appears and Esc returns to a working Browse mode.
- [x] 5.5 Manual: with folder `auth` collapsed and the cursor on a config, create and delete configs on disk, press `R`, and confirm the collapse state and cursor position hold.
