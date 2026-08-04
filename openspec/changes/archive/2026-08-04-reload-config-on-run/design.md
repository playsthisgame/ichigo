## Context

`App` (`src/tui/mod.rs:77`) owns a `Vec<Entry>` built once by `load_entries()` in `App::new()`. `Entry::kind` flattens the parsed YAML into display-oriented fields — `method`, `url`, `headers`, `query`, `body_data`, `profiles` — so the render pass never touches the filesystem. That was the right call for rendering; the problem is that the *action* paths read from the same snapshot:

| Site | Reads from | Freshness |
|---|---|---|
| `execute_request` (mod.rs:418) | `RequestConfig::load` | fresh |
| `execute_chain` (mod.rs:442) | `ChainConfig::load` | fresh |
| `execute_test` (mod.rs:409) | `RequestConfig::load` | fresh |
| `try_run_selected` (mod.rs:219) | `self.entries[idx]` | **stale** |
| `try_test_selected` (mod.rs:303) | `self.entries[idx]` | **stale** |
| `confirm_profile_select` (mod.rs:372) | `self.entries.iter().find(..)` | **stale** |

The three stale sites are exactly the ones that produce `profile_params` and the `vars` vector. Those become the `HashMap` passed into `send_request`, and `utils::interpolate` prefers that map over the environment — so a stale profile value wins over everything, including a correct value in the freshly-loaded YAML. That is the bug: the config is reloaded, but the *variables* substituted into it are not.

`start_chain` (mod.rs:255) is already the shape we want — it calls `ChainConfig::load` and reads profiles off the fresh parse. The fix generalizes that pattern to requests.

The only existing reload code is inlined in `save_new_request` (mod.rs:644–655): rebuild entries, rebuild tree, re-locate the saved entry by name, reselect it. It drops the collapsed-folder set on the floor only by luck — `collapsed_folders` is a separate field keyed by path string, so it survives; but the selection restore ignores the filter path entirely.

## Goals / Non-Goals

**Goals:**
- Every request the TUI sends uses profile values and placeholder names parsed from the config file's current on-disk contents.
- The detail pane stops disagreeing with what was just sent.
- The user can resync the config list with the filesystem without restarting.
- Refresh preserves collapsed folders, filter text, and cursor position.
- A config that becomes invalid or disappears produces a visible error, not a stale run or a panic.

**Non-Goals:**
- Automatic filesystem watching or polling. The event loop stays blocking on `event::read()`; no `notify` dependency, no background thread, no `event::poll` timeout. Reload-on-action covers the correctness problem, and `R` covers list drift.
- Refreshing environment variables. A process cannot see exports made in its parent shell after launch. Values that must survive a token rotation belong in a profile.
- Caching or invalidation heuristics (mtime comparison, content hashing). At human interaction rates a `read_to_string` + `serde_yaml::from_str` of one small file is not worth a cache.
- Changes to the CLI subcommands. Each `ichigo run` is already a fresh process.

## Decisions

### 1. Reload at action start, not at render time

Re-read the config when the user presses Enter/`t`, not on every draw. Rendering happens on every keypress including cursor movement; parsing YAML for the whole list on each frame would be wasteful and would make the list flicker between valid and invalid states while a file is being written.

*Alternative considered:* reload inside `execute_request` only. Rejected — that is already fresh; the stale data is consumed *before* `execute_request` is reached, while building the profile and var prompts.

*Alternative considered:* mtime check before reloading. Rejected as premature; the reload is one small file read on a keypress.

### 2. A single `load_entry(name) -> Result<Entry>` in `tree.rs`

`load_entries()` walks both config directories and parses every file. The action paths need exactly one entry. Factor the per-file body of `load_entries` into `load_entry(name: &str, global: bool) -> Result<Entry>` and have `load_entries` call it in the loop, so the chain-vs-request detection (`content.contains("steps:")`) and the flattening into `EntryKind` stay defined in exactly one place.

*Alternative considered:* have the action paths call `RequestConfig::load` / `ChainConfig::load` directly and read `.profiles` off that. Rejected — it duplicates the `steps:` sniffing at three more call sites and gives the action paths a different notion of "what is this entry" than the list has.

### 3. Refresh the cached entry in place after reloading

Once an action has a fresh `Entry`, write it back to `self.entries[idx]`. This is what stops the detail pane from contradicting the request that was just sent, and it costs nothing since the parse already happened. The entry's position in `entries` is unchanged, so `tree` and `list_state` stay valid — no rebuild needed.

A rename or deletion cannot be handled this way; those are the `R` path.

### 4. Failure surfaces as a Response-mode error and aborts the action

If `load_entry` fails (file deleted, YAML broken mid-edit), set `Mode::Response { status: 0, body: "Error: …" }` and do not run. This matches how `start_chain` (mod.rs:256) and `execute_request` (mod.rs:432) already report failures, so the error rendering and dismissal path already exist. Critically, the action must **abort** rather than fall back to the cached entry — silently running with stale data is the exact bug being fixed.

Note `load_entries` currently has two latent panics on this path: `resolve_config_path(&r.name).unwrap()` (tree.rs:31) and `serde_yaml::from_str(&content)?` on the request branch (tree.rs:42) — the chain branch swallows parse errors via `unwrap_or` but the request branch propagates. `load_entry` must return `Result` and be handled, and the `unwrap()` becomes a proper error. This also fixes `App::new()` aborting startup when any one config is malformed.

### 5. `R` for refresh, because `r` is taken

`handlers.rs:186` binds `Char('r') | Enter` to run. Rebinding `r` would break muscle memory for the most-used key in the TUI. `R` is unbound in Browse (taken: `q j k G g <space> r <enter> t n e c d f`) and reads as "reload".

`R` applies only in Browse mode and only when the filter is not capturing input — `handle_key_filter` intercepts printable characters first (mod.rs:661), so a literal `R` typed into the filter is unaffected.

### 6. `reload_entries` restores selection by name, and handles the filter path

Extract the block at mod.rs:644–655 into `fn reload_entries(&mut self, select: Option<&str>)`. Two corrections over the inlined version:

- Capture the currently selected entry's *name* before reloading, since indices shift when a config is added or removed.
- Restore the cursor through whichever addressing mode is active: `visible_rows` when `using_tree()`, `filtered_indices` when a filter is set. The existing code only handles the tree path.
- New folders introduced by newly-discovered configs default to expanded (absent from `collapsed_folders`), matching `tui-folder-tree`'s "all folders start expanded". `collapsed_folders` may retain paths for folders that no longer exist; that is harmless — it is a `HashSet<String>` consulted by lookup — and clearing it would lose the user's collapse state.

`save_new_request` then calls `reload_entries(Some(&name))` instead of carrying its own copy.

## Risks / Trade-offs

- **A config edited to invalid YAML now blocks the run where it previously ran the stale-but-valid cached copy.** → Intended. The error names the file and the parse failure, and Esc returns to Browse with the entry still listed, so the user can fix the file and press Enter again.

- **Reload adds a file read + YAML parse to the Enter keypress.** → Single small file, already done on the same keypress by `execute_request`; the marginal cost is one `read_to_string`. The net change is one extra parse per run, on a path that then blocks on a network request for orders of magnitude longer.

- **`R` is a manual step the user must remember for configs added or renamed on disk.** → Accepted for this change; correctness of what gets *sent* no longer depends on it, which was the actual complaint. The footer hint makes it discoverable. If manual refresh proves annoying in practice, adding `event::poll` + directory mtime checks is a self-contained follow-up that does not invalidate any of this work.

- **`R` after an external delete moves the cursor.** → `reload_entries` falls back to the nearest valid position when the previously-selected name is gone, rather than clearing the selection.

- **Two files now know how to turn a path into an `Entry`.** → No: decision 2 exists specifically to keep it at one (`tree::load_entry`), with `load_entries` as a loop over it.
