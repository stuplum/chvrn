# `chvrn-git` public API

Status: approved for implementation. This contract describes the Git crate's production interfaces and behavioural tests.

## Dependencies

Production crate: standard library, `similar` 2.7, and a Git executable available on `PATH`. Test-only dev dependency: `tempfile`. Tests use `std::process::Command` for real local Git fixtures. No network or global Git configuration is required.

## Types and signatures

`RepositoryState` and `ReviewState` below denote private implementation state, not public types. The public signatures and enum variants are the contract.

```rust
use std::path::{Path, PathBuf};

pub struct Repository {
    state: RepositoryState,
}

impl Repository {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, GitError>;
    pub fn discover(start: &Path) -> Result<Self, GitError>;
    pub fn root(&self) -> &Path;
    pub fn index_path(&self) -> &Path;
    pub fn conflict(&self, path: &Path) -> Result<Option<ConflictSnapshot>, GitError>;
    pub fn validate_conflict(&self, snapshot: &ConflictSnapshot) -> Result<(), GitError>;
    pub fn merge_base(&self, revision: &str) -> Result<String, GitError>;
    pub fn changes(&self, base: &str) -> Result<Vec<Change>, GitError>;
    pub fn review(&self, base: Base, paths: &[PathBuf]) -> Result<Review, GitError>;
    pub fn review_patch(&self, base: Base, patch: &[u8]) -> Result<Review, GitError>;
    pub fn validate_review(&self, review: &Review) -> Result<(), GitError>;
    pub fn review_after_changes(&self, before: &Review, changes: &[PatchCandidate]) -> Result<Review, GitError>;
    pub fn stage(&self, review: &Review, hunks: &[HunkId]) -> Result<(), GitError>;
    pub fn stage_file(&self, review: &Review, path: &Path) -> Result<(), GitError>;
    pub fn rejection_candidate(&self, review: &Review, hunk: HunkId) -> Result<PatchCandidate, GitError>;
    pub fn reject(&self, review: &Review, hunks: &[HunkId]) -> Result<(), GitError>;
    pub fn reject_file(&self, review: &Review, path: &Path) -> Result<(), GitError>;
    pub fn save_worktree(&self, review: &Review, path: &Path, bytes: &[u8]) -> Result<(), GitError>;
    pub fn save_files(&self, review: &Review, files: &[(PathBuf, Vec<u8>)]) -> Result<(), GitError>;
    pub fn export_patch(&self, review: &Review) -> Result<Vec<u8>, GitError>;
    pub fn preview_patch(&self, review: &Review, patch: &[u8]) -> Result<Vec<PatchCandidate>, GitError>;
    pub fn import_patch(&self, review: &Review, patch: &[u8]) -> Result<(), GitError>;
}

pub struct ConflictSnapshot;
impl ConflictSnapshot {
    pub fn path(&self) -> &Path;
    pub fn base(&self) -> &[u8];
    pub fn ours(&self) -> &[u8];
    pub fn theirs(&self) -> &[u8];
}

pub enum Base {
    Index,
    Revision(String),
}

pub struct Review {
    state: ReviewState,
}

impl Review {
    pub fn base(&self) -> &Base;
    pub fn resolved_revision(&self) -> Option<&str>;
    pub fn files(&self) -> &[ReviewedFile];
    pub fn file(&self, path: &Path) -> Option<&ReviewedFile>;
    pub fn hunks(&self, path: &Path) -> &[Hunk];
}

pub struct ReviewedFile {
    pub path: PathBuf,
    pub base: Option<Vec<u8>>,
    pub base_mode: Option<u32>,
    pub index: Option<Vec<u8>>,
    pub worktree: Option<Vec<u8>>,
    pub mode: Option<u32>,
    pub content: ContentKind,
}

pub struct PatchCandidate {
    pub path: PathBuf,
    pub bytes: Option<Vec<u8>>,
    pub mode: Option<u32>,
}

pub struct Hunk {
    pub id: HunkId,
    pub old_start: usize,
    pub old_end: usize,
    pub new_start: usize,
    pub new_end: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HunkId(u128);

pub struct Change {
    pub old_path: Option<PathBuf>,
    pub path: PathBuf,
    pub kind: ChangeKind,
    pub content: ContentKind,
}

#[derive(Debug, Eq, PartialEq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
}

#[derive(Debug, Eq, PartialEq)]
pub enum ContentKind {
    Text,
    Binary,
}

#[derive(Debug, Eq, PartialEq)]
pub enum GitError {
    StaleReview,
    InvalidBase,
    ForeignHunk,
    UnsafePath,
    MalformedPatch,
    BinaryContent,
    GitFailure,
    IoFailure,
    StaleConflict,
    ForeignConflict,
    MissingConflictSide { stage: u8 },
    NonRegularConflict { stage: u8 },
    PartialWrite { applied: Vec<PathBuf>, failed: PathBuf },
}
```

`GitError` variants are comparable without message wording. `HunkId` is opaque and scoped to one `Review`; hunk bounds are zero-based, half-open line ranges in the base and worktree. Paths are repository-relative native `PathBuf`s. `open` requires the exact repository root; `discover` accepts a nested directory or existing file and resolves its containing root. An empty `paths` slice means every changed path, including untracked files. Explicit absolute, escaping or symlink-intermediate/target paths fail closed. `Review::hunks` returns an empty slice for paths with no textual hunks. A hunk from another review returns `ForeignHunk`; the wrong review base for `stage` or `reject` returns `InvalidBase`; changed inspected content returns `StaleReview`. None of those errors writes anything.

`merge_base(revision)` returns a common-ancestor commit ID for the repository's `HEAD` and the supplied revision using `git merge-base`. Empty, option-like and NUL-containing revisions return `InvalidBase`; missing refs or no common ancestor return `GitFailure`. It uses the repository's sanitised Git subprocess environment and does not modify the index or worktree.

`Base::Index` compares the exact inspected index entries against worktree bytes and modes, including staged content on the same path. `Base::Revision` compares the named tree against worktree bytes and modes; the revision is resolved and pinned when creating the review. `changes(base)` compares a named revision against index and worktree combined: a path is reported if either layer differs, with rename source in `old_path`, and reports untracked files. A binary file (NUL or invalid UTF-8) is labelled `Binary`; text operations on its hunks are unavailable. Rename reporting is based on Git's rename detection, not just delete/add names. Paths with non-UTF-8 bytes are preserved as native paths on Unix.

`ReviewedFile::base` contains bytes from the selected index/revision, `index` contains staged bytes and `worktree` contains inspected filesystem bytes. Absence is `None`, distinct from an empty file. `mode` describes inspected worktree permissions (Git file mode), and `content` marks binary/invalid UTF-8 rather than rewriting it. `Review::resolved_revision` exposes the pinned tree ID. `save_worktree` and `save_files` validate reviewed snapshots before atomic per-file worktree replacement, preserve executable permissions, and never stage. `review_patch` captures every path touched by a validated patch even on a clean worktree or absent new target. `preview_patch` returns target bytes and modes without writing the worktree or index; `import_patch` applies those same validated candidates subject to another stale check.

`base_mode` captures the selected base's Git file mode. Compare it with `mode` to detect permission changes even when the compared bytes are identical.

Reviews include unresolved paths, even when worktree content matches a revision base. An unresolved index entry has no stage-zero bytes and is represented as `None` in `ReviewedFile::index`. `conflict` reads literal-pathspec stage entries and their blobs without changing the index or worktree; non-conflicted paths return `None`. An absent base stage means empty bytes, but both ours and theirs must exist. Sources and the worktree must be regular UTF-8 text files. The snapshot retains the repository and index identity, exact stage modes/object IDs, worktree bytes and permissions.

`validate_conflict` rejects a snapshot from another repository/index with `ForeignConflict` and changed entries, worktree bytes or permissions with `StaleConflict`. Unsafe paths and unsupported content retain their specific errors. `index_path` exposes the absolute index path resolved by Git, including linked-worktree locations, so callers can watch index changes outside the worktree.

`review_after_changes(before, changes)` is the post-mutation re-inspection path for review acceptance. Each candidate names a repository-relative path and the exact authorised post-write bytes and Git mode (`None`/`None` for deletion); paths may be newly introduced, but duplicate or unsafe paths are rejected. It checks the original pinned revision and exact index snapshot, requires every other inspected worktree file to remain unchanged, and requires each candidate to match the actual post-write bytes and mode. It returns a fresh review covering the union of all originally inspected paths and candidate paths, even if some are now equal to the base or absent. A mismatch returns `StaleReview`, never a newly approved snapshot; the returned review supports `validate_review` at asynchronous feedback delivery. This method does not write files or index entries.

`validate_review` checks every inspected worktree path, the exact inspected index and, for `Base::Revision`, that its named reference still resolves to the captured tree. Moving `HEAD` to another tree is stale; an immutable revision SHA remains valid when `HEAD` moves. This also applies to unchanged sessions and does not mutate anything. `stage` is valid only for `Base::Index` reviews. It applies chosen text hunks to the index, preserving previously staged material and unrelated worktree edits; it does not change worktree bytes or commit. `stage_file` stages complete reviewed bytes and mode, including mode-only changes, empty files and opaque binary content. `reject` is valid only for `Base::Revision` reviews and restores selected worktree hunks to the pinned revision without changing the index or other hunks. `reject_file` restores complete reviewed text and mode. A stale requested path or index rejects the mutation before writing; no silent reload, rebase or HEAD substitution. A filesystem failure after multi-file preflight can report `PartialWrite { applied, failed }`; cross-file atomicity is not promised. Replacement is atomic per file.

`export_patch` exports the complete `Base::Revision` review as an uncoloured Git-compatible unified patch. Git must be able to apply it to the matching base and reverse it back to the original bytes, including CRLF and no-final-newline content. Header style, context count and timestamps are not part of the contract. For paths requiring Git quoting or extended headers, export/import retain exact path bytes and mode/rename metadata; no shell quoting. `import_patch` applies a validated unified patch against the worktree snapshot captured by its `Review` (either base); every touched path must be within that review. All patch paths, preimages, bounds and target safety are checked before writing any file. A malformed/ambiguous patch or a stale inspected snapshot leaves all paths and index entries untouched; import never stages or commits. Binary patch bodies are rejected rather than rewritten lossily. Patch newline control markers are patch syntax, not file content.

The crate does not run hooks, external diff drivers or remote operations. Git subprocesses use argument vectors and machine-readable NUL-delimited status output, with environment isolation sufficient that repository configuration cannot run external programs during inspection. The CLI owns non-TTY rendering and exit codes; this crate returns data/errors without terminal control sequences.
