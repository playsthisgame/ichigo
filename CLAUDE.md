# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Use available skills (via the Skill tool) when they are relevant — for example, `/code-review` when reviewing changes, `/verify` when confirming a fix works, `/run` when launching the app, and `/security-review` for security-sensitive changes.

## Commands

```sh
cargo build                   # debug build
cargo build --release         # release build
cargo run -- <subcommand>     # run with args, e.g. `cargo run -- list`
cargo test                    # run tests
cargo clippy                  # lint
```

The binary is `target/debug/ichigo` (or `target/release/ichigo`).

Releases are built with [cargo-dist](https://github.com/axodotdev/cargo-dist) via `dist-workspace.toml` and the GitHub Actions workflow at `.github/workflows/release.yml`.

## Architecture

The project is a single-binary Rust CLI. Four top-level source files plus the `src/tui/` module:

**`src/main.rs`** — CLI entry point. Defines the `Cli` / `Commands` enum (via clap derive), dispatches to command functions (`cmd_new`, `cmd_run`, etc.), and embeds shell completion scripts as `const` strings. Chain detection is done by a simple `content.contains("steps:")` string check — there is no separate file format; a config is a chain if and only if it has a `steps:` key.

**`src/config.rs`** — All config types and file I/O. Two top-level config shapes: `RequestConfig` (single request) and `ChainConfig` (contains `Vec<RequestConfig>` as steps). Also defines `Body`, `Profile`, and `RequestEntry`. Key path logic:
- Local: `.ichigo/<name>.yaml`
- Global: `~/.config/ichigo/<name>.yaml`
- `resolve_config_path` checks local first, then global — local always wins.
- Names support `/`-delimited subfolders (e.g. `folder/request`), stored as subdirectories inside `.ichigo/`.

**`src/utils.rs`** — `send_request()` builds a blocking `reqwest` client and fires the HTTP request. `interpolate()` does `{{VAR}}` substitution: it checks the provided `vars` map first, then falls back to environment variables, leaving unresolved placeholders as-is (`{{VAR}}`).

**`src/curl.rs`** — Converts between a `RequestConfig` and a cURL command in both directions. Pure string work, no IO, unit tested.

`to_curl()` renders a config + resolved vars as a paste-ready command. It takes the same two inputs as `send_request` and **must stay in step with it** — a command that disagrees with what ichigo sends is worse than none. Substitution is shared via `utils::interpolate`; the duplicated rules (and the real drift risk) are query folding into the URL and the body's `Content-Type` header. Values are single-quoted unconditionally, with `'` escaped as `'\''`.

`from_curl()` is its inverse: it tokenizes with shell quoting rules (`'…'`, `"…"`, `$'…'`, `\`-continuations) and maps flags onto config fields. Two rules make the pair agree, and both are normalizations rather than transcription: a `Content-Type` header on a command with a body is stored as `body.content_type` and *never* in `headers` (`to_curl` re-derives it, so a config holding both emits the header twice), and a query string is lifted off the URL into `query` unless a key repeats or the URL carries `{{VAR}}`. The flag table is **deny-by-default** — an unrecognized flag is an error naming it, because a parser that skips what it does not understand turns `curl -F file=@x URL` into a bodyless GET and the user finds out against a real server. The round-trip tests at the bottom of the file are what keep the two directions honest; that is why both live in one module.

**`src/runner.rs`** — Executes a single `RequestConfig` (or one step of a chain). Handles profile variable injection, prints the response (status + body), and extracts values from the JSON response using dot-notation paths (e.g. `$.token` → `token`). Returns extracted variables so the caller can pass them to the next chain step.

**`src/tester.rs`** — Runs a request N times sequentially (blocking), collects per-iteration timings and status code counts, then renders an ASCII bar chart (status distribution) and ASCII line graph (latency over time).

**`src/tui/`** — The ratatui TUI, split across four files:
- `mod.rs` — `App` state, the `Mode` enum, the action paths (`try_run_selected`, `execute_request`, …), and the event loop.
- `tree.rs` — reading configs off disk (`load_entry` / `load_entries`) and the folder-tree model.
- `handlers.rs` — one key handler per mode.
- `render.rs` — all drawing.

`App` holds the list of entries and a `Mode` enum that drives all rendering and input. Modes are:
- `Browse` → main list + detail pane
- `ProfileSelect` → pick a profile before running
- `VarInput` → fill in `{{VAR}}` placeholders before running
- `TestInput` → fill vars + iteration count before a load test
- `Response` / `TestResponse` → show results; `f` filters lines, `V`/`y` copy a line range
- `NewRequest` / `NewProfile` → create/edit requests and their profiles in-TUI
- `ProfileList` → the draft's profiles; pick one to edit, add, or delete
- `EditHeaders` → the draft's headers as name/value pairs
- `ImportCurl` → paste buffer for importing a cURL command
- `ConfirmDelete` → confirm before deleting

**Headers.** `h` from Browse and `Ctrl+e` from the request form open `EditHeaders` (`Ctrl+h` is kept as an alias but must never be the only binding — many terminals and tmux configs bind Ctrl+H to backward-delete-char and send `0x7F`, which arrives as a plain Backspace and silently deletes a character; a plain key in Browse is the one binding nothing can intercept). `apply_headers` folds the rows back into the draft and is where the two rules live. Duplicate names are refused **case-insensitively**, because HTTP does not distinguish case and a `HashMap` would silently keep whichever row landed last. A `Content-Type` row is moved into `body.content_type` when the draft has a body and never left in `headers` — the same normalization `from_curl` performs, for the same reason: `to_curl` re-derives that header from the body, so a config holding both emits it twice. Note that `Ctrl+<letter>` reaches a handler as `KeyCode::Char` **plus** a CONTROL modifier, so text-entry arms must exclude it — before `Ctrl+h` existed, pressing it typed a literal `h` into the focused field, and any text arm matching bare `Char(c)` still has that bug.

**Response lines.** `visible_response_lines` is the single definition of which lines the filter leaves showing, and `draw_response`, `G`, cursor movement, and both copy paths all read it. It exists because those had already drifted: the predicate was spelled out separately at each site and the copy path never got one, so filtering to two lines and pressing `c` handed over the whole body. `cursor` and `anchor` index *that* list, not `body.lines()`, which is why every edit to the filter resets them — carrying them over leaves the cursor on an unrelated line and a selection spanning lines the user never saw. The pane scrolls to follow the cursor rather than the reverse, so a selection cannot be extended past the edge of the view. `Esc` drops a selection before it leaves the pane, and the selection highlight is a base `Line::style` so `colorize_json_line`'s colours survive it.

**Profiles.** `ProfileList` is the only door into `NewProfile` — `p` from Browse and `Ctrl+p` from the request form both land there, so add/edit/delete all start from one screen. Its rows are `0..profiles.len()` plus a trailing "new" row, which is why an empty list opens with "new" already selected. `NewProfile` carries `editing: Option<usize>`, and `upsert_profile` uses it to replace rather than push; without it, editing a profile and keeping its name appended a second profile under that name and the picker could only ever reach the first. That index also makes a name clash decidable — a profile keeping its own name is not a clash, a profile taking a sibling's is, and the second is refused because a duplicate is unreachable from both the picker and `--profile`. Edits mutate only the draft; `save_new_request` remains the single path that writes a config, so Esc out of the form discards profile changes too.

**The help overlay.** `App::show_help` is a `bool`, not a `Mode` variant, because the `?` keymap draws *over* the current pane and dismisses back to it — as a mode it would have to record which of the ten it interrupted. `draw` renders it last so it lands on top of both panes and the hint line, and `handle_key` checks it first, swallowing the dismissing key: letting it through would make `d` close the overlay *and* open the delete confirmation behind it. `?` only opens from Browse, since every other mode is either a text field where `?` is a literal character or a pane whose own hints already fit. The Browse hint line is deliberately short (6 entries) — it once listed all fifteen bindings at 155 columns, which an 80-column terminal truncated silently, hiding the last five. Keep new bindings out of it and in `HELP_COLUMNS`.

Variable placeholder names are extracted by `extract_var_names` (scans url, headers, query, body for `{{...}}`) to build the `VarInput` field list. The TUI clipboard copy (`c` key) uses `pbcopy` and is macOS-only.

**`RequestDraft`.** `NewRequest` and `NewProfile` both hold a `RequestDraft` — the four editable fields plus the headers, query, body, and extract the form does *not* edit. Those are carried in the draft rather than re-read from disk at save time, because an imported or cloned request has no file to re-read; recovering them from disk silently dropped them (which is why `c` used to clone only a request's method and URL). Build a draft through `RequestDraft::blank()` or `from_config()`; `edit_selected` / `clone_selected` / `confirm_import_curl` are the three entry points.

**Pasting.** The event loop enables bracketed paste (`EnableBracketedPaste` on entry, `DisableBracketedPaste` on exit — skip the latter and the terminal keeps emitting paste markers) and handles `Event::Paste`, which only `ImportCurl` accepts. Enter in that pane inserts a newline and `Ctrl+s` confirms, precisely because a terminal *without* bracketed paste delivers a multi-line paste as characters with Enters between the lines; binding Enter to confirm would parse only the first line.

**Pending actions.** `ProfileSelect` and `VarInput` are shared by every action that needs a profile or variables, so each carries a `PendingAction` (`Run` / `Test` / `Curl`) naming its destination. Both `confirm_profile_select` and `handle_key_var_input`'s Enter dispatch on it. A new action must set it at *every* construction site of both modes — miss the `VarInput` one and the action silently falls through to running the request, because that is where the pipeline used to be hardcoded. `Mode::Response` likewise carries a `ResponseKind` (`Http(u16)` / `Error` / `Curl`) instead of encoding "not a response" as status `0`; build it through `App::show_message` / `show_error` rather than spelling out the variant.

**Config freshness.** The TUI is meant to stay open for long sessions, so no action may rely on the entry snapshot taken at startup. `App::entries` is a display cache only. Every action path (`try_run_selected`, `try_test_selected`, `confirm_profile_select`, `start_chain`) re-reads the config through `tree::load_entry` before deriving profiles or `{{VAR}}` names — those feed the vars map, and `interpolate` prefers that map over everything else, so a stale value there silently wins over a correct one in the file. On a load failure the action aborts via `App::show_error`; it must never fall back to the cached entry. `R` in Browse mode calls `reload_entries` for a full resync (picks up files added/renamed/deleted on disk); `r` is run. Environment-sourced `{{VAR}}` values cannot be refreshed — the process env is fixed at launch.
