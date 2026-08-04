# config-reload Specification

## Purpose
TBD - created by archiving change reload-config-on-run. Update Purpose after archive.
## Requirements
### Requirement: Request actions resolve variables from the current file on disk

When the user starts a run or a load test from Browse mode, the TUI SHALL parse the selected config from disk at that moment and derive the offered profiles and the `{{VAR}}` placeholder list from that parse. The TUI SHALL NOT use the cached entry loaded at startup to build profile choices or variable prompts.

#### Scenario: Profile params edited on disk are used on the next run

- **WHEN** a config's profile `params` are edited in the YAML file while the TUI is open, and the user then selects that config and presses Enter
- **THEN** the profile list SHALL show the edited profile and selecting it SHALL inject the edited param values into the request

#### Scenario: A newly added placeholder is prompted for

- **WHEN** a `{{TOKEN}}` placeholder is added to a config's headers on disk while the TUI is open, and the user then runs that config
- **THEN** the variable input form SHALL include a `TOKEN` field, and the sent request SHALL NOT contain a literal `{{TOKEN}}`

#### Scenario: A removed profile is no longer offered

- **WHEN** a profile is deleted from a config on disk while the TUI is open, and the user then runs that config
- **THEN** the profile selection list SHALL NOT include the deleted profile

#### Scenario: Load test uses fresh profiles

- **WHEN** the user presses `t` on a config whose profiles were edited on disk since the TUI opened
- **THEN** the profile selection shown before the test SHALL reflect the on-disk profiles

#### Scenario: Chain profiles and variables are reloaded

- **WHEN** the user runs a chain config whose profiles or step placeholders were edited on disk since the TUI opened
- **THEN** the profile choices and variable prompts SHALL reflect the edited file

### Requirement: The cached entry is updated after a reload

After an action re-reads a config from disk, the TUI SHALL replace that config's cached entry so subsequent rendering reflects the reloaded contents.

#### Scenario: Detail pane matches what was sent

- **WHEN** a config's URL is changed on disk, the user runs it, and then returns to Browse mode
- **THEN** the detail pane SHALL show the new URL

#### Scenario: Reload does not disturb list position

- **WHEN** a config is reloaded as part of running it
- **THEN** the cursor position, tree structure, and collapsed-folder state SHALL be unchanged on return to Browse mode

### Requirement: A config that fails to load aborts the action with a visible error

If the selected config cannot be read or parsed when an action starts, the TUI SHALL display the error and SHALL NOT send any request. The TUI SHALL NOT fall back to the cached copy of that config.

#### Scenario: Invalid YAML reports a parse error

- **WHEN** the user runs a config whose file has been edited into invalid YAML
- **THEN** the response pane SHALL display an error naming the failure, and no request SHALL be sent

#### Scenario: Deleted config reports a missing-file error

- **WHEN** the user runs a config whose file was deleted from disk since the TUI opened
- **THEN** the response pane SHALL display an error, and no request SHALL be sent

#### Scenario: The session survives the error

- **WHEN** the user dismisses a config load error
- **THEN** the TUI SHALL return to Browse mode with the session intact

#### Scenario: A malformed config does not prevent startup

- **WHEN** the TUI is opened and one config file in the config directories contains invalid YAML
- **THEN** the TUI SHALL still start and list the remaining configs

### Requirement: Browse mode supports manual refresh of the config list

Browse mode SHALL bind `R` to reloading the full config list from both the local and global config directories, so configs created, renamed, or deleted on disk while the TUI is open become visible. The `r` key SHALL continue to run the selected config.

#### Scenario: A newly created config appears

- **WHEN** a new YAML config is created in `.ichigo/` while the TUI is open and the user presses `R`
- **THEN** the new config SHALL appear in the list

#### Scenario: A deleted config disappears

- **WHEN** a config file is deleted on disk while the TUI is open and the user presses `R`
- **THEN** that config SHALL no longer appear in the list

#### Scenario: `r` still runs

- **WHEN** the user presses `r` on a selected config
- **THEN** the config SHALL run, exactly as before this change

#### Scenario: `R` typed into the filter is not a refresh

- **WHEN** the filter input is active and the user types `R`
- **THEN** the character SHALL be appended to the filter query and no refresh SHALL occur

### Requirement: Refresh preserves user interface state

A manual refresh SHALL preserve the collapsed-folder set, the active filter query, and the cursor's logical position. Because a refresh can change entry indices, the cursor SHALL be restored by config name rather than by index.

#### Scenario: Collapsed folders stay collapsed

- **WHEN** the user collapses folder `auth`, a new config is added on disk, and the user presses `R`
- **THEN** `auth` SHALL still be collapsed

#### Scenario: Cursor stays on the same config

- **WHEN** the cursor is on config `ping`, a config sorting before `ping` is added on disk, and the user presses `R`
- **THEN** the cursor SHALL still be on `ping`

#### Scenario: Cursor recovers when its config is gone

- **WHEN** the cursor is on a config that has been deleted on disk and the user presses `R`
- **THEN** the cursor SHALL move to a valid position in the refreshed list rather than becoming unselected or out of bounds

#### Scenario: Refresh under an active filter

- **WHEN** a filter query is active and the user presses `R`
- **THEN** the filter query SHALL remain applied and the cursor SHALL be restored within the filtered results

#### Scenario: Newly discovered folders start expanded

- **WHEN** a config in a folder not previously present is added on disk and the user presses `R`
- **THEN** the new folder SHALL appear expanded

### Requirement: The refresh key is discoverable

The Browse mode key hints SHALL include the `R` refresh binding.

#### Scenario: Footer lists the refresh key

- **WHEN** the TUI is in Browse mode
- **THEN** the key hints SHALL show `R` labeled as refresh or reload

