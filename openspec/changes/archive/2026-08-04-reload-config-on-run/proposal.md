## Why

The TUI is designed to stay open for long sessions, but it builds its entire in-memory model of every config exactly once — `App::new()` calls `load_entries()` at startup (`src/tui/mod.rs:94`) and the snapshot is only ever rebuilt after an *in-TUI* save. When a token expires and the user updates a profile's `params` in the YAML from another terminal, the TUI keeps injecting the old values, so the request goes out with a stale token and there is no way to pick up the edit short of quitting and relaunching.

The failure is specifically in the **variable-resolution** path, not the request path. `execute_request`, `execute_chain`, and `execute_test` already call `RequestConfig::load` / `ChainConfig::load` and get a fresh URL, headers, and body from disk. But the `profiles` list handed to `ProfileSelect`, and the `{{VAR}}` names used to build the `VarInput` field list, are read off the startup snapshot in `try_run_selected`, `try_test_selected`, and `confirm_profile_select`. The result is a config that is half-fresh and half-stale: the URL reflects your edit, the profile values do not.

## What Changes

- **Read the config from disk at the moment an action starts.** `try_run_selected`, `try_test_selected`, and `confirm_profile_select` load the selected entry's YAML fresh instead of reading `App.entries`. Profiles offered in `ProfileSelect` and placeholders collected for `VarInput` / `TestInput` therefore always match the file on disk.
- **Refresh the cached entry after a run.** Once an action re-reads a config, the corresponding `Entry` in `App.entries` is updated in place so the detail pane stops showing values that contradict what was just sent.
- **Add a manual full-list refresh.** A new `R` key in Browse mode re-runs `load_entries()`, picking up configs that were created, renamed, or deleted on disk while the TUI was open. `r` is already bound to *run*, so refresh takes `R`.
- **Preserve UI state across a refresh.** A refresh keeps the collapsed-folder set, the active filter, and the cursor position (tracked by entry *name*, since a refresh can shift entry indices).
- **Report load failures without losing the session.** If a config was edited into invalid YAML or deleted out from under the cursor, the action surfaces the error in the Response pane and returns to Browse instead of running with stale data or panicking.

Not changing: the CLI subcommands (each `ichigo run` is already a fresh process), the on-disk YAML format, and the request-sending path in `utils::send_request`.

## Capabilities

### New Capabilities
- `config-reload`: Guarantees that every request the TUI sends — including its profile values and variable prompts — is derived from the current contents of the config file on disk, and gives the user an explicit way to resync the config list with the filesystem.

### Modified Capabilities

None. `tui-folder-tree` keeps all its existing requirements; refresh behavior is additive and its interaction with the tree (preserving collapse state and cursor) is specified in `config-reload`.

## Impact

- `src/tui/mod.rs` — `try_run_selected`, `try_test_selected`, `confirm_profile_select`, `start_chain`; new `reload_entries` / `refresh_entry` helpers factored out of the existing reload block at lines 644–655.
- `src/tui/handlers.rs` — new `R` binding in `handle_key_browse`.
- `src/tui/render.rs` — footer/help hint for `R`.
- `src/tui/tree.rs` — `load_entries` may need a single-entry sibling so one config can be re-read without walking both config directories.
- No new dependencies, no background threads, no change to the blocking `event::read()` loop.
- **Known limitation, out of scope:** `{{VAR}}` values that fall back to `std::env::var` still resolve against the environment ichigo was launched with. A process cannot observe exports made in its parent shell afterward, so an env-sourced token stays stale until relaunch. Storing such values in a profile makes them reloadable; this should be called out in the README.
