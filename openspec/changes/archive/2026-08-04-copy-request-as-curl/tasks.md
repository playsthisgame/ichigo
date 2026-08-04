## 1. cURL generation module

- [x] 1.1 Make `utils::interpolate` `pub(crate)` so generation shares the exact substitution the request path uses instead of reimplementing it.
- [x] 1.2 Create `src/curl.rs` with `pub fn to_curl(config: &RequestConfig, vars: &HashMap<String, String>) -> String` and a private `shell_quote`; register the module in `main.rs`. No IO, no TUI types.
- [x] 1.3 Implement `shell_quote`: wrap in single quotes, replace each embedded `'` with `'\''`. Quote unconditionally — no "looks safe" fast path.
- [x] 1.4 Assemble the command: `-X <METHOD>` omitted for GET; URL with query params folded in and percent-encoded (build via the `url` crate, already present through reqwest, rather than hand-rolling); `-H` per header sorted by name; `--data-raw` for the body plus a `Content-Type` header from `body.content_type`. Emit multi-line with ` \` continuations.
- [x] 1.5 Add a comment in `to_curl` pointing at `utils::send_request` (and vice versa) naming the duplicated assembly rules — query folding and body Content-Type — as the drift risk.
- [x] 1.6 Unit tests in `curl.rs`: GET omits `-X`; POST includes it; headers sorted and stable across two calls; query params percent-encoded into the URL (cover space, `&`, `=`, non-ASCII); body emitted with Content-Type; multi-line body preserved; single quotes escaped as `'\''`; `$`/backtick/space values quoted; JSON body intact; `{{VAR}}` substituted from the vars map; environment fallback when absent from the map.

## 2. Portable clipboard

- [x] 2.1 Change `render::copy_to_clipboard` to return `Result<(), String>` and try `pbcopy`, `wl-copy`, `xclip -selection clipboard`, `xsel --clipboard --input` in order, using the first that spawns and accepts the write. Probe by spawning, not by reading `$DISPLAY`/`$WAYLAND_DISPLAY`.
- [x] 2.2 On exhaustion return an error naming the tools tried.
- [x] 2.3 Update the existing response/test-result copy (`handlers.rs:495`) to surface the failure instead of discarding it — this is the pre-existing silent no-op the change fixes.

## 3. Thread the pending action through the mode pipeline

- [x] 3.1 Add `enum PendingAction { Run, Test, Curl }` (Clone, Copy, PartialEq) to `tui/mod.rs`.
- [x] 3.2 Replace `Mode::ProfileSelect { for_test: bool }` with `{ action: PendingAction }`; update its construction sites (mod.rs:314, :351, :405) and `render.rs` if it reads the field.
- [x] 3.3 Add `action: PendingAction` to `Mode::VarInput` and set it at every construction site (mod.rs:329, :368, :487).
- [x] 3.4 Rewrite `confirm_profile_select`'s terminal branch (mod.rs:477) as a `match` on the action, with `Curl` routing to the new render step.
- [x] 3.5 Rewrite `handle_key_var_input`'s Enter (handlers.rs:370) to dispatch on the action field rather than always running — without this, any request whose profile misses a placeholder would run instead of copying.

## 4. The copy-as-cURL action

- [x] 4.1 Add `try_copy_curl_selected` to `App`: reload the entry from disk via `reload_selected`, reject `EntryKind::Chain` with "Copying chains as cURL is not supported." (mirroring `try_test_selected` at mod.rs:308), then enter the profile/var flow with `PendingAction::Curl`.
- [x] 4.2 Add the terminal step: load the `RequestConfig`, call `curl::to_curl` with the resolved vars, copy the result, and display it with the copy outcome. Reuse `Mode::Response` per the design.
- [x] 4.3 Give the Response pane a way to distinguish "generated command" from "HTTP response" and from "error", so a command is not titled `Error` via the `status: 0` overload. Contain the change in the Response rendering — do not add a mode.
- [x] 4.4 Bind `KeyCode::Char('y')` in `handle_key_browse` to `try_copy_curl_selected`. Confirm it does not disturb `c`, `r`, or `R`.
- [x] 4.5 Add `y copy cURL` to the Browse key hints in `render.rs`.

## 5. Docs

- [x] 5.1 Add `y` to the README keybinding table and describe the feature (profile selection, what the command contains).
- [x] 5.2 Add a README caution that generated commands contain fully-resolved credentials and should not be pasted into public issues or shared logs.
- [x] 5.3 Note the clipboard backends and their fallback order in the README, replacing any claim that copy is macOS-only.
- [x] 5.4 Update `CLAUDE.md`: the new `src/curl.rs` module, `PendingAction` threading through `ProfileSelect`/`VarInput`, and the rule that generation must stay in step with `send_request`.

## 6. Verification

- [x] 6.1 `cargo test` and `cargo clippy` pass clean.
- [x] 6.2 Verify against a live echo server that the generated command and an actual ichigo run produce byte-identical requests — same method, URL, query, headers, body. Run the copied command with `curl` and diff the echoed request against ichigo's own.
- [x] 6.3 Manual: `y` on a request with a profile → command carries the real token; paste it into a shell and confirm it executes.
- [x] 6.4 Manual: `y` on a request whose profile misses a placeholder → var form appears, and confirming copies rather than sending.
- [x] 6.5 Manual: `y` on a chain → unsupported message, nothing copied.
- [x] 6.6 Manual: values containing a single quote, a `$`, and a multi-line JSON body all survive the paste.
- [x] 6.7 Manual: clipboard failure path reports rather than silently succeeding (simulate by making the backends unavailable via `PATH`).
