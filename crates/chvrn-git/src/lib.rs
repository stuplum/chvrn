mod patch;
mod transaction;

pub use patch::PatchCandidate;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Base {
    Index,
    Revision(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HunkId(u128);

#[derive(Clone, Debug)]
pub struct Hunk {
    pub id: HunkId,
    pub old_start: usize,
    pub old_end: usize,
    pub new_start: usize,
    pub new_end: usize,
    old_bytes: std::ops::Range<usize>,
    new_bytes: std::ops::Range<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
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
    PartialWrite {
        applied: Vec<PathBuf>,
        failed: PathBuf,
    },
}

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for GitError {}

#[derive(Clone, Debug)]
pub struct Change {
    pub old_path: Option<PathBuf>,
    pub path: PathBuf,
    pub kind: ChangeKind,
    pub content: ContentKind,
}

#[derive(Clone, Debug)]
pub struct ReviewedFile {
    pub path: PathBuf,
    pub base: Option<Vec<u8>>,
    pub index: Option<Vec<u8>>,
    pub worktree: Option<Vec<u8>>,
    pub mode: Option<u32>,
    pub content: ContentKind,
    hunks: Vec<Hunk>,
    base_mode: Option<u32>,
    worktree_mode: Option<u32>,
}

pub struct Review {
    base: Base,
    resolved_revision: Option<String>,
    files: Vec<ReviewedFile>,
    index_snapshot: Option<Vec<u8>>,
    repository_root: PathBuf,
}

impl Review {
    pub fn base(&self) -> &Base {
        &self.base
    }

    pub fn resolved_revision(&self) -> Option<&str> {
        self.resolved_revision.as_deref()
    }

    pub fn files(&self) -> &[ReviewedFile] {
        &self.files
    }

    pub fn file(&self, path: &Path) -> Option<&ReviewedFile> {
        self.files.iter().find(|file| file.path == path)
    }

    pub fn hunks(&self, path: &Path) -> &[Hunk] {
        self.file(path)
            .map_or(&[][..], |file| file.hunks.as_slice())
    }
}

pub struct Repository {
    root: PathBuf,
    index_path: PathBuf,
}

impl Repository {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, GitError> {
        let root = fs::canonicalize(root).map_err(|_| GitError::IoFailure)?;
        let top = git_output(&root, &words(&["rev-parse", "--show-toplevel"]), None, None)?;
        let top = path_from_git(trim_newline(&top))?;
        if fs::canonicalize(top).map_err(|_| GitError::GitFailure)? != root {
            return Err(GitError::UnsafePath);
        }
        let index = git_output(
            &root,
            &words(&["rev-parse", "--git-path", "index"]),
            None,
            None,
        )?;
        let index = path_from_git(trim_newline(&index))?;
        let index_path = if index.is_absolute() {
            index
        } else {
            root.join(index)
        };
        Ok(Self { root, index_path })
    }

    pub fn discover(start: &Path) -> Result<Self, GitError> {
        let start = fs::canonicalize(start).map_err(|_| GitError::IoFailure)?;
        let directory = if start.is_dir() {
            start
        } else {
            start.parent().ok_or(GitError::UnsafePath)?.to_path_buf()
        };
        let top = git_output(
            &directory,
            &words(&["rev-parse", "--show-toplevel"]),
            None,
            None,
        )?;
        let root = path_from_git(trim_newline(&top))?;
        Self::open(root)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn changes(&self, base: &str) -> Result<Vec<Change>, GitError> {
        let tree = self.resolve_revision(base)?;
        let mut args = words(&[
            "diff",
            "--name-status",
            "-z",
            "--find-renames",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
        ]);
        args.push(tree.clone().into());
        args.push("--".into());
        let output = self.git(&args, None, None)?;
        let fields = nul_fields(&output)?;
        let mut changes = Vec::new();
        let mut index = 0;
        while index < fields.len() {
            let status = fields[index];
            index += 1;
            let kind = match status.first().copied() {
                Some(b'A') => ChangeKind::Added,
                Some(b'D') => ChangeKind::Deleted,
                Some(b'R') => ChangeKind::Renamed,
                Some(b'M' | b'T' | b'C') => ChangeKind::Modified,
                _ => return Err(GitError::GitFailure),
            };
            let first = path_from_git(*fields.get(index).ok_or(GitError::GitFailure)?)?;
            index += 1;
            let (old_path, path) = if kind == ChangeKind::Renamed {
                let second = path_from_git(*fields.get(index).ok_or(GitError::GitFailure)?)?;
                index += 1;
                (Some(first), second)
            } else {
                (None, first)
            };
            let worktree = self.worktree_file(&path)?;
            let bytes = if worktree.bytes.is_some() {
                worktree.bytes
            } else if let Some(old) = &old_path {
                self.tree_file(&tree, old)?.bytes
            } else {
                self.tree_file(&tree, &path)?.bytes
            };
            changes.push(Change {
                old_path,
                path,
                kind,
                content: classify(bytes.as_deref()),
            });
        }
        let untracked = self.git(
            &words(&["ls-files", "--others", "--exclude-standard", "-z"]),
            None,
            None,
        )?;
        for raw in nul_fields(&untracked)? {
            let path = path_from_git(raw)?;
            if changes.iter().any(|change| change.path == path) {
                continue;
            }
            let content = classify(self.worktree_file(&path)?.bytes.as_deref());
            changes.push(Change {
                old_path: None,
                path,
                kind: ChangeKind::Added,
                content,
            });
        }
        changes.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(changes)
    }

    pub fn review(&self, base: Base, paths: &[PathBuf]) -> Result<Review, GitError> {
        let resolved_revision = match &base {
            Base::Index => None,
            Base::Revision(revision) => Some(self.resolve_revision(revision)?),
        };
        let requested: BTreeSet<PathBuf> = if paths.is_empty() {
            let mut discovered = BTreeSet::new();
            match &base {
                Base::Index => {
                    let changed = self.git(
                        &words(&[
                            "diff",
                            "--name-only",
                            "-z",
                            "--no-ext-diff",
                            "--no-textconv",
                        ]),
                        None,
                        None,
                    )?;
                    for raw in nul_fields(&changed)? {
                        discovered.insert(path_from_git(raw)?);
                    }
                    let others = self.git(
                        &words(&["ls-files", "--others", "--exclude-standard", "-z"]),
                        None,
                        None,
                    )?;
                    for raw in nul_fields(&others)? {
                        discovered.insert(path_from_git(raw)?);
                    }
                }
                Base::Revision(_) => {
                    for change in
                        self.changes(resolved_revision.as_deref().ok_or(GitError::InvalidBase)?)?
                    {
                        if let Some(old) = change.old_path {
                            discovered.insert(old);
                        }
                        discovered.insert(change.path);
                    }
                }
            }
            discovered
        } else {
            paths.iter().cloned().collect()
        };
        self.capture_review(base, resolved_revision, requested)
    }

    fn capture_review(
        &self,
        base: Base,
        resolved_revision: Option<String>,
        requested: BTreeSet<PathBuf>,
    ) -> Result<Review, GitError> {
        let mut files = Vec::with_capacity(requested.len());
        let mut next_hunk_id = (NEXT_ID.fetch_add(1, Ordering::Relaxed) as u128) << 64;
        for path in requested {
            self.validate_path(&path)?;
            let index = self.index_file(&path)?;
            let worktree = self.worktree_file(&path)?;
            let original = if let Some(tree) = &resolved_revision {
                self.tree_file(tree, &path)?
            } else {
                index.clone()
            };
            let kind = classify(original.bytes.as_deref()).max(classify(worktree.bytes.as_deref()));
            let hunks = if kind == ContentKind::Text {
                build_hunks(
                    original.bytes.as_deref().unwrap_or_default(),
                    worktree.bytes.as_deref().unwrap_or_default(),
                    &mut next_hunk_id,
                )?
            } else {
                Vec::new()
            };
            files.push(ReviewedFile {
                path,
                base: original.bytes,
                index: index.bytes,
                worktree: worktree.bytes,
                mode: worktree.mode,
                content: kind,
                hunks,
                base_mode: original.mode,
                worktree_mode: worktree.mode,
            });
        }
        let index_snapshot = match fs::read(&self.index_path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Err(GitError::IoFailure),
        };
        Ok(Review {
            base,
            resolved_revision,
            files,
            index_snapshot,
            repository_root: self.root.clone(),
        })
    }

    pub fn review_after_changes(
        &self,
        before: &Review,
        changes: &[PatchCandidate],
    ) -> Result<Review, GitError> {
        self.check_review_origin(before)?;
        let mut expected = BTreeMap::new();
        let mut paths: BTreeSet<PathBuf> =
            before.files.iter().map(|file| file.path.clone()).collect();
        for candidate in changes {
            self.validate_path(&candidate.path)?;
            if expected
                .insert(candidate.path.as_path(), candidate)
                .is_some()
            {
                return Err(GitError::StaleReview);
            }
            paths.insert(candidate.path.clone());
        }
        self.preflight(
            before,
            before
                .files
                .iter()
                .filter(|file| !expected.contains_key(file.path.as_path()))
                .map(|file| file.path.as_path()),
            true,
        )?;
        let after =
            self.capture_review(before.base.clone(), before.resolved_revision.clone(), paths)?;
        if after.index_snapshot != before.index_snapshot
            || after.resolved_revision != before.resolved_revision
        {
            return Err(GitError::StaleReview);
        }
        for file in &after.files {
            if let Some(candidate) = expected.get(file.path.as_path()) {
                if file.worktree.as_deref() != candidate.bytes.as_deref()
                    || file.mode != candidate.mode
                {
                    return Err(GitError::StaleReview);
                }
            } else {
                let original = before.file(&file.path).ok_or(GitError::StaleReview)?;
                if file.worktree != original.worktree || file.mode != original.mode {
                    return Err(GitError::StaleReview);
                }
            }
        }
        self.validate_review(&after)?;
        Ok(after)
    }

    fn resolve_revision(&self, revision: &str) -> Result<String, GitError> {
        if revision.is_empty() || revision.starts_with('-') || revision.contains('\0') {
            return Err(GitError::InvalidBase);
        }
        let mut args = words(&["rev-parse", "--verify"]);
        args.push(format!("{revision}^{{tree}}").into());
        let output = self
            .git(&args, None, None)
            .map_err(|_| GitError::InvalidBase)?;
        String::from_utf8(trim_newline(&output).to_vec()).map_err(|_| GitError::GitFailure)
    }

    fn tree_file(&self, tree: &str, path: &Path) -> Result<FileState, GitError> {
        let args = vec![
            "ls-tree".into(),
            "-z".into(),
            tree.into(),
            "--".into(),
            path.as_os_str().to_os_string(),
        ];
        let output = self.git(&args, None, None)?;
        let Some(record) = nul_fields(&output)?.first().copied() else {
            return Ok(FileState::empty());
        };
        let (meta, recorded_path) = record.split_once_byte(b'\t').ok_or(GitError::GitFailure)?;
        if recorded_path != path_bytes(path).as_slice() {
            return Err(GitError::GitFailure);
        }
        let parts: Vec<&[u8]> = meta.split(|byte| *byte == b' ').collect();
        if parts.len() != 3 || parts[1] != b"blob" {
            return Err(GitError::GitFailure);
        }
        self.blob_state(parts[0], parts[2])
    }

    fn index_file(&self, path: &Path) -> Result<FileState, GitError> {
        let args = vec![
            "ls-files".into(),
            "--stage".into(),
            "-z".into(),
            "--".into(),
            path.as_os_str().to_os_string(),
        ];
        let output = self.git(&args, None, None)?;
        let Some(record) = nul_fields(&output)?.first().copied() else {
            return Ok(FileState::empty());
        };
        let (meta, recorded_path) = record.split_once_byte(b'\t').ok_or(GitError::GitFailure)?;
        if recorded_path != path_bytes(path).as_slice() {
            return Err(GitError::GitFailure);
        }
        let parts: Vec<&[u8]> = meta.split(|byte| *byte == b' ').collect();
        if parts.len() != 3 || parts[2] != b"0" {
            return Err(GitError::GitFailure);
        }
        self.blob_state(parts[0], parts[1])
    }

    fn blob_state(&self, mode: &[u8], oid: &[u8]) -> Result<FileState, GitError> {
        let mode = std::str::from_utf8(mode).map_err(|_| GitError::GitFailure)?;
        let mode = u32::from_str_radix(mode, 8).map_err(|_| GitError::GitFailure)?;
        if mode == 0o120000 {
            return Err(GitError::UnsafePath);
        }
        if mode != 0o100644 && mode != 0o100755 {
            return Err(GitError::BinaryContent);
        }
        let oid = std::str::from_utf8(oid).map_err(|_| GitError::GitFailure)?;
        let bytes = self.git(
            &vec!["cat-file".into(), "blob".into(), oid.into()],
            None,
            None,
        )?;
        Ok(FileState {
            bytes: Some(bytes),
            mode: Some(mode),
        })
    }

    fn worktree_file(&self, path: &Path) -> Result<FileState, GitError> {
        self.validate_path(path)?;
        match fs::read(self.root.join(path)) {
            Ok(bytes) => {
                let metadata =
                    fs::metadata(self.root.join(path)).map_err(|_| GitError::IoFailure)?;
                if !metadata.is_file() {
                    return Err(GitError::UnsafePath);
                }
                #[cfg(unix)]
                let mode = {
                    use std::os::unix::fs::PermissionsExt;
                    if metadata.permissions().mode() & 0o111 != 0 {
                        0o100755
                    } else {
                        0o100644
                    }
                };
                #[cfg(not(unix))]
                let mode = 0o100644;
                Ok(FileState {
                    bytes: Some(bytes),
                    mode: Some(mode),
                })
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(FileState::empty()),
            Err(_) => Err(GitError::IoFailure),
        }
    }

    fn validate_path(&self, path: &Path) -> Result<(), GitError> {
        if path.as_os_str().is_empty()
            || path_bytes(path).contains(&0)
            || path.components().any(|part| !matches!(part, Component::Normal(_)))
            || path.components().next().is_some_and(|part| {
                matches!(part, Component::Normal(name) if path_bytes(Path::new(name)).eq_ignore_ascii_case(b".git"))
            })
        {
            return Err(GitError::UnsafePath);
        }
        let mut cursor = self.root.clone();
        for part in path.components() {
            cursor.push(part.as_os_str());
            match fs::symlink_metadata(&cursor) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(GitError::UnsafePath);
                }
                Ok(_) => (),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(_) => return Err(GitError::IoFailure),
            }
        }
        Ok(())
    }

    fn zero_object_id(&self) -> Result<Vec<u8>, GitError> {
        let oid = self.git(&words(&["hash-object", "--stdin"]), Some(b""), None)?;
        let width = trim_newline(&oid).len();
        if width != 40 && width != 64 {
            return Err(GitError::GitFailure);
        }
        Ok(vec![b'0'; width])
    }

    fn git(
        &self,
        args: &[OsString],
        input: Option<&[u8]>,
        index: Option<&Path>,
    ) -> Result<Vec<u8>, GitError> {
        git_output(&self.root, args, input, index)
    }
}

#[derive(Clone)]
struct FileState {
    bytes: Option<Vec<u8>>,
    mode: Option<u32>,
}

impl FileState {
    fn empty() -> Self {
        Self {
            bytes: None,
            mode: None,
        }
    }
}

fn classify(bytes: Option<&[u8]>) -> ContentKind {
    if bytes.is_some_and(|data| data.contains(&0) || std::str::from_utf8(data).is_err()) {
        ContentKind::Binary
    } else {
        ContentKind::Text
    }
}

fn words(args: &[&str]) -> Vec<OsString> {
    args.iter().map(|arg| OsString::from(*arg)).collect()
}

fn trim_newline(bytes: &[u8]) -> &[u8] {
    bytes.strip_suffix(b"\n").unwrap_or(bytes)
}

fn nul_fields(bytes: &[u8]) -> Result<Vec<&[u8]>, GitError> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    if !bytes.ends_with(&[0]) {
        return Err(GitError::GitFailure);
    }
    Ok(bytes[..bytes.len() - 1].split(|byte| *byte == 0).collect())
}

trait SplitByte {
    fn split_once_byte(&self, needle: u8) -> Option<(&[u8], &[u8])>;
}
impl SplitByte for [u8] {
    fn split_once_byte(&self, needle: u8) -> Option<(&[u8], &[u8])> {
        let at = self.iter().position(|byte| *byte == needle)?;
        Some((&self[..at], &self[at + 1..]))
    }
}

#[cfg(unix)]
fn path_from_git(bytes: &[u8]) -> Result<PathBuf, GitError> {
    use std::os::unix::ffi::OsStringExt;
    Ok(OsString::from_vec(bytes.to_vec()).into())
}
#[cfg(not(unix))]
fn path_from_git(bytes: &[u8]) -> Result<PathBuf, GitError> {
    Ok(PathBuf::from(
        std::str::from_utf8(bytes).map_err(|_| GitError::UnsafePath)?,
    ))
}

#[cfg(unix)]
fn path_bytes(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}
#[cfg(not(unix))]
fn path_bytes(path: &Path) -> Vec<u8> {
    path.to_string_lossy().as_bytes().to_vec()
}

fn git_output(
    root: &Path,
    args: &[OsString],
    input: Option<&[u8]>,
    index: Option<&Path>,
) -> Result<Vec<u8>, GitError> {
    git_output_with_objects(root, args, input, index, None)
}

fn git_output_with_objects(
    root: &Path,
    args: &[OsString],
    input: Option<&[u8]>,
    index: Option<&Path>,
    objects: Option<(&Path, &Path)>,
) -> Result<Vec<u8>, GitError> {
    let mut command = Command::new("git");
    command
        .current_dir(root)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_EXTERNAL_DIFF")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.hooksPath")
        .env("GIT_CONFIG_VALUE_0", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    }
    if let Some((directory, alternate)) = objects {
        command.env("GIT_OBJECT_DIRECTORY", directory);
        command.env("GIT_ALTERNATE_OBJECT_DIRECTORIES", alternate);
    }
    if input.is_some() {
        command.stdin(Stdio::piped());
    }
    let mut child = command.spawn().map_err(|_| GitError::GitFailure)?;
    if let Some(input) = input {
        child
            .stdin
            .take()
            .ok_or(GitError::GitFailure)?
            .write_all(input)
            .map_err(|_| GitError::GitFailure)?;
    }
    let output = child.wait_with_output().map_err(|_| GitError::GitFailure)?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(GitError::GitFailure)
    }
}

fn build_hunks(old: &[u8], new: &[u8], next_hunk_id: &mut u128) -> Result<Vec<Hunk>, GitError> {
    let old = std::str::from_utf8(old).map_err(|_| GitError::BinaryContent)?;
    let new = std::str::from_utf8(new).map_err(|_| GitError::BinaryContent)?;
    let diff = similar::TextDiff::from_lines(old, new);
    let old_offsets = line_offsets(old.as_bytes());
    let new_offsets = line_offsets(new.as_bytes());
    let mut hunks = Vec::new();
    for group in diff.grouped_ops(3) {
        let changed: Vec<_> = group
            .iter()
            .filter(|op| op.tag() != similar::DiffTag::Equal)
            .collect();
        let Some(first) = changed.first() else {
            continue;
        };
        let last = changed.last().expect("first changed operation");
        let old_range = first.old_range().start..last.old_range().end;
        let new_range = first.new_range().start..last.new_range().end;
        hunks.push(Hunk {
            id: HunkId(*next_hunk_id),
            old_start: old_range.start,
            new_start: new_range.start,
            old_bytes: old_offsets[old_range.start]..old_offsets[old_range.end],
            old_end: old_range.end,
            new_end: new_range.end,
            new_bytes: new_offsets[new_range.start]..new_offsets[new_range.end],
        });
        *next_hunk_id += 1;
    }
    Ok(hunks)
}

fn line_offsets(data: &[u8]) -> Vec<usize> {
    let mut offsets = vec![0];
    for (index, byte) in data.iter().enumerate() {
        if *byte == b'\n' {
            offsets.push(index + 1);
        }
    }
    if *offsets.last().expect("initial offset") != data.len() {
        offsets.push(data.len());
    }
    offsets
}
