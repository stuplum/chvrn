# How to capture the Chvrn demo

[Back to Chvrn](../README.md) · [User guide](usage.md)

The README reserves one prominent visual for a three-way merge. Its current SVG is explicitly a layout placeholder, not a screenshot. Replace it with a real terminal capture of the scene below. One image is enough for the landing page; an optional Git-review capture belongs in the user guide only if it adds useful context.

## Prerequisites

- Install `chvrn` using the [source instructions](../README.md#install), or put the built executable on `PATH`.
- Use a real terminal at **132 columns × 24 rows**, with truecolour enabled and a normal monospace font. Leave `NO_COLOR` unset for the hero image.
- Use a POSIX shell and Git for the optional repository scene. Keep the same shell for each setup and its launch command so the temporary-directory variables remain available.
- Crop to the terminal content. Exclude desktop chrome, unrelated tabs, credentials and personal paths. Capture at native resolution or 2× scale; do not shrink the text to fit a wider scene.

## Primary scene: a merge halfway through review

The inputs below contain two conflicts, with opposing one-line/two-line changes. They are deliberately short enough to show both conflicts, the connectors, syntax highlighting, gutter controls and the overview strips at once.

### 1. Create disposable inputs

Run this outside any repository you are actively reviewing. It creates a new directory and does not modify your project:

```sh
demo=$(mktemp -d "${TMPDIR:-/tmp}/chvrn-merge.XXXXXX")
cat > "$demo/base.ts" <<'EOF'
export const notifications = [
  "email",
];

export const delivery = {
  retries: 3,
  timeout: 5000,
};

export const reviewers = [
  "team",
];

export const audit = {
  enabled: true,
  level: "info",
};
EOF
cat > "$demo/ours.ts" <<'EOF'
export const notifications = [
  "owner",
  "archive",
];

export const delivery = {
  retries: 3,
  timeout: 5000,
};

export const reviewers = [
  "maintainer",
];

export const audit = {
  enabled: true,
  level: "info",
};
EOF
cat > "$demo/theirs.ts" <<'EOF'
export const notifications = [
  "team",
];

export const delivery = {
  retries: 3,
  timeout: 5000,
};

export const reviewers = [
  "security",
  "release",
];

export const audit = {
  enabled: true,
  level: "info",
};
EOF
printf 'Demo directory: %s\n' "$demo"
```

### 2. Open the merge

```sh
env -u NO_COLOR chvrn merge \
  --base "$demo/base.ts" \
  --ours "$demo/ours.ts" \
  --theirs "$demo/theirs.ts" \
  --output "$demo/result.ts"
```

No Git repository or agent is needed. The initial selection is the first conflict. The result initially shows ours in both unresolved regions; that does not mean the choices are accepted.

### 3. Capture the exact hero state

Press **`t` once**, choosing theirs for `notifications`, then stop.

The frame should contain:

- **Ours | Merged result | Theirs** across the screen, with TypeScript highlighting.
- The resolved `notifications` result containing only `"team"`.
- The two-line ours block containing `"owner"` and `"archive"`, connected to the shorter result, with an available **`↘` insert-below** control.
- The still-unresolved `reviewers` conflict: `"maintainer"` in ours/provisional result, versus `"security"` and `"release"` in theirs, with source-choice controls.
- The change-overview strips and the normal footer. There is no final-write prompt yet because one conflict remains.

Capture **before** inserting the remaining source or resolving `reviewers`. Do not capture a help overlay or a confirmation prompt as the primary image: both hide the interaction the image is meant to explain.

Save the final image as `docs/assets/merge-preview.png` within your Chvrn checkout. Replace the README image link with that asset, use the alt text below, and remove both the placeholder caption and the old SVG:

> Chvrn merging TypeScript files: a resolved notification list with an insert-below control, beside an unresolved reviewer list across Ours, Merged result and Theirs.

### Optional short recording instead of more screenshots

Use the same inputs and terminal size. A 15–20 second recording can show the interaction more clearly than a gallery:

1. Hold the initial frame for two seconds.
2. Press `t` for the first conflict; pause on the hero state above.
3. Click the remaining `↘` beside `notifications`. The result now contains `"team"`, `"owner"`, `"archive"`, in that order; the control disappears.
4. Press `u`, pause, then Ctrl-R. The inserted block and its available control should undo/redo together.
5. Click the theirs `«` control beside `reviewers`. The result becomes `"security"`, `"release"`; final merge confirmation opens.
6. Press `n` to return to review, then `s` to reopen confirmation. Pause so the explicit write decision is visible.
7. Press `y` to write and exit. Show the resulting file briefly, if useful:

   ```sh
   cat "$demo/result.ts"
   ```

Keep a static poster image in the README even if the recording is linked separately. Avoid an autoplaying loop that repeatedly flashes the full terminal. The demonstration is human-controlled, not an automatic or AI merge.

### Verification and exit

Before confirmed saving, `result.ts` must not exist in this fresh directory. Choosing both blocks or inserting a remaining source does not itself write a file. After the recording sequence, the output contains all three chosen notification entries and the two theirs reviewer entries; `delivery` and `audit` are unchanged. The three source files remain unchanged throughout.

For a screenshot-only session, press `q` after capture and `y` if asked to discard the review. That `y` answers the **discard prompt**, not merge confirmation. If a merge-write prompt is open, dismiss it with `n` before quitting. The temporary files can be kept for another capture; re-run setup for a fresh output destination.

## Optional scene: Git review with staged and unstaged changes

Do not add a second README image just to show another two-pane view. This scene is useful in the user guide or a longer demo because it shows which content is being reviewed against which base.

Create an isolated repository with a committed title and ending, a staged title change, an unstaged ending change and a separate untracked file:

```sh
repo_demo=$(mktemp -d "${TMPDIR:-/tmp}/chvrn-review.XXXXXX")
git -C "$repo_demo" init -q
printf 'Title\nkeep one\nkeep two\nkeep three\nkeep four\nkeep five\nkeep six\nClosing\n' > "$repo_demo/note.txt"
git -C "$repo_demo" add -- note.txt
git -C "$repo_demo" -c user.name='Chvrn demo' -c user.email=demo@example.invalid -c core.hooksPath=/dev/null commit -qm 'Initial fixture'
printf 'Staged title\nkeep one\nkeep two\nkeep three\nkeep four\nkeep five\nkeep six\nClosing\n' > "$repo_demo/note.txt"
git -C "$repo_demo" add -- note.txt
printf 'Staged title\nkeep one\nkeep two\nkeep three\nkeep four\nkeep five\nkeep six\nUnstaged closing\n' > "$repo_demo/note.txt"
printf 'Untracked note\n' > "$repo_demo/new.txt"
```

Open an index review of the tracked file:

```sh
(cd "$repo_demo" && env -u HERDR_ENV chvrn review --base index note.txt)
```

Capture the comparison **before pressing `S` or editing**. Both panes show `Staged title`; the displayed difference is `Closing` versus `Unstaged closing`. This demonstrates that the already-staged title is the review base, not an omitted change.

For a recording, press `S` to stage only the selected ending hunk, then `q` to leave. Check the actual index and worktree separately:

```sh
git -C "$repo_demo" diff --cached -- note.txt
git -C "$repo_demo" diff -- note.txt
```

The index now contains both title and ending changes; there are no unstaged changes to `note.txt`. Quitting did not undo the explicit staging action. `new.txt` remains untracked.

To show revision review instead, run:

```sh
(cd "$repo_demo" && env -u HERDR_ENV chvrn review --base HEAD)
```

Select `note.txt` with Ctrl-N/Ctrl-P, using the footer filename to identify it. `HEAD` review shows both title and ending changes against the original commit; the untracked file is another review entry. Do not imply there is a separate staged/unstaged dashboard or a GitHub review connection.

## Troubleshooting

- **JSON instead of the TUI:** launch in a real terminal without a pipe, output redirection, `--non-interactive` or `--format text/json`.
- **Only one pane:** widen the terminal. The capture recipe uses 132 columns so three panes have readable text.
- **No colours:** unset `NO_COLOR` and use a truecolour-capable terminal. No icon font is necessary.
- **No `↘` after the first choice:** start from fresh inputs, choose `t` on the first conflict once, and capture before accepting/inserting ours.
- **Write confirmation already showing:** both conflicts were resolved. Press `n`, then `u` to restore the last unresolved choice, or quit and reopen fresh inputs.
- **Unexpected agent errors during Git review:** use the shown `env -u HERDR_ENV` invocation to keep this capture standalone.
- **A stale-file warning:** do not edit the fixture from another process while reviewing. Quit and reopen the fixture rather than bypassing the write guard.
