use crate::{ContentKind, GitError, Repository, SplitByte, classify, nul_fields, path_bytes};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct ConflictSnapshot {
    repository_root: PathBuf,
    index_path: PathBuf,
    path: PathBuf,
    entries: [Option<IndexEntry>; 4],
    base: Vec<u8>,
    ours: Vec<u8>,
    theirs: Vec<u8>,
    worktree: ConflictWorktree,
}

impl ConflictSnapshot {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn base(&self) -> &[u8] {
        &self.base
    }

    pub fn ours(&self) -> &[u8] {
        &self.ours
    }

    pub fn theirs(&self) -> &[u8] {
        &self.theirs
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct IndexEntry {
    pub(super) mode: u32,
    pub(super) oid: String,
}

#[derive(Debug, Eq, PartialEq)]
struct ConflictWorktree {
    bytes: Vec<u8>,
    permissions: fs::Permissions,
}

impl Repository {
    pub fn conflict(&self, path: &Path) -> Result<Option<ConflictSnapshot>, GitError> {
        let pinned = self.pin_index()?;
        let snapshot = pinned.repository.conflict_pinned(path)?;
        pinned.validate(self)?;
        Ok(snapshot)
    }

    fn conflict_pinned(&self, path: &Path) -> Result<Option<ConflictSnapshot>, GitError> {
        let entries = self.index_entries(path)?;
        if entries[1..].iter().all(Option::is_none) {
            return Ok(None);
        }
        for stage in [2, 3] {
            if entries[stage].is_none() {
                return Err(GitError::MissingConflictSide { stage: stage as u8 });
            }
        }
        let mut sources = [Vec::new(), Vec::new(), Vec::new()];
        for stage in 1..=3 {
            if let Some(entry) = &entries[stage] {
                if !matches!(entry.mode, 0o100644 | 0o100755) {
                    return Err(GitError::NonRegularConflict { stage: stage as u8 });
                }
                let file = self.blob_file(entry.mode, &entry.oid)?;
                if classify(file.bytes.as_deref()) != ContentKind::Text {
                    return Err(GitError::BinaryContent);
                }
                sources[stage - 1] = file.bytes.ok_or(GitError::GitFailure)?;
            }
        }
        let worktree = self.conflict_worktree(path)?;
        if classify(Some(&worktree.bytes)) != ContentKind::Text {
            return Err(GitError::BinaryContent);
        }
        let [base, ours, theirs] = sources;
        let snapshot = ConflictSnapshot {
            repository_root: self.root.clone(),
            index_path: self.index_path.clone(),
            path: path.to_path_buf(),
            entries,
            base,
            ours,
            theirs,
            worktree,
        };
        self.validate_conflict(&snapshot)?;
        Ok(Some(snapshot))
    }

    pub fn validate_conflict(&self, snapshot: &ConflictSnapshot) -> Result<(), GitError> {
        if self.root != snapshot.repository_root || self.index_path != snapshot.index_path {
            return Err(GitError::ForeignConflict);
        }
        if self.index_entries(&snapshot.path)? != snapshot.entries {
            return Err(GitError::StaleConflict);
        }
        if self.conflict_worktree(&snapshot.path)? != snapshot.worktree {
            return Err(GitError::StaleConflict);
        }
        Ok(())
    }

    pub(super) fn index_entries(&self, path: &Path) -> Result<[Option<IndexEntry>; 4], GitError> {
        self.validate_path(path)?;
        let output = self.git(
            &[
                "--literal-pathspecs".into(),
                "ls-files".into(),
                "--stage".into(),
                "-z".into(),
                "--".into(),
                path.as_os_str().to_os_string(),
            ],
            None,
            None,
        )?;
        let mut entries = [None, None, None, None];
        let expected_path = path_bytes(path);
        for record in nul_fields(&output)? {
            let (meta, recorded_path) =
                record.split_once_byte(b'\t').ok_or(GitError::GitFailure)?;
            if recorded_path != expected_path {
                return Err(GitError::GitFailure);
            }
            let mut fields = meta.split(|byte| *byte == b' ');
            let mode = fields.next().ok_or(GitError::GitFailure)?;
            let oid = fields.next().ok_or(GitError::GitFailure)?;
            let stage = fields.next().ok_or(GitError::GitFailure)?;
            if fields.next().is_some()
                || stage.len() != 1
                || !(b'0'..=b'3').contains(&stage[0])
                || !matches!(oid.len(), 40 | 64)
                || !oid.iter().all(u8::is_ascii_hexdigit)
            {
                return Err(GitError::GitFailure);
            }
            let mode = std::str::from_utf8(mode).map_err(|_| GitError::GitFailure)?;
            let mode = u32::from_str_radix(mode, 8).map_err(|_| GitError::GitFailure)?;
            let oid = std::str::from_utf8(oid)
                .map_err(|_| GitError::GitFailure)?
                .to_owned();
            let entry = &mut entries[usize::from(stage[0] - b'0')];
            if entry.replace(IndexEntry { mode, oid }).is_some() {
                return Err(GitError::GitFailure);
            }
        }
        if entries[0].is_some() && entries[1..].iter().any(Option::is_some) {
            return Err(GitError::GitFailure);
        }
        Ok(entries)
    }

    fn conflict_worktree(&self, path: &Path) -> Result<ConflictWorktree, GitError> {
        self.validate_path(path)?;
        let mut file = self
            .open_worktree_file(path)?
            .ok_or(GitError::StaleConflict)?;
        let metadata = file.metadata().map_err(|_| GitError::IoFailure)?;
        if !metadata.is_file() {
            return Err(GitError::NonRegularConflict { stage: 0 });
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|_| GitError::IoFailure)?;
        Ok(ConflictWorktree {
            bytes,
            permissions: metadata.permissions(),
        })
    }
}
