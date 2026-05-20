# ichigo

A CLI HTTP client. Store named request configs in your project or globally, then run them by name.

## Installation

```sh
cargo install --path .
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

### `run`

```sh
ichigo run config-server-prod
ichigo run create-user --var TOKEN=abc123 --var USER_ID=42
```

Variables replace `{{PLACEHOLDER}}` tokens anywhere in the config (url, headers, query, body). They are resolved in this order: `--var` flags → environment variables.

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
```

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
