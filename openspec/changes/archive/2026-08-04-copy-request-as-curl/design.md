## Context

The TUI's action pipeline is `Browse → ProfileSelect → VarInput → <terminal step>`. Two things make adding a third terminal step more than a new key binding:

1. **`ProfileSelect` carries `for_test: bool`** (mod.rs:25), consumed by `confirm_profile_select`'s terminal branch at mod.rs:477. A bool has room for exactly the two destinations that exist.
2. **`VarInput` carries no destination at all.** `handle_key_var_input`'s Enter (handlers.rs:370) hardcodes `execute_chain` / `execute_request`. Any request that reaches `VarInput` — which is every request whose profile does not cover all its placeholders — runs. There is no way for a non-run action to survive the variable prompt.

So the shape of this change is: make the destination explicit and thread it through both modes, then add a third terminal step that renders instead of sends.

The generation itself must agree with `utils::send_request` (utils.rs:9), because a command that differs from what ichigo would actually send is worse than no command. The details that matter:

- `send_request` interpolates url, header keys *and* values, query keys and values, and body data — all through `interpolate` (utils.rs:75), which prefers the vars map and falls back to the process environment.
- Query params are attached with `req.query(&query)`, i.e. appended to the URL and percent-encoded by reqwest.
- A body sets `Content-Type` from `body.content_type` **in addition to** whatever `headers` already contains.

`config-reload` (just archived) established that no action may derive its values from `App::entries`. This feature is a new action path and inherits that rule.

## Goals / Non-Goals

**Goals:**
- Produce a cURL command that, pasted into a shell, issues the same request ichigo would have sent for the same profile and variables.
- Survive pasting: correct shell quoting for tokens, JSON bodies, and values containing quotes or spaces.
- Show the command before copying, so the user can verify it.
- Make clipboard copying work on Linux and fail loudly when it cannot work.
- Keep cURL generation out of the event loop — pure functions, unit tested.

**Non-Goals:**
- Chains. The value threading between steps has no cURL equivalent; `y` on a chain reports unsupported.
- A CLI subcommand (`ichigo curl <name>`). Plausible follow-up, but the request was for the TUI, and the CLI already has `--var` flags that make a shell-side equivalent easy.
- Importing cURL commands back into configs. Different feature, different parser.
- Round-trip fidelity for exotic reqwest behavior (redirect policy, timeouts, connection reuse). The command reproduces the *request*, not the client configuration.
- A clipboard crate. The backends are binaries probed at runtime; adding `arboard` or similar would pull X11/Wayland linkage into a tool that otherwise builds anywhere.

## Decisions

### 1. Replace `for_test: bool` with a `PendingAction` enum, on both modes

```rust
#[derive(Clone, Copy, PartialEq)]
enum PendingAction { Run, Test, Curl }
```

`ProfileSelect { for_test: bool }` becomes `ProfileSelect { action: PendingAction }`, and `VarInput` gains the same field. `confirm_profile_select`'s branch at mod.rs:477 and `handle_key_var_input`'s Enter both `match` on it.

*Alternative considered:* keep the bool and add a parallel `for_curl: bool`. Rejected — two bools encode four states, two of which are nonsense, and the next action makes it eight.

*Alternative considered:* a separate `CurlProfileSelect` mode. Rejected — it would duplicate the entire profile-picker render and key handler for a one-word difference in the terminal step.

Threading the action through `VarInput` is the part that actually unblocks the feature: without it, any request whose profile does not cover every placeholder would fall into `VarInput` and then run instead of producing a command.

### 2. cURL generation lives in its own module as pure functions

New `src/curl.rs` (a sibling of `utils.rs`, not inside `tui/`), exposing roughly:

```rust
pub fn to_curl(config: &RequestConfig, vars: &HashMap<String, String>) -> String
fn shell_quote(s: &str) -> String
```

It takes an already-loaded `RequestConfig` and the resolved vars map — the same two things `send_request` takes — and returns a string. No IO, no clipboard, no TUI types. That makes the interesting part (quoting, query folding, header ordering) unit-testable without a terminal, and keeps the door open for a CLI subcommand later without moving code.

It must call the same `utils::interpolate` that `send_request` calls, rather than reimplementing substitution. `interpolate` is currently private to `utils`; it becomes `pub(crate)`.

### 3. Quote everything with single quotes; escape by breaking out

Every interpolated value goes through `shell_quote`, which wraps in `'…'` and replaces each embedded `'` with `'\''`. This is the only escaping rule POSIX shells need inside single quotes, and it handles the cases that actually occur: JSON bodies full of double quotes, tokens with `$`, URLs with `&`, values with spaces.

*Alternative considered:* double quotes with backslash escaping. Rejected — inside double quotes the shell still expands `$`, `` ` ``, and `\`, so a token containing `$` would be silently mangled. Single quotes suppress all expansion.

Deliberately quote even values that look safe. Conditional quoting is a bug generator, and the visual noise is the price of a command that always works.

### 4. Output format: multi-line with `\` continuations

```
curl -X POST 'https://api.example.com/users?page=2' \
  -H 'Authorization: Bearer abc123' \
  -H 'Content-Type: application/json' \
  --data-raw '{"name":"ada"}'
```

Rules:
- `-X <METHOD>` is omitted for GET, since that is cURL's default and its presence is noise. Included for everything else.
- Query params are folded into the URL rather than emitted as `--data-urlencode`, matching how `send_request` attaches them, and percent-encoded the same way.
- The body uses `--data-raw`, not `-d`. `-d` strips newlines and would corrupt a multi-line JSON body; `--data-raw` also avoids `@`-prefixed values being read as filenames.
- `Content-Type` is emitted from `body.content_type` when a body exists, after the config's own headers, mirroring `send_request`'s ordering.
- Headers come from a `HashMap`, so iteration order is nondeterministic. Sort by name before emitting — a command that differs between invocations is confusing to diff and impossible to test.

### 5. Reuse `Mode::Response` to display the command

The generated command goes into `Mode::Response` with `status: 0` and a title indicating cURL plus copy status. Scroll, `f` filter, `c` re-copy, and Esc all work with no new code.

*Alternative considered:* a dedicated `CurlPreview` mode. Rejected — it buys a nicer title and costs a duplicated render path, key handler, and scroll implementation.

`status: 0` currently renders as "Error" in the response title (that is how load failures display). The title logic needs a way to distinguish "no HTTP status because this is not a response" from "no HTTP status because the request failed" — a small addition to the Response mode's rendering, not a new mode.

### 6. Clipboard: ordered backend probe, surfaced result

`copy_to_clipboard` returns `Result<(), String>` instead of `()`, and tries in order: `pbcopy`, `wl-copy`, `xclip -selection clipboard`, `xsel --clipboard --input`. First one that spawns and accepts the write wins. If all fail, the error names what was tried.

Probing by attempting to spawn (rather than checking `$WAYLAND_DISPLAY` / `$DISPLAY`) keeps it to one mechanism and works under tmux and SSH where those variables lie.

The existing response-copy `c` key (handlers.rs:495) must also surface the failure — today it discards it. That is the pre-existing silent-failure bug this change fixes.

### 7. Chains rejected at the action's entry point

`try_copy_curl_selected` checks `EntryKind::Chain` and shows "Copying chains as cURL is not supported." This mirrors `try_test_selected` (mod.rs:308) exactly, including reusing the Response pane for the message. The check happens after the config is reloaded from disk, so a config that has *become* a chain since startup is rejected correctly.

## Risks / Trade-offs

- **The command embeds live secrets.** → Inherent to the feature; a cURL command with `{{TOKEN}}` unresolved would be useless. Mitigation is documentation: a README note that generated commands contain real credentials and should not be pasted into public issues. Not something the code should second-guess, since the user explicitly asked for this request with this profile.

- **Generation could drift from `send_request` if one changes without the other.** → Sharing `interpolate` covers substitution, which is the subtle part. The assembly rules (query folding, Content-Type from body) are duplicated logic and are the real drift risk. Tests assert the specific correspondences, and a comment in each function points at the other.

- **Percent-encoding may not exactly match reqwest's.** → reqwest uses its own query serializer; hand-rolling encoding risks differing on edge characters. Prefer building the URL with the `url` crate (already in the tree via reqwest) over hand-rolling, and test with values containing spaces, `&`, `=`, and non-ASCII.

- **`--data-raw` is not in ancient cURL.** → It landed in 7.43 (2015). Acceptable; `-d`'s newline stripping is a correctness bug, which is worse than an old-cURL incompatibility.

- **Response mode's `status: 0` overload.** → Decision 5 notes this needs a real distinction rather than another magic number. Contain it in the Response rendering rather than letting "0 means three different things" spread.

- **Sorting headers makes output differ from insertion order.** → Insertion order does not exist; `HashMap` iteration is already arbitrary, so nothing is lost and determinism is gained.
