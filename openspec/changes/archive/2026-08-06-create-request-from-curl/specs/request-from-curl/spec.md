## ADDED Requirements

### Requirement: A cURL command is parsed into a request config

The system SHALL parse a cURL command string into a request config, mapping the URL, `-X/--request`, `-H/--header`, and the `-d/--data/--data-raw/--data-ascii/--data-binary`, `--data-urlencode`, and `--json` body flags onto the config's `url`, `method`, `headers`, `query`, and `body` fields. Parsing SHALL be a pure function of the command text: it SHALL NOT read files, the environment, or the network.

The command SHALL be required to begin with the word `curl`, after any leading shell prompt marker (`$ ` or `% `) is stripped.

#### Scenario: A command with headers and a body becomes a config

- **WHEN** `curl -X POST 'https://api.example.com/users' -H 'Accept: application/json' -H 'Content-Type: application/json' --data-raw '{"name":"ada"}'` is parsed
- **THEN** the config SHALL have method `POST`, url `https://api.example.com/users`, an `Accept: application/json` header, and a body whose data is `{"name":"ada"}`

#### Scenario: The URL may be given positionally or with --url

- **WHEN** a command supplies its target either as a bare argument or via `--url 'https://api.example.com/x'`
- **THEN** both SHALL produce the same `url` field

#### Scenario: Repeated data flags concatenate

- **WHEN** a command contains `-d 'a=1' -d 'b=2'`
- **THEN** the body data SHALL be `a=1&b=2`, matching how curl combines them

#### Scenario: Input that is not a curl command is rejected

- **WHEN** text that does not begin with `curl` is parsed
- **THEN** parsing SHALL fail with an error, and no config SHALL be produced

### Requirement: Shell quoting is honored when tokenizing

Tokenizing SHALL apply POSIX shell quoting rules: single-quoted runs are literal, double-quoted runs unescape `\"`, `\\`, `` \` ``, and `\$`, `$'…'` runs unescape ANSI-C sequences (`\n`, `\t`, `\r`, `\\`, `\'`, `\xHH`), a backslash before a newline is a line continuation, and adjacent quoted and unquoted runs concatenate into a single token. An unterminated quote SHALL be an error.

#### Scenario: A multi-line command with continuations is parsed as one command

- **WHEN** a command spans several lines joined by trailing `\` characters
- **THEN** it SHALL parse as if written on one line

#### Scenario: An escaped single quote inside a value survives

- **WHEN** a header value is written as `'X-Note: it'\''s here'`
- **THEN** the parsed header value SHALL be `it's here`

#### Scenario: ANSI-C quoted bodies are decoded

- **WHEN** a body is written as `$'line1\nline2'`
- **THEN** the parsed body data SHALL contain a real newline between `line1` and `line2`

#### Scenario: An unterminated quote fails

- **WHEN** a command contains an opening quote with no closing quote
- **THEN** parsing SHALL fail with an error rather than returning a truncated value

### Requirement: The method is derived the way curl derives it

When no `-X/--request` flag is present, the method SHALL be derived as curl derives it: `HEAD` when `-I/--head` is given, otherwise `POST` when the command carries body data, otherwise `GET`. An explicit `-X` value SHALL take precedence and SHALL be uppercased.

#### Scenario: A body without -X implies POST

- **WHEN** `curl 'https://api.example.com/x' -d 'a=1'` is parsed
- **THEN** the config's method SHALL be `POST`

#### Scenario: A bare URL is a GET

- **WHEN** `curl 'https://api.example.com/x'` is parsed
- **THEN** the config's method SHALL be `GET`

#### Scenario: -I means HEAD

- **WHEN** a command contains `-I` and no `-X`
- **THEN** the config's method SHALL be `HEAD`

#### Scenario: An explicit method is uppercased

- **WHEN** a command contains `-X delete`
- **THEN** the config's method SHALL be `DELETE`

### Requirement: Content-Type is stored on the body, not in headers

When a parsed command has body data, a `Content-Type` header SHALL be removed from `headers` and stored as the body's `content_type`. When body data is present with no `Content-Type` header, the body's content type SHALL default to `application/x-www-form-urlencoded`, which is curl's own default. A `Content-Type` header on a command with no body data SHALL remain in `headers`, and no body SHALL be created.

This mirrors the rendering direction, which derives a `Content-Type` header from the body in addition to the config's own headers; storing it in both places would render the header twice.

#### Scenario: Content-Type moves onto the body

- **WHEN** a command with `-H 'Content-Type: application/json'` and `--data-raw '{}'` is parsed
- **THEN** the body's content type SHALL be `application/json` and `headers` SHALL NOT contain a `Content-Type` entry

#### Scenario: A body without a content type gets curl's default

- **WHEN** a command has `-d 'a=1'` and no `Content-Type` header
- **THEN** the body's content type SHALL be `application/x-www-form-urlencoded`

#### Scenario: A bodyless command keeps its Content-Type header

- **WHEN** a command has `-H 'Content-Type: application/json'` and no data flag
- **THEN** the config SHALL have no body, and the `Content-Type` header SHALL remain in `headers`

#### Scenario: --json sets the content type and Accept

- **WHEN** a command uses `--json '{"a":1}'`
- **THEN** the body content type SHALL be `application/json`, the body data SHALL be `{"a":1}`, and an `Accept: application/json` header SHALL be present

### Requirement: Query parameters are lifted out of the URL when they can be

A query string on the parsed URL SHALL be decoded into the config's `query` map and removed from `url`, provided every key in it is distinct and the URL parses. When a key repeats, or the URL does not parse (for example because it carries `{{VAR}}` placeholders), the query string SHALL be left on `url` verbatim and `query` SHALL be left empty.

#### Scenario: Distinct query keys move into the query map

- **WHEN** `curl 'https://api.example.com/x?page=2&limit=10'` is parsed
- **THEN** `url` SHALL be `https://api.example.com/x` and `query` SHALL contain `page=2` and `limit=10`

#### Scenario: Percent-encoded values are decoded

- **WHEN** a URL carries `?q=a+b%26c`
- **THEN** the `query` map SHALL hold the decoded value `a b&c`

#### Scenario: A repeated key keeps the query on the URL

- **WHEN** a URL carries `?tag=a&tag=b`
- **THEN** `url` SHALL retain the full query string and `query` SHALL be empty, so that no value is lost

#### Scenario: A URL with placeholders keeps its query

- **WHEN** the URL is `https://{{HOST}}/x?page=2`
- **THEN** `url` SHALL retain `?page=2` and the placeholder SHALL be preserved verbatim

### Requirement: Commands that cannot be represented are rejected by name

Parsing SHALL fail with an error naming the offending flag when the command uses a flag that changes what is sent but has no config equivalent — including `-F/--form`, an `@`-prefixed data value, and any flag not in the recognized set. Bundled short flags SHALL be split before this check so an unsupported flag inside a bundle is still caught. Parsing SHALL NOT drop such a flag silently.

#### Scenario: Multipart form uploads are refused

- **WHEN** a command contains `-F 'file=@photo.png'`
- **THEN** parsing SHALL fail with an error naming `-F`, and no config SHALL be produced

#### Scenario: File-backed data is refused

- **WHEN** a command contains `-d '@payload.json'`
- **THEN** parsing SHALL fail with an error, rather than storing the literal `@payload.json` as the body

#### Scenario: An unrecognized flag is refused

- **WHEN** a command contains a flag the parser does not recognize
- **THEN** parsing SHALL fail with an error naming that flag

#### Scenario: An unsupported flag inside a bundle is caught

- **WHEN** a command contains a bundled short flag group in which one letter is unsupported
- **THEN** parsing SHALL fail naming that letter

### Requirement: Flags that do not change the stored request are ignored

Flags describing client behavior rather than the request SHALL be accepted and ignored, including `-s/--silent`, `-v/--verbose`, `-i/--include`, `-o/--output`, `-k/--insecure`, `-L/--location`, and `--compressed`. `--compressed` SHALL NOT contribute an `Accept-Encoding` header, because the HTTP client negotiates encoding itself and storing the header would misstate what is sent.

#### Scenario: A command copied from devtools parses despite noise flags

- **WHEN** a command includes `--compressed` alongside its headers and body
- **THEN** parsing SHALL succeed, and the config SHALL contain no `Accept-Encoding` header

#### Scenario: Output and verbosity flags are dropped

- **WHEN** a command includes `-s`, `-v`, and `-o /dev/null`
- **THEN** parsing SHALL succeed and none of them SHALL appear in the config

### Requirement: Convenience header flags become headers

`-u/--user`, `-b/--cookie`, `-A/--user-agent`, and `-e/--referer` SHALL be translated into the headers curl would send: `Authorization: Basic <base64 of user:password>`, `Cookie`, `User-Agent`, and `Referer` respectively. A `-u` value with no `:` SHALL be an error, because curl resolves that form by prompting interactively and a parser cannot.

#### Scenario: Basic auth becomes an Authorization header

- **WHEN** a command contains `-u 'ada:hunter2'`
- **THEN** the config SHALL have an `Authorization` header whose value is `Basic YWRhOmh1bnRlcjI=`

#### Scenario: A password-less -u is refused

- **WHEN** a command contains `-u 'ada'` with no colon
- **THEN** parsing SHALL fail with an error explaining that the password must be supplied

#### Scenario: User agent and cookie flags become headers

- **WHEN** a command contains `-A 'ichigo/1.0'` and `-b 'session=abc'`
- **THEN** the config SHALL have `User-Agent: ichigo/1.0` and `Cookie: session=abc` headers

### Requirement: Generated commands parse back to the request they came from

A cURL command produced by the request-as-curl rendering SHALL parse back into a config equal to the original in method, url, headers, query, and body, for a request whose variables were all resolved at render time. Fields a cURL command cannot express — name, description, profiles, and extract — SHALL come back empty for the caller to supply.

#### Scenario: A rendered request round-trips

- **WHEN** a request with headers, query parameters, and a JSON body is rendered as a cURL command and that command is parsed
- **THEN** the resulting config SHALL match the original's method, url, headers, query, and body

#### Scenario: Non-representable fields come back empty

- **WHEN** a request carrying a description and profiles is rendered and parsed back
- **THEN** the resulting config SHALL have no description and no profiles, and its name SHALL be left for the caller to set

### Requirement: The CLI creates a request from a command on stdin

`ichigo new <name>` SHALL accept a `--from-curl` flag that reads a cURL command from standard input, parses it, and writes the resulting config under the given name, honoring `--global` and the existing name validation and collision rules. `--from-curl` SHALL conflict with `--url` and `--method`, which the command already supplies.

#### Scenario: A piped command is stored

- **WHEN** a cURL command is piped into `ichigo new api/login --from-curl`
- **THEN** a config named `api/login` SHALL be written containing the command's method, url, headers, and body

#### Scenario: Conflicting flags are rejected

- **WHEN** `--from-curl` is passed together with `--url` or `--method`
- **THEN** the command SHALL fail with an error rather than silently preferring one source

#### Scenario: A parse failure writes nothing

- **WHEN** the piped text fails to parse
- **THEN** the error SHALL be reported and no config file SHALL be created

### Requirement: The TUI imports a pasted command into the new-request form

Browse mode SHALL bind a key that opens an import pane accepting a pasted cURL command. Confirming the pane SHALL parse the buffer and, on success, open the existing new-request form prefilled with the parsed method, url, headers, query, and body, with the name field empty and focused. On failure the error SHALL be shown with the pasted text retained so it can be corrected or re-pasted.

Within the import pane, Enter SHALL insert a newline rather than confirm, since a paste in a terminal without bracketed paste support delivers newlines as Enter key events. Confirmation SHALL be a distinct key, and Esc SHALL cancel.

#### Scenario: A pasted command prefills the form

- **WHEN** the user pastes a cURL command into the import pane and confirms
- **THEN** the new-request form SHALL appear with method and url filled from the command and the cursor in the name field

#### Scenario: Nothing is written until the form is saved

- **WHEN** the user imports a command and then presses Esc from the new-request form
- **THEN** no config file SHALL be created

#### Scenario: A bad command is reported without losing the paste

- **WHEN** the pasted text fails to parse
- **THEN** the pane SHALL show the error and SHALL still contain the pasted text

#### Scenario: Enter does not submit the pane

- **WHEN** the user presses Enter while the import pane is focused
- **THEN** a newline SHALL be inserted into the buffer and the pane SHALL remain open

### Requirement: Imported headers, query, and body survive the form

The new-request form SHALL carry the headers, query, body, and extract of the request it is editing or importing, and SHALL write those values when the request is saved. Saving SHALL NOT re-read them from disk, so a request that has no file on disk yet keeps everything the parse produced.

#### Scenario: An imported body is written to the config

- **WHEN** a command with headers and a body is imported, named, and saved
- **THEN** the written YAML SHALL contain those headers and that body

#### Scenario: Editing an existing request preserves its unedited fields

- **WHEN** an existing request with headers and a body is opened in the form, its description changed, and saved
- **THEN** the written YAML SHALL still contain the original headers and body
