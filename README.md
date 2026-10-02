# Chvrn

**Editable diffs, three-way merges and Git review in your terminal.**

[![Chvrn merging TypeScript files across Ours, Merged result and Theirs.](docs/assets/merge-terminal-capture.png)](docs/assets/merge-terminal-capture.png)

*Rendered terminal output. [Capture details.](docs/demo.md)*

- **Compare and edit.** Two editable panes, connected changes and syntax highlighting.
- **Resolve merges.** Choose either side, keep both or edit the result, with undo/redo.
- **Review Git changes.** Inspect your worktree, stage hunks and use Git's diff and merge tools.

## Install

Requires stable Rust and Git.

```sh
cargo install --locked --git https://github.com/stuplum/chvrn.git chvrn-cli
```

Run `chvrn` or `chvrn review` inside a Git repository to review changes since your branch diverged from `main`. Set `CHVRN_BASE_BRANCH` to use another target branch. Use `chvrn review --base index` for unstaged changes only. Press `?` for help.

Use `--theme darcula` or another [bundled or external theme](docs/usage.md#themes). Save your usual choice in Chvrn's configuration file; the original appearance remains the default.

## Documentation

[User guide](docs/usage.md) · [Integrations](docs/integrations.md) · [Development](docs/development.md)

## Status

Early development. Tested interactively on macOS arm64. [Current limitations.](docs/usage.md#safety-and-current-limitations)

## Licence

[GPLv3 only](LICENSE). Copyright (c) 2026 Stuart Plumbley.

Bundled Helix theme adaptations retain MPL-2.0 and their original-project notices. [Theme sources, licences and attribution](crates/chvrn-tui/themes/ATTRIBUTION.txt).
