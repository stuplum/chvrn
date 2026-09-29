use crate::{
    Base, ContentKind, GitError, Hunk, HunkId, NEXT_ID, Repository, Review, ReviewedFile, classify,
    words,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

pub(crate) struct Cleanup(pub(crate) PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        if self.0.is_dir() {
            let _ = fs::remove_dir_all(&self.0);
        } else {
            let _ = fs::remove_file(&self.0);
        }
    }
}

struct IndexUpdate<'a> {
    path: &'a Path,
    bytes: Option<std::borrow::Cow<'a, [u8]>>,
    mode: Option<u32>,
}

impl Repository {
    pub fn stage(&self, review: &Review, hunks: &[HunkId]) -> Result<(), GitError> {
        if review.base != Base::Index {
            return Err(GitError::InvalidBase);
        }
        self.check_review_origin(review)?;
        let selected = select_hunks(review, hunks)?;
        let mut updates = Vec::with_capacity(selected.len());
        for (path, picked) in selected {
            let file = review.file(path).ok_or(GitError::ForeignHunk)?;
            let next = replace_selected(
                file.base.as_deref().unwrap_or_default(),
                file.worktree.as_deref().unwrap_or_default(),
                &picked,
                false,
            )?;
            let bytes = if file.worktree.is_none() && next.is_empty() {
                None
            } else {
                Some(std::borrow::Cow::Owned(next))
            };
            updates.push(IndexUpdate {
                path,
                bytes,
                mode: file.base_mode.or(file.worktree_mode),
            });
        }
        self.stage_updates(review, &updates)
    }

    pub fn stage_file(&self, review: &Review, path: &Path) -> Result<(), GitError> {
        if review.base != Base::Index {
            return Err(GitError::InvalidBase);
        }
        self.check_review_origin(review)?;
        let file = review.file(path).ok_or(GitError::UnsafePath)?;
        self.stage_updates(
            review,
            &[IndexUpdate {
                path,
                bytes: file.worktree.as_deref().map(std::borrow::Cow::Borrowed),
                mode: file.worktree_mode,
            }],
        )
    }

    fn stage_updates(&self, review: &Review, updates: &[IndexUpdate<'_>]) -> Result<(), GitError> {
        if updates.is_empty() {
            return Ok(());
        }
        let lock_path = self.index_lock_path();
        let lock = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    GitError::StaleReview
                } else {
                    GitError::IoFailure
                }
            })?;
        let lock_guard = Cleanup(lock_path.clone());
        if read_optional(&self.index_path)? != review.index_snapshot {
            return Err(GitError::StaleReview);
        }
        self.preflight(review, updates.iter().map(|update| update.path), true)?;
        let candidate = self.index_path.with_file_name(format!(
            "chvrn-index-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let candidate_guard = Cleanup(candidate.clone());
        if let Some(data) = &review.index_snapshot {
            fs::write(&candidate, data).map_err(|_| GitError::IoFailure)?;
        } else {
            self.git(&words(&["read-tree", "--empty"]), None, Some(&candidate))?;
        }
        let mut entries = Vec::new();
        let zero_oid = if updates.iter().any(|update| update.bytes.is_none()) {
            Some(self.zero_object_id()?)
        } else {
            None
        };
        for update in updates {
            if let Some(bytes) = &update.bytes {
                let oid = self.git(
                    &words(&["hash-object", "-w", "--stdin"]),
                    Some(bytes.as_ref()),
                    None,
                )?;
                let mode = update.mode.unwrap_or(0o100644);
                entries.extend_from_slice(format!("{mode:o} ").as_bytes());
                entries.extend_from_slice(crate::trim_newline(&oid));
                entries.push(b'\t');
            } else {
                entries.extend_from_slice(b"0 ");
                entries.extend_from_slice(zero_oid.as_deref().ok_or(GitError::GitFailure)?);
                entries.push(b'\t');
            }
            entries.extend_from_slice(&crate::path_bytes(update.path));
            entries.push(0);
        }
        self.git(
            &words(&["update-index", "-z", "--index-info"]),
            Some(&entries),
            Some(&candidate),
        )?;
        if read_optional(&self.index_path)? != review.index_snapshot {
            return Err(GitError::StaleReview);
        }
        self.preflight(review, updates.iter().map(|update| update.path), true)?;
        let updated = fs::read(&candidate).map_err(|_| GitError::IoFailure)?;
        fs::write(&lock_path, updated).map_err(|_| GitError::IoFailure)?;
        drop(lock);
        fs::rename(&lock_path, &self.index_path).map_err(|_| GitError::IoFailure)?;
        drop(candidate_guard);
        drop(lock_guard);
        Ok(())
    }

    pub fn reject(&self, review: &Review, hunks: &[HunkId]) -> Result<(), GitError> {
        if !matches!(review.base, Base::Revision(_)) {
            return Err(GitError::InvalidBase);
        }
        self.check_review_origin(review)?;
        let selected = select_hunks(review, hunks)?;
        self.preflight(review, selected.keys().copied(), false)?;
        let mut replacements = Vec::new();
        for (path, picked) in selected {
            let file = review.file(path).ok_or(GitError::ForeignHunk)?;
            let bytes = replace_selected(
                file.worktree.as_deref().unwrap_or_default(),
                file.base.as_deref().unwrap_or_default(),
                &picked,
                true,
            )?;
            let content = if file.base.is_none() && bytes.is_empty() {
                None
            } else {
                Some(bytes)
            };
            replacements.push((path.to_path_buf(), content));
        }
        self.replace_files(review, &replacements)
    }

    pub fn rejection_candidate(
        &self,
        review: &Review,
        hunk: HunkId,
    ) -> Result<crate::PatchCandidate, GitError> {
        if !matches!(review.base, Base::Revision(_)) {
            return Err(GitError::InvalidBase);
        }
        self.check_review_origin(review)?;
        let selected = select_hunks(review, &[hunk])?;
        let (path, picked) = selected.into_iter().next().ok_or(GitError::ForeignHunk)?;
        let file = review.file(path).ok_or(GitError::ForeignHunk)?;
        let bytes = replace_selected(
            file.worktree.as_deref().unwrap_or_default(),
            file.base.as_deref().unwrap_or_default(),
            &picked,
            true,
        )?;
        let bytes = if file.base.is_none() && bytes.is_empty() {
            None
        } else {
            Some(bytes)
        };
        let mode = bytes.as_ref().map(|_| file.mode.unwrap_or(0o100644));
        Ok(crate::PatchCandidate {
            path: path.to_path_buf(),
            bytes,
            mode,
        })
    }

    pub fn reject_file(&self, review: &Review, path: &Path) -> Result<(), GitError> {
        if !matches!(review.base, Base::Revision(_)) {
            return Err(GitError::InvalidBase);
        }
        self.check_review_origin(review)?;
        let file = review.file(path).ok_or(GitError::UnsafePath)?;
        if file.content != ContentKind::Text {
            return Err(GitError::BinaryContent);
        }
        let mut modes = BTreeMap::new();
        if let Some(mode) = file.base_mode {
            modes.insert(path.to_path_buf(), mode);
        }
        self.replace_files_with_modes(review, &[(path.to_path_buf(), file.base.clone())], &modes)
    }

    pub fn save_worktree(
        &self,
        review: &Review,
        path: &Path,
        bytes: &[u8],
    ) -> Result<(), GitError> {
        self.check_review_origin(review)?;
        let file = review.file(path).ok_or(GitError::UnsafePath)?;
        if file.content != ContentKind::Text || classify(Some(bytes)) != ContentKind::Text {
            return Err(GitError::BinaryContent);
        }
        self.replace_files(review, &[(path.to_path_buf(), Some(bytes.to_vec()))])
    }

    pub fn save_files(
        &self,
        review: &Review,
        files: &[(PathBuf, Vec<u8>)],
    ) -> Result<(), GitError> {
        self.check_review_origin(review)?;
        let mut replacements = Vec::with_capacity(files.len());
        let mut seen = BTreeSet::new();
        for (path, bytes) in files {
            if !seen.insert(path) {
                return Err(GitError::UnsafePath);
            }
            let file = review.file(path).ok_or(GitError::UnsafePath)?;
            if file.content != ContentKind::Text || classify(Some(bytes)) != ContentKind::Text {
                return Err(GitError::BinaryContent);
            }
            replacements.push((path.clone(), Some(bytes.clone())));
        }
        self.replace_files(review, &replacements)
    }

    pub(crate) fn replace_files(
        &self,
        review: &Review,
        replacements: &[(PathBuf, Option<Vec<u8>>)],
    ) -> Result<(), GitError> {
        self.replace_files_with_modes(review, replacements, &BTreeMap::new())
    }

    pub(crate) fn replace_files_with_modes(
        &self,
        review: &Review,
        replacements: &[(PathBuf, Option<Vec<u8>>)],
        modes: &BTreeMap<PathBuf, u32>,
    ) -> Result<(), GitError> {
        self.check_review_origin(review)?;
        let mut seen = BTreeSet::new();
        for (path, bytes) in replacements {
            if !seen.insert(path) {
                return Err(GitError::UnsafePath);
            }
            self.validate_path(path)?;
            if review.file(path).is_none() {
                return Err(GitError::UnsafePath);
            }
            if bytes
                .as_deref()
                .is_some_and(|contents| classify(Some(contents)) == ContentKind::Binary)
            {
                return Err(GitError::BinaryContent);
            }
        }
        self.preflight(
            review,
            replacements.iter().map(|(path, _)| path.as_path()),
            false,
        )?;
        let mut applied = Vec::new();
        for (path, bytes) in replacements {
            let file = review.file(path).ok_or(GitError::UnsafePath)?;
            if let Err(error) = self.preflight(review, [path.as_path()], false) {
                return Err(if applied.is_empty() {
                    error
                } else {
                    GitError::PartialWrite {
                        applied,
                        failed: path.clone(),
                    }
                });
            }
            if let Err(error) =
                self.replace_one(path, bytes.as_deref(), file, modes.get(path).copied())
            {
                return Err(if applied.is_empty() {
                    error
                } else {
                    GitError::PartialWrite {
                        applied,
                        failed: path.clone(),
                    }
                });
            }
            applied.push(path.clone());
        }
        Ok(())
    }

    pub fn validate_review(&self, review: &Review) -> Result<(), GitError> {
        self.check_review_origin(review)?;
        self.preflight(
            review,
            review.files.iter().map(|file| file.path.as_path()),
            true,
        )
    }

    pub(crate) fn preflight<'a>(
        &self,
        review: &Review,
        paths: impl IntoIterator<Item = &'a Path>,
        check_index: bool,
    ) -> Result<(), GitError> {
        self.check_review_base(review)?;
        if check_index && read_optional(&self.index_path)? != review.index_snapshot {
            return Err(GitError::StaleReview);
        }
        for path in paths {
            let file = review.file(path).ok_or(GitError::UnsafePath)?;
            let current = self.worktree_file(path)?;
            if current.bytes != file.worktree || current.mode != file.worktree_mode {
                return Err(GitError::StaleReview);
            }
            if check_index {
                let index = self.index_file(path)?;
                if index.bytes != file.index {
                    return Err(GitError::StaleReview);
                }
            }
        }
        Ok(())
    }

    pub(crate) fn check_review_base(&self, review: &Review) -> Result<(), GitError> {
        match (&review.base, &review.resolved_revision) {
            (Base::Index, None) => Ok(()),
            (Base::Revision(name), Some(expected)) => {
                let actual = self
                    .resolve_revision(name)
                    .map_err(|_| GitError::StaleReview)?;
                if &actual == expected {
                    Ok(())
                } else {
                    Err(GitError::StaleReview)
                }
            }
            _ => Err(GitError::StaleReview),
        }
    }

    pub(crate) fn check_review_origin(&self, review: &Review) -> Result<(), GitError> {
        if self.root != review.repository_root {
            Err(GitError::ForeignHunk)
        } else {
            Ok(())
        }
    }

    fn replace_one(
        &self,
        path: &Path,
        bytes: Option<&[u8]>,
        file: &ReviewedFile,
        mode_override: Option<u32>,
    ) -> Result<(), GitError> {
        self.validate_path(path)?;
        let target = self.root.join(path);
        let Some(bytes) = bytes else {
            match fs::remove_file(target) {
                Ok(()) => return Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(_) => return Err(GitError::IoFailure),
            }
        };
        let parent = target.parent().ok_or(GitError::UnsafePath)?;
        fs::create_dir_all(parent).map_err(|_| GitError::IoFailure)?;
        self.validate_path(path)?;
        let name = format!(
            ".chvrn-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        );
        let temp = parent.join(name);
        let guard = Cleanup(temp.clone());
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|_| GitError::IoFailure)?;
        output.write_all(bytes).map_err(|_| GitError::IoFailure)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = if let Some(mode) = mode_override {
                if mode != 0o100644 && mode != 0o100755 {
                    return Err(GitError::UnsafePath);
                }
                if mode == 0o100755 { 0o755 } else { 0o644 }
            } else if file.worktree.is_some() {
                fs::metadata(&target)
                    .map_err(|_| GitError::IoFailure)?
                    .permissions()
                    .mode()
            } else if file.base_mode == Some(0o100755) {
                0o755
            } else {
                0o644
            };
            fs::set_permissions(&temp, fs::Permissions::from_mode(mode))
                .map_err(|_| GitError::IoFailure)?;
        }
        output.sync_all().map_err(|_| GitError::IoFailure)?;
        drop(output);
        self.validate_path(path)?;
        let current = self.worktree_file(path)?;
        if current.bytes != file.worktree || current.mode != file.worktree_mode {
            return Err(GitError::StaleReview);
        }
        fs::rename(&temp, &target).map_err(|_| GitError::IoFailure)?;
        drop(guard);
        Ok(())
    }

    fn index_lock_path(&self) -> PathBuf {
        let mut name = self.index_path.as_os_str().to_os_string();
        name.push(".lock");
        PathBuf::from(name)
    }
}

fn select_hunks<'a>(
    review: &'a Review,
    ids: &[HunkId],
) -> Result<BTreeMap<&'a Path, Vec<&'a Hunk>>, GitError> {
    let mut selected = BTreeMap::new();
    let mut unique = BTreeSet::new();
    for id in ids {
        if !unique.insert(id.0) {
            return Err(GitError::ForeignHunk);
        }
        let (file, hunk) = review
            .files
            .iter()
            .find_map(|file| {
                file.hunks
                    .iter()
                    .find(|hunk| hunk.id == *id)
                    .map(|hunk| (file, hunk))
            })
            .ok_or(GitError::ForeignHunk)?;
        selected
            .entry(file.path.as_path())
            .or_insert_with(Vec::new)
            .push(hunk);
    }
    Ok(selected)
}

fn replace_selected(
    original: &[u8],
    replacement: &[u8],
    hunks: &[&Hunk],
    reverse: bool,
) -> Result<Vec<u8>, GitError> {
    let mut sorted = hunks.to_vec();
    sorted.sort_by_key(|hunk| {
        if reverse {
            hunk.new_bytes.start
        } else {
            hunk.old_bytes.start
        }
    });
    let mut output = Vec::with_capacity(original.len());
    let mut cursor = 0;
    for hunk in sorted {
        let (from, to) = if reverse {
            (&hunk.new_bytes, &hunk.old_bytes)
        } else {
            (&hunk.old_bytes, &hunk.new_bytes)
        };
        if from.start < cursor || from.end > original.len() || to.end > replacement.len() {
            return Err(GitError::StaleReview);
        }
        output.extend_from_slice(&original[cursor..from.start]);
        output.extend_from_slice(&replacement[to.clone()]);
        cursor = from.end;
    }
    output.extend_from_slice(&original[cursor..]);
    Ok(output)
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, GitError> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(GitError::IoFailure),
    }
}
