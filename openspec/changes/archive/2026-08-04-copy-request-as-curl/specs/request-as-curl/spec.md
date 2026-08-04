## ADDED Requirements

### Requirement: Browse mode copies the selected request as a cURL command

Browse mode SHALL bind `y` to generating a cURL command for the selected request. The action SHALL use the same profile selection and variable prompting flow as running the request, and SHALL read the config from disk when the action starts rather than from the cached entry list.

#### Scenario: Profile values are substituted into the command

- **WHEN** the user presses `y` on a request with a `dev` profile that sets `TOKEN`, and selects that profile
- **THEN** the generated command SHALL contain the profile's `TOKEN` value, not the literal `{{TOKEN}}`

#### Scenario: Uncovered placeholders are prompted for

- **WHEN** the chosen profile does not supply every `{{VAR}}` the request uses
- **THEN** the variable input form SHALL appear, and confirming it SHALL produce the cURL command rather than sending the request

#### Scenario: A request with no profiles and no variables copies immediately

- **WHEN** the user presses `y` on a request that defines no profiles and contains no placeholders
- **THEN** the command SHALL be generated without an intervening profile or variable prompt

#### Scenario: Edits made on disk are reflected

- **WHEN** a request's URL is changed on disk while the TUI is open and the user then presses `y`
- **THEN** the generated command SHALL use the new URL

#### Scenario: A config that fails to load reports an error

- **WHEN** the user presses `y` on a request whose file has been deleted or edited into invalid YAML
- **THEN** an error SHALL be displayed and no command SHALL be generated

### Requirement: The generated command reproduces what ichigo would send

The command SHALL be equivalent to the request `send_request` would issue for the same config and resolved variables: same method, same URL including query parameters, same headers, same body. Variable substitution SHALL use the same resolution order as the request path — the supplied variables first, then environment variables.

#### Scenario: Method is included for non-GET requests

- **WHEN** the request method is `POST`
- **THEN** the command SHALL include `-X POST`

#### Scenario: GET omits the redundant method flag

- **WHEN** the request method is `GET`
- **THEN** the command SHALL NOT include `-X GET`

#### Scenario: Headers are emitted as -H flags

- **WHEN** the request defines an `Authorization` header
- **THEN** the command SHALL contain a `-H` flag carrying that header name and its interpolated value

#### Scenario: Header order is deterministic

- **WHEN** the same request is copied twice
- **THEN** both commands SHALL be byte-identical, with headers in a stable order

#### Scenario: Query parameters are folded into the URL

- **WHEN** the request defines query parameters
- **THEN** they SHALL appear in the command's URL as a percent-encoded query string, and SHALL NOT be emitted as separate flags

#### Scenario: Body is emitted with its content type

- **WHEN** the request has a body with `content_type: application/json`
- **THEN** the command SHALL carry the body data and a `Content-Type: application/json` header

#### Scenario: Multi-line bodies are preserved

- **WHEN** the request body spans multiple lines
- **THEN** the command SHALL preserve the newlines rather than collapsing them

#### Scenario: Environment fallback matches the request path

- **WHEN** a placeholder is not covered by the profile or the variable form but is set in the environment
- **THEN** the command SHALL contain the environment value

### Requirement: The command is safe to paste into a shell

Every interpolated value in the command SHALL be quoted so that a shell reproduces it literally, with no expansion or word splitting.

#### Scenario: Values containing quotes survive

- **WHEN** a header or body value contains a single quote
- **THEN** the generated command SHALL escape it such that the shell passes the original character through

#### Scenario: Values containing shell metacharacters are not expanded

- **WHEN** a token value contains `$`, backticks, or spaces
- **THEN** the command SHALL quote it so the shell does not expand or split it

#### Scenario: JSON bodies are quoted intact

- **WHEN** the body is a JSON object containing double quotes
- **THEN** the command SHALL carry the JSON unmodified

### Requirement: The command is displayed and copied to the clipboard

Generating a command SHALL display it and SHALL copy it to the system clipboard in one action. The display SHALL indicate whether the copy succeeded.

#### Scenario: Command is shown after generation

- **WHEN** a command is generated
- **THEN** it SHALL be displayed to the user

#### Scenario: Successful copy is indicated

- **WHEN** the command is generated and the clipboard write succeeds
- **THEN** the display SHALL indicate that it was copied

#### Scenario: Failed copy is indicated without losing the command

- **WHEN** the clipboard write fails
- **THEN** the display SHALL still show the command and SHALL indicate that copying failed

#### Scenario: The displayed command can be re-copied

- **WHEN** a generated command is on screen and the user presses the copy key
- **THEN** the command SHALL be copied again

#### Scenario: The display is not reported as an HTTP error

- **WHEN** a generated command is displayed
- **THEN** it SHALL NOT be labelled as an error or carry a misleading HTTP status

#### Scenario: Dismissing returns to Browse

- **WHEN** the user presses Esc on a displayed command
- **THEN** the TUI SHALL return to Browse mode

### Requirement: Chains are rejected

Chain configs SHALL NOT be copied as cURL, because a chain threads values extracted from one step into the next and a cURL command cannot express that.

#### Scenario: Pressing the key on a chain reports it is unsupported

- **WHEN** the user presses `y` on a chain config
- **THEN** a message SHALL state that copying chains as cURL is not supported, and no command SHALL be generated

#### Scenario: A config that became a chain on disk is rejected

- **WHEN** a config that was a request at startup has been replaced on disk with a chain, and the user presses `y`
- **THEN** it SHALL be rejected as a chain

### Requirement: The copy key is discoverable

The Browse mode key hints SHALL include the `y` binding.

#### Scenario: Footer lists the copy-as-cURL key

- **WHEN** the TUI is in Browse mode
- **THEN** the key hints SHALL show `y` labelled as copying cURL

### Requirement: Existing Browse keys are unaffected

Adding the cURL binding SHALL NOT change the behavior of any existing Browse key.

#### Scenario: c still duplicates a request

- **WHEN** the user presses `c` in Browse mode
- **THEN** the request-duplication form SHALL open, as before this change

#### Scenario: r still runs

- **WHEN** the user presses `r` in Browse mode
- **THEN** the request SHALL run, as before this change
