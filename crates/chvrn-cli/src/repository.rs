use crate::files::{GuardedFile, read_bytes};
use crate::herdr_ui::{self, HerdrUi, Notice};
use crate::jev_ui::JevUi;
use crate::language::LanguageUi;
use crate::socket_ui::{PendingDecision, SocketUi};
use crate::standalone::{diff_value, print_value, snapshot};
use crate::terminal::{self, ReviewHost};
use crate::watch::{BackgroundDiff, FileWatch};
use crate::{HerdrMode, Options, Result, ReviewArgs};
use chvrn_core::TextSnapshot;
use chvrn_git::{Base, ConflictSnapshot, ContentKind, PatchCandidate, Repository, Review};
use chvrn_integrations::herdr::{HunkRange, ReviewedFile};
use chvrn_tui::{
    DiffRequestId, Pane, RepositoryReviewMode, ReviewInput, ReviewOutcome, ReviewSession,
    ReviewSubmission,
};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_SNAPSHOT: AtomicU64 = AtomicU64::new(1);

pub fn snapshot_id() -> String {
    format!(
        "chvrn-{}-{}",
        std::process::id(),
        NEXT_SNAPSHOT.fetch_add(1, Ordering::Relaxed)
    )
}

fn base(name: &str) -> Base {
    if name == "index" {
        Base::Index
    } else {
        Base::Revision(name.into())
    }
}

fn review_base(repo: &Repository, explicit: Option<&str>) -> Result<Base> {
    if let Some(name) = explicit {
        return Ok(base(name));
    }
    let branch = match std::env::var("CHVRN_BASE_BRANCH") {
        Ok(branch) => branch,
        Err(std::env::VarError::NotPresent) => "main".into(),
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err("CHVRN_BASE_BRANCH must contain a valid UTF-8 branch name".into());
        }
    };
    repo.merge_base(&branch).map(Base::Revision).map_err(|_| {
        format!(
            "cannot find a merge base between HEAD and {branch:?} (CHVRN_BASE_BRANCH, default main); ensure both refs exist and share history, or supply --base <revision|index>"
        ).into()
    })
}

fn path_value(path: &Path) -> Value {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        json!(path.as_os_str().as_bytes())
    }
    #[cfg(not(unix))]
    {
        json!(path.to_string_lossy())
    }
}

struct PatchPreview {
    snapshot: String,
    patch: Vec<u8>,
    candidates: Vec<PatchCandidate>,
    socket_snapshot: Option<String>,
}

pub fn review(args: ReviewArgs, options: &Options) -> Result<u8> {
    let interactive = options.interactive();
    let jev = JevUi::from_options(options)?;
    if matches!(options.herdr, Some(HerdrMode::Gate)) && !interactive {
        return Err(
            "an explicit review gate requires an interactive terminal; no review was accepted"
                .into(),
        );
    }
    let repo = Repository::discover(&std::env::current_dir()?)?;
    let review_base = review_base(&repo, args.base.as_deref())?;
    if args.open_companion {
        let base_name = match &review_base {
            Base::Index => "index",
            Base::Revision(name) => name,
        };
        return options.runtime.run(herdr_ui::open_companion(
            &args,
            options,
            repo.root(),
            base_name,
        ))?;
    }
    if !interactive && (args.report.is_some() || args.export_patch.is_some()) {
        return Err(
            "--report and --export-patch require an interactive terminal and review actions; omit these options for headless review"
                .into(),
        );
    }
    let patch = args.patch.as_deref().map(read_bytes).transpose()?;
    let inspected = match &patch {
        Some(patch) => repo.review_patch(review_base, patch)?,
        None => repo.review(review_base, &args.paths)?,
    };
    let preview = patch
        .map(|patch| -> Result<PatchPreview> {
            let candidates = repo.preview_patch(&inspected, &patch)?;
            Ok(PatchPreview {
                patch,
                candidates,
                socket_snapshot: None,
                snapshot: snapshot_id(),
            })
        })
        .transpose()?;
    if !interactive {
        let files: Vec<Value> = match &preview {
            Some(preview) => preview
                .candidates
                .iter()
                .map(|candidate| {
                    let file = inspected.file(&candidate.path);
                    let before = file.and_then(|file| file.worktree.as_deref());
                    let mut value = diff_value(
                        before.unwrap_or_default(),
                        candidate.bytes.as_deref().unwrap_or_default(),
                        &candidate.path,
                        options.whitespace.into(),
                    );
                    value["equal"] = json!(
                        value["equal"] == true
                            && before.is_some() == candidate.bytes.is_some()
                            && file.and_then(|file| file.mode) == candidate.mode
                    );
                    value["path"] = json!(candidate.path.to_string_lossy());
                    value["path_bytes"] = path_value(&candidate.path);
                    value["deleted"] = json!(candidate.bytes.is_none());
                    value
                })
                .collect(),
            None => inspected
                .files()
                .iter()
                .map(|file| {
                    let mut value = diff_value(
                        file.base.as_deref().unwrap_or_default(),
                        file.worktree.as_deref().unwrap_or_default(),
                        &file.path,
                        options.whitespace.into(),
                    );
                    value["equal"] = json!(
                        value["equal"] == true
                            && file.base.is_some() == file.worktree.is_some()
                            && file.base_mode == file.mode
                    );
                    value["path"] = json!(file.path.to_string_lossy());
                    value["path_bytes"] = path_value(&file.path);
                    value["deleted"] = json!(file.worktree.is_none());
                    value
                })
                .collect(),
        };
        let base_name = match inspected.base() {
            Base::Index => "index",
            Base::Revision(name) => name,
        };
        let different = files.iter().any(|file| file["equal"] != true);
        print_value(
            &json!({"base": base_name, "patch_preview": preview.is_some(), "files": files}),
            options.format,
        )?;
        return Ok(u8::from(different));
    }
    let report = args
        .report
        .as_deref()
        .map(GuardedFile::capture)
        .transpose()?;
    let export = args
        .export_patch
        .as_deref()
        .map(GuardedFile::capture)
        .transpose()?;
    let snapshot = snapshot_id();
    let watch = FileWatch::new(&[repo.root(), repo.index_path()], true)?;
    let language = LanguageUi::new(options, repo.root())?;
    let herdr = HerdrUi::new(options)?;
    let socket = args
        .socket
        .as_deref()
        .map(|path| SocketUi::new(path, repo.root(), &inspected, &snapshot, &options.runtime))
        .transpose()?;
    let mut host = RepositoryHost {
        repo,
        inspected: Arc::new(inspected),
        args,
        options,
        selected: 0,
        jev,
        merge: None,
        preview,
        decisions: BTreeMap::new(),
        accepted_paths: BTreeSet::new(),
        comment: String::new(),
        editing_comment: false,
        snapshot,
        report,
        export,
        paths: Vec::new(),
        watch,
        language,
        herdr,
        socket,
        pending_socket_decision: None,
        socket_candidates: VecDeque::new(),
        loading: None,
        needs_refresh: false,
        background: BackgroundDiff::new(),
        pending_review: None,
        pending_request: None,
        refresh_conflict: false,
        finished: None,
    };
    host.reset_paths();
    let mut session = host.session()?;
    terminal::run(&mut session, &mut host)
}

struct RepositoryRefresh {
    previous: Arc<Review>,
    selected: Option<PathBuf>,
    incoming: Arc<Review>,
    text: Option<(TextSnapshot, TextSnapshot)>,
}

struct RepositoryMerge {
    source: ConflictSnapshot,
    output: GuardedFile,
    stale: bool,
}

enum SocketReceipt {
    Awaiting(PendingDecision),
    Failed(String),
}

struct PendingSocketDecision {
    preview: PatchPreview,
    accepted: bool,
    receipt: SocketReceipt,
}

struct RepositoryHost<'a> {
    repo: Repository,
    inspected: Arc<Review>,
    args: ReviewArgs,
    options: &'a Options,
    selected: usize,
    jev: Option<JevUi>,
    merge: Option<RepositoryMerge>,
    preview: Option<PatchPreview>,
    decisions: BTreeMap<PathBuf, Vec<Value>>,
    accepted_paths: BTreeSet<PathBuf>,
    comment: String,
    editing_comment: bool,
    snapshot: String,
    report: Option<GuardedFile>,
    export: Option<GuardedFile>,
    paths: Vec<PathBuf>,
    watch: FileWatch,
    language: LanguageUi,
    herdr: Option<HerdrUi>,
    socket: Option<SocketUi>,
    pending_socket_decision: Option<PendingSocketDecision>,
    socket_candidates: VecDeque<chvrn_integrations::socket::PatchCandidate>,
    loading: Option<std::sync::mpsc::Receiver<Result<Option<RepositoryRefresh>>>>,
    needs_refresh: bool,
    background: BackgroundDiff,
    pending_review: Option<Arc<Review>>,
    pending_request: Option<DiffRequestId>,
    refresh_conflict: bool,
    finished: Option<u8>,
}

impl Drop for RepositoryHost<'_> {
    fn drop(&mut self) {
        if let Some(pending) = &self.pending_socket_decision {
            match &pending.receipt {
                SocketReceipt::Failed(error) => eprintln!("chvrn: {error}"),
                SocketReceipt::Awaiting(_) if pending.accepted => {
                    eprintln!("chvrn: patch was applied, but its socket receipt was not confirmed")
                }
                SocketReceipt::Awaiting(_) => {
                    eprintln!("chvrn: files unchanged; declined socket receipt was not confirmed")
                }
            }
        }
    }
}

impl RepositoryHost<'_> {
    fn poll_socket_decision(&mut self, session: &mut ReviewSession) -> Result<()> {
        let Some(mut pending) = self.pending_socket_decision.take() else {
            return Ok(());
        };
        let completion = match &mut pending.receipt {
            SocketReceipt::Awaiting(receipt) => receipt.try_complete(),
            SocketReceipt::Failed(_) => None,
        };
        let Some(completion) = completion else {
            self.pending_socket_decision = Some(pending);
            return Ok(());
        };
        let result: Result<()> = match completion {
            Err(error) => Err(format!(
                "{}; socket receipt was not confirmed: {error}",
                if pending.accepted {
                    "Patch was applied"
                } else {
                    "Files unchanged"
                },
            )
            .into()),
            Ok(()) => {
                let transition = if pending.accepted {
                    self.checked_transition(&pending.preview.candidates)
                } else {
                    self.reload(session)
                };
                transition.map_err(|error| {
                    format!(
                    "Socket receipt confirmed, but repository state could not be refreshed: {error}"
                ).into()
                })
            }
        };
        if let Err(error) = result {
            pending.receipt = SocketReceipt::Failed(error.to_string());
            self.pending_socket_decision = Some(pending);
            return Err(error);
        }
        self.decisions.clear();
        self.accepted_paths.clear();
        self.selected = 0;
        self.replace_session(session)?;
        session.set_message(if pending.accepted {
            "Patch candidate explicitly accepted and applied; accepted receipt confirmed"
        } else {
            "Patch candidate explicitly declined; files unchanged; declined receipt confirmed"
        });
        Ok(())
    }

    fn retire_refresh(&mut self) {
        let was_loading = self.loading.take().is_some();
        let was_pending = self.pending_review.take().is_some();
        self.needs_refresh |= was_loading || was_pending;
        self.pending_request = None;
        self.refresh_conflict = false;
    }

    fn replace_session(&mut self, session: &mut ReviewSession) -> Result<()> {
        self.retire_refresh();
        *session = self.session()?;
        Ok(())
    }

    fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    fn reset_paths(&mut self) {
        self.paths = match &self.preview {
            Some(preview) => preview
                .candidates
                .iter()
                .map(|file| file.path.clone())
                .collect(),
            None => self
                .inspected
                .files()
                .iter()
                .map(|file| file.path.clone())
                .collect(),
        };
    }

    fn current_path(&self) -> Result<PathBuf> {
        self.paths()
            .get(self.selected)
            .cloned()
            .ok_or_else(|| "no file is selected".into())
    }

    fn configure_footer(&self, session: &mut ReviewSession) {
        let mode = if self.preview.is_some() {
            RepositoryReviewMode::PatchPreview
        } else if matches!(self.inspected.base(), Base::Index) {
            RepositoryReviewMode::Index
        } else {
            RepositoryReviewMode::Revision
        };
        let count = self.paths().len();
        let position = if count == 0 { 0 } else { self.selected + 1 };
        session.set_repository_review(mode, format!("{position}/{count}"));
    }

    fn session(&self) -> Result<ReviewSession> {
        if self.paths.is_empty() {
            let mut session = ReviewSession::two_way("", "");
            session.set_theme(Arc::clone(&self.options.loaded_theme));
            session.set_read_only(Pane::Left, true);
            session.set_read_only(Pane::Right, true);
            self.configure_footer(&mut session);
            session.set_message("No changes. Watching for new snapshots; q exits without approval");
            return Ok(session);
        }
        let path = self.current_path()?;
        let file = self
            .inspected
            .file(&path)
            .ok_or("selected file has no inspected snapshot")?;
        let (left, right) = match &self.preview {
            Some(preview) => (
                file.worktree.as_deref().unwrap_or_default(),
                preview
                    .candidates
                    .iter()
                    .find(|candidate| candidate.path == path)
                    .and_then(|candidate| candidate.bytes.as_deref())
                    .unwrap_or_default(),
            ),
            None => (
                file.base.as_deref().unwrap_or_default(),
                file.worktree.as_deref().unwrap_or_default(),
            ),
        };
        let binary = file.content == ContentKind::Binary
            || snapshot(left).is_err()
            || snapshot(right).is_err();
        let mut session = if binary {
            ReviewSession::two_way(
                &format!("Binary input: {} bytes\n", left.len()),
                &format!("Binary input: {} bytes\n", right.len()),
            )
        } else {
            ReviewSession::two_way(std::str::from_utf8(left)?, std::str::from_utf8(right)?)
        };
        session.set_theme(Arc::clone(&self.options.loaded_theme));
        session.set_paths(&path, &path);
        session.set_whitespace_policy(self.options.whitespace.into());
        session.set_read_only(Pane::Left, true);
        session.set_read_only(Pane::Right, binary || self.preview.is_some());
        self.configure_footer(&mut session);
        Ok(session)
    }

    fn open_merge(&mut self, session: &mut ReviewSession) -> Result<()> {
        if session.is_dirty()
            || session.is_local_diff_pending()
            || self.preview.is_some()
            || self.refresh_conflict
            || self.pending_review.is_some()
            || self.loading.is_some()
            || self.herdr.as_ref().is_some_and(|herdr| herdr.pending)
        {
            return Err("finish or discard edits, previews and pending review updates before opening a merge".into());
        }
        self.repo.validate_review(&self.inspected)?;
        let path = self.current_path()?;
        let source = self.repo.conflict(&path).map_err(|error| match error {
            chvrn_git::GitError::MissingConflictSide { .. } => "modify/delete conflicts require an explicit keep/delete decision in Git; nothing changed".to_owned(),
            chvrn_git::GitError::NonRegularConflict { .. } | chvrn_git::GitError::BinaryContent => "only regular UTF-8 text conflicts can open in merge mode; resolve this file with Git".to_owned(),
            _ => error.to_string(),
        })?.ok_or("selected file has no unresolved Git conflict")?;
        let output = GuardedFile::capture(&self.repo.root().join(source.path()))?;
        self.repo.validate_conflict(&source)?;
        let mut merge = ReviewSession::three_way(
            std::str::from_utf8(source.base())?,
            std::str::from_utf8(source.ours())?,
            std::str::from_utf8(source.theirs())?,
        );
        merge.set_theme(Arc::clone(&self.options.loaded_theme));
        merge.set_paths(&path, &path);
        merge.set_output_path(&path);
        merge.set_read_only(Pane::Ours, true);
        merge.set_read_only(Pane::Theirs, true);
        merge.set_whitespace_policy(self.options.whitespace.into());
        merge.set_merge_advice_enabled(self.jev.is_some());
        session.cancel_merge_advice();
        self.merge = Some(RepositoryMerge {
            source,
            output,
            stale: false,
        });
        *session = merge;
        Ok(())
    }

    fn leave_merge(&mut self, session: &mut ReviewSession) -> Result<()> {
        if session.is_dirty() {
            return Err(
                "merge has unsaved changes; save it, undo changes, or quit and confirm discard"
                    .into(),
            );
        }
        session.cancel_merge_advice();
        self.reload(session)?;
        self.merge = None;
        Ok(())
    }

    fn reload(&mut self, session: &mut ReviewSession) -> Result<()> {
        self.retire_refresh();
        let path = self.current_path().ok();
        self.inspected = Arc::new(
            self.repo
                .review(self.inspected.base().clone(), &self.args.paths)?,
        );
        self.needs_refresh = false;
        self.reset_paths();
        self.invalidate()?;
        self.selected = path
            .and_then(|path| self.paths.iter().position(|candidate| *candidate == path))
            .unwrap_or(0);
        if self.paths.is_empty() {
            *session = ReviewSession::two_way("", "");
            session.set_theme(Arc::clone(&self.options.loaded_theme));
            session.set_read_only(Pane::Left, true);
            session.set_read_only(Pane::Right, true);
            self.configure_footer(session);
            session.set_message("No remaining differences. q exits without sending approval");
        } else {
            *session = self.session()?;
        }
        Ok(())
    }

    fn selected_git_hunk(&self, session: &ReviewSession) -> Result<chvrn_git::HunkId> {
        if session.is_dirty() || self.preview.is_some() {
            return Err("save or discard local edits before a Git action; patch previews cannot stage or reject".into());
        }
        let path = self.current_path()?;
        let (old, new) = session
            .selected_hunk_ranges()
            .ok_or("select a textual hunk first")?;
        self.inspected.hunks(&path).iter()
            .find(|hunk| hunk.old_start == old.start && hunk.old_end == old.end && hunk.new_start == new.start && hunk.new_end == new.end)
            .map(|hunk| hunk.id)
            .ok_or_else(|| "selected presentation hunk does not match one exact Git hunk; use exact whitespace mode".into())
    }

    fn invalidate(&mut self) -> Result<()> {
        if let Some(herdr) = &self.herdr {
            herdr.invalidate(&self.snapshot)?;
        }
        self.snapshot = snapshot_id();
        self.decisions.clear();
        self.accepted_paths.clear();
        self.socket_candidates.clear();
        if let Some(socket) = &self.socket {
            socket.refresh(&self.inspected, &self.snapshot)?;
        }
        Ok(())
    }

    fn record_decision(&mut self, path: &Path, accepted: Vec<Value>, rejected: Vec<Value>) {
        if let Some(decisions) = self.decisions.get_mut(path) {
            decisions.retain(|decision| {
                decision["rejected"]
                    .as_array()
                    .is_some_and(|ranges| !ranges.is_empty())
            });
        }
        let snapshot = self
            .preview
            .as_ref()
            .map_or(&self.snapshot, |preview| &preview.snapshot);
        self.decisions
            .entry(path.to_path_buf())
            .or_default()
            .push(json!({
                "path": path.to_string_lossy(), "path_bytes": path_value(path),
                "snapshot_id": snapshot, "accepted": accepted, "rejected": rejected,
            }));
    }

    fn checked_transition(&mut self, changes: &[PatchCandidate]) -> Result<()> {
        let next = match self.repo.review_after_changes(&self.inspected, changes) {
            Ok(next) => next,
            Err(error) => {
                self.decisions.clear();
                self.accepted_paths.clear();
                self.needs_refresh = true;
                return Err(format!("Files were changed, but the resulting state is stale: {error}; no approval was sent").into());
            }
        };
        if let Some(herdr) = &self.herdr {
            if herdr.pending {
                herdr.invalidate(&self.snapshot)?;
            }
        }
        self.inspected = Arc::new(next);
        self.snapshot = snapshot_id();
        self.reset_paths();
        self.socket_candidates.clear();
        if let Some(socket) = &self.socket {
            socket.refresh(&self.inspected, &self.snapshot)?;
        }
        Ok(())
    }

    fn report_files(&self) -> Result<Vec<ReviewedFile>> {
        self.decisions
            .iter()
            .flat_map(|(path, decisions)| decisions.iter().map(move |decision| (path, decision)))
            .map(|(path, decision)| {
                let path = url::Url::from_file_path(self.repo.root().join(path))
                    .map_err(|_| "review path cannot be represented as a file URI")?
                    .to_string();
                Ok(ReviewedFile {
                    path,
                    snapshot_id: decision["snapshot_id"]
                        .as_str()
                        .ok_or("review decision has no snapshot")?
                        .into(),
                    accepted: serde_json::from_value::<Vec<HunkRange>>(
                        decision["accepted"].clone(),
                    )?,
                    rejected: serde_json::from_value::<Vec<HunkRange>>(
                        decision["rejected"].clone(),
                    )?,
                })
            })
            .collect()
    }

    fn publish_refresh(&mut self, session: &mut ReviewSession, rebuild: bool) -> Result<()> {
        let old_path = self.current_path().ok();
        if let Some(review) = self.pending_review.take() {
            self.pending_request = None;
            self.inspected = review;
            self.reset_paths();
            self.selected = old_path
                .and_then(|path| self.paths.iter().position(|candidate| *candidate == path))
                .unwrap_or(0);
            self.refresh_conflict = false;
            if rebuild {
                *session = self.session()?;
            }
            if !rebuild {
                session.set_read_only(Pane::Right, self.paths.is_empty());
            }
            if let Ok(path) = self.current_path() {
                session.set_paths(&path, &path);
                session.set_read_only(Pane::Left, true);
            }
            self.configure_footer(session);
            if let Some(socket) = &self.socket {
                socket.refresh(&self.inspected, &self.snapshot)?;
            }
            session
                .set_message("Loaded a fresh repository snapshot; previous decisions invalidated");
        }
        Ok(())
    }
}

impl ReviewHost for RepositoryHost<'_> {
    fn finished(&self) -> Option<u8> {
        self.finished
    }

    fn tick(&mut self, session: &mut ReviewSession) -> Result<()> {
        if self.pending_socket_decision.is_some() {
            return self.poll_socket_decision(session);
        }
        let path = self
            .repo
            .root()
            .join(self.current_path().unwrap_or_default());
        self.language.tick(session, &path)?;
        if self.language.viewing_definition() {
            return Ok(());
        }
        if let Some(merge) = &mut self.merge {
            if self.watch.changed()? && self.repo.validate_conflict(&merge.source).is_err() {
                merge.stale = true;
                session.set_merge_advice_enabled(false);
                session.set_message("Git conflict inputs changed. Saving is blocked; return to review or quit and reopen");
            }
            if let Some(jev) = &mut self.jev {
                jev.tick(session);
            }
            return Ok(());
        }
        if let Some(jev) = &mut self.jev {
            jev.tick(session);
        }
        if let Some(herdr) = &mut self.herdr {
            for notice in herdr.notices() {
                match notice {
                    Notice::Offer => session.set_message("Agent review gate ready. Inspect changes and explicitly submit; quit never approves"),
                    Notice::Invalidated => {
                        self.decisions.clear();
                        self.accepted_paths.clear();
                        session.set_message("Agent or inspected snapshot changed; pending review invalidated");
                    }
                    Notice::Delivered => {
                        session.set_message("Submitted review delivered exactly once to the selected agent");
                        if herdr.mode == HerdrMode::Gate { self.finished = Some(0); }
                    }
                    Notice::Identity(None) => session.set_message("Agent lifecycle/session identity unverified; automatic gate and feedback are disabled"),
                    Notice::Identity(Some(_)) => session.set_message("Verified agent lifecycle active; companion remains passive until a real transition"),
                    Notice::Message(message) | Notice::Failed(message) => session.set_message(message),
                }
            }
        }
        if self.herdr.as_ref().is_some_and(|herdr| herdr.pending) {
            return Ok(());
        }
        if let Some(socket) = &self.socket {
            for candidate in socket.candidates() {
                self.socket_candidates.push_back(candidate);
                session.set_message("Socket patch candidate queued. v opens a read-only preview; nothing has been applied");
            }
            for error in socket.errors() {
                session.set_message(format!("Socket: {error}"));
            }
        }
        self.needs_refresh |= self.watch.changed()?;
        if let Some(loading) = &self.loading {
            if let Ok(result) = loading.try_recv() {
                self.loading = None;
                if let Some(refresh) = result? {
                    if !Arc::ptr_eq(&self.inspected, &refresh.previous)
                        || self.current_path().ok() != refresh.selected
                    {
                        self.needs_refresh = true;
                    } else {
                        self.invalidate()?;
                        self.preview = None;
                        self.refresh_conflict = false;
                        if let Some((left, right)) = refresh.text {
                            let request = session.request_diff_snapshots(left, right);
                            self.pending_request = Some(request.id());
                            self.background.request(request);
                        } else {
                            self.pending_request = None;
                            self.refresh_conflict = session.is_dirty();
                        }
                        self.pending_review = Some(refresh.incoming);
                        if self.pending_request.is_none() && !self.refresh_conflict {
                            self.publish_refresh(session, true)?;
                        }
                    }
                }
            }
        }
        if self.needs_refresh && self.loading.is_none() {
            self.needs_refresh = false;
            let root = self.repo.root().to_path_buf();
            let base = self.inspected.base().clone();
            let paths = self.args.paths.clone();
            let previous = Arc::clone(&self.inspected);
            let selected = self.current_path().ok();
            let (send, receive) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let result = (|| {
                    let repo = Repository::open(root)?;
                    let incoming = repo.review(base, &paths)?;
                    if repo.validate_review(&previous).is_ok() && same_review(&previous, &incoming)
                    {
                        return Ok(None);
                    }
                    let file = selected
                        .as_deref()
                        .and_then(|path| incoming.file(path))
                        .or_else(|| incoming.files().first());
                    let pair = file
                        .map(|file| {
                            (
                                file.base.as_deref().unwrap_or_default(),
                                file.worktree.as_deref().unwrap_or_default(),
                            )
                        })
                        .unwrap_or((&[], &[]));
                    let text = snapshot(pair.0).ok().zip(snapshot(pair.1).ok());
                    Ok(Some(RepositoryRefresh {
                        previous,
                        selected,
                        incoming: Arc::new(incoming),
                        text,
                    }))
                })();
                let _ = send.send(result);
            });
            self.loading = Some(receive);
        }
        if let Some(completion) = self.background.latest() {
            if self.pending_request == Some(completion.id()) {
                match session.handle(ReviewInput::DiffReady(completion)) {
                    ReviewOutcome::RefreshConflict => {
                        self.refresh_conflict = true;
                        session.set_message("External edits conflict with local edits. R discards local edits and reloads; submission is blocked");
                    }
                    ReviewOutcome::RefreshApplied => self.publish_refresh(session, false)?,
                    _ => self.retire_refresh(),
                }
            }
        }
        Ok(())
    }

    fn input(&mut self, session: &mut ReviewSession, event: &Event) -> Result<bool> {
        if self.pending_socket_decision.is_some() {
            return match event {
                Event::Key(key) if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) => Ok(false),
                Event::Resize(_, _) => Ok(false),
                _ => Err("socket receipt is pending or failed; no further changes can be submitted; q quits".into()),
            };
        }
        let path = self
            .repo
            .root()
            .join(self.current_path().unwrap_or_default());
        if self.language.input(session, &path, event)? {
            return Ok(true);
        }
        if self.merge.is_some() {
            if let Some(jev) = &mut self.jev {
                if jev.input(session, event) {
                    return Ok(true);
                }
            }
            if !session.is_editing()
                && matches!(event, Event::Key(key) if key.kind == KeyEventKind::Press && key.code == KeyCode::Esc && key.modifiers.is_empty())
            {
                self.leave_merge(session)?;
                return Ok(true);
            }
            return Ok(false);
        }
        let Event::Key(key) = event else {
            return Ok(false);
        };
        if key.kind != KeyEventKind::Press {
            return Ok(false);
        }
        if self.editing_comment {
            match key.code {
                KeyCode::Enter | KeyCode::Esc => self.editing_comment = false,
                KeyCode::Backspace => {
                    self.comment.pop();
                }
                KeyCode::Char(character) => self.comment.push(character),
                _ => {}
            }
            session.set_message(format!("Review comment: {}", self.comment));
            return Ok(true);
        }
        if session.is_editing() {
            return Ok(false);
        }
        if matches!(key.code, KeyCode::Char('S' | 'x' | 'v'))
            && self.herdr.as_ref().is_some_and(|herdr| herdr.pending)
        {
            return Err(
                "review mutations are blocked while submitted feedback awaits delivery".into(),
            );
        }
        if key.code == KeyCode::Char('R') && self.refresh_conflict {
            let rebuild = self.pending_request.is_none();
            if !rebuild
                && session.handle(ReviewInput::DiscardAndReload) != ReviewOutcome::RefreshApplied
            {
                self.retire_refresh();
                return Err("refresh was superseded; waiting for the current snapshot".into());
            }
            self.publish_refresh(session, rebuild)?;
            return Ok(true);
        }
        if key.code == KeyCode::Char('m') && key.modifiers.is_empty() {
            self.open_merge(session)?;
            return Ok(true);
        }
        if key.code == KeyCode::Char('v') {
            if session.is_dirty() || self.preview.is_some() {
                return Err("finish or discard the current edits/preview first".into());
            }
            let candidate = self
                .socket_candidates
                .pop_front()
                .ok_or("no socket candidate is waiting")?;
            let expected = self
                .inspected
                .files()
                .iter()
                .position(|file| file.path == Path::new(&candidate.path))
                .map(|index| format!("{}:{index}", self.snapshot));
            if expected.as_deref() != Some(&candidate.snapshot_id) {
                return Err("socket candidate belongs to an older inspected snapshot".into());
            }
            let patch = candidate.patch.into_bytes();
            let candidates = self.repo.preview_patch(&self.inspected, &patch)?;
            self.preview = Some(PatchPreview {
                patch,
                candidates,
                socket_snapshot: Some(candidate.snapshot_id),
                snapshot: snapshot_id(),
            });
            self.decisions.clear();
            self.accepted_paths.clear();
            self.reset_paths();
            self.selected = 0;
            self.replace_session(session)?;
            return Ok(true);
        }
        if key.code == KeyCode::Char('x') && self.preview.is_some() {
            let permit = self
                .preview
                .as_ref()
                .and_then(|preview| preview.socket_snapshot.as_ref())
                .map(|_| {
                    self.socket
                        .as_ref()
                        .ok_or("socket is unavailable")?
                        .reserve_decision()
                })
                .transpose()?;
            let preview = self.preview.take().ok_or("patch preview is unavailable")?;
            if let (Some(snapshot), Some(permit)) = (&preview.socket_snapshot, permit) {
                let receipt = permit.record(snapshot, false);
                self.retire_refresh();
                self.pending_socket_decision = Some(PendingSocketDecision {
                    preview,
                    accepted: false,
                    receipt: SocketReceipt::Awaiting(receipt),
                });
                session.set_message("Files unchanged; waiting for declined socket receipt");
            } else {
                self.finished = Some(1);
            }
            return Ok(true);
        }
        if key.code == KeyCode::Char('E') {
            let path = self.current_path()?;
            let (_, range) = session
                .selected_hunk_ranges()
                .ok_or("select a hunk to explain")?;
            self.herdr
                .as_ref()
                .ok_or("hunk explanations require a selected herdr agent")?
                .explain(&self.snapshot, &path, range)?;
            session
                .set_message("Explanation request queued; only verified ready agents receive it");
            return Ok(true);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('n' | 'p'))
        {
            if session.is_dirty() {
                return Err("submit this file or discard its edits before switching files".into());
            }
            let count = self.paths().len();
            if count == 0 {
                return Ok(true);
            }
            self.selected = if key.code == KeyCode::Char('n') {
                (self.selected + 1).min(count - 1)
            } else {
                self.selected.saturating_sub(1)
            };
            self.replace_session(session)?;
            return Ok(true);
        }
        match key.code {
            KeyCode::Char('S') => {
                let hunk = self.selected_git_hunk(session)?;
                self.repo.stage(&self.inspected, &[hunk])?;
                self.reload(session)?;
                session.set_message(
                    "Selected hunk staged; worktree unchanged. Review acceptance remains separate",
                );
            }
            KeyCode::Char('x') => {
                let id = self.selected_git_hunk(session)?;
                let path = self.current_path()?;
                let hunk = self
                    .inspected
                    .hunks(&path)
                    .iter()
                    .find(|hunk| hunk.id == id)
                    .ok_or("selected hunk is unavailable")?;
                let range = json!({"start": hunk.new_start, "end": hunk.new_end});
                let candidate = self.repo.rejection_candidate(&self.inspected, id)?;
                self.repo.reject(&self.inspected, &[id])?;
                self.record_decision(&path, Vec::new(), vec![range]);
                self.checked_transition(&[candidate])?;
                self.accepted_paths.remove(&path);
                if self.inspected.hunks(&path).is_empty() {
                    self.accepted_paths.insert(path.clone());
                }
                self.selected = self
                    .paths
                    .iter()
                    .position(|candidate| *candidate == path)
                    .unwrap_or(0);
                self.replace_session(session)?;
                session.set_message("Selected hunk rejected; its snapshot-bound decision is retained for explicit submission");
            }
            KeyCode::Char('c') => {
                self.editing_comment = true;
                session.set_message(format!("Review comment: {}", self.comment));
            }
            KeyCode::Char('P') => {
                let patch = self.repo.export_patch(&self.inspected)?;
                let output = self
                    .export
                    .as_mut()
                    .ok_or("supply --export-patch PATH before exporting with P")?;
                output.write(&patch)?;
                session.set_message(format!("Patch written to {}", output.path.display()));
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    fn submit(
        &mut self,
        session: &mut ReviewSession,
        submission: ReviewSubmission,
    ) -> Result<bool> {
        if self.pending_socket_decision.is_some() {
            return Err(
                "socket receipt is pending or failed; patch application will not be repeated"
                    .into(),
            );
        }
        if let Some(merge) = &mut self.merge {
            if merge.stale {
                return Err("Git conflict inputs changed; nothing written".into());
            }
            let result = submission.result.ok_or("merge submission has no result")?;
            self.repo.validate_conflict(&merge.source)?;
            merge.output.write(result.as_bytes())?;
            session.cancel_merge_advice();
            self.reload(session)?;
            self.merge = None;
            session.set_message(
                "Merged file saved; index unchanged. Use git add to stage the resolved file",
            );
            return Ok(false);
        }
        if self.refresh_conflict || self.pending_review.is_some() {
            return Err(
                "a newer repository snapshot is pending; review it before submitting".into(),
            );
        }
        if self.herdr.as_ref().is_some_and(|herdr| herdr.pending) {
            return Err("this review is already awaiting delivery".into());
        }
        self.repo.validate_review(&self.inspected)?;
        if let Some(output) = &self.report {
            output.validate()?;
        }
        if self.herdr.is_some() {
            for path in &self.paths {
                url::Url::from_file_path(self.repo.root().join(path))
                    .map_err(|_| "review path cannot be represented as a file URI")?;
            }
        }
        let mut prepared = self
            .herdr
            .as_ref()
            .map(|herdr| {
                herdr.prepare_report(self.snapshot.clone(), Vec::new(), self.comment.clone())
            })
            .transpose()?;
        if let Ok(path) = self.current_path() {
            let file = self
                .inspected
                .file(&path)
                .ok_or("selected snapshot is unavailable")?;
            if self.preview.is_none()
                && file.content != ContentKind::Binary
                && submission.right.as_bytes() != file.worktree.as_deref().unwrap_or_default()
            {
                let mode = Some(file.replacement_mode());
                self.repo
                    .save_worktree(&self.inspected, &path, submission.right.as_bytes())?;
                self.checked_transition(&[PatchCandidate {
                    path: path.clone(),
                    bytes: Some(submission.right.into_bytes()),
                    mode,
                }])?;
                self.accepted_paths.remove(&path);
            }
            if !self.accepted_paths.contains(&path) {
                let accepted = if let Some(preview) = &self.preview {
                    let candidate = preview
                        .candidates
                        .iter()
                        .find(|candidate| candidate.path == path)
                        .ok_or("selected patch candidate is unavailable")?;
                    let before = self
                        .inspected
                        .file(&path)
                        .and_then(|file| file.worktree.as_deref())
                        .unwrap_or_default();
                    let value = diff_value(
                        before,
                        candidate.bytes.as_deref().unwrap_or_default(),
                        &path,
                        self.options.whitespace.into(),
                    );
                    value["hunks"]
                        .as_array()
                        .map(|hunks| hunks.iter().map(|hunk| hunk["right"].clone()).collect())
                        .unwrap_or_default()
                } else {
                    self.inspected
                        .hunks(&path)
                        .iter()
                        .map(|hunk| json!({"start": hunk.new_start, "end": hunk.new_end}))
                        .collect()
                };
                self.record_decision(&path, accepted, Vec::new());
                self.accepted_paths.insert(path);
            }
            if let Some(next) = self
                .paths
                .iter()
                .position(|path| !self.accepted_paths.contains(path))
            {
                self.selected = next;
                self.replace_session(session)?;
                return Ok(false);
            }
        }
        let permit = self
            .preview
            .as_ref()
            .and_then(|preview| preview.socket_snapshot.as_ref())
            .map(|_| {
                self.socket
                    .as_ref()
                    .ok_or("socket is unavailable")?
                    .reserve_decision()
            })
            .transpose()?;
        if let Some(preview) = self.preview.take() {
            if let Err(error) = self.repo.import_patch(&self.inspected, &preview.patch) {
                self.preview = Some(preview);
                return Err(error.into());
            }
            if let (Some(snapshot), Some(permit)) = (&preview.socket_snapshot, permit) {
                let receipt = permit.record(snapshot, true);
                self.retire_refresh();
                self.pending_socket_decision = Some(PendingSocketDecision {
                    preview,
                    accepted: true,
                    receipt: SocketReceipt::Awaiting(receipt),
                });
                session.set_message("Patch applied; waiting for accepted socket receipt");
                return Ok(false);
            }
            self.checked_transition(&preview.candidates)?;
        }
        let report = if let Some(report) = &mut prepared {
            report.snapshot_id = self.snapshot.clone();
            report.files = self.report_files()?;
            serde_json::to_value(report)?
        } else {
            json!({"id": snapshot_id(), "snapshot_id": self.snapshot, "files": self.decisions.values().flatten().collect::<Vec<_>>(), "comment": self.comment, "submitted": true})
        };
        if let Some(output) = &mut self.report {
            output.write(&serde_json::to_vec_pretty(&report)?)
                .map_err(|error| format!("Review output could not be persisted: {error}; any explicitly saved file changes remain, but no feedback was queued"))?;
        }
        if let (Some(herdr), Some(report)) = (&mut self.herdr, prepared) {
            herdr.submit(
                Arc::clone(&self.inspected),
                self.repo.root().to_path_buf(),
                report,
            )?;
            session
                .set_message("Explicit review submitted; waiting for verified feedback delivery");
            Ok(false)
        } else {
            Ok(true)
        }
    }
}

fn same_review(left: &Review, right: &Review) -> bool {
    left.resolved_revision() == right.resolved_revision()
        && left
            .files()
            .iter()
            .all(|file| right.file(&file.path).is_some() || file.base == file.worktree)
        && right.files().iter().all(|right| {
            left.file(&right.path).is_some_and(|left| {
                left.base == right.base
                    && left.index == right.index
                    && left.worktree == right.worktree
                    && left.mode == right.mode
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{OutputFormat, Whitespace};
    use chvrn_tui::WhitespacePolicy;
    use crossterm::event::KeyEvent;
    use ratatui::{Terminal, backend::TestBackend};
    use std::{fs, process::Command};

    fn with_host(base_name: &str, preview: bool, check: impl FnOnce(&mut RepositoryHost<'_>)) {
        let root = tempfile::tempdir().unwrap();
        for name in ["first.rs", "last.rs"] {
            fs::write(root.path().join(name), "fn before() {}\n").unwrap();
        }
        for args in [
            vec!["init", "-q"],
            vec!["add", "."],
            vec!["commit", "-qm", "initial"],
        ] {
            let output = Command::new("git")
                .args([
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "-c",
                    "core.hooksPath=/dev/null",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .current_dir(root.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        for name in ["first.rs", "last.rs"] {
            fs::write(root.path().join(name), "fn after() {}\n").unwrap();
        }
        let options = Options {
            format: OutputFormat::Auto,
            non_interactive: false,
            jev: false,
            theme: None,
            loaded_theme: Arc::default(),
            herdr: None,
            agent: None,
            whitespace: Whitespace::Exact,
            lsp: None,
            lsp_args: Vec::new(),
            runtime: Default::default(),
        };
        let repo = Repository::discover(root.path()).unwrap();
        let inspected = repo.review(base(base_name), &[]).unwrap();
        let preview = preview.then(|| {
            let patch = b"diff --git a/first.rs b/first.rs\n--- a/first.rs\n+++ b/first.rs\n@@ -1 +1 @@\n-fn after() {}\n+fn candidate() {}\n".to_vec();
            PatchPreview {
                candidates: repo.preview_patch(&inspected, &patch).unwrap(),
                patch,
                snapshot: snapshot_id(),
                socket_snapshot: None,
            }
        });
        let mut host = RepositoryHost {
            repo,
            inspected: Arc::new(inspected),
            args: ReviewArgs {
                base: Some(base_name.into()),
                ..ReviewArgs::default()
            },
            options: &options,
            selected: 0,
            jev: None,
            merge: None,
            preview,
            decisions: BTreeMap::new(),
            accepted_paths: BTreeSet::new(),
            comment: String::new(),
            editing_comment: false,
            snapshot: snapshot_id(),
            report: None,
            export: None,
            paths: Vec::new(),
            watch: FileWatch::new(&[root.path()], true).unwrap(),
            language: LanguageUi::new(&options, root.path()).unwrap(),
            herdr: None,
            socket: None,
            pending_socket_decision: None,
            socket_candidates: VecDeque::new(),
            loading: None,
            needs_refresh: false,
            background: BackgroundDiff::new(),
            pending_review: None,
            pending_request: None,
            refresh_conflict: false,
            finished: None,
        };
        host.reset_paths();
        check(&mut host);
    }

    fn screen(session: &mut ReviewSession, width: u16) -> Vec<String> {
        session.handle(ReviewInput::Resize { width, height: 12 });
        let mut terminal = Terminal::new(TestBackend::new(width, 12)).unwrap();
        terminal.draw(|frame| session.render(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..12)
            .map(|row| {
                (0..width)
                    .map(|column| buffer[(column, row)].symbol())
                    .collect()
            })
            .collect()
    }

    fn git(root: &Path, args: &[&str]) -> std::process::Output {
        Command::new("git")
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .current_dir(root)
            .output()
            .unwrap()
    }

    fn with_conflict(check: impl FnOnce(&mut RepositoryHost<'_>)) {
        with_host("HEAD", false, |host| {
            let root = host.repo.root();
            for args in [
                vec!["add", "."],
                vec!["commit", "-qm", "common"],
                vec!["branch", "-M", "main"],
                vec!["checkout", "-qb", "other"],
            ] {
                assert!(git(root, &args).status.success());
            }
            fs::write(root.join("first.rs"), "fn theirs() {}\n").unwrap();
            assert!(git(root, &["commit", "-qam", "theirs"]).status.success());
            assert!(git(root, &["checkout", "-q", "main"]).status.success());
            fs::write(root.join("first.rs"), "fn ours() {}\n").unwrap();
            assert!(git(root, &["commit", "-qam", "ours"]).status.success());
            assert_eq!(git(root, &["merge", "other"]).status.code(), Some(1));
            host.inspected = Arc::new(
                host.repo
                    .review(Base::Revision("HEAD".into()), &[])
                    .unwrap(),
            );
            host.reset_paths();
            check(host);
        });
    }

    fn key(character: char) -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE))
    }

    fn choose_theirs(session: &mut ReviewSession) -> ReviewSubmission {
        session.handle(ReviewInput::Key(KeyEvent::new(
            KeyCode::Char('t'),
            KeyModifiers::NONE,
        )));
        match session.handle(ReviewInput::Key(KeyEvent::new(
            KeyCode::Char('y'),
            KeyModifiers::NONE,
        ))) {
            ReviewOutcome::Submitted(submission) => submission,
            outcome => panic!("expected confirmed merge submission, got {outcome:?}"),
        }
    }

    #[test]
    fn repository_suggestion_requires_request_application_and_separate_save() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::time::{Duration, Instant};

        with_conflict(|host| {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            host.jev = Some(JevUi::new(
                chvrn_integrations::jev::JevClient::new(chvrn_integrations::jev::JevConfig {
                    api_key: "fixture-key".into(),
                    endpoint: format!("http://{}/v1/systemone", listener.local_addr().unwrap()),
                    timeout: Duration::from_secs(3),
                })
                .unwrap(),
            ));
            let path = host.repo.root().join("first.rs");
            let original = fs::read(&path).unwrap();
            let stages = git(host.repo.root(), &["ls-files", "--stage"]).stdout;
            let mut session = host.session().unwrap();
            assert!(host.input(&mut session, &key('m')).unwrap());
            assert!(screen(&mut session, 200).join("\n").contains("[J]"));
            host.tick(&mut session).unwrap();
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
            assert!(host.input(&mut session, &key('J')).unwrap());
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "no request after J");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                headers.push(byte[0]);
                assert!(headers.len() < 8192);
            }
            let headers = String::from_utf8(headers).unwrap();
            let length: usize = headers
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .unwrap()
                .1
                .trim()
                .parse()
                .unwrap();
            assert!(length <= 24 * 1024);
            let mut body = vec![0; length];
            stream.read_exact(&mut body).unwrap();
            let reply = serde_json::to_vec(&json!({
                "model": "fixture",
                "answers": {"resolution": {
                    "type": "choice", "choice": "theirs", "confidence": 0.8,
                    "probabilities": {"ours": 0.1, "theirs": 0.8, "leave_unresolved": 0.1}
                }}
            }))
            .unwrap();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", reply.len()).unwrap();
            stream.write_all(&reply).unwrap();
            drop(stream);
            let deadline = Instant::now() + Duration::from_secs(3);
            while !session.is_review_modal() {
                host.tick(&mut session).unwrap();
                assert!(Instant::now() < deadline, "suggestion did not arrive");
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(session.pane_text(Pane::Result), "fn ours() {}\n");
            assert_eq!(fs::read(&path).unwrap(), original);
            session.handle(ReviewInput::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            )));
            assert_eq!(session.pane_text(Pane::Result), "fn theirs() {}\n");
            assert_eq!(fs::read(&path).unwrap(), original);
            let submission = match session.handle(ReviewInput::Key(KeyEvent::new(
                KeyCode::Char('y'),
                KeyModifiers::NONE,
            ))) {
                ReviewOutcome::Submitted(submission) => submission,
                outcome => panic!("expected save confirmation, got {outcome:?}"),
            };
            assert!(!host.submit(&mut session, submission).unwrap());
            assert_eq!(fs::read(&path).unwrap(), b"fn theirs() {}\n");
            assert_eq!(
                git(host.repo.root(), &["ls-files", "--stage"]).stdout,
                stages
            );
        });
    }

    #[test]
    fn repository_conflict_merge_saves_only_after_confirmation_and_never_stages() {
        with_conflict(|host| {
            let path = host.repo.root().join("first.rs");
            let original = fs::read(&path).unwrap();
            let stages = git(host.repo.root(), &["ls-files", "--stage"]).stdout;
            let mut session = host.session().unwrap();
            assert!(host.input(&mut session, &key('m')).unwrap());
            assert_eq!(session.unresolved_conflicts(), 1);
            assert_eq!(session.pane_text(Pane::Ours), "fn ours() {}\n");
            assert_eq!(session.pane_text(Pane::Theirs), "fn theirs() {}\n");
            assert_eq!(fs::read(&path).unwrap(), original);
            let submission = choose_theirs(&mut session);
            assert_eq!(fs::read(&path).unwrap(), original);
            assert!(!host.submit(&mut session, submission).unwrap());
            assert_eq!(fs::read(&path).unwrap(), b"fn theirs() {}\n");
            assert_eq!(
                git(host.repo.root(), &["ls-files", "--stage"]).stdout,
                stages
            );
            assert_eq!(session.pane_text(Pane::Right), "fn theirs() {}\n");
        });
    }

    #[test]
    fn repository_merge_does_not_overwrite_changes_made_after_entering_merge() {
        with_conflict(|host| {
            let path = host.repo.root().join("first.rs");
            let mut session = host.session().unwrap();
            assert!(host.input(&mut session, &key('m')).unwrap());
            let submission = choose_theirs(&mut session);
            fs::write(&path, "fn external() {}\n").unwrap();
            assert!(host.submit(&mut session, submission).is_err());
            assert_eq!(fs::read(&path).unwrap(), b"fn external() {}\n");
            assert_eq!(
                git(host.repo.root(), &["show", ":2:first.rs"]).stdout,
                b"fn ours() {}\n"
            );
        });
    }

    #[test]
    fn leaving_repository_merge_invalidates_advice_before_reopening_the_conflict() {
        with_conflict(|host| {
            host.jev = Some(JevUi::new(
                chvrn_integrations::jev::JevClient::new(chvrn_integrations::jev::JevConfig {
                    api_key: "fixture-key".into(),
                    endpoint: "http://127.0.0.1:9/v1/systemone".into(),
                    timeout: std::time::Duration::from_secs(1),
                })
                .unwrap(),
            ));
            let path = host.repo.root().join("first.rs");
            let original = fs::read(&path).unwrap();
            let mut session = host.session().unwrap();
            assert!(host.input(&mut session, &key('m')).unwrap());
            let request = session.begin_merge_advice().unwrap();
            assert!(
                host.input(
                    &mut session,
                    &Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
                )
                .unwrap()
            );
            assert!(host.input(&mut session, &key('m')).unwrap());
            assert!(!session.receive_merge_advice(
                request,
                Ok(chvrn_core::merge_advice::MergeAdviceSuggestion {
                    choice: chvrn_core::merge_advice::MergeAdviceChoice::Theirs,
                    confidence: 0.9,
                    model: "fixture".into(),
                })
            ));
            assert_eq!(session.unresolved_conflicts(), 1);
            assert_eq!(fs::read(&path).unwrap(), original);
        });
    }

    #[test]
    fn repository_merge_without_opt_in_keeps_suggestions_disabled() {
        with_conflict(|host| {
            let mut session = host.session().unwrap();
            assert!(host.input(&mut session, &key('m')).unwrap());
            assert!(matches!(
                session.begin_merge_advice(),
                Err(chvrn_tui::MergeAdviceError::Disabled)
            ));
        });
    }

    #[test]
    fn entering_repository_merge_refuses_to_discard_unsaved_review_edits() {
        with_conflict(|host| {
            let path = host.repo.root().join("first.rs");
            let original = fs::read(&path).unwrap();
            let mut session = host.session().unwrap();
            session
                .replace_pane_text(Pane::Right, "local edits\n")
                .unwrap();
            assert!(host.input(&mut session, &key('m')).is_err());
            assert_eq!(session.pane_text(Pane::Right), "local edits\n");
            assert_eq!(fs::read(&path).unwrap(), original);
        });
    }

    #[test]
    fn repository_footer_keeps_the_filename_right_aligned_after_switching_files() {
        with_host("index", false, |host| {
            let mut session = host.session().unwrap();
            for width in [64, 80, 132] {
                let rows = screen(&mut session, width);
                assert!(rows[11].ends_with("first.rs"), "{}", rows[11]);
                assert!(rows[0].contains("1/2"));
                for key in ["[s]", "[q]", "[?]"] {
                    assert!(rows[11].contains(key), "{}", rows[11]);
                }
            }
            host.input(
                &mut session,
                &Event::Key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL)),
            )
            .unwrap();
            let rows = screen(&mut session, 132);
            assert!(rows[11].ends_with("last.rs"));
            assert!(rows[0].contains("2/2"));
        });
    }

    #[test]
    fn repository_footer_offers_actions_for_the_selected_review_mode() {
        for (base, preview, present, absent) in [
            ("index", false, "[S]", "[x]"),
            ("HEAD", false, "[x]", "[S]"),
            ("index", true, "[x]", "[S]"),
        ] {
            with_host(base, preview, |host| {
                let mut session = host.session().unwrap();
                let rows = screen(&mut session, 132);
                assert!(rows[11].contains(present), "{}", rows[11]);
                assert!(!rows[11].contains(absent), "{}", rows[11]);
                assert!(rows[11].contains("[Ctrl-N/P]"));
                assert!(rows[11].contains("[c]"));
                assert!(rows[11].ends_with("first.rs"));
                if preview {
                    assert!(rows[0].contains("PATCH PREVIEW"));
                    assert!(rows[0].contains("1/1"));
                }
                let narrow = screen(&mut session, 32);
                for key in ["[s]", "[q]", "[?]"] {
                    assert!(narrow[11].contains(key), "{}", narrow[11]);
                }
            });
        }
    }

    #[test]
    fn repository_editing_and_discard_confirmation_override_navigation_shortcuts() {
        with_host("index", false, |host| {
            let mut session = host.session().unwrap();
            session.go_to(Pane::Right, 0, 0);
            session.handle(ReviewInput::Key(KeyEvent::new(
                KeyCode::Char('i'),
                KeyModifiers::NONE,
            )));
            let rows = screen(&mut session, 132);
            assert!(rows[11].contains("[Esc]"));
            assert!(rows[11].ends_with("first.rs"));
            for key in ["[S]", "[x]", "[c]", "[s]", "[q]"] {
                assert!(!rows[11].contains(key));
            }
            session.handle(ReviewInput::Paste("edited ".into()));
            session.handle(ReviewInput::Key(KeyEvent::new(
                KeyCode::Esc,
                KeyModifiers::NONE,
            )));
            assert_eq!(
                session.handle(ReviewInput::Key(KeyEvent::new(
                    KeyCode::Char('q'),
                    KeyModifiers::NONE
                ))),
                ReviewOutcome::DiscardRequired,
            );
            let rows = screen(&mut session, 132);
            assert!(rows[11].contains("discards"));
            assert!(!rows[11].contains("first.rs"));
            assert!(!rows[11].contains("[s]"));
        });
    }

    fn queue_external_refresh(host: &mut RepositoryHost<'_>, session: &mut ReviewSession) {
        host.watch = FileWatch::new(&[], false).unwrap();
        let path = host.current_path().unwrap();
        fs::write(host.repo.root().join(&path), "fn refreshed() {}\n").unwrap();
        let incoming = Arc::new(
            host.repo
                .review(host.inspected.base().clone(), &[])
                .unwrap(),
        );
        let file = incoming.file(&path).unwrap();
        let request = session.request_diff_snapshots(
            snapshot(file.base.as_deref().unwrap_or_default()).unwrap(),
            snapshot(file.worktree.as_deref().unwrap_or_default()).unwrap(),
        );
        host.pending_request = Some(request.id());
        host.pending_review = Some(incoming);
        host.background.request(request);
    }

    #[test]
    fn changing_files_during_refresh_retires_old_presentation_authority() {
        with_host("index", false, |host| {
            let mut session = host.session().unwrap();
            queue_external_refresh(host, &mut session);
            host.input(
                &mut session,
                &Event::Key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL)),
            )
            .unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            loop {
                host.tick(&mut session).unwrap();
                if host.pending_review.is_none() && host.loading.is_none() && !host.needs_refresh {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "refresh never retired: loading={}, pending={}, needs_refresh={}",
                    host.loading.is_some(),
                    host.pending_review.is_some(),
                    host.needs_refresh,
                );
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert_eq!(host.current_path().unwrap(), Path::new("last.rs"));
            assert_eq!(session.pane_text(Pane::Right), "fn after() {}\n");
            assert_eq!(
                host.inspected
                    .file(Path::new("first.rs"))
                    .unwrap()
                    .worktree
                    .as_deref(),
                Some(b"fn refreshed() {}\n".as_slice()),
            );
        });
    }

    #[test]
    fn whitespace_change_during_refresh_publishes_matching_repository_authority() {
        with_host("index", false, |host| {
            let mut session = host.session().unwrap();
            queue_external_refresh(host, &mut session);
            session.set_whitespace_policy(WhitespacePolicy::IgnoreEdge);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while host.pending_review.is_some() {
                host.tick(&mut session).unwrap();
                assert!(
                    std::time::Instant::now() < deadline,
                    "refresh never published"
                );
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert_eq!(session.pane_text(Pane::Right), "fn refreshed() {}\n");
            assert_eq!(
                host.inspected
                    .file(Path::new("first.rs"))
                    .unwrap()
                    .worktree
                    .as_deref(),
                Some(session.pane_text(Pane::Right).as_bytes()),
            );
            assert!(host.repo.validate_review(&host.inspected).is_ok());
        });
    }

    fn with_socket_preview(check: impl FnOnce(&mut RepositoryHost<'_>, &mut ReviewSession)) {
        use std::io::{Read, Write};
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::net::UnixStream;
        with_host("index", false, |host| {
            host.watch = FileWatch::new(&[], false).unwrap();
            let directory = tempfile::Builder::new()
                .permissions(fs::Permissions::from_mode(0o700))
                .tempdir_in("/tmp")
                .unwrap();
            let path = directory.path().join("review.sock");
            host.socket = Some(
                SocketUi::new(
                    &path,
                    host.repo.root(),
                    &host.inspected,
                    &host.snapshot,
                    &host.options.runtime,
                )
                .unwrap(),
            );
            let request = serde_json::to_vec(&json!({
                "type": "patch_candidate",
                "snapshot": format!("{}:0", host.snapshot),
                "path": "first.rs",
                "patch": "--- a/first.rs\n+++ b/first.rs\n@@ -1 +1 @@\n-fn after() {}\n+fn candidate() {}\n",
            })).unwrap();
            let mut connection = UnixStream::connect(&path).unwrap();
            connection
                .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                .unwrap();
            connection
                .set_write_timeout(Some(std::time::Duration::from_secs(3)))
                .unwrap();
            connection
                .write_all(&(request.len() as u32).to_be_bytes())
                .unwrap();
            connection.write_all(&request).unwrap();
            let mut length = [0; 4];
            connection.read_exact(&mut length).unwrap();
            let size = u32::from_be_bytes(length) as usize;
            assert!(size <= 65_536);
            let mut response = vec![0; size];
            connection.read_exact(&mut response).unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&response).unwrap()["status"],
                "queued"
            );
            let mut session = host.session().unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while host.socket_candidates.is_empty() {
                host.tick(&mut session).unwrap();
                assert!(
                    std::time::Instant::now() < deadline,
                    "candidate was not delivered"
                );
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            host.input(&mut session, &key('v')).unwrap();
            check(host, &mut session);
        });
    }

    fn preview_submission(session: &ReviewSession) -> ReviewSubmission {
        ReviewSubmission {
            left: session.pane_text(Pane::Left),
            right: session.pane_text(Pane::Right),
            result: None,
        }
    }

    #[test]
    fn socket_receipt_gates_refresh_and_prevents_duplicate_application() {
        for accepted in [true, false] {
            with_socket_preview(|host, session| {
                let before = host.snapshot.clone();
                if accepted {
                    assert!(!host.submit(session, preview_submission(session)).unwrap());
                } else {
                    host.input(session, &key('x')).unwrap();
                }
                let expected = if accepted {
                    b"fn candidate() {}\n".as_slice()
                } else {
                    b"fn after() {}\n".as_slice()
                };
                assert_eq!(
                    fs::read(host.repo.root().join("first.rs")).unwrap(),
                    expected
                );
                assert_eq!(host.snapshot, before);
                assert!(host.submit(session, preview_submission(session)).is_err());
                assert!(
                    host.input(
                        session,
                        &Event::Key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL,))
                    )
                    .is_err()
                );
                host.needs_refresh = true;
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
                while host.pending_socket_decision.is_some() {
                    host.tick(session).unwrap();
                    assert!(
                        std::time::Instant::now() < deadline,
                        "receipt did not complete"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                assert_ne!(host.snapshot, before);
                assert_eq!(
                    fs::read(host.repo.root().join("first.rs")).unwrap(),
                    expected
                );
                assert_eq!(
                    host.inspected
                        .file(Path::new("first.rs"))
                        .unwrap()
                        .worktree
                        .as_deref(),
                    Some(expected)
                );
                assert!(host.preview.is_none());
            });
        }
    }

    #[test]
    fn full_socket_decision_queue_refuses_application_without_consuming_preview() {
        with_socket_preview(|host, session| {
            let permits: Vec<_> = (0..32)
                .map(|_| host.socket.as_ref().unwrap().reserve_decision().unwrap())
                .collect();
            assert!(host.submit(session, preview_submission(session)).is_err());
            assert!(host.preview.is_some());
            assert!(host.pending_socket_decision.is_none());
            assert_eq!(
                fs::read(host.repo.root().join("first.rs")).unwrap(),
                b"fn after() {}\n"
            );
            drop(permits);
            assert!(!host.submit(session, preview_submission(session)).unwrap());
            assert_eq!(
                fs::read(host.repo.root().join("first.rs")).unwrap(),
                b"fn candidate() {}\n"
            );
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while host.pending_socket_decision.is_some() {
                host.tick(session).unwrap();
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        });
    }

    #[test]
    fn post_receipt_external_change_blocks_approval_without_reapplying_the_patch() {
        with_socket_preview(|host, session| {
            assert!(!host.submit(session, preview_submission(session)).unwrap());
            fs::write(host.repo.root().join("first.rs"), b"fn external() {}\n").unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            loop {
                if host.tick(session).is_err() {
                    break;
                }
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert!(matches!(
                host.pending_socket_decision,
                Some(PendingSocketDecision {
                    accepted: true,
                    receipt: SocketReceipt::Failed(_),
                    ..
                })
            ));
            assert!(host.submit(session, preview_submission(session)).is_err());
            host.tick(session).unwrap();
            assert_eq!(
                fs::read(host.repo.root().join("first.rs")).unwrap(),
                b"fn external() {}\n"
            );
            assert!(!host.input(session, &key('q')).unwrap());
            assert!(host.finished.is_none());
        });
    }

    #[test]
    fn failed_socket_receipt_retains_applied_bytes_without_retrying_or_approving() {
        with_socket_preview(|host, session| {
            host.socket
                .as_ref()
                .unwrap()
                .refresh(&host.inspected, "replacement-authority")
                .unwrap();
            assert!(!host.submit(session, preview_submission(session)).unwrap());
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            loop {
                if host.tick(session).is_err() {
                    break;
                }
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert!(matches!(
                host.pending_socket_decision,
                Some(PendingSocketDecision {
                    accepted: true,
                    receipt: SocketReceipt::Failed(_),
                    ..
                })
            ));
            assert!(host.submit(session, preview_submission(session)).is_err());
            host.tick(session).unwrap();
            assert_eq!(
                fs::read(host.repo.root().join("first.rs")).unwrap(),
                b"fn candidate() {}\n"
            );
            assert!(!host.input(session, &key('q')).unwrap());
            assert!(host.finished.is_none());
        });
    }
}
