# chvrn CLI contract for test review

The coordinator owns `/Users/stuart.plumbley/Personal Workspace/chvrn/crates/chvrn-cli/tests/cli.rs`. These are black-box consumer tests for the eventual `chvrn` Cargo binary. No binary or implementation exists at the test-review checkpoint. Proposed dev dependencies: `tempfile` and `serde_json`.

## File comparison

`chvrn diff LEFT RIGHT --format json` reads explicit file paths, writes one JSON document to stdout and never modifies either input. Missing input is an error, not an empty file. JSON contains `equal: bool` and `hunks: [{left: {start, end}, right: {start, end}}]`, with zero-based half-open line ranges. Exit code 0 means equal, 1 means differences, and 2 means operational/usage failure. JSON output cannot contain terminal control sequences outside JSON strings.

The eventual interactive invocation selects the TUI only when the relevant terminal streams are TTYs. Explicit machine-readable output never launches it.

## Headless merge

`chvrn merge --base BASE --ours OURS --theirs THEIRS --output RESULT --non-interactive` combines non-conflicting changes into RESULT while preserving the three inputs. Success is exit 0. Unresolved conflicts are exit 1 and must not create or replace RESULT. Operational/usage errors are exit 2. Existing-output and concurrent-write protections from the design still apply to successful writes; specifying an output is not permission to overwrite a later concurrent edit.

## Review gate

`chvrn review --herdr gate --agent AGENT --non-interactive` cannot accept a review on the user's behalf. Outside herdr it exits 2 without modifying files or contacting a different session. Standalone diff and merge remain usable outside herdr. Companion and automatic modes, Git difftool/mergetool entry points, patch input and socket composition retain the requirements in the main design and require real-runtime verification after implementation.

These tests prove CLI-visible data, exit codes and side effects once the real binary exists. They do not substitute for the core/Git tests or interactive terminal smoke checks.
