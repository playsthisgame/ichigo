## Why

Getting a request *into* ichigo is the slowest part of using it. Every browser devtools panel, every API doc, and every teammate's bug report speaks cURL — and today the only way to turn one into a stored config is to read the command by eye and retype the URL, each `-H`, and the body into `ichigo new` or the TUI's new-request form. The command is already a complete, unambiguous description of the request; ichigo should be able to read it.

The reverse direction already exists: `src/curl.rs` renders a `RequestConfig` as a cURL command. This is that function run backwards, and building it next to `to_curl` lets the two be tested against each other.

## What Changes

- **A cURL command can be parsed into a `RequestConfig`.** A new `curl::from_curl` tokenizes the command with shell quoting rules and maps the flags ichigo can represent — URL, `-X`, `-H`, the `-d` family, `--json`, `-G`, `-I`, `-u`, `-b`, `-A`, `-e` — onto config fields. It is pure string work with no IO, the mirror of `to_curl`, and unit tested alongside it.
- **`ichigo new` gains `--from-curl`.** `ichigo new <name> --from-curl` reads the command from stdin (`pbpaste | ichigo new api/login --from-curl`, or heredoc), parses it, and writes the config. `--url` and `--method` are rejected alongside it, since the command supplies both.
- **The TUI imports a pasted command.** A new key in Browse mode opens an import pane; the user pastes, confirms, and lands in the existing `NewRequest` form with method, URL, headers, query, and body already filled from the parse — so the name, description, and profiles are still theirs to set, and nothing is written to disk until they save. Bracketed paste is enabled so a multi-line command arrives as one event instead of a stream of keystrokes with Enters in it.
- **Unrepresentable commands fail loudly.** `-F/--form` (multipart), `@file` data references, and unrecognized flags are reported as errors naming the offending flag. Flags with no config equivalent that do not change what is sent (`-s`, `-v`, `-i`, `-k`, `-L`, `--compressed`, …) are ignored knowingly. Silently dropping a flag that changes the request is the one failure mode worse than not supporting it.
- **The parse normalizes toward ichigo's shape.** A query string on the URL is lifted into the `query` map (only when every key is unique — a repeated key cannot survive a `HashMap`, so those stay on the URL). A `Content-Type` header is moved into `body.content_type` rather than left in `headers`, because `to_curl` re-derives that header from the body and would otherwise emit it twice.
- **Method is derived the way curl derives it.** Explicit `-X` wins; otherwise `-I` means HEAD, a body means POST, and everything else is GET.

Not changing: the YAML format, the request-sending path, `to_curl`'s output, and how variables are resolved. Nothing in a parsed command becomes a `{{VAR}}` placeholder — guessing which literal is a secret is a separate feature and a worse default than leaving the value visible where the user can see and replace it.

## Capabilities

### New Capabilities
- `request-from-curl`: Parsing a cURL command into a stored request config — the tokenizer, the flag-to-field mapping, the failure rules for what cannot be represented, and the CLI and TUI entry points that reach it.

### Modified Capabilities

None. `request-as-curl`, `clipboard-copy`, `config-reload`, and `tui-folder-tree` keep their requirements as written. The round-trip guarantee (a command produced by `to_curl` parses back to the same request) is a constraint on the new parser, so it belongs in the new spec rather than as a change to `request-as-curl`.

## Impact

- `src/curl.rs` — gains `from_curl` plus the shell tokenizer and flag table. The module doc grows a second half; the existing `to_curl` is untouched. Round-trip tests join the existing unit tests.
- `src/main.rs` — `Commands::New` gains `--from-curl`; `cmd_new` branches on it, reads stdin, and writes the parsed config. Conflicting-flag validation and the completion scripts (`const` strings) both need the new flag.
- `src/tui/mod.rs` — a new `Mode` for the import buffer, its entry point from Browse, and the transition that prefills `NewRequest` from a parsed config. `Mode::NewRequest` currently rebuilds headers/query/body only when editing an existing file (mod.rs:768); an imported request has to carry them through the form without a file to load them from.
- `src/tui/handlers.rs` — the Browse binding, a handler for the import mode, and `Event::Paste` routing.
- `src/tui/mod.rs` event loop — `event::read()` currently matches only `Event::Key`; bracketed paste must be enabled on entry, disabled on exit, and its event handled.
- `src/tui/render.rs` — the import pane and a Browse key hint.
- No new dependencies. Shell tokenizing and percent-decoding are done with what is already in tree (`reqwest::Url` for the query split).
- README needs the new flag and key documented.
