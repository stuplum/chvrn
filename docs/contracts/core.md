# chvrn-core public API

Status: implementation drafted against the reviewed integration tests; executable verification belongs to the coordinator. All paths below are relative to the `chvrn_core` crate root; the core crate never depends on Git, terminal state, herdr or LSP.

## Text snapshots and positions

```rust
#[derive(Clone)]
pub struct TextSnapshot { /* opaque immutable text and snapshot identity */ }
#[derive(Debug, PartialEq, Eq)]
pub enum TextError { InvalidUtf8, BinaryInput }
impl TextSnapshot {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, TextError>;
    pub fn as_bytes(&self) -> &[u8];
    pub fn text(&self) -> &str;
    pub fn same_identity(&self, other: &Self) -> bool;
}
```

`TextSnapshot` preserves UTF-8 bytes, LF/CRLF/CR line endings and presence or absence of a final newline. Invalid UTF-8 is rejected, not decoded with replacement; a NUL-containing input is identified as binary and rejected with `BinaryInput`. Independently constructed snapshots have distinct identities even when their bytes match. Cloning a snapshot retains its identity. `same_identity` compares snapshot authority, not byte equality; an edit followed by undo creates a different identity despite restoring equal bytes. Reads borrow immutable storage without copying; callers cannot mutate a snapshot through either view. No raw identifier is exposed.

Line numbers in rendered rows are one-based; hunk line ranges are zero-based half-open ranges. Character offsets in intraline changes and editor operations count Unicode scalar values, never UTF-8 bytes. Structural source ranges are zero-based half-open byte ranges into `TextSnapshot::as_bytes()`, always ending on UTF-8 boundaries. CRLF is one line terminator, not two lines or an editable cursor position between CR and LF.

## Line and intraline diff

```rust
pub mod diff {
    use std::ops::Range;
    use crate::TextSnapshot;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum WhitespacePolicy { Exact, IgnoreEdge, IgnoreAll, IgnoreBlankLines }
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum ApplyDirection { LeftToRight, RightToLeft }
    #[derive(Debug, PartialEq, Eq)]
    pub enum ApplyError { StaleSnapshot, ForeignHunk }

    pub struct RenderedLine { pub number: usize, pub text: String }
    pub struct AlignedRow {
        pub left: Option<RenderedLine>,
        pub right: Option<RenderedLine>,
    }
    pub struct Hunk {
        pub left_lines: Range<usize>,
        pub right_lines: Range<usize>,
    }
    pub struct Diff { /* opaque endpoints, rows and hunks */ }
    impl Diff {
        pub fn between(left: &TextSnapshot, right: &TextSnapshot, policy: WhitespacePolicy) -> Self;
        pub fn rows(&self) -> &[AlignedRow];
        pub fn hunks(&self) -> &[Hunk];
        pub fn apply_hunk(&self, hunk: &Hunk, current: &TextSnapshot, direction: ApplyDirection)
            -> Result<TextSnapshot, ApplyError>;
    }

    #[derive(Debug, PartialEq, Eq)]
    pub struct IntralineChange {
        pub left: Range<usize>,
        pub right: Range<usize>,
    }
    pub fn intraline_spans(left: &str, right: &str) -> Vec<IntralineChange>;
}
```

`Diff::between` uses patience matching of unique complete lines, then deterministic order-preserving fallback for regions without unique anchors. Unchanged aligned lines retain both line numbers; inserted/deleted lines use `None` as virtual fillers, never blank text inserted into either snapshot. Within a changed block, lines pair from the start and surplus lines receive fillers. `RenderedLine::text` excludes the terminator; the snapshot retains it. For a move expressible as a line diff, unique unchanged anchors take priority over pairing unrelated lines.

`Exact` compares original full lines including terminators. `IgnoreEdge` ignores leading/trailing horizontal whitespace in line contents, `IgnoreAll` ignores all horizontal whitespace in line contents, and `IgnoreBlankLines` ignores lines whose contents contain only horizontal whitespace. Policies do not ignore terminator differences on retained lines, are independent (not cumulative), and affect alignment and reported hunks only. In particular, ignoring blank lines does not imply ignoring whitespace in nonblank lines. None changes the bytes returned by a snapshot or applied hunk. `intraline_spans` returns changed runs in Unicode-scalar offsets excluding line terminators, coalesced to avoid splitting an extended grapheme cluster; spans may be zero-width for insertion/deletion.

A hunk copies exact source bytes to the opposite destination, even if its surrounding alignment used a whitespace policy. `LeftToRight` takes `current` with this diff's **right** snapshot identity and replaces the selected right-side range with left-side bytes. `RightToLeft` takes `current` with this diff's **left** snapshot identity and replaces the selected left-side range with right-side bytes. A hunk from another diff is `ForeignHunk`. A different destination snapshot identity is `StaleSnapshot`, including one independently constructed with equal content. Applying a hunk returns a new snapshot; the diff must be recomputed before another application. Empty half-open hunk ranges represent boundary insertion/deletion. No partial mutation is possible on error.

## Rope-backed editing

```rust
pub mod edit {
    use std::ops::Range;
    use crate::TextSnapshot;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum CursorMotion { Left, Right, Up, Down }
    #[derive(Debug, PartialEq, Eq)]
    pub enum EditError { OutOfBounds, InvalidLineEndingBoundary, BinaryInput }
    pub struct TextBuffer { /* opaque rope, cursor and history */ }
    #[derive(Clone)]
    pub struct CapturedText;
    impl CapturedText {
        pub fn same_identity(&self, snapshot: &TextSnapshot) -> bool;
        pub fn snapshot(self) -> TextSnapshot;
    }
    impl TextBuffer {
        pub fn new(snapshot: TextSnapshot) -> Self;
        pub fn text(&self) -> String;
        pub fn snapshot(&self) -> TextSnapshot;
        pub fn capture(&self) -> CapturedText;
        pub fn len_bytes(&self) -> usize;
        pub fn len_chars(&self) -> usize;
        pub fn line_count(&self) -> usize;
        pub fn char_to_line(&self, char_offset: usize) -> Result<usize, EditError>;
        pub fn line_to_char(&self, line: usize) -> Option<usize>;
        pub fn line_text(&self, line: usize) -> Option<String>;
        pub fn slice_chars(&self, chars: Range<usize>) -> Result<String, EditError>;
        pub fn cursor(&self) -> usize;
        pub fn set_cursor(&mut self, char_offset: usize) -> Result<(), EditError>;
        pub fn move_cursor(&mut self, motion: CursorMotion) -> bool;
        pub fn insert(&mut self, text: &str) -> Result<(), EditError>;
        pub fn delete(&mut self, chars: Range<usize>) -> Result<(), EditError>;
        pub fn replace(&mut self, chars: Range<usize>, text: &str) -> Result<(), EditError>;
        pub fn undo(&mut self) -> bool;
        pub fn redo(&mut self) -> bool;
    }
}
```

Positions count Unicode scalar values; the cursor starts at zero. Insert happens at the cursor and moves it past inserted text. Delete removes a half-open character range and places the cursor at the range start. `replace` removes and inserts as one atomic undo step and leaves the cursor after the replacement. Left/right move one scalar without crossing into the middle of CRLF; up/down preserve the desired scalar column, clamping at shorter lines and document ends. Mutating operations are individual undo steps and clear redo after a new edit. Undo/redo restore text and cursor, including original line endings and final-newline state. Out-of-bounds edits and splitting CRLF fail without mutation; inserting NUL fails with `BinaryInput`. `snapshot()` returns a stable cached identity until an edit, then a new identity; undo does not resurrect stale diff authority. Alignment fillers are not represented in the rope and cannot be edited through `TextBuffer`.

`capture()` clones the persistent rope root and shared edit identity without flattening the document. A `CapturedText` can be sent to a worker; `snapshot(self)` flattens there and retains the captured edit identity even if the buffer has changed in the meantime. `CapturedText::same_identity(&TextSnapshot)` compares that identity in constant time without flattening either text. Two captures of one edit and `TextBuffer::snapshot()` share identity; edit, undo and redo each establish a fresh identity. `len_bytes()`, `len_chars()` and `line_count()` query the rope without materialising text. Line indices are zero-based; `line_to_char` and `line_text` return `None` for a missing line, and `line_text` copies only that line. `char_to_line` and `slice_chars` reject out-of-bounds and CRLF-splitting character offsets, while `slice_chars` copies only its requested range.

## Three-way merge

```rust
pub mod merge {
    use std::ops::Range;
    use crate::TextSnapshot;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct ConflictId(pub usize);
    pub struct Conflict {
        pub id: ConflictId,
        pub base_lines: Range<usize>,
        pub ours_lines: Range<usize>,
        pub theirs_lines: Range<usize>,
    }
    pub struct PreviewConflict {
        pub id: ConflictId,
        pub result_chars: Range<usize>,
        pub result_lines: Range<usize>,
    }
    pub enum ConflictResolution { Ours, Theirs, Both, Manual(String) }
    #[derive(Debug, PartialEq, Eq)]
    pub enum ResolveError { UnknownConflict, BinaryInput }
    pub struct Merge { /* opaque base, sides, result and conflict resolutions */ }
    impl Merge {
        pub fn three_way(base: &TextSnapshot, ours: &TextSnapshot, theirs: &TextSnapshot) -> Self;
        pub fn conflicts(&self) -> &[Conflict];
        pub fn preview(&self) -> TextSnapshot;
        pub fn preview_conflicts(&self) -> Vec<PreviewConflict>;
        pub fn result(&self) -> Option<TextSnapshot>;
        pub fn resolve(&mut self, id: ConflictId, choice: ConflictResolution)
            -> Result<(), ResolveError>;
    }
}
```

Non-overlapping changes from both sides compose in base order with their exact bytes, including distinct character edits within one multi-line hunk. Same edits on both sides coalesce. Incompatible overlapping edits remain unresolved until explicitly chosen; `result()` is `None` while any conflict remains. `preview()` returns a cached immutable snapshot with independent edits, resolved choices and provisional exact ours bytes for unresolved conflicts. `preview_conflicts()` contains only unresolved conflicts and their exact provisional ours spans in the current preview. `result_chars` counts Unicode scalar values and `result_lines` is the zero-based half-open set of preview lines intersecting those bytes, or an empty range for a zero-width conflict. Spans shift when an earlier conflict is resolved. `conflicts()` keeps every original conflict and its source line ranges, including resolved ones, so IDs stay stable. `Ours`/`Theirs` select the original side's exact content, `Both` inserts ours then theirs, and `Manual` inserts the supplied exact text at that conflict; NUL-containing manual text is rejected. Unrelated edits survive all choices. No conflict-marker text is emitted unless a user explicitly enters it as manual text. Conflict IDs are stable within one `Merge` and not transferable between merges.

## Tree-sitter structural analysis

```rust
pub mod structural {
    use std::{ops::Range, path::Path};
    use crate::TextSnapshot;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Language { Rust, TypeScript, Tsx, JavaScript, Jsx, Python, Json }
    impl Language { pub fn for_path(path: &Path) -> Option<Self>; }
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum StructuralChangeKind { Move, Reflow, Renamed, ChangedTokens }
    pub struct StructuralChange {
        pub kind: StructuralChangeKind,
        pub before: Range<usize>,
        pub after: Range<usize>,
    }
    pub struct StructuralDiff { /* opaque classified changes */ }
    impl StructuralDiff { pub fn changes(&self) -> &[StructuralChange]; }
    #[derive(Debug, PartialEq, Eq)]
    pub enum StructuralError { UnsupportedLanguage, ParseFailure }
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum HighlightKind { Keyword, Identifier, String, Number, Comment, Type, Function, Punctuation }
    pub struct HighlightSpan { pub bytes: Range<usize>, pub kind: HighlightKind }
    pub fn highlight(language: Option<Language>, text: &TextSnapshot)
        -> Result<Vec<HighlightSpan>, StructuralError>;
    pub struct StructuralAnalysis;
    impl StructuralAnalysis {
        pub fn compare(language: Option<Language>, before: &TextSnapshot, after: &TextSnapshot)
            -> Result<StructuralDiff, StructuralError>;
    }
}
```

Explicit grammar registration maps `.rs`, `.ts`, `.tsx`, `.js`, `.jsx`, `.py` and `.json` to real Tree-sitter grammars. Other paths return `None` and ordinary textual diff/edit remains available; structural comparison or highlighting with `None` returns `UnsupportedLanguage`. `highlight` runs compiled Tree-sitter grammar queries and returns byte ranges into the original snapshot; callers convert them to grapheme and terminal-cell positions. Structural comparison parses actual syntax trees rather than a fake parser or line-only heuristic. Changed byte locations in `before` and `after` refer to their respective snapshots. A moved unchanged named syntax unit is `Move`; a syntax unit with unchanged token stream but changed layout is `Reflow`; changing only a declaration's name with unchanged body is `Renamed`; changing a call target or other token content is `ChangedTokens`. A changed literal must not be called a rename, move or reflow. Changes are reported at the affected top-level syntax-unit level, without nested duplicate records for the same edit. Structure is informational and never authorises a byte-inexact hunk application.

## Dependencies proposed for implementation

Runtime: `ropey` 1.6, `similar` 2.7 for Myers fallback and intraline matching, `tree-sitter` 0.25, compatible `tree-sitter-rust`, `tree-sitter-typescript` (TypeScript and TSX), `tree-sitter-javascript` (JS and JSX), `tree-sitter-python`, `tree-sitter-json`, and `unicode-segmentation` 1 for grapheme-safe intraline coalescing. `std` suffices for all core integration tests: no test-only dependencies. The coordinator owns manifest versions and verification.
