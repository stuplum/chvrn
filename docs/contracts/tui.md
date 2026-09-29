# chvrn-tui review-session API

Status: approved implementation contract. Production code exists; executable verification and real-terminal smoke are owned by the coordinator.

The crate owns input dispatch, editable review buffers and Ratatui rendering. `chvrn-core` owns diff alignment, snapshot identity, edit history, merge resolution and Tree-sitter syntax spans; the TUI maps core Unicode-scalar positions to grapheme cursors and terminal display cells. The CLI owns terminal setup, lifecycle and integration with Git/herdr. Neither a submitted review nor a hunk application writes files, stages an index or sends herdr feedback.

## Public Rust API

```rust
use std::{ops::Range, path::Path};
use crossterm::event::{KeyEvent, MouseEvent};
use chvrn_core::{edit::CapturedText, TextSnapshot};
use ratatui::Frame;
use chvrn_tui::{Language, WhitespacePolicy};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Pane {
    Left,
    Right,
    Ours,
    Result,
    Theirs,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cursor {
    pub pane: Pane,
    pub aligned_row: usize,
    pub line: usize,
    pub grapheme: usize,
}

pub enum ReviewInput {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Paste(String),
    Resize { width: u16, height: u16 },
    DiffReady(DiffCompletion),
    ConfirmDiscard,
    DiscardAndReload,
}

#[derive(Debug, Eq, PartialEq)]
pub struct ReviewSubmission {
    pub left: String,
    pub right: String,
    pub result: Option<String>,
}

#[derive(Debug, Eq, PartialEq)]
pub enum ReviewOutcome {
    Continue,
    DiscardRequired,
    LocalDiffPending,
    RefreshConflict,
    UnresolvedConflicts(usize),
    Submitted(ReviewSubmission),
    Quit,
}

#[derive(Debug, Eq, PartialEq)]
pub enum ReviewEditError {
    ReadOnly,
    UnresolvedConflicts,
    InvalidText,
}

pub struct DiffRequest;
pub struct DiffCompletion;
pub struct ReviewSession;

impl ReviewSession {
    pub fn two_way(left: &str, right: &str) -> Self;
    pub fn three_way(base: &str, ours: &str, theirs: &str) -> Self;
    pub fn handle(&mut self, input: ReviewInput) -> ReviewOutcome;
    pub fn request_diff(&mut self, left: &str, right: &str) -> DiffRequest;
    pub fn request_diff_snapshots(&mut self, left: TextSnapshot, right: TextSnapshot) -> DiffRequest;
    pub fn render(&self, frame: &mut Frame<'_>);
    pub fn pane_text(&self, pane: Pane) -> String;
    pub fn pane_snapshot(&self, pane: Pane) -> TextSnapshot;
    pub fn pane_capture(&self, pane: Pane) -> CapturedText;
    pub fn pane_matches_snapshot(&self, pane: Pane, snapshot: &TextSnapshot) -> bool;
    pub fn focus(&self) -> Pane;
    pub fn cursor(&self) -> Cursor;
    pub fn is_editing(&self) -> bool;
    pub fn is_dirty(&self) -> bool;
    pub fn is_local_diff_pending(&self) -> bool;
    pub fn poll_background(&mut self);
    pub fn is_read_only(&self, pane: Pane) -> bool;
    pub fn accepted_generation(&self) -> u64;
    pub fn selected_hunk(&self) -> Option<usize>;
    pub fn selected_hunk_ranges(&self) -> Option<(Range<usize>, Range<usize>)>;
    pub fn hunk_count(&self) -> usize;
    pub fn unresolved_conflicts(&self) -> usize;
    pub fn whitespace_policy(&self) -> WhitespacePolicy;
    pub fn set_whitespace_policy(&mut self, policy: WhitespacePolicy);
    pub fn set_paths(&mut self, left: &Path, right: &Path);
    pub fn set_read_only(&mut self, pane: Pane, read_only: bool);
    pub fn set_message(&mut self, message: impl Into<String>);
    pub fn replace_pane_text(&mut self, pane: Pane, text: &str) -> Result<(), ReviewEditError>;
    pub fn go_to(&mut self, pane: Pane, line: usize, grapheme: usize);
}

impl DiffRequest {
    pub fn generation(&self) -> u64;
    pub fn compute(self) -> DiffCompletion;
}

impl DiffCompletion {
    pub fn generation(&self) -> u64;
}
```

`ReviewSubmission` returns the reviewed buffers. In two-way mode, `result` is `None`; in three-way mode, `result` is the resolved merge buffer. `pane_text` requires a pane belonging to the current mode; an unavailable pane is a programmer error, not an empty file. Both constructors accept UTF-8 content only; binary and invalid UTF-8 rejection belongs to the file-ingest boundary. `three_way` retains base internally but renders ours/result/theirs as Ours/Merged result/Theirs. The initial focus is left in two-way mode and result in three-way mode. Initially the first hunk is selected if one exists; hunk indices are zero-based. `Cursor::line` and `grapheme` are zero-based logical text positions; `aligned_row` indexes the internal shared alignment, not a screen row, and may identify a virtual filler after a hunk action.

`pane_snapshot` returns a cheap clone retaining the current core snapshot identity; `TextSnapshot::same_identity` must be checked before applying delayed LSP edits or patches. Every actual edit, undo and redo gets a new identity even if bytes return to an earlier value. `selected_hunk_ranges` returns current core diff's zero-based half-open left and right line ranges, or `None` outside two-way mode. Git operations must match all four range bounds against their own reviewed hunk identity and must not stage/reject while `is_dirty` is true. `is_dirty` is conservative: once a local mutation occurs it remains true even if undo restores equal text, until an explicit reload creates a clean review.

`pane_capture` obtains an immutable core buffer capture without flattening the rope; its `snapshot` preserves the identity of the current buffer at capture time. `pane_matches_snapshot` compares that identity to the pane's current buffer, so a delayed formatter or LSP result cannot overwrite an intervening edit. `is_read_only` reports current pane permissions; the merge source panes cannot be made editable.

`request_diff` is a two-way review API; a three-way merge needs base/ours/theirs and cannot be refreshed from only two endpoints. Calling it on a merge session is a caller error.

`request_diff_snapshots` accepts already owned core snapshots and retains their identities in the resulting request. Use it when the filesystem worker has already read snapshots; `request_diff` remains the UTF-8 string convenience entry point. Neither API computes the diff on the caller thread.

`request_diff` copies its two input snapshots into an owned request, assigns a monotonically increasing generation and returns promptly without computing the diff. The caller runs `DiffRequest::compute` off the input/render thread and dispatches its completion with `ReviewInput::DiffReady`. Only a completion for the latest requested generation may replace the displayed snapshots/alignment. An older completion cannot change text, hunk selection or the displayed diff, regardless of arrival order. Even the latest completion cannot overwrite dirty buffers, whether edits predate the request or occur before its completion: `handle` retains the edited buffers and pending newer snapshots, returns `RefreshConflict`, and blocks submission. `DiscardAndReload` explicitly abandons local edits and adopts the latest pending snapshots. If the user wants to retain edits, the host must reconcile them against those newer snapshots in a separate review/merge session, then submit that reconciled session against its inspected snapshot; it cannot make the conflicted session silently submit. The host must still perform its snapshot preflight before any write.

`DiffRequest::generation` and `DiffCompletion::generation` carry the same token. `ReviewSession::accepted_generation` starts at zero and advances only when that exact completion replaces displayed buffers, never for a stale completion or `RefreshConflict`. The host may publish new filesystem write guards only after observing the accepted generation equal the completion's generation. A whitespace-policy change during an in-flight request invalidates that request's presentation; the host requests a fresh diff. `DiffRequest::compute` also prepares registered syntax spans off the input/render thread.

Large local buffer mutations defer diff alignment and syntax generation to a coalescing worker. `is_local_diff_pending` remains true until `poll_background` accepts the latest generation; the host calls `poll_background` on each review tick before drawing. While pending, `selected_hunk_ranges` is `None`, hunk apply is disabled, and `s` returns `LocalDiffPending` without a submission. Older local completions cannot replace the latest edit or undo. External refresh generations remain independently guarded.

## Input and result semantics

`handle` processes key presses and repeats, not releases. In navigation mode: `j`/`k` or arrows move through the focused pane's real lines without stopping on alignment fillers, `h`/`l` move by grapheme, `]`/`[` select next/previous hunk without wrapping, Tab switches the focused pane, `a` copies only the selected hunk from the focused source pane to the other destination pane in two-way mode, `i` enters insert mode, `u` undoes the latest mutation across panes and Ctrl-R redoes it. Up/Down, Home/End and PageUp/PageDown move the viewport/cursor; Shift-Left/Right and horizontal mouse-wheel events scroll by display cells. `w` cycles independently selectable exact, ignore-edge, ignore-all and ignore-blank-lines presentation policies; matching never normalises bytes. `?` opens help. The CLI reserves navigation-mode Ctrl-N/P, S, x, K, g, F, E, c and P for file/Git/LSP/agent actions.

In insert mode printable characters insert at the cursor, Enter inserts a line break, Tab inserts a tab, Backspace/Delete remove one adjacent grapheme and Escape returns to navigation. `ReviewInput::Paste(String)` inserts its whole string as one undoable action only in insert mode. Cursor columns count extended grapheme clusters, not bytes or terminal cells. `go_to` focuses a pane and clamps its logical line/grapheme target before revealing the cursor. An insertion-side filler is not a text line: entering insert mode there maps to the following real line's start, or to EOF when no line follows. `replace_pane_text` is a read-only-checked full-buffer replacement in one core undo step; it refuses an unresolved merge result so formatting cannot silently resolve conflicts. The host must pin `pane_snapshot` identity before invoking delayed edits. Whitespace and final-newline bytes outside an edit remain untouched.

For a selected three-way conflict, `o`, `t` and `b` choose ours, theirs and both respectively. Merge pane headings, legends and help use ours/theirs consistently with the shortcuts and CLI argument names. Both concatenates the complete competing conflict regions in ours-then-theirs order, preserving their line endings. Core `Merge::preview` combines independent edits and shows ours provisionally at unresolved spans; it cannot be submitted while a conflict remains. Editing the result does not by itself resolve the conflict; `r` explicitly accepts the manually edited result region. Ours/theirs source panes remain read-only. Core `TextBuffer` supplies actual undo/redo, while the TUI retains conflict-coordinate/resolution metadata so undoing a choice restores unresolved state. A successful merge submit requires zero unresolved conflicts; otherwise `handle` returns `UnresolvedConflicts(count)` and does not emit a submission.

After a merge choice, resolved-region controls insert the complete original source conflict block before or after its tracked result range. Ours uses `↗` / `↘`; theirs uses `↖` / `↙`. Insertion is additive without deduplication, preserves source line endings and existing manual edits, and supports an empty chosen result. Tracked result ranges move with edits and participate in undo/redo alongside unresolved-conflict metadata. These actions neither resolve unrelated conflicts nor submit the result.

`s` explicitly submits a valid review, but returns `RefreshConflict` instead if an external refresh is pending against dirty buffers; unresolved merge conflicts likewise block merge submission. `q` on a clean session returns `Quit`; on a dirty session it returns `DiscardRequired` without discarding or submitting. Escape dismisses that confirmation and retains edits. Only `ConfirmDiscard` while confirmation is active returns `Quit` and drops the review without a submission. `Submitted` and `Quit` are distinct terminal outcomes. The review host must not infer approval from a quit.

`LocalDiffPending` is not approval or a terminal outcome; the review remains open for further editing, navigation or an explicit quit.

## Rendering contract

`render` draws into the supplied Ratatui `Frame` for the active size. At normal width it shows borderless two-way or Ours/Merged result/Theirs editors with continuous real lines in each pane, subdued line numbers, full-width change shading and per-pane change-overview strips. Cached projections map each pane back to the shared alignment. Paired change bands supply half-open source ranges and change kind; Unicode half-block gutters connect unequal-height bands, including insertion/deletion seams. Unresolved conflicts have distinct shading and `»` / `«` source-choice controls. Resolved conflicts have separate styling and additive insertion controls. Action projections retain original conflict identity independently of visual diff bands. Changed intraline graphemes receive stronger backgrounds while preserving syntax foregrounds. Rendering clips at display-cell boundaries, never in the middle of a wide or combining grapheme, and cannot modify source buffers. Narrow widths show only the focused pane. Tests assert content, source targeting and meaningful cell-style differences rather than copied screenshots or incidental spacing.

Connector rasterisation covers each column's horizontal extent rather than sampling a single point, so steep thin bands remain connected. Painting one half-cell preserves the other half's existing colour. Gutter controls are drawn after all band backgrounds so adjoining bands cannot cover an action.

During deferred alignment, the renderer shows bounded slices of current buffer lines with an updating status instead of showing stale hunk rows or stale syntax. Viewport grapheme-to-cell checkpoints bound clipping and cursor lookup for long lines; unchanged source buffers remain intact.

`set_paths` registers a supported language using core Tree-sitter highlighting for Rust, TypeScript/TSX, JavaScript/JSX, Python and JSON. Other UTF-8 files remain editable with a visible plain-text status. `set_message` places host file path/error/status text in the footer without interpreting it as a command. Left-clicking a rendered pane targets the displayed real source line. Gutter controls share render/hit-test geometry and overlap priority: a two-way chevron copies its source hunk to the opposite pane, a merge-source chevron resolves only its identified conflict, and a diagonal control inserts into its identified resolved region. Pending local matching suppresses actions. Right-clicking a two-way hunk gutter also applies from that pane. The wheel traverses real lines. Syntax spans, pane projections and change bands are cached on diff refresh, not rebuilt in `render`; large local updates prepare projections on the worker. Region and overview lookup use sorted bands, and connector-band drawing is bounded to the visible window.

`Resize` updates the session viewport and keeps the focused cursor visible vertically and horizontally using the renderer's actual pane geometry; callers resize the real Ratatui terminal/backend separately. `TestBackend` validates rendered cells, narrow fallback, syntax styles, mouse dispatch and input-driven view changes, but cannot validate Crossterm raw-mode, alternate-screen or extended-keyboard restoration. After CLI integration, a runtime smoke in a real terminal must exercise normal and error exits, resize, mouse input, focus switching, edit/undo, apply, conflict resolution, dirty quit and submit, then inspect terminal state and focus restoration. Truecolour is the intended palette; `NO_COLOR` disables colour output. No private-use font glyphs are required.

## Dependencies

The crate depends on `chvrn-core`, `ratatui = "0.29"`, `crossterm = "0.28"`, `unicode-segmentation` and `unicode-width`. Integration tests use these plus `std`, without additional test-only dependencies. Ratatui `Terminal<TestBackend>` exercises the real `render` method, not terminal lifecycle setup.
