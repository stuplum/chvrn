# Chvrn CLI contract

The `chvrn-cli` package builds the `chvrn` executable. [Black-box consumer tests](../../crates/chvrn-cli/tests/cli.rs) exercise its headless interface; the [user guide](../usage.md) describes interactive workflows and controls. The [original test-review package](../review/README.md) is historical, not current implementation status.

## Routing and terminal selection

The commands are `diff`, `merge`, `review`, `difftool` and `mergetool`. No subcommand selects repository review against the index. The TUI opens only with terminal stdin and stdout, `--format auto` and no `--non-interactive`. Explicit `--format text` or `--format json` never launches it. Headless `auto` uses JSON.

Operational/usage failures exit `2`. Quitting an interactive session exits `1`, never approval. Successful explicit interactive submission exits `0`. Repository submission can advance through multiple files before the process completes.

## File comparison

`chvrn diff LEFT RIGHT --format json` reads explicit paths and never modifies either input. Missing input is an error, not an empty file. Text JSON contains `equal: bool` and `hunks: [{left: {start, end}, right: {start, end}}]`, using zero-based half-open line ranges. Binary/invalid UTF-8 content is identified rather than opened for text editing. Equal content exits `0`; differences exit `1`. JSON has no terminal control sequences outside JSON strings.

Interactive file comparison permits editing both buffers. `s` explicitly saves changed files; guarded replacement is atomic per file, not across both inputs. Quit discards no dirty text without confirmation and never implies saving.

## Merge

`chvrn merge --base BASE --ours OURS --theirs THEIRS --output RESULT --non-interactive` combines conflict-free changes into RESULT while preserving the three inputs. Success exits `0`. Unresolved conflicts exit `1` without creating or replacing RESULT. Existing-output and concurrent-write guards also apply to successful writes.

Interactive merging shows ours/result/theirs with read-only source panes. Conflicts require source selection or manual acceptance. Resolving the final conflict opens confirmation; `s` can request confirmation manually. Only explicit `y` confirmation submits a resolved result for a guarded write. `n` or Escape returns to review.

`merge --jev` and `mergetool --jev` enable on-demand suggestions only in a TTY with `--format auto` and without `--non-interactive`. Other output modes fail with exit `2` before writing. Only opted-in runs read `TYPESAFE_API_KEY`; a missing or invalid key is an error, while ordinary merging requires none.

The host routes `J` to a single-flight background client. Responses enter the TUI's snapshot-bound advice lifecycle and cannot mutate or submit a result themselves. A cancelled request retains its network slot until completion; dropping the host does not join the network thread. Entering an LSP definition view cancels advice before parking the original session. Help, suggestion review in the footer and write/discard dialogs own input ahead of host shortcuts; an external-refresh conflict is not such a modal, so host-owned `R` recovery remains reachable.

## Repository review

`--base index` compares worktree content with the inspected index. A revision base such as `HEAD` compares the worktree with its resolved tree. Positional review paths are repository-relative. The review retains index/worktree/reference state for the operations that validate it.

Headless review emits `{base, patch_preview, files}` or a text summary, without staging, rejecting, importing a patch or submitting a report. Each file's `equal` combines content comparison under the selected whitespace policy with matching file existence and Git file mode. Empty-file additions/deletions and mode-only changes are unequal even without textual hunks. The exit code is `1` if any file is unequal, otherwise `0`. Patch previews compare the inspected worktree with the proposed bytes, existence and mode; ordinary review compares the selected base with the worktree.

Interactive `S` stages an exact textual hunk from an index review. Interactive `x` restores a textual hunk from a revision review, or declines an active patch preview. Both Git mutations are distinct from `s` review submission; quitting does not undo completed mutations. No action implicitly commits.

`--patch FILE` previews candidate changes; interactive acceptance of every candidate file is required before application. `--export-patch PATH` needs revision-based interactive review and `P`. `--report PATH` writes after completed interactive submission. Either output option in headless review fails with exit code `2` and an actionable diagnostic before inspecting changed files or accessing output destinations. `--open-companion` retains its separate launcher behaviour and forwards these options to the interactive companion.

## Git tools

`difftool` accepts explicit paths or Git's `LOCAL`/`REMOTE` environment variables and reuses file comparison. `mergetool` accepts merge flags or Git's `BASE`/`LOCAL`/`REMOTE`/`MERGED` and reuses three-way merging. Missing required paths are errors. These entry points retain normal interactive/headless and write semantics.

## Optional integrations

Explicit `--herdr` modes require `HERDR_ENV=1`; gate mode also requires a TTY. Interactive Git review inside Herdr defaults to automatic mode, with `--agent` overriding the inherited `HERDR_PANE_ID`. Standalone file diff/merge do not construct the repository's Herdr adapter. No headless invocation accepts an agent review on the user's behalf.

LSP requires explicit configuration. The private socket is started only by interactive repository review. Protocol/lifecycle tests do not establish arbitrary server/platform compatibility; see the [integration guide](../integrations.md) and [integration contract](integrations.md).
