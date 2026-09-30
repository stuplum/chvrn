# Agent guide

## Project

Chvrn is a Rust 2024 Cargo workspace. Use stable Rust. The package is `chvrn-cli`; the executable is `chvrn`.

- `chvrn-core`: snapshots, diff, editing/undo and merge. [Contract](docs/contracts/core.md).
- `chvrn-tui`: review sessions, input and terminal rendering. [Contract](docs/contracts/tui.md).
- `chvrn-git`: repository inspection, patches and guarded index/worktree mutations. [Contract](docs/contracts/git.md).
- `chvrn-integrations`: language servers, Herdr and private Unix sockets. [Contract](docs/contracts/integrations.md).
- `chvrn-cli`: commands, terminal lifecycle and library composition. [Contract](docs/contracts/cli.md).

Core stays independent of Git, terminal processes and Herdr. The TUI returns outcomes and buffers; the CLI/host owns writes and integration effects.

## Checks

For code changes, run relevant tests and the workspace checks from the repository root:

```sh
cargo +stable fmt --all --check
cargo +stable test --workspace --locked
```

Build an executable when needed with `cargo +stable build --locked`. Exercise changed terminal behaviour using the [demo guide](docs/demo.md); rendered-cell tests alone do not verify a real terminal. See [development](docs/development.md) for toolchain setup and platform constraints.

## Safety boundaries

- Keep editing, review submission, Git staging, committing and agent permission approval separate. Quitting is not approval.
- Interactive merges write only after confirmation. Choosing a source or editing a conflict is not a write. Preserve the separate headless-merge semantics.
- Preserve inspected-content, reference and destination checks before writes. External changes must not silently replace dirty buffers or retain stale review decisions.
- Undo/redo restores merge decisions and source controls as well as text, including choices that change no bytes.
- Keep file snapshot identity distinct from terminal-output revisions and agent-session identity.

## Documentation

Keep the README a short landing page: proposition, image, features, installation and links. Put detailed instructions in the existing [user guide](docs/usage.md), [integration guide](docs/integrations.md) and [development guide](docs/development.md). Keep documentation in the repository; do not introduce a documentation site without a separate request.

Update the relevant guide and contract when behaviour changes. Distinguish library capabilities from exposed CLI/TUI features, and terminal-output renderings from native screenshots.
