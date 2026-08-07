## Context

`src/curl.rs` already renders a `RequestConfig` as a cURL command. This change adds the inverse. The two directions are not symmetric, and most of the design is about naming the asymmetries:

- **Rendering has one input format; parsing has many.** `to_curl` emits one canonical shape. `from_curl` has to accept whatever Chrome devtools, an API doc, or a teammate's shell history produced — different flag spellings for the same thing (`-d`, `--data`, `--data-raw`, `--data-binary`), different quoting styles, backslash line continuations, and flags ichigo has no field for.
- **Rendering cannot fail; parsing can.** `to_curl` returns `String`. `from_curl` returns `Result<RequestConfig>`, and *what it refuses* is as much of the design as what it accepts.
- **Rendering resolves variables; parsing does not create them.** `to_curl` takes a vars map and substitutes. `from_curl` produces literals. Nothing in a pasted command tells you which value is a secret worth templating.

Two existing structures constrain the TUI half. The event loop matches only `Event::Key` (mod.rs:879), so a multi-line paste currently arrives as a stream of `Char` events with `Enter`s embedded — in the new-request form those Enters would each trigger a save. And `save_new_request` recovers headers/query/body/extract by re-loading the original file from disk (mod.rs:768), which an imported request does not have.

## Goals / Non-Goals

**Goals:**
- Turn a pasted cURL command into a `RequestConfig` covering the fields ichigo actually has: method, url, headers, query, body.
- Round-trip: a command produced by `to_curl` parses back to the request it came from.
- Refuse clearly. A flag that changes what is sent and has no config equivalent is an error naming the flag, never a silent drop.
- Keep parsing pure — no IO, no TUI types — so the tokenizer and flag table are unit-testable next to `to_curl`'s tests.
- Reach it from both the CLI and the TUI, with the TUI showing the parse in the editable new-request form before anything touches disk.

**Non-Goals:**
- Inferring `{{VAR}}` placeholders. Guessing that `Bearer eyJ…` should become `{{TOKEN}}` is a heuristic that is wrong often enough to be worse than the literal the user can see and edit.
- Importing a command as a chain, or appending a step to an existing chain. One command is one request.
- Multipart (`-F`). `Body` is a content type plus a string; a multipart form is neither, and faking it would produce configs that send the wrong bytes.
- `@file` data references, `--config`, `--next`, proxies, TLS client certs, cookie jars. Each either reads the filesystem at parse time or describes client behavior rather than the request.
- A general shell parser. The tokenizer handles the quoting a cURL command uses, not `$(…)`, pipes, or variable expansion.
- Reading the system clipboard programmatically. "Pasted in" means the terminal's paste, plus stdin for the CLI — no `pbpaste`-style read path and no new capability surface.

## Decisions

### 1. `from_curl` lives in `src/curl.rs`, beside `to_curl`

```rust
pub fn from_curl(input: &str) -> Result<RequestConfig>
fn tokenize(input: &str) -> Result<Vec<String>>
```

The module doc already says `to_curl` "must stay in step with `send_request`". The parser makes that a three-way constraint, and the cheapest way to keep it honest is for the round-trip tests to sit in the same file as both functions. Splitting `curl.rs` into a directory module was considered and rejected: the file lands around 700 lines including tests, which is smaller than `tui/mod.rs`, and the pairing is the point.

`from_curl` takes only the command text. It does not take a name — the name comes from the CLI argument or the TUI form — so it returns a config with `name` left as an empty string for the caller to fill. The alternative, threading a name parameter through, puts a concern the parser has no opinion about into its signature.

### 2. Hand-rolled tokenizer, deny-by-default flag table

The tokenizer walks the string once with a small state machine:

| Construct | Handling |
|---|---|
| `'…'` | Literal to the closing quote; no escapes (matches POSIX and `shell_quote`'s output) |
| `"…"` | `\"`, `\\`, `` \` ``, `\$` unescape; everything else literal |
| `$'…'` | ANSI-C: `\n`, `\t`, `\r`, `\\`, `\'`, `\xHH` — Chrome on Linux emits this for bodies with control characters |
| `\` + newline | Line continuation, dropped |
| Unquoted whitespace | Token separator |
| Unterminated quote | `Err` |

Adjacent quoted and unquoted runs concatenate into one token (`-H'X: 1'` and `--data-raw'{"a":1}'` both occur in the wild).

The first token must be `curl`; a leading `$ ` or `%` prompt marker is stripped first, since people copy prompts. Everything after is matched against an explicit table:

- **Mapped**: `--url`, `-X/--request`, `-H/--header`, `-d/--data/--data-raw/--data-ascii/--data-binary`, `--data-urlencode`, `--json`, `-G/--get`, `-I/--head`, `-u/--user`, `-b/--cookie`, `-A/--user-agent`, `-e/--referer`.
- **Ignored**: flags that do not change the request ichigo would store — `-s/--silent`, `-v/--verbose`, `-i/--include`, `-o/--output`, `-k/--insecure`, `-L/--location`, `--compressed`, `-#`, `--max-time`, and friends. `--compressed` is the interesting one: it does add an `Accept-Encoding` header, but reqwest negotiates that itself, so storing it would make the config lie about what ichigo sends.
- **Everything else**: `Err(anyhow!("unsupported curl flag: {flag}"))`.

Deny-by-default is the whole safety story. An allow-anything parser that skips what it does not recognize will happily turn `curl -F file=@x https://…` into a bodyless GET, and the user finds out when the request fails against a real server. Bundled short flags (`-sSL`) are split before lookup so a bundle containing an unsupported flag is still caught.

*Alternative considered:* a shell-words crate (`shlex`). Rejected — none of the candidates handle `$'…'`, and a ~120-line tokenizer with no dependency is a better trade for a tool that advertises building anywhere.

### 3. Method derivation mirrors curl's own rules

Explicit `-X` wins, uppercased. Otherwise `-I` → HEAD, a body present → POST, else GET. This is what curl does, so a command that would have sent a POST becomes a POST config. Deriving nothing and defaulting to GET would silently change the request whenever the source command omitted `-X`, which is most POSTs from a `-d`-using API doc.

### 4. Body assembly, and Content-Type moving out of headers

Multiple `-d` values concatenate with `&`, as curl does. `--data-urlencode` percent-encodes its value first. `--json` sets the data and implies `Content-Type: application/json` plus `Accept: application/json`.

The `Content-Type` header, if present, is **removed from `headers` and stored as `body.content_type`**. This is not cosmetic: `to_curl` emits a `Content-Type` header derived from the body *in addition to* everything in `headers`, so a config holding it in both places renders a command with the header twice. When there is a body but no `Content-Type`, curl's own default applies: `application/x-www-form-urlencoded`.

A `Content-Type` with no body stays in `headers` untouched — `Body` requires data, and inventing an empty one would make `to_curl` emit `--data-raw ''`.

### 5. Query lifting is conditional on uniqueness

A query string on the URL is decoded into the `query` map and stripped from `url`, so the config is editable in the shape ichigo's other tooling expects. Two cases block it:

- **A repeated key** (`?tag=a&tag=b`). `query` is a `HashMap<String, String>`; lifting would drop a value.
- **A URL that will not parse**, typically because it carries `{{VAR}}` placeholders from a `to_curl` round-trip.

In both cases the query stays on the `url` string verbatim, which is still a correct config — `send_request` appends `query` to whatever the URL already has. Decoding uses `Url::query_pairs()` rather than hand-rolled splitting, so `+` decodes to a space and matches what `to_curl`'s `query_pairs_mut` produced.

### 6. `-u user:pass` becomes an `Authorization: Basic` header

That is what curl puts on the wire, and ichigo has no auth concept to store it in otherwise. It needs base64, which the tree does not have; the encoder is ~15 lines of table lookup with its own tests, which is a better trade than a dependency for one flag. If `-u` has no `:`, curl prompts for the password interactively — a parser cannot, so that form is an error.

*Alternative considered:* rejecting `-u` outright. Rejected — it is common in API docs, and the translation is exact rather than a guess.

### 7. CLI: `ichigo new <name> --from-curl` reads stdin

```sh
pbpaste | ichigo new api/login --from-curl
ichigo new api/login --from-curl <<'EOF'
curl -X POST 'https://api.example.com/login' -H 'Content-Type: application/json' --data-raw '{"u":"ada"}'
EOF
```

Stdin, not an argument. A cURL command passed as an argument gets tokenized by the user's shell first, so every quote in it needs re-escaping — the exact tedium this feature exists to remove. `--from-curl` is therefore a boolean flag, and it conflicts with `--url` and `--method`, which the command already supplies. Passing them together is an error rather than a silent precedence rule.

Reusing `new` rather than adding an `import` subcommand keeps one place that names, validates, and writes a config, and inherits the existing `--global` and name-collision handling.

### 8. TUI: a paste buffer mode that hands off to the existing form

`i` in Browse (unbound today) opens `Mode::ImportCurl { buffer, error }`. Confirming parses; on success the app enters `Mode::NewRequest` prefilled with the parsed method and URL and the name field empty and focused. On failure the error renders in the pane with the buffer intact so the user can fix or re-paste.

Nothing is written until the user saves the form. Import is a prefill, not a create — the user still names it, can add a description and profiles, and sees exactly what will be stored.

**Keys inside the import pane:** `Enter` inserts a newline, `Ctrl-S` confirms, `Esc` cancels. Enter cannot mean "confirm" here, because that is precisely the key a non-bracketed paste delivers in the middle of a multi-line command.

**Bracketed paste** is enabled with `EnableBracketedPaste` on entry (paired with `DisableBracketedPaste` on exit alongside the existing `LeaveAlternateScreen`), and the event loop grows an `Event::Paste(text)` arm that appends to the buffer when the mode accepts it. Terminals that support it deliver the paste as one event, which is faster and avoids per-character re-renders. Terminals that do not fall back to the character path, which still works because of the Enter binding above.

### 9. `NewRequest` carries headers/query/body/extract explicitly

Today `save_new_request` reloads the original file to recover the fields the form does not edit (mod.rs:768). An imported request has no original file, so those fields would be dropped at save — every header and the body silently gone.

Rather than special-casing the import path, `Mode::NewRequest` gains the four fields, populated at construction: from the loaded config when editing, from the parse when importing, empty when creating. `save_new_request` then reads them from the mode instead of hitting the disk. This deletes the load-at-save step rather than adding a branch to it, and removes a latent bug where a file edited externally between opening and saving the form would have its stale headers written back.

### 10. Round-trip is a tested property, with its normalizations named

`from_curl(to_curl(c, &vars))` must equal `c` in method, url, headers, query, and body — for configs whose vars are all resolved. The normalizations that make it hold are decisions 4 and 5 (Content-Type placement, query lifting), and the ones that make it *not* hold exactly are worth stating: header ordering does not survive (both sides are `HashMap`s, so there is no order to lose), `name`/`description`/`profiles`/`extract` are not representable in a command and come back empty, and a config with a repeated query key or an unresolved placeholder round-trips with its query on the URL instead of in the map — same request, different shape.

## Risks / Trade-offs

- **A pasted command contains live credentials, and now they land in a YAML file.** → Same exposure `to_curl` already documents, in the other direction. The config file is local and the user pasted the command deliberately, but the README note should say that an imported config holds the literal token and is worth converting to a `{{VAR}}` plus a profile before committing `.ichigo/` to a repo.

- **Deny-by-default will reject commands users consider reasonable.** → Accepted deliberately, and cheap to walk back: adding a flag to the ignore or mapped list is a one-line change plus a test, whereas a config that silently sends the wrong thing costs a debugging session. The error names the flag so the report is actionable.

- **The tokenizer is hand-rolled and quoting is where parsers go wrong.** → Contained: no IO, no state beyond the buffer, and tests drive it with real Chrome/Firefox "copy as cURL" output including `$'…'` bodies, embedded quotes, and continuations. The round-trip property covers `shell_quote`'s output specifically.

- **Windows-style commands (`^` continuations, `"`-quoted everything) will not parse.** → Out of scope; the TUI is already POSIX-shell-shaped. The failure is a clear tokenizer error, not a wrong config.

- **`--compressed` is dropped, so a stored config differs from the pasted command in `Accept-Encoding`.** → Correct behavior: reqwest sets that header itself, and storing it would make the config claim something `send_request` does not do. Worth a line in the README's list of ignored flags.

- **Bracketed paste changes terminal state.** → It must be disabled on the same path that disables raw mode and leaves the alternate screen, including the error path, or a panic leaves the user's terminal echoing paste markers.

- **`-d` with a very large body pasted into a TUI buffer.** → No limit imposed; the pane scrolls and the parse is linear. Not worth a cap.
