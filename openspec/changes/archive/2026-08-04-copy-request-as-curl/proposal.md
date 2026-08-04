## Why

Requests that live in ichigo are trapped there. Sharing one with a teammate, pasting it into a bug report, or handing it to a tool that speaks cURL means retyping the URL, headers, and body by hand and re-substituting the profile's variable values from memory — exactly the values ichigo already knows how to resolve. cURL is the lingua franca for "here is the request I made"; ichigo should be able to emit it.

The pieces are already in place: profile selection and `{{VAR}}` resolution are solved by the existing `ProfileSelect` → `VarInput` flow, and `utils::interpolate` is the function that decides what actually goes on the wire. Generating a cURL command is that same pipeline with a different terminal step — render instead of send.

## What Changes

- **`y` in Browse mode copies the selected request as a cURL command.** It runs the existing profile picker and variable prompts, then renders the command and copies it to the clipboard. `y` is the vim yank idiom and is unbound in Browse today.
- **The generated command reflects what ichigo would actually send.** Variables are substituted through `utils::interpolate` with the same resolved map the request path uses, so a `{{TOKEN}}` becomes the profile's real token. Query parameters are folded into the URL, the body's `content_type` becomes a `Content-Type` header, and every value is single-quoted with embedded quotes escaped so the command survives a paste into a shell.
- **The command is displayed before it is copied.** It renders in the existing Response pane with a "copied" indicator, so the user can read, scroll, and re-copy it rather than pasting blind.
- **Chains are rejected.** A chain threads extracted values between steps (`$.token` from one step feeding `{{TOKEN}}` in the next), which a cURL command cannot express. Pressing `y` on a chain reports that it is unsupported, mirroring how load-testing already rejects chains.
- **Clipboard copying becomes portable and honest.** `copy_to_clipboard` currently spawns `pbcopy` and discards the error, so on Linux it silently does nothing — including for the existing response-copy `c` key. It gains a backend chain (`pbcopy` → `wl-copy` → `xclip` → `xsel`) and reports failure in the UI when no tool is available. **This fixes existing behavior**, not just the new feature.
- **`ProfileSelect`'s `for_test: bool` becomes an action enum.** The flag has only ever encoded run-vs-test; cURL is a third destination. `VarInput` gains the same field, since it currently hardcodes "run" as its terminal step.

Not changing: the YAML format, the CLI subcommands, the request-sending path, and how variables are resolved.

## Capabilities

### New Capabilities
- `request-as-curl`: Turning a stored request plus a chosen profile into an equivalent, paste-ready cURL command, and showing it to the user.
- `clipboard-copy`: Getting text onto the system clipboard across platforms, and reporting when that is not possible. Covers the new cURL copy and the existing response/test-result copy.

### Modified Capabilities

None. `config-reload` and `tui-folder-tree` keep their requirements. The new action inherits `config-reload`'s guarantee by construction — it reads the config through the same `tree::load_entry` path, so the generated command reflects the file on disk rather than the startup snapshot.

## Impact

- `src/tui/mod.rs` — `Mode::ProfileSelect.for_test` → an action enum; same field added to `Mode::VarInput`; new `try_copy_curl_selected` and `render_curl` action paths; `confirm_profile_select`'s terminal branch (mod.rs:477) becomes a three-way match.
- `src/tui/handlers.rs` — `y` binding in `handle_key_browse`; `handle_key_var_input`'s Enter (handlers.rs:370) must dispatch on the new action field instead of always running.
- `src/tui/render.rs` — `copy_to_clipboard` rewritten with backend fallback and a success/failure result; Response pane grows a "copied" indicator; `y` added to the Browse key hints.
- New module for cURL generation (shell quoting, query folding, header and body assembly) with unit tests — this is pure string work and should not live in the TUI event path.
- No new dependencies; the clipboard backends are external binaries probed at runtime, not crates.
- **Security note:** the generated command contains fully-resolved secrets, and the point of the feature is that it does. Worth a README caution against pasting one into a public issue.
