use crate::transaction::Cleanup;
use crate::{
    ContentKind, GitError, NEXT_ID, Repository, Review, classify, git_output,
    git_output_with_objects, nul_fields, path_bytes, path_from_git, trim_newline, words,
};
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PatchCandidate {
    pub path: PathBuf,
    pub bytes: Option<Vec<u8>>,
    pub mode: Option<u32>,
}

impl Repository {
    pub fn export_patch(&self, review: &Review) -> Result<Vec<u8>, GitError> {
        self.check_review_origin(review)?;
        let tree = review
            .resolved_revision
            .as_ref()
            .ok_or(GitError::InvalidBase)?;
        self.preflight(
            review,
            review.files.iter().map(|file| file.path.as_path()),
            false,
        )?;
        if review
            .files
            .iter()
            .any(|file| file.content == ContentKind::Binary)
        {
            return Err(GitError::BinaryContent);
        }
        let scratch = Scratch::new()?;
        let objects = scratch.path.join("objects");
        fs::create_dir(&objects).map_err(|_| GitError::IoFailure)?;
        let alternate = self.git(&words(&["rev-parse", "--git-path", "objects"]), None, None)?;
        let alternate = path_from_git(trim_newline(&alternate))?;
        let alternate = if alternate.is_absolute() {
            alternate
        } else {
            self.root.join(alternate)
        };
        let index = scratch.path.join("index");
        let mut read_tree = words(&["read-tree"]);
        read_tree.push(tree.into());
        git_output_with_objects(
            &self.root,
            &read_tree,
            None,
            Some(&index),
            Some((&objects, &alternate)),
        )?;
        let mut entries = Vec::new();
        let zero_oid = if review
            .files
            .iter()
            .any(|file| file.base.is_some() && file.worktree.is_none())
        {
            Some(self.zero_object_id()?)
        } else {
            None
        };
        for file in &review.files {
            if file.base == file.worktree && file.base_mode == file.worktree_mode {
                continue;
            }
            if let Some(bytes) = &file.worktree {
                let oid = git_output_with_objects(
                    &self.root,
                    &words(&["hash-object", "-w", "--stdin"]),
                    Some(bytes),
                    None,
                    Some((&objects, &alternate)),
                )?;
                entries.extend_from_slice(
                    format!("{:o} ", file.worktree_mode.unwrap_or(0o100644)).as_bytes(),
                );
                entries.extend_from_slice(trim_newline(&oid));
                entries.push(b'\t');
            } else {
                entries.extend_from_slice(b"0 ");
                entries.extend_from_slice(zero_oid.as_deref().ok_or(GitError::GitFailure)?);
                entries.push(b'\t');
            }
            entries.extend_from_slice(&path_bytes(&file.path));
            entries.push(0);
        }
        if !entries.is_empty() {
            git_output_with_objects(
                &self.root,
                &words(&["update-index", "-z", "--index-info"]),
                Some(&entries),
                Some(&index),
                Some((&objects, &alternate)),
            )?;
        }
        self.preflight(
            review,
            review.files.iter().map(|file| file.path.as_path()),
            false,
        )?;
        let mut diff = words(&[
            "diff",
            "--cached",
            "--binary",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--find-renames",
        ]);
        diff.push(tree.into());
        diff.push("--".into());
        git_output_with_objects(
            &self.root,
            &diff,
            None,
            Some(&index),
            Some((&objects, &alternate)),
        )
    }

    pub fn review_patch(&self, base: crate::Base, patch: &[u8]) -> Result<Review, GitError> {
        let scratch = Scratch::new()?;
        let touched = self.validated_patch_paths(patch, &scratch.path)?;
        self.review(base, &touched.into_iter().collect::<Vec<_>>())
    }

    pub fn import_patch(&self, review: &Review, patch: &[u8]) -> Result<(), GitError> {
        let candidates = self.preview_patch(review, patch)?;
        let modes = candidates
            .iter()
            .filter_map(|candidate| candidate.mode.map(|mode| (candidate.path.clone(), mode)))
            .collect();
        let replacements: Vec<_> = candidates
            .into_iter()
            .map(|candidate| (candidate.path, candidate.bytes))
            .collect();
        self.replace_files_with_modes(review, &replacements, &modes)
    }

    pub fn preview_patch(
        &self,
        review: &Review,
        patch: &[u8],
    ) -> Result<Vec<PatchCandidate>, GitError> {
        self.check_review_origin(review)?;
        let scratch = Scratch::new()?;
        let touched = self.validated_patch_paths(patch, &scratch.path)?;
        for path in &touched {
            self.validate_path(path)?;
            let file = review.file(path).ok_or(GitError::UnsafePath)?;
            if file.content != ContentKind::Text {
                return Err(GitError::BinaryContent);
            }
        }
        self.preflight(review, touched.iter().map(PathBuf::as_path), false)?;
        for path in &touched {
            let file = review.file(path).ok_or(GitError::UnsafePath)?;
            if let Some(bytes) = &file.worktree {
                let destination = scratch.path.join(&file.path);
                let parent = destination.parent().ok_or(GitError::UnsafePath)?;
                fs::create_dir_all(parent).map_err(|_| GitError::IoFailure)?;
                let mut output = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&destination)
                    .map_err(|_| GitError::IoFailure)?;
                output.write_all(bytes).map_err(|_| GitError::IoFailure)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = if file.worktree_mode == Some(0o100755) {
                        0o755
                    } else {
                        0o644
                    };
                    fs::set_permissions(destination, fs::Permissions::from_mode(mode))
                        .map_err(|_| GitError::IoFailure)?;
                }
            }
        }
        git_output(
            &scratch.path,
            &words(&["apply", "--check", "-"]),
            Some(patch),
            None,
        )
        .map_err(|_| GitError::MalformedPatch)?;
        git_output(&scratch.path, &words(&["apply", "-"]), Some(patch), None)
            .map_err(|_| GitError::MalformedPatch)?;
        let mut candidates = Vec::with_capacity(touched.len());
        for path in touched {
            let destination = scratch.path.join(&path);
            match fs::symlink_metadata(&destination) {
                Ok(metadata) if !metadata.is_file() => return Err(GitError::UnsafePath),
                Ok(_) => (),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(_) => return Err(GitError::IoFailure),
            }
            let bytes = match fs::read(&destination) {
                Ok(bytes) => Some(bytes),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(_) => return Err(GitError::IoFailure),
            };
            if bytes
                .as_deref()
                .is_some_and(|bytes| classify(Some(bytes)) == ContentKind::Binary)
            {
                return Err(GitError::BinaryContent);
            }
            #[cfg(unix)]
            let mode = if bytes.is_some() {
                use std::os::unix::fs::PermissionsExt;
                let permission = fs::metadata(&destination)
                    .map_err(|_| GitError::IoFailure)?
                    .permissions()
                    .mode();
                Some(if permission & 0o111 != 0 {
                    0o100755
                } else {
                    0o100644
                })
            } else {
                None
            };
            #[cfg(not(unix))]
            let mode = bytes.as_ref().map(|_| 0o100644);
            candidates.push(PatchCandidate { path, bytes, mode });
        }
        self.preflight(
            review,
            candidates.iter().map(|candidate| candidate.path.as_path()),
            false,
        )?;
        Ok(candidates)
    }
}

impl Repository {
    fn validated_patch_paths(
        &self,
        patch: &[u8],
        scratch: &Path,
    ) -> Result<BTreeSet<PathBuf>, GitError> {
        if patch
            .split(|byte| *byte == b'\n')
            .any(|line| line == b"GIT binary patch")
        {
            return Err(GitError::BinaryContent);
        }
        let mut declared = BTreeSet::new();
        let mut previous = b"".as_slice();
        let mut before_previous = b"".as_slice();
        for line in patch.split(|byte| *byte == b'\n') {
            if line.ends_with(b" 120000")
                && [
                    b"new file mode ".as_slice(),
                    b"new mode ".as_slice(),
                    b"index ".as_slice(),
                ]
                .iter()
                .any(|prefix| line.starts_with(prefix))
            {
                return Err(GitError::UnsafePath);
            }
            for prefix in [b"--- a/".as_slice(), b"+++ b/".as_slice()] {
                if let Some(path) = line.strip_prefix(prefix) {
                    self.validate_path(&path_from_git(path)?)?;
                }
            }
            if line.starts_with(b"@@")
                && before_previous.starts_with(b"--- ")
                && previous.starts_with(b"+++ ")
            {
                for (header, prefix) in [
                    (before_previous, b"--- a/".as_slice()),
                    (previous, b"+++ b/".as_slice()),
                ] {
                    if let Some(path) = header.strip_prefix(prefix) {
                        declared.insert(path_from_git(path)?);
                    }
                }
            }
            before_previous = previous;
            previous = line;
        }
        let stats = git_output(
            scratch,
            &words(&["apply", "--numstat", "-z", "--unsafe-paths", "-"]),
            Some(patch),
            None,
        )
        .map_err(|_| GitError::MalformedPatch)?;
        let touched = patch_paths(&stats).map_err(|_| GitError::MalformedPatch)?;
        if touched.is_empty() {
            return Err(GitError::MalformedPatch);
        }
        if !declared.is_subset(&touched) {
            return Err(GitError::MalformedPatch);
        }
        for path in &touched {
            self.validate_path(path)?;
        }
        Ok(touched)
    }
}

struct Scratch {
    path: PathBuf,
    _cleanup: Cleanup,
}

impl Scratch {
    fn new() -> Result<Self, GitError> {
        let path = std::env::temp_dir().join(format!(
            "chvrn-git-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&path)
                .map_err(|_| GitError::IoFailure)?;
        }
        #[cfg(not(unix))]
        fs::create_dir(&path).map_err(|_| GitError::IoFailure)?;
        Ok(Self {
            _cleanup: Cleanup(path.clone()),
            path,
        })
    }
}

fn patch_paths(data: &[u8]) -> Result<BTreeSet<PathBuf>, GitError> {
    let fields = nul_fields(data)?;
    let mut paths = BTreeSet::new();
    let mut index = 0;
    while index < fields.len() {
        let record = fields[index];
        index += 1;
        let mut columns = record.splitn(3, |byte| *byte == b'\t');
        let added = columns.next().ok_or(GitError::MalformedPatch)?;
        let deleted = columns.next().ok_or(GitError::MalformedPatch)?;
        let path = columns.next().ok_or(GitError::MalformedPatch)?;
        if added.is_empty() || deleted.is_empty() {
            return Err(GitError::MalformedPatch);
        }
        if path.is_empty() {
            let old = *fields.get(index).ok_or(GitError::MalformedPatch)?;
            let new = *fields.get(index + 1).ok_or(GitError::MalformedPatch)?;
            index += 2;
            if !paths.insert(path_from_git(old)?) || !paths.insert(path_from_git(new)?) {
                return Err(GitError::MalformedPatch);
            }
        } else if !paths.insert(path_from_git(path)?) {
            return Err(GitError::MalformedPatch);
        }
    }
    Ok(paths)
}
