# chvrn

Editable terminal diff, three-way merge and Git review, usable standalone or alongside a herdr agent. Review submission, file writes, staging and quitting are separate actions. Quitting never approves a review.

## Build and run

Requires Rust 1.85 or newer and Git. The local socket integration requires Unix. The exercised platform is macOS arm64.

From the repository root:

```sh
cargo build --release --locked
"$PWD/target/release/chvrn" --help
```

Optional installation into Cargo's binary directory:

```sh
cargo install --locked --path "$PWD/crates/chvrn-cli"
```

The following commands assume the installed `chvrn` is on `PATH`.

```sh
chvrn diff /path/to/before.rs /path/to/after.rs
chvrn merge --base /path/to/base.rs --ours /path/to/ours.rs --theirs /path/to/theirs.rs --output /path/to/result.rs
chvrn review --base HEAD
chvrn review --base index
chvrn review --base HEAD --report /tmp/chvrn-review.json
chvrn diff /path/to/before.rs /path/to/after.rs --format json
```

Run repository review commands from the repository being reviewed. With no subcommand, `chvrn` reviews worktree changes against the index. Positional review paths restrict the inspected files. `--base HEAD` compares against the resolved revision and checks that a named reference has not moved before mutation or feedback delivery.

A terminal on both stdin and stdout enables the TUI. `--non-interactive`, `--format text`, `--format json`, or redirected input/output use headless output without terminal control sequences. Headless review only inspects. Headless merge writes the output only when all conflicts merge automatically. Exit codes: `0` for equal headless diff/clean merge or explicit successful interactive submission, `1` for differences, unresolved merge or interactive quit, and `2` for errors.

## Terminal presentation

Two-way comparisons show borderless left/right editors. Three-way merges show **Ours | Merged result | Theirs**, keeping the common base internal and both source panes read-only.

Each pane displays continuous real lines rather than blank alignment rows. Shaded change regions connect across unequal heights through Unicode half-block gutters. Changed text has stronger background emphasis without losing syntax colours. Each pane has a change-overview strip, including offscreen changes.

Click `»` or `«` to copy only that source hunk across a two-way comparison, or choose that source for one unresolved merge conflict. Accepted sources have no gutter control. The remaining source offers one insert-below control: `↘` from ours or `↙` from theirs. Inserting that source consumes its control; choosing both leaves neither control. Insertion preserves manual edits and duplicate lines, supports undo/redo of both text and available controls, and leaves unrelated conflicts untouched. Merge choices remain undoable even when they leave the text unchanged, including accepting a deletion. Saving remains explicit.

Truecolour terminals give the intended palette. `NO_COLOR` disables colours. No Powerline/Nerd Font is required; connectors use ordinary Unicode block characters and arrows. Terminal cells approximate diagonal edges rather than reproducing a graphical editor's smooth curves.

The normal footer highlights shortcuts before a dimmed, right-aligned filename. Two-way comparisons show the focused file; merges show the output file. Filenames shorten before essential save, quit and help shortcuts are dropped. Merge-choice hints appear only for a selected unresolved conflict; insert mode shows editing controls instead. Warnings and host status messages take precedence over the normal footer.

The header emphasises `modified` in amber and `INSERT` in cyan and bold. State labels remain readable with `NO_COLOR`; colour is not the only indicator.

## Controls

Press `?` for editor help, including full source and output paths. Help wraps long paths; Up/Down and PageUp/PageDown scroll, and Home/End jump to the start/end. Escape closes help or leaves insert mode before navigation actions.

| Key | Action |
| --- | --- |
| Arrows or `h`, `j`, `k`, `l` | Move by displayed row/grapheme |
| Tab / Shift-Tab | Focus next/previous pane, wrapping at either end |
| `[` / `]` | Previous/next hunk |
| `a` | Copy selected hunk from focused source to the opposite pane in a two-way diff |
| `i`, Escape | Enter/leave insert mode |
| `u`, Ctrl-R | Undo/redo |
| Home, End, PageUp, PageDown | Navigate |
| Shift-Left / Shift-Right | Horizontal scroll |
| `w` | Cycle exact, ignore-edge, ignore-all and ignore-blank-lines matching |
| `s` | Save/submit explicitly |
| `q` | Quit without approval; dirty buffers require `y` to discard |
| `R` | Explicitly discard local edits and load a conflicting external refresh |
| `o`, `t`, `b` | Resolve selected merge conflict with ours, theirs or both |
| `r` | Accept a manually edited merge conflict region |

Mouse clicks focus/select real source lines. Gutter controls act on their indicated source and difference; right-clicking a two-way hunk gutter also copies it from that pane. The wheel scrolls through real lines. Narrow terminals show only the focused pane; resizing keeps its cursor visible.

Repository-only controls:

| Key | Action |
| --- | --- |
| Ctrl-N / Ctrl-P | Next/previous file |
| `S` | Stage selected exact hunk against the index |
| `x` | Reject selected hunk, or decline the current patch preview |
| `c` | Enter review comment; Enter ends comment entry |
| `v` | Open the next queued socket patch candidate |
| `P` | Export the inspected patch to `--export-patch` |
| `E` | Ask the selected herdr agent to explain the selected hunk |

Repository base panes are read-only. Dirty edits must be submitted or discarded before switching files or staging/rejecting hunks. Acceptance does not commit or implicitly stage. Rejected ranges retain their inspected snapshot identities in submitted reports.

## Patches and Git tools

```sh
chvrn review --base HEAD --patch /path/to/candidate.patch
chvrn review --base HEAD --export-patch /tmp/chvrn-export.patch
```

Imported patches remain previews until explicit submission. The `difftool` subcommand accepts two paths or Git's `LOCAL`/`REMOTE` environment variables. `mergetool` accepts explicit `--base`, `--ours`, `--theirs`, `--output` paths or Git's `BASE`/`LOCAL`/`REMOTE`/`MERGED` variables. Configure Git locally if wanted:

```sh
git config diff.tool chvrn
git config difftool.chvrn.cmd 'chvrn difftool "$LOCAL" "$REMOTE"'
git config merge.tool chvrn
git config mergetool.chvrn.cmd 'chvrn mergetool --base "$BASE" --ours "$LOCAL" --theirs "$REMOTE" --output "$MERGED"'
git config mergetool.chvrn.trustExitCode true
```

## Herdr

Standalone operation does not require herdr. Integrated commands require `HERDR_ENV=1` and an explicit agent name or pane ID.

```sh
chvrn review --base HEAD --herdr companion --agent chvrn-engine
chvrn review --base HEAD --herdr auto --agent chvrn-engine
chvrn review --base HEAD --herdr gate --agent chvrn-engine
chvrn review --base HEAD --herdr companion --agent chvrn-engine --open-companion
```

Companion mode stays passive. Auto mode offers a gate only after verified lifecycle transitions. `--open-companion` creates a split without taking focus. Explicit gate mode opens the current review directly.

Authoritative lifecycle and session identity are required for automatic transitions and feedback delivery. For omp, install herdr's official integration with `herdr integration install omp`, activate it in a new agent process, then inspect `herdr agent get` and `herdr agent explain --json`. Screen text or an unverified idle status is not authority. The installed integration was exercised against herdr 0.9.0.

An agent's permission/question prompt is independent of code approval. Submitted feedback waits for verified input readiness; chvrn never answers that prompt. The complete inspected Git state is revalidated before delivery. An ambiguous delivery failure is reported as uncertain rather than retried blindly. Resolve the agent's prompt separately while feedback is pending.

## Language server

```sh
chvrn diff /path/to/before.cpp /path/to/after.cpp --lsp /usr/bin/clangd --lsp-arg=--log=error
```

`K` requests hover, `D` diagnostics, `g` definition and `F` formatting for the focused document. Escape returns from the temporary definition view. Formatting is an undoable in-memory edit; `s` still controls saving. Same-document definitions use the inspected in-memory text, including unsaved text. Delayed responses must match the current buffer identity. Repeated diagnostics reuse only that snapshot's diagnostic batch.

No server is started without `--lsp`. Unsupported capabilities and server errors are visible. Reads have a 30-second timeout. Diagnostics require a matching document version. Real clangd formatting, hover, diagnostics and definitions were exercised; other server combinations are not runtime-verified.

## Local socket

```sh
chvrn review --base HEAD --socket /tmp/chvrn-private/review.sock
```

The socket parent must be owner-private. A missing parent is created with mode `0700`; the socket has mode `0600`. Messages use a four-byte big-endian byte length followed by UTF-8 JSON, with a 65,536-byte frame limit. Clients obtain current per-file IDs with `{"type":"inspect"}`, then send `patch_candidate` and query `review_status`. Socket proposals cannot bypass preview or explicit approval. Accepted/declined receipts survive snapshot refresh while the process runs, not process restart. Full schemas are in the integration contract.

## Safety and limits

- UTF-8 text is editable. Binary/invalid UTF-8 content is identified, not lossily decoded for editing.
- Original line endings and final-newline state are retained. Whitespace modes change matching, not stored bytes.
- Rust, TypeScript/TSX, JavaScript/JSX, Python and JSON have Tree-sitter syntax/structural analysis. Other UTF-8 files use textual diff. Structural classification works on top-level syntax units; it is not semantic refactoring.
- Stale buffer results, moved references, changed index/worktree snapshots, unsafe paths and symlink targets are rejected before the relevant operation.
- File replacement is atomic per file, not across multiple files. Partial filesystem failures are reported. There is no filesystem-wide lock against arbitrary external writers.
- Large local diff/syntax refreshes run on a coalescing worker. Hunk actions and submission wait for the current alignment. Startup and merge construction still perform work before the first frame.
- Windows, Linux runtime behaviour and language servers other than clangd have not been exercised here. Native non-UTF-8 path regression tests exclude Apple filesystems, which rejected the fixture itself.
- No claim of IntelliJ feature parity or general performance superiority.

## Development and contracts

```sh
cargo test --workspace --locked
cargo fmt --all --check
```

One workspace, five crates: the CLI composes core diff/edit/merge, Git transactions, the terminal UI and external integrations. Four Jev-selected `openai-codex/gpt-6-sol` workers implemented the library slices in separate herdr panes; the coordinator implemented the CLI and ran integrated verification.

Contracts: [core](docs/contracts/core.md), [Git](docs/contracts/git.md), [terminal UI](docs/contracts/tui.md), [integrations](docs/contracts/integrations.md). The approved [design](docs/superpowers/specs/2026-09-28-chvrn-design.md) records scope and ownership.
