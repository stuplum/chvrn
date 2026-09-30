# Chvrn

**Editable diffs, three-way merges and Git review in your terminal.**

[![Chvrn merging TypeScript files across Ours, Merged result and Theirs, with connected change regions.](docs/assets/merge-terminal-capture.png)](docs/assets/merge-terminal-capture.png)

*Rendered from captured Chvrn terminal output, not a native-window screenshot. [Reproduce the demo.](docs/demo.md)*

Chvrn keeps comparison and editing in the same view. Follow a change across connected panes, copy just the hunk you want, or edit the result directly. Review and resolve without switching between a diff viewer and an editor.

[Install](#install) · [Try it](#quick-start) · [User guide](docs/usage.md)

## Work in the comparison

- **Follow real source lines.** Panes keep their own continuous lines. Shaded connectors join changes of different heights; intraline highlights and change-overview strips help you keep your place.
- **Choose individual changes.** Copy a hunk in either direction in a two-way diff. In a merge, choose ours, theirs or both, or edit the result and accept that region. An unaccepted source can still be inserted below your chosen result.
- **Keep reviewing before you write.** Edits and merge choices support undo/redo. Resolving the last conflict opens confirmation, not an automatic save. Quitting is never approval.

| Workflow | What you can do |
| --- | --- |
| Two-way diff | Edit either file and copy selected hunks between them. |
| Three-way merge | Work in **Ours · Merged result · Theirs**, with read-only sources and an editable result. Independent changes merge automatically; unresolved conflicts block saving. |
| Local Git review | Compare the worktree with the index or a revision, stage or restore individual hunks in the appropriate review mode, and submit comments in a JSON report. No implicit staging or committing. |
| Language-aware editing | Syntax highlighting for Rust, TypeScript/TSX, JavaScript/JSX, Python and JSON. Optional LSP hover, diagnostics, definitions and formatting. Other UTF-8 files remain editable. |

## Install

Install directly from GitHub with **stable Rust and Git**. Cargo downloads and compiles the source; no manual clone is needed:

```sh
cargo install --locked --git https://github.com/stuplum/chvrn.git chvrn-cli
chvrn --help
```

Cargo installs the `chvrn` executable into its binary directory, normally `~/.cargo/bin`; that directory must be on `PATH`. A truecolour terminal gives the intended palette. No Nerd Font is required.

[Build without installing and run development checks.](docs/development.md)

## Quick start

Try an editable comparison using disposable files:

```sh
demo=$(mktemp -d)
printf 'Hello\nKeep this line\n' > "$demo/before.txt"
printf 'Welcome\nKeep this line\nOne more line\n' > "$demo/after.txt"
chvrn diff "$demo/before.txt" "$demo/after.txt"
```

Click a `»` or `«` gutter control to copy a hunk. Press `i` to edit, Escape to return to navigation, and `u` to undo. `s` saves; `q` quits without saving pending edits.

Review a repository from its working directory:

```sh
chvrn
chvrn review --base HEAD
```

The first compares the worktree with the index. The second compares it with `HEAD`. Use `S` to stage a hunk in an index review; use `x` to restore a hunk from the chosen revision in a revision review. **These actions write immediately**, separately from submitting the review.

Merge three files into a separate result:

```sh
chvrn merge --base /path/to/base.ts \
  --ours /path/to/ours.ts --theirs /path/to/theirs.ts \
  --output /path/to/result.ts
```

Choose conflicts with `o`, `t` or `b`. At the final confirmation, `y` writes the result; `n` or Escape returns to review. The [walk-through](docs/demo.md) provides ready-to-use inputs.

## Use with Git

Run these inside the repository where you want Chvrn as your diff and merge tool. They change repository-local Git configuration:

```sh
git config diff.tool chvrn
git config difftool.chvrn.cmd 'chvrn difftool "$LOCAL" "$REMOTE"'
git config merge.tool chvrn
git config mergetool.chvrn.cmd 'chvrn mergetool --base "$BASE" --ours "$LOCAL" --theirs "$REMOTE" --output "$MERGED"'
git config mergetool.chvrn.trustExitCode true
```

Then run `git difftool` for changes or `git mergetool` for merge conflicts. [Details and write behaviour.](docs/usage.md#git-difftool-and-mergetool)

## Essential controls

| Key | Action |
| --- | --- |
| `[` / `]`, Tab / Shift-Tab | Previous/next change; switch panes |
| `i`, Escape | Enter/leave insert mode |
| `u`, Ctrl-R | Undo/redo |
| `o` / `t` / `b`, `r` | Choose a merge source or both; accept a manually edited conflict |
| `s` | Save a file comparison, submit the current repository file, or open merge confirmation |
| `y`, `n` / Escape | Confirm the merge write, or return to review |
| `q`, `?` | Quit without approval; show help |

Keyboard and mouse are supported. Dirty buffers require confirmation before quitting. [Full controls, Git actions and safety guidance.](docs/usage.md)

## Optional integrations

Chvrn does not require an agent or Herdr. Inside Herdr it can accompany a selected agent, offer review after verified lifecycle transitions and return explicitly submitted feedback. An agent's permission prompt remains a separate decision.

A configured language server adds editor assistance; a private local socket lets tools propose patches for human review. [LSP, Herdr and socket setup.](docs/integrations.md)

## Project status

Early development. The exercised interactive platform is **macOS arm64**. CI is configured for Linux and macOS; that is not a claim of Linux terminal verification or Windows support. Installation is currently from source.

Text editing requires UTF-8. Binary content is identified rather than rewritten. See [current limitations](docs/usage.md#safety-and-current-limitations) before using scripted or integration workflows.

## Documentation

- [User guide](docs/usage.md): commands, complete controls, patches, reports and safety.
- [Integrations](docs/integrations.md): optional language servers, Herdr and local socket access.
- [Demo and capture guide](docs/demo.md): reproducible merge and Git-review scenes.
- [Development](docs/development.md): builds, checks, workspace structure and implementation contracts.

## Licence

Copyright (c) 2026 Stuart Plumbley.

Chvrn is licensed under the [GNU General Public License, version 3 only](LICENSE) (`GPL-3.0-only`).

Commercial use is permitted. If you distribute Chvrn or a modified version, you must comply with the GPLv3 source-code and notice requirements. Using Chvrn to review or edit your own code does not change that code's licence.
