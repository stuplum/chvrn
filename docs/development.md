# Development

[Chvrn](../README.md) is one Cargo workspace with five crates. This page covers source builds, checks and implementation references. For day-to-day commands, use the [user guide](usage.md).

## Build and install

Use stable Rust and Git. The workspace declares Rust `1.85` as its minimum and uses edition `2024`; the configured CI jobs use stable Rust, not a separate minimum-version matrix. The current integration crate depends on Unix APIs, so do not assume the standalone binary builds on Windows merely because a particular command does not use a socket.

Install directly from GitHub without maintaining a local checkout:

```sh
cargo install --locked --git https://github.com/stuplum/chvrn.git chvrn-cli
```

For development, clone the repository and build without installing:

```sh
git clone https://github.com/stuplum/chvrn.git
cd chvrn
cargo build --release --locked
"$PWD/target/release/chvrn" --help
```

Or install from that local checkout into Cargo's binary directory:

```sh
cargo install --locked --path crates/chvrn-cli
```

The package is named `chvrn-cli`; the executable is **`chvrn`**. The source-install route is documented rather than an unverified package-manager command or release-binary URL.

The release profile uses thin LTO and strips debug information. A local macOS Rust toolchain has reported a `rust-objcopy` missing-`libLLVM.dylib` stripping warning while still producing a working binary. That is a toolchain diagnostic, not a reason to suppress build failures or claim every installation is verified.

## Checks

```sh
rustup toolchain install stable --profile minimal --component rustfmt
cargo +stable fmt --all --check
cargo +stable test --workspace --locked
```

[CI](../.github/workflows/ci.yml) configures these checks on Linux and macOS for pushes and pull requests. Checkout is pinned to a commit, credentials are not persisted, and repository permissions are read-only. There are no release or deployment jobs. A workflow definition is not evidence that a particular hosted run or interactive platform has passed.

Git tests use temporary repositories. TUI tests exercise input and rendered cells, but cannot prove real terminal raw-mode restoration, mouse delivery or a language server's behaviour. Use the [demo guide](demo.md) for live file/merge/Git review checks; real integrations need their own configured processes and verified sessions.

## Workspace and contracts

| Crate | Responsibility | Contract |
| --- | --- | --- |
| `chvrn-core` | Text snapshots, line/intraline diff, editing/undo, merge and structural analysis | [Core](contracts/core.md) |
| `chvrn-tui` | Review sessions, keyboard/mouse input and terminal rendering | [TUI](contracts/tui.md) |
| `chvrn-git` | Repository inspection, patches and guarded index/worktree mutations | [Git](contracts/git.md) |
| `chvrn-integrations` | LSP processes, Herdr lifecycle/feedback and private Unix sockets | [Integrations](contracts/integrations.md) |
| `chvrn-cli` | Command-line entry points, terminal lifecycle and composition of the libraries | [CLI](contracts/cli.md) |

Core has no dependency on Git, terminal processes or Herdr. The TUI returns review outcomes and buffers; the CLI/host owns writes and integration effects. An internal API existing in a crate does not imply a corresponding user-facing command or control. Examples include structural change classifications and file-level binary/mode staging APIs.

The design separates editing, review submission, Git staging, committing and agent permission approval. It also separates file content identity from terminal-output revisions and agent-session identity. Those distinctions explain the detailed safety and integration contracts; users should not need them to start a comparison.

## Design history

The original design and later terminal-presentation plan informed the implementation, including the shift from visible alignment fillers to continuous source lines with connector gutters. Those planning files live in a locally ignored directory and are not published GitHub links. The current user guide and TUI contract describe the rendered interaction model; the older plans are not a claim that every proposed capability is exposed in the CLI.

The [test-first review package](review/README.md) is a historical checkpoint before production implementation, not current project status. It retains the original proposed interfaces, test-review rationale and [redacted model-routing record](review/model-routing.json). Four Jev-selected `openai-codex/gpt-6-sol` workers implemented library slices in separate Herdr panes; the coordinator composed the CLI and performed integrated verification. This implementation history is not a product dependency or a model-performance benchmark.

Optional Jev-assisted merge choices remain a [backlog item](../TODO.md), not a shipped capability. Current merge choices are made by the reviewer.

## Documentation and captures

- [User guide](usage.md): supported workflows, controls and current limitations.
- [Integrations](integrations.md): prerequisites and operational boundaries.
- [Demo guide](demo.md): disposable fixtures, expected states and visual capture instructions.

Keep claims tied to observable behaviour. Distinguish library APIs, CLI features, automated checks and real-terminal verification rather than presenting them as interchangeable evidence.
