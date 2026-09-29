# chvrn test-first review

This is a specification and test-source review package, not a built application. The user selected a review checkpoint before production implementation. No production source, Cargo manifests, binary, commits or remote repository are included at this checkpoint.

## Start here

- Design: `/Users/stuart.plumbley/Personal Workspace/chvrn/docs/superpowers/specs/2026-09-28-chvrn-design.md`
- Implementation plan: `/Users/stuart.plumbley/Personal Workspace/chvrn/docs/superpowers/plans/2026-09-28-chvrn.md`
- Jev routing request and response, without credentials: `/Users/stuart.plumbley/Personal Workspace/chvrn/docs/review/model-routing.json`

## Test groups

### Core diff, edits and merge

Contract: `/Users/stuart.plumbley/Personal Workspace/chvrn/docs/contracts/core.md`

Tests:
- `/Users/stuart.plumbley/Personal Workspace/chvrn/crates/chvrn-core/tests/diff.rs`
- `/Users/stuart.plumbley/Personal Workspace/chvrn/crates/chvrn-core/tests/edit_merge.rs`
- `/Users/stuart.plumbley/Personal Workspace/chvrn/crates/chvrn-core/tests/structural.rs`

Review the expected Patience alignment, exact-byte preservation across whitespace policies, source-to-destination hunk direction, stale snapshot rejection, Unicode editing/undo, independent versus conflicting three-way edits, and structural move/reflow/rename classification. Initial structural grammars cover Rust, TypeScript/TSX, JavaScript/JSX, Python and JSON; unsupported text keeps ordinary diff/edit behaviour.

### Git and patch safety

Contract: `/Users/stuart.plumbley/Personal Workspace/chvrn/docs/contracts/git.md`

Tests:
- `/Users/stuart.plumbley/Personal Workspace/chvrn/crates/chvrn-git/tests/review_mutations.rs`
- `/Users/stuart.plumbley/Personal Workspace/chvrn/crates/chvrn-git/tests/patches.rs`
- `/Users/stuart.plumbley/Personal Workspace/chvrn/crates/chvrn-git/tests/discovery_and_safety.rs`
- Fixture support: `/Users/stuart.plumbley/Personal Workspace/chvrn/crates/chvrn-git/tests/support/mod.rs`

These tests use real temporary Git repositories, not mocked Git replies. Review selected-hunk staging with existing staged changes, non-HEAD rejection, stale index/worktree rejection, executable modes, renamed/deleted/untracked/native paths, unsafe path refusal and Git patch apply/reverse interoperability. No assertion pins incidental patch header formatting.

### Editable TUI

Contract: `/Users/stuart.plumbley/Personal Workspace/chvrn/docs/contracts/tui.md`

Tests:
- `/Users/stuart.plumbley/Personal Workspace/chvrn/crates/chvrn-tui/tests/review_input.rs`
- `/Users/stuart.plumbley/Personal Workspace/chvrn/crates/chvrn-tui/tests/review_render.rs`

Review filler-row editing, focused-source hunk actions, Unicode grapheme deletion, undo/redo, merge choices, explicit submit versus dirty quit, stale background results and dirty-buffer refresh conflicts. Render tests inspect Ratatui TestBackend cells and compare intraline emphasis within each pane. They do not claim real terminal restoration, mouse or scrolling verification.

### Herdr and external protocols

Contract: `/Users/stuart.plumbley/Personal Workspace/chvrn/docs/contracts/integrations.md`

Tests:
- `/Users/stuart.plumbley/Personal Workspace/chvrn/crates/chvrn-integrations/tests/herdr_lifecycle.rs`
- `/Users/stuart.plumbley/Personal Workspace/chvrn/crates/chvrn-integrations/tests/socket_protocol.rs`
- `/Users/stuart.plumbley/Personal Workspace/chvrn/crates/chvrn-integrations/tests/lsp_protocol.rs`

Review verified lifecycle transitions, blocked feedback queuing, session/content invalidation, bounded Unix socket framing, path/snapshot checks, LSP UTF-16 positions and versioned formatting/diagnostics. Protocol peers exercise the proposed real adapter; they are not an assertion that a real language server or herdr workflow already works.

### CLI consumer tests

Contract: `/Users/stuart.plumbley/Personal Workspace/chvrn/docs/contracts/cli.md`

Tests: `/Users/stuart.plumbley/Personal Workspace/chvrn/crates/chvrn-cli/tests/cli.rs`

The coordinator added black-box tests for headless JSON comparison and exit codes, missing-file errors, exact-byte non-conflicting merge output, preserving an existing result on unresolved conflicts, and refusing an explicit herdr gate outside herdr. These call the eventual real binary, not a command mock.

## Decisions worth reviewing

1. One repository with five Cargo crates, rather than separately versioning the diff tool and its herdr adapter.
2. Hunk direction always means source to destination. A left-to-right action changes the right buffer.
3. New disk snapshots never replace dirty editor buffers. A refresh conflict blocks submission until explicitly reconciled or discarded/reloaded.
4. Review approval is not staging, committing or approving an agent's permission prompt. Quitting is not approval.
5. Terminal output revision, pane/PTY identity, agent-session identity and file snapshot identity are distinct. A model/harness restart can preserve the same PTY.
6. Current herdr 0.9 reports these omp agents as idle even during visible work. Automatic gate/feedback remain disabled for unverified reporting. Verifying the real lifecycle integration is required during implementation, not omitted from scope.
7. Jev chose `gpt-6-sol` with high reasoning for all four workers. The user selected omp as their harness. This is a routing recommendation, not a benchmark result.

## Verification performed

- All 13 Rust test/support files parsed successfully with the installed `rustfmt`, using `--edition 2024 --emit stdout --config skip_children=true`. Files were not formatted or modified. This was a syntax check, not typechecking or test execution.
- The handwritten CRLF/no-final-newline patch fixture passed actual `git apply --check`, apply and reverse-apply in an isolated temporary repository. Expected file bytes matched after each operation; the temporary repository was removed.
- Production interfaces do not exist yet. No typecheck, Cargo test, application smoke, real language-server test or completed herdr review workflow is claimed.

## What this checkpoint does not prove

Test source can be parsed and reviewed before implementation, but it cannot establish type correctness, passing behaviour or a meaningful red/green cycle against missing production APIs. No passing product tests are claimed. After approval, the implementation plan requires focused executable tests, real Git/CLI scenarios, actual terminal/herdr interaction and a real language-server smoke run.

The four agent panes remain available:

| Owner | Pane | Harness/model |
|---|---|---|
| `chvrn-engine` | `w9:p2` | omp, `openai-codex/gpt-6-sol`, high |
| `chvrn-tui` | `w9:p3` | omp, `openai-codex/gpt-6-sol`, high |
| `chvrn-git` | `w9:p4` | omp, `openai-codex/gpt-6-sol`, high |
| `chvrn-integrations` | `w9:p5` | omp, `openai-codex/gpt-6-sol`, high |
