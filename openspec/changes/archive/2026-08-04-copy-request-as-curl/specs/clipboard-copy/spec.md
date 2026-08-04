## ADDED Requirements

### Requirement: Clipboard copying works across platforms

Copying text to the system clipboard SHALL attempt the available platform clipboard tools in order — `pbcopy`, then `wl-copy`, then `xclip`, then `xsel` — using the first one that is present and accepts the text.

#### Scenario: macOS uses pbcopy

- **WHEN** a copy is performed on a system where `pbcopy` is available
- **THEN** the text SHALL be placed on the clipboard via `pbcopy`

#### Scenario: Wayland falls through to wl-copy

- **WHEN** `pbcopy` is unavailable and `wl-copy` is present
- **THEN** the text SHALL be placed on the clipboard via `wl-copy`

#### Scenario: X11 falls through to xclip or xsel

- **WHEN** neither `pbcopy` nor `wl-copy` is available and `xclip` or `xsel` is present
- **THEN** the text SHALL be placed on the clipboard via that tool

#### Scenario: Text is copied verbatim

- **WHEN** text containing newlines, quotes, and non-ASCII characters is copied
- **THEN** the clipboard contents SHALL match the input exactly

### Requirement: Clipboard failure is reported, never silent

A copy that cannot be completed SHALL report failure to the caller so the UI can tell the user. Copy operations SHALL NOT fail silently.

#### Scenario: No clipboard tool available

- **WHEN** a copy is attempted and none of the supported clipboard tools are present
- **THEN** the operation SHALL report a failure identifying that no clipboard tool was found

#### Scenario: The UI surfaces the failure

- **WHEN** a copy fails
- **THEN** the user SHALL see an indication that copying did not succeed

#### Scenario: Success is distinguishable from failure

- **WHEN** a copy succeeds
- **THEN** the operation SHALL report success, distinctly from the failure case

### Requirement: Existing copy actions use the portable path

The response and test-result copy actions SHALL use the same clipboard mechanism and SHALL report failure, rather than discarding it as they did previously.

#### Scenario: Response copy works on Linux

- **WHEN** the user copies a response body on a Linux system with a supported clipboard tool installed
- **THEN** the response SHALL be placed on the clipboard

#### Scenario: Response copy reports failure

- **WHEN** the user copies a response body and no clipboard tool is available
- **THEN** the user SHALL see an indication that copying failed

#### Scenario: Test results copy through the same path

- **WHEN** the user copies test results
- **THEN** the same clipboard mechanism and failure reporting SHALL apply
