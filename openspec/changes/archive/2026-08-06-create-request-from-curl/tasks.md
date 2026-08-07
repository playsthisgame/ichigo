## 1. Tokenizer

- [x] 1.1 Add `fn tokenize(input: &str) -> Result<Vec<String>>` to `src/curl.rs`: single-pass state machine over single-quoted, double-quoted, `$'…'`, and unquoted runs, with adjacent runs concatenating into one token.
- [x] 1.2 Handle escapes per context — none inside `'…'`; `\"`, `\\`, `` \` ``, `\$` inside `"…"`; `\n`, `\t`, `\r`, `\\`, `\'`, `\xHH` inside `$'…'` — and treat a backslash before a newline as a line continuation.
- [x] 1.3 Return an error on an unterminated quote instead of a truncated token.
- [x] 1.4 Unit tests: continuations, `'X-Note: it'\''s here'` → `it's here`, `$'line1\nline2'` → real newline, `-H'X: 1'` concatenation, unterminated quote errors.

## 2. Flag table and `from_curl`

- [x] 2.1 Add `pub fn from_curl(input: &str) -> Result<RequestConfig>`: strip a leading `$ `/`% ` prompt marker, require a first token of `curl`, and leave `name` empty for the caller to fill.
- [x] 2.2 Split bundled short flags (`-sSL`) before lookup so an unsupported letter inside a bundle is still caught.
- [x] 2.3 Map the recognized flags: `--url`, `-X/--request`, `-H/--header`, `-d/--data/--data-raw/--data-ascii/--data-binary`, `--data-urlencode` (percent-encode its value), `--json`, `-G/--get`, `-I/--head`.
- [x] 2.4 Translate the convenience flags into headers: `-u/--user` → `Authorization: Basic …`, `-b/--cookie` → `Cookie`, `-A/--user-agent` → `User-Agent`, `-e/--referer` → `Referer`. Error on a `-u` value with no `:`.
- [x] 2.5 Add the ~15-line base64 encoder with its own tests (empty, 1-, 2-, 3-byte remainders, non-ASCII) — used only by `-u`.
- [x] 2.6 Accept and ignore the no-op flags: `-s/--silent`, `-v/--verbose`, `-i/--include`, `-o/--output`, `-k/--insecure`, `-L/--location`, `--compressed`, `-#`, `--max-time`. `--compressed` must NOT add an `Accept-Encoding` header.
- [x] 2.7 Reject everything else — `-F/--form`, an `@`-prefixed data value, and any unrecognized flag — with an error naming the flag. Deny by default; never skip an unknown flag silently.
- [x] 2.8 Derive the method: explicit `-X` uppercased wins; else `-I` → HEAD; else body present → POST; else GET.
- [x] 2.9 Assemble the body: concatenate repeated data values with `&`; move a `Content-Type` header out of `headers` into `body.content_type`; default to `application/x-www-form-urlencoded` when a body has no content type; leave a `Content-Type` header alone and create no body when there is no data. `--json` implies content type `application/json` plus an `Accept: application/json` header.
- [x] 2.10 Implement `-G`: fold body data into the query string rather than sending it as a body.
- [x] 2.11 Lift the URL's query string into `query` via `Url::query_pairs()`, but only when the URL parses and every key is distinct; otherwise leave the query on `url` verbatim and `query` empty.
- [x] 2.12 Unit tests for §2: headers+body command, positional vs `--url`, repeated `-d`, method derivation (all four paths), Content-Type placement in all three cases, `--json`, distinct-key query lifting, `+`/`%26` decoding, repeated-key and `{{VAR}}`-placeholder URLs keeping their query, `-u` encoding and the colon-less error, `-F`/`@file`/unknown-flag/bundle rejections, noise flags ignored with no `Accept-Encoding`, non-`curl` input rejected.
- [x] 2.13 Round-trip tests: `from_curl(to_curl(c, &vars))` matches `c` in method, url, headers, query, and body for a resolved config; and name/description/profiles/extract come back empty.
- [x] 2.14 Update the `curl.rs` module doc to cover both directions and name the shared normalizations (Content-Type placement, query folding) as the drift risk.

## 3. CLI entry point

- [x] 3.1 Add `--from-curl` (boolean) to `Commands::New` in `src/main.rs`, documented as reading the command from stdin.
- [x] 3.2 Make `--from-curl` conflict with `--url` and `--method` via clap, so passing both errors instead of picking a winner.
- [x] 3.3 Branch `cmd_new`: read stdin to a string, call `from_curl`, set the config's `name` from the argument, and write it through the existing path so `--global`, name validation, and collision handling all still apply.
- [x] 3.4 On a parse error, report it and write nothing.
- [x] 3.5 Add `--from-curl` to the embedded zsh, bash, and fish completion scripts in `main.rs`.

## 4. Carry headers/query/body through the new-request form

- [x] 4.1 Add `headers`, `query`, `body`, and `extract` fields to `Mode::NewRequest`, populated at construction — from the loaded config when editing, empty when creating.
- [x] 4.2 Rewrite `save_new_request` (mod.rs:768) to read those four fields from the mode instead of re-loading the original file at save time. This deletes the load-at-save step rather than branching around it.
- [x] 4.3 Verify editing an existing request still preserves its headers, query, body, and extract, including after a rename.

## 5. TUI import pane

- [x] 5.1 Enable bracketed paste on TUI entry (`EnableBracketedPaste`) and disable it on exit alongside `LeaveAlternateScreen`, including the error path, so a failure cannot leave the terminal emitting paste markers.
- [x] 5.2 Add an `Event::Paste(text)` arm to the event loop (mod.rs:879), routed to the app so a mode that accepts pasted text can append it.
- [x] 5.3 Add `Mode::ImportCurl { buffer, error }` and bind `i` in `handle_key_browse` to open it. Confirm `i` collides with nothing in Browse.
- [x] 5.4 Add its key handler: printable chars and paste events append, Backspace deletes, Enter inserts a newline, Ctrl-S confirms, Esc cancels.
- [x] 5.5 On confirm, parse the buffer; on success enter `Mode::NewRequest` prefilled with the parsed method, url, headers, query, and body, name empty and focused; on failure set `error` and keep the buffer intact.
- [x] 5.6 Render the import pane in `render.rs` — buffer with wrapping, the error when present, and its key hints — plus an `i import cURL` hint in the Browse key line.

## 6. Docs

- [x] 6.1 README: document `ichigo new <name> --from-curl` with a `pbpaste |` example and a heredoc example.
- [x] 6.2 README: add `i` to the TUI keybinding table and describe the import → form → save flow.
- [x] 6.3 README: list which flags are mapped, which are ignored, and which are refused, and note that `--compressed` is deliberately dropped because the client negotiates encoding itself.
- [x] 6.4 README: caution that an imported config stores literal credentials, and suggest replacing them with a `{{VAR}}` plus a profile before committing `.ichigo/`.
- [x] 6.5 CLAUDE.md: `curl.rs` now parses as well as renders; `Mode::ImportCurl` and the bracketed-paste requirement in the mode list; `NewRequest` now carries headers/query/body/extract instead of re-reading them at save.

## 7. Verification

- [x] 7.1 `cargo test` and `cargo clippy` pass clean.
- [x] 7.2 Parse real "Copy as cURL" output from Chrome and Firefox (including a `$'…'` body and a `--compressed` flag) and confirm the stored config is correct.
- [x] 7.3 Against a local echo server, run an imported config with `ichigo run` and the original command with `curl`, and diff the echoed requests — same method, path, query, headers, and body.
- [x] 7.4 Manual: TUI import with a multi-line paste in a terminal that supports bracketed paste, and in one that does not (verify Enter inserts a newline rather than submitting).
- [x] 7.5 Manual: Esc from the prefilled form writes no file; saving writes the headers and body.
- [x] 7.6 Manual: a command with `-F` is refused in both the CLI and the TUI with an error naming the flag.
