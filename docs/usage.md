# User guide

[Chvrn](../README.md) combines comparison and editing. This guide covers standalone files and local Git review. [Integrations](integrations.md) covers language servers, Herdr and socket clients.

## Commands and output modes

| Command | Purpose |
| --- | --- |
| `chvrn` | Review worktree changes against the index in the current repository. |
| `chvrn diff LEFT RIGHT` | Compare two explicit files. Both text buffers are editable. |
| `chvrn merge --base BASE --ours OURS --theirs THEIRS --output RESULT` | Combine three inputs into a separate merge result. |
| `chvrn review --base index [PATHS]...` | Review unstaged worktree differences and non-ignored untracked files. |
| `chvrn review --base REVISION [PATHS]...` | Compare the worktree with a revision such as `HEAD`. |
| `chvrn difftool [LEFT] [RIGHT]` | File comparison using explicit paths or Git's `LOCAL`/`REMOTE`. |
| `chvrn mergetool` | Merge using Git's `BASE`/`LOCAL`/`REMOTE`/`MERGED`, or explicit merge flags. |

Use `chvrn --help` or `chvrn COMMAND --help` for the complete option list. File arguments may be absolute paths. Positional **repository review** paths are repository-relative, even when invoked from a subdirectory; absolute, escaping and symlink paths are refused.

A TUI opens only when both stdin and stdout are terminals, `--format` is `auto` and `--non-interactive` is absent. `--format text`, `--format json`, redirected streams or `--non-interactive` select headless operation. Headless `auto` produces JSON, not the interactive display.

```sh
chvrn diff /path/to/before.rs /path/to/after.rs --format json
chvrn review --base HEAD --format text
```

Headless diff and repository review inspect without changing files. **Headless merge writes its output when it can merge without conflicts**; unresolved conflicts leave an existing output untouched. A single `-` input is supported for headless `diff` via stdin; both inputs cannot be stdin. A patch may also be read with `--patch -`, but redirected stdin means there is no interactive approval session.

| Exit | Meaning |
| --- | --- |
| `0` | Equal headless diff/review, a successfully written conflict-free headless merge, or successful explicit interactive submission. |
| `1` | Detected headless differences, unresolved merge conflicts, a declined patch or interactive quit. |
| `2` | Operational or usage error. |

Headless repository equality is based on compared content and deletion state. An added empty file or mode-only change can report equal and exit `0`; do not use that result as a complete Git cleanliness check.

## Two-way comparison

Try a comparison using disposable files:

```sh
demo=$(mktemp -d)
printf 'Hello\nKeep this line\n' > "$demo/before.txt"
printf 'Welcome\nKeep this line\nOne more line\n' > "$demo/after.txt"
chvrn diff "$demo/before.txt" "$demo/after.txt"
```

Both files are editable in `diff`. Click `»` or `«` beside a hunk to copy that source region into the other buffer, or focus the source pane and press `a`. This is a selected-hunk operation, not whole-file replacement. Edit either pane with `i`, then press Escape to return to navigation.

`s` writes changed files and exits. `q` does not save pending edits; dirty buffers require explicit discard confirmation. A multi-file save is not atomic across both files. Source files are checked again before saving, and a partial failure names files already written.

## Three-way merge

The visible panes are **Ours | Merged result | Theirs**. The base is retained internally; it is not a fourth pane. Source panes are read-only. Independent changes combine automatically. Unresolved regions provisionally show ours, but this is not acceptance and does not permit saving.

For the selected conflict:

| Key | Result |
| --- | --- |
| `o` | Choose ours. |
| `t` | Choose theirs. |
| `b` | Keep both complete regions, ours then theirs. |
| `r` | Accept the manually edited result region. Editing alone does not resolve it. |

Accepted sources lose their gutter controls. A remaining non-empty source offers one insert-below action: `↘` for ours or `↙` for theirs. It inserts the original source block below the chosen result, preserving manual edits and duplicate lines. It consumes that source's control without resolving unrelated conflicts. There is no insert-above action.

Undo/redo restores text, conflict decisions and available controls together. A choice is undoable even when it changes no text, including accepting a deletion.

Resolving the final conflict opens confirmation automatically. `s` opens it manually, including for a conflict-free merge:

- `y` writes the result and exits.
- `n` or Escape returns to review without writing. Dismiss confirmation before further editing, insertion or quitting.
- Other keys, mouse actions and paste cannot change the result while confirmation is open.
- Unresolved conflicts report their remaining count and block submission. Pending alignment and external changes also block unsafe submission.

The source files and output destination are checked again on confirmed submission. Merely opening a merge, choosing a source or dismissing confirmation does not write the output.

## Local Git review

Run these in the repository to inspect:

```sh
chvrn review --base index
chvrn review --base HEAD
chvrn review --base HEAD src/main.rs
chvrn review --base HEAD --report /tmp/chvrn-review.json
```

The base pane is read-only; the worktree pane is editable. Review proceeds file by file. `s` saves any edits to the current file, records its acceptance and advances to the next undecided file. Final submission writes a report when `--report` was supplied. A report contains reviewed files, accepted/rejected ranges and a comment; it is not a GitHub pull-request review. Put report and export destinations outside the reviewed worktree to avoid introducing new review targets.

`--base index` compares with the current index, including already staged content on a changed path. Staged-only changes are not shown when the worktree equals the index. `--base HEAD` compares the selected revision with the worktree, not a separate staged-versus-unstaged dashboard. Named revisions are resolved at inspection and checked before submission; an immutable commit ID avoids following a moving reference.

### Git actions are separate from submission

| Key | Action and required mode |
| --- | --- |
| Ctrl-N / Ctrl-P | Next/previous file. Submit or discard dirty edits before switching. |
| `S` | Stage the selected textual hunk, **index review only**. Writes the index immediately, preserving unrelated staged content and leaving worktree bytes unchanged. |
| `x` | Restore the selected worktree hunk from the review's revision, **revision review only**. Writes the worktree immediately, without changing the index. |
| `c`, Enter | Enter a review comment; finish comment entry. |
| `P` | Write the inspected revision-based patch to the supplied `--export-patch` destination. |
| `v` | Preview the next queued socket candidate. |
| `E` | Request an explanation from the selected Herdr agent, when configured and ready. |

**`S` and `x` are immediate Git/file operations, not pending editor changes. Quitting does not reverse them.** No action implicitly commits. Review submission does not implicitly stage. Recorded rejection ranges remain associated with what was inspected.

Git actions require a selected presentation hunk matching one exact Git hunk. If a whitespace filter combines or hides changes, return to exact matching with `w`. Dirty editor buffers and active patch previews block staging/rejection. Binary files and mode-only changes have no textual hunk, so there is no TUI per-hunk action for them, even though the Git library has file-level APIs.

### External changes

Ordinary file comparison and repository review watch for filesystem changes. Clean buffers can refresh; incoming changes never silently replace dirty edits. A conflicting refresh blocks submission. `R` explicitly discards local edits and reloads the incoming state. Review decisions are invalidated when the inspected state changes.

## Patches and reports

```sh
chvrn review --base HEAD --patch /path/to/candidate.patch
chvrn review --base HEAD --export-patch /tmp/chvrn-export.patch
```

`--patch` opens a preview, including files not already changed in the worktree. Accept every preview file with `s` to apply the candidate. `x` declines a preview; unlike `x` in ordinary revision review, it does not restore a worktree hunk. Headless patch review only prints the preview.

Patch export requires a revision-based review and `P`. Merely supplying `--export-patch` does not create a file. Likewise, `--report` writes only on completed interactive submission. Neither option creates its output in headless review.

Patch processing validates paths and preimages, preserves CRLF and missing final-newline markers, and does not stage or commit. Binary patches are not supported. Preflight validation is not a multi-file filesystem transaction; later write failures can leave earlier files changed and are reported.

## Git difftool and mergetool

Run these inside the repository where you want Chvrn as your diff and merge tool. They change repository-local Git configuration:

```sh
git config diff.tool chvrn
git config difftool.chvrn.cmd 'chvrn difftool "$LOCAL" "$REMOTE"'
git config merge.tool chvrn
git config mergetool.chvrn.cmd 'chvrn mergetool --base "$BASE" --ours "$LOCAL" --theirs "$REMOTE" --output "$MERGED"'
git config mergetool.chvrn.trustExitCode true
```

Then run:

```sh
git difftool
git mergetool
```

`difftool` reuses ordinary two-file comparison. Git may supply temporary comparison files, so edits apply to the supplied paths, not necessarily to your working copy. For worktree editing and staging, use `chvrn review` instead.

`mergetool` reads `BASE`, `LOCAL`, `REMOTE` and `MERGED` when explicit paths are absent. The merge sources remain unchanged and the confirmed result is written to `MERGED`. `mergetool.chvrn.trustExitCode true` lets Git distinguish successful submission from quitting or failure. Run interactively when human confirmation is required; the ordinary headless merge rules still apply when no TTY is available.

## Complete editor controls

Printable shortcuts below apply in navigation mode, not insert mode.

| Key | Action |
| --- | --- |
| Arrows or `h`, `j`, `k`, `l` | Move by displayed row/grapheme. |
| Tab / Shift-Tab | Focus next/previous pane, wrapping at either end. |
| `[` / `]` | Previous/next hunk. |
| `a` | Copy the selected two-way hunk from the focused source to the opposite buffer. |
| `i`, Escape | Enter insert mode; return to navigation. |
| Enter, Tab | Insert a line break or tab in insert mode. |
| Backspace / Delete | Remove an adjacent grapheme in insert mode. |
| Paste | Insert as one undoable action in insert mode. |
| `u`, Ctrl-R | Undo/redo edits and merge decisions. |
| Home / End, PageUp / PageDown | Navigate within lines and through the file. |
| Shift-Left / Shift-Right | Horizontal scroll. |
| `w` | Cycle exact, ignore-edge, ignore-all and ignore-blank-lines comparison. |
| `s` | Save/submit, or open merge confirmation. |
| `q` | Quit without approval; `y` explicitly discards dirty buffers when prompted. |
| `R` | Discard local edits and load a conflicting external refresh. |
| `o`, `t`, `b`, `r` | Merge conflict choices and manual acceptance, described above. |
| `?` | Open help, including full source/output paths. |

In help, Up/Down and PageUp/PageDown scroll; Home/End jump to its start/end. Escape closes help. Full paths wrap rather than disappearing offscreen. LSP adds `K`, `D`, `g` and `F` when configured; see [integration controls](integrations.md#language-server).

Mouse clicks focus/select source lines. Left-click gutter controls to act on their indicated source and difference. Right-clicking a two-way hunk gutter also copies from that source. The wheel scrolls through real lines. At narrow widths, only the focused pane is shown; Tab still switches panes, and resizing keeps the cursor visible.

## Terminal presentation

Continuous source lines are connected across unequal change heights with Unicode half-block gutters. Changed regions are shaded, with stronger intraline emphasis that retains syntax colours. Each pane's overview strip shows offscreen changes; a solid block in the same column marks its current viewport, brightening where it overlaps changes.

The normal footer puts shortcuts before a dimmed, right-aligned filename: the focused file for comparisons, the output for merges. Long names shorten before essential controls disappear. Merge-choice hints appear only for an unresolved selection. Insert mode advertises editing controls. Warnings, pending work and confirmation prompts replace the normal footer.

The header distinguishes `modified` and `INSERT` with labels as well as colour. Truecolour gives the intended palette; a non-empty `NO_COLOR` disables colours. No Powerline/Nerd Font is required. Unicode terminal cells approximate the diagonal connectors; they are not graphical curves.

## Safety and current limitations

- **Text fidelity:** UTF-8 text is editable; binary or invalid UTF-8 inputs are identified rather than lossily decoded. Original line endings and final-newline state are retained. Whitespace matching modes do not normalise saved bytes.
- **Write guards:** The relevant operation checks inspected content, reference and destination state. Unsafe paths and symlink targets are refused. These checks do not constitute a filesystem-wide lock against arbitrary concurrent writers.
- **Per-file replacement:** Replacement is atomic per file, not across a collection of files. Partial filesystem failures are reported. Do not assume a failed multi-file operation wrote nothing.
- **Large files:** Large local diff/syntax refreshes use a coalescing background worker. Hunk actions and submission wait for current alignment. Startup and initial merge construction still run before the first frame. No general speed advantage has been measured.
- **Language scope:** Tree-sitter highlighting covers `.rs`, `.ts`, `.tsx`, `.js`, `.jsx`, `.py` and `.json`. Other UTF-8 files use textual comparison. The core library also classifies top-level structural changes; these classifications are not displayed by the TUI and are not semantic refactoring or syntax-aware merge.
- **Git scope:** Review is worktree-versus-index or worktree-versus-revision, not a three-layer Git dashboard. Headless equality has the empty-file/mode-only caveat above. TUI staging/rejection requires textual hunks. Report/export flags require interactive actions.
- **Platforms:** Interactive checks have exercised macOS arm64. Linux/macOS CI configuration does not establish Linux terminal behaviour. Windows is not a supported claim; the current integration crate uses Unix APIs. Native non-UTF-8 path tests exclude Apple filesystems, which rejected the fixture itself.
- **Integrations:** Previous real-server checks exercised clangd and Herdr 0.9.0. Other language-server combinations are not runtime-verified. See the [integration guide](integrations.md) for language IDs, explicit setup and delivery limits.

For implementation-level guarantees and ownership, read the [core](contracts/core.md), [Git](contracts/git.md), [TUI](contracts/tui.md) and [CLI](contracts/cli.md) contracts. For reproducible interactive examples, use the [demo guide](demo.md).
