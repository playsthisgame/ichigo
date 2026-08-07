# ichigo

A CLI HTTP client. Store named request configs in your project or globally, then run them by name.

![ichigo TUI](https://raw.githubusercontent.com/playsthisgame/ichigo/main/assets/demo.png)

## Installation

**Via shell script (macOS/Linux):**

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/playsthisgame/ichigo/releases/latest/download/ichigo-installer.sh | sh
```

**Via PowerShell (Windows):**

```powershell
powershell -ExecutionPolicy ByPass -c "irm https://github.com/playsthisgame/ichigo/releases/latest/download/ichigo-installer.ps1 | iex"
```

**Via Cargo:**

```sh
cargo install ichigo
```

## Config files

Configs live in one of two places:

- `.ichigo/` — project-local (checked into git alongside your code)
- `~/.config/ichigo/` — global (available everywhere)

When a name exists in both, the local one wins.

## Commands

```text
ichigo new <name>              Create a new request config
ichigo run <name>              Execute a request
ichigo list                    List all configs
ichigo show <name>             Print a config file
ichigo delete <name>           Delete a config
ichigo test <name>             Run a load test
ichigo copy <name> <new-name>  Duplicate a config under a new name
```

### `new`

```sh
ichigo new config-server-prod --method GET --url https://api.example.com/health
ichigo new config-server-dev  --method GET --url https://localhost:8080/health
ichigo new create-user    --method POST --url https://api.example.com/users
```

Flags:

- `-m, --method` — HTTP method (default: `GET`)
- `-u, --url` — target URL
- `-g, --global` — save to `~/.config/ichigo/` instead of `.ichigo/`
- `--from-curl` — build the request from a cURL command read from stdin

#### Creating a request from a cURL command

Anything that hands you a cURL command — a browser's "Copy as cURL", an API doc,
a teammate's bug report — can become a config directly:

```sh
pbpaste | ichigo new api/login --from-curl
```

```sh
ichigo new api/login --from-curl <<'EOF'
curl -X POST 'https://api.example.com/login' \
  -H 'Content-Type: application/json' \
  --data-raw '{"user":"ada"}'
EOF
```

The command is read from stdin rather than taken as an argument, so you never
have to re-escape its quotes. `--from-curl` cannot be combined with `--method`
or `--url`, since the command already supplies both.

What the parser does with each flag:

| | |
| --- | --- |
| **Mapped** | `--url`, `-X/--request`, `-H/--header`, `-d/--data/--data-raw/--data-ascii/--data-binary`, `--data-urlencode`, `--json`, `-G/--get`, `-I/--head`, `-u/--user`, `-b/--cookie`, `-A/--user-agent`, `-e/--referer` |
| **Ignored** | `-s`, `-S`, `-v`, `-i`, `-k`, `-L`, `-f`, `-o`, `-w`, `--compressed`, `--max-time`, `--connect-timeout`, `--retry`, `--limit-rate` — these describe how curl behaves, not what it sends |
| **Refused** | `-F/--form` (multipart), `@file` data references, and any flag not listed above |

An unrecognized flag is an error naming the flag, not a silent skip: a config
that quietly drops half of what you pasted is worse than one that refuses to be
created. If ichigo refuses a flag you need, the command is unchanged in your
clipboard.

Some details worth knowing:

- The method follows curl's own rules — an explicit `-X` wins, `-I` means HEAD,
  a body means POST, everything else is GET.
- A query string on the URL is split into the `query` map. If a key repeats
  (`?tag=a&tag=b`), the query stays on the URL instead, since the map holds one
  value per key.
- A `Content-Type` header becomes the body's `content_type`. A body with no
  content type gets curl's default, `application/x-www-form-urlencoded`.
- `--compressed` is dropped rather than stored as an `Accept-Encoding` header —
  ichigo's HTTP client negotiates encoding itself, so storing the header would
  make the config claim something it does not do.
- `-u user:pass` becomes the `Authorization: Basic …` header curl would send.

> **An imported config holds the literal values from the command**, including
> whatever token or cookie was in it. Before committing `.ichigo/` to a repo,
> replace those with `{{VAR}}` placeholders and put the real values in a
> [profile](#profiles) or an environment variable.

### `run`

```sh
ichigo run config-server-prod
ichigo run create-user --var TOKEN=abc123 --var USER_ID=42
```

Variables replace `{{PLACEHOLDER}}` tokens anywhere in the config (url, headers, query, body). They are resolved in this order: `--var` flags → environment variables.

Flags:

- `-v, --var KEY=VALUE` — set a variable
- `--verbose` — print the full response (status, headers, body)
- `-p, --profile` — use a named profile (see [Profiles](#profiles))

### Chaining requests

A chain config runs multiple requests in sequence, passing extracted values from one step into the next. Only the final step's response is printed; use `--verbose` to see all steps.

```sh
ichigo run login-and-fetch --verbose
```

Chain configs are detected automatically by the presence of a `steps:` key — no separate command needed.

### `copy`

Duplicates an existing config under a new name. Useful for creating variants of a request (e.g. a prod and dev version of the same endpoint).

```sh
ichigo copy config-server-prod config-server-dev
ichigo copy config-server-prod config-server-staging --global
```

Flags:

- `-g, --global` — look for the source in `~/.config/ichigo/` and save the copy there too

Without `--global`, both the source and the copy are in `.ichigo/`. With `--global`, both are in `~/.config/ichigo/`. The copy command will error if the source does not exist or if a config with the new name already exists.

### `test`

Runs the request N times and reports status code counts and timing stats.

```sh
ichigo test config-server-prod --iter 100
ichigo test create-user --iter 50 --profile staging
```

Flags:

- `-v, --var KEY=VALUE` — set a variable
- `-i, --iter` — number of iterations
- `-p, --profile` — use a named profile

### `tui`

Opens an interactive terminal UI for browsing, running, and load-testing your configs.

```sh
ichigo tui
```

Keybindings:

| Key | Action |
| --- | ------ |
| `j` / `k` | Navigate list |
| `gg` / `G` | Jump to top / bottom |
| `r` / Enter | Run selected request |
| `t` | Load-test selected request |
| `y` | Copy selected request as a cURL command |
| `i` | Import a pasted cURL command as a new request |
| `n` / `e` / `c` | New / edit / clone a request |
| `R` | Refresh the config list from disk |
| `q` | Quit |
| Esc | Go back |

The TUI is meant to be left open. Every run and load test re-reads the config file from disk first, so edits you make in another terminal — rotating a token in a profile, adding a header, changing a URL — take effect on the next run with no restart. Press `R` when you have created, renamed, or deleted config *files* on disk and want them to show up in the list.

Pressing `y` turns the selected request into a cURL command and copies it to the clipboard. It runs the same profile picker and variable prompts as a normal run, so the command it produces carries the resolved values — the profile's real token, not `{{TOKEN}}`. The command is shown before it is copied, and `c` re-copies it. Chains cannot be copied this way: a chain feeds values extracted from one step into the next, which a single cURL command has no way to express.

> **The generated command contains your real credentials.** That is what makes it useful, and also what makes it unsafe to paste into a public issue, a pull request, or a shared log. Redact before sharing.

Copying uses whichever clipboard tool is available, tried in order: `pbcopy` (macOS), `wl-copy` (Wayland), `xclip`, then `xsel` (X11). If none is installed, the TUI says so rather than failing silently.

Pressing `i` does the reverse: it opens a pane you paste a cURL command into.
Press `Ctrl+s` to import it, and the new-request form opens with the method,
URL, headers, query, and body already filled in — you supply the name, and
nothing is written until you save. Enter inserts a newline rather than
importing, so a multi-line paste works even in terminals that do not support
bracketed paste. If the command uses something ichigo cannot store, the pane
reports which flag and keeps your text so you can edit it. The
[`--from-curl` section](#creating-a-request-from-a-curl-command) covers exactly
which flags are mapped, ignored, and refused.

If the selected request has profiles, a profile picker appears before the variable input screen. Use `j`/`k` to choose a profile (or `(no profile)` to skip), then press Enter. Any variables not covered by the profile can still be filled in manually.

## Config format

```yaml
name: create-user
method: POST
url: https://api.example.com/users
description: "Create a new user"

headers:
  Authorization: "Bearer {{TOKEN}}"
  Accept: application/json

query:
  version: "2"

body:
  content_type: application/json
  data: |
    {
      "name": "{{USER_NAME}}"
    }
```

### Chain config format

Use `steps:` instead of a top-level request. Each step is a full request config, and `extract:` maps variable names to JSON paths in the response. Extracted values are available as `{{VAR}}` in all subsequent steps.

```yaml
name: login-and-fetch
steps:
  - name: login
    method: POST
    url: https://api.example.com/auth/login
    body:
      content_type: application/json
      data: |
        {
          "username": "{{USERNAME}}",
          "password": "{{PASSWORD}}"
        }
    extract:
      TOKEN: $.token

  - name: fetch-profile
    method: GET
    url: https://api.example.com/me
    headers:
      Authorization: "Bearer {{TOKEN}}"
```

### Profiles

Profiles let you bundle a set of variable values under a name, so you can switch between environments (e.g. dev vs staging vs prod) without retyping vars each time.

```yaml
name: create-user
method: POST
url: https://{{HOST}}/users
headers:
  Authorization: "Bearer {{TOKEN}}"
body:
  content_type: application/json
  data: |
    {
      "name": "{{USER_NAME}}"
    }

profiles:
  - name: dev
    params:
      HOST: localhost:8080
      TOKEN: dev-token-123

  - name: staging
    params:
      HOST: staging.example.com
      TOKEN: stg-token-456
```

Run with a profile from the CLI:

```sh
ichigo run create-user --profile dev
ichigo test create-user --iter 50 --profile staging
```

Any `{{PLACEHOLDER}}` not covered by the chosen profile is still resolved from `--var` flags or environment variables as normal. In the TUI, a profile picker appears automatically when profiles are present — pick one (or skip) and fill in any remaining variables interactively.

Profile values are re-read from the file every time you run, so a token you rotate in the YAML is picked up on the next run of a long-lived TUI session. Values resolved from **environment variables** are not: a process cannot see exports its parent shell makes after launch, so an env-sourced token stays stale until you restart ichigo. Put values that rotate in a profile.

## Shell completions

ichigo can generate a completion script so that pressing `Tab` autocompletes config names:

```sh
ichigo run config<TAB>
# → config-server-dev  config-server-prod
```

The completions resolve config names live from `.ichigo/` and `~/.config/ichigo/`, so new configs appear automatically without any extra setup.

### Zsh

Add this line to your `~/.zshrc`:

```zsh
eval "$(ichigo completions zsh)"
```

> **Powerlevel10k users:** place this line *after* the instant prompt block at the top of your `.zshrc`.

### Bash

Add this line to your `~/.bashrc`:

```sh
eval "$(ichigo completions bash)"
```

### Fish

Add this line to `~/.config/fish/config.fish`:

```fish
ichigo completions fish | source
```

After adding the line, open a new terminal (or `source` the file) and tab completion will be active.
