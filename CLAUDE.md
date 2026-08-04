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
- `Response` / `TestResponse` → show results, supports `f` to filter response lines
- `NewRequest` / `NewProfile` → create/edit requests and their profiles in-TUI
- `ConfirmDelete` → confirm before deleting

Variable placeholder names are extracted by `extract_var_names` (scans url, headers, query, body for `{{...}}`) to build the `VarInput` field list. The TUI clipboard copy (`c` key) uses `pbcopy` and is macOS-only.

**Config freshness.** The TUI is meant to stay open for long sessions, so no action may rely on the entry snapshot taken at startup. `App::entries` is a display cache only. Every action path (`try_run_selected`, `try_test_selected`, `confirm_profile_select`, `start_chain`) re-reads the config through `tree::load_entry` before deriving profiles or `{{VAR}}` names — those feed the vars map, and `interpolate` prefers that map over everything else, so a stale value there silently wins over a correct one in the file. On a load failure the action aborts via `App::show_error`; it must never fall back to the cached entry. `R` in Browse mode calls `reload_entries` for a full resync (picks up files added/renamed/deleted on disk); `r` is run. Environment-sourced `{{VAR}}` values cannot be refreshed — the process env is fixed at launch.
