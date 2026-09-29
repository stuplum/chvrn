use crate::files::{GuardedFile, read_bytes};
use crate::herdr_ui::{self, HerdrUi, Notice};
use crate::language::LanguageUi;
use crate::socket_ui::SocketUi;
use crate::standalone::{diff_value, print_value, snapshot};
use crate::terminal::{self, ReviewHost};
use crate::watch::{BackgroundDiff, FileWatch};
use crate::{HerdrMode, Options, Result, ReviewArgs};
use chvrn_core::TextSnapshot;
use chvrn_git::{Base, ContentKind, PatchCandidate, Repository, Review};
use chvrn_integrations::herdr::{HunkRange, ReviewedFile};
use chvrn_tui::{Pane, ReviewInput, ReviewOutcome, ReviewSession, ReviewSubmission};
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
    if matches!(options.herdr, Some(HerdrMode::Gate)) && !options.interactive() {
        return Err(
            "an explicit review gate requires an interactive terminal; no review was accepted"
                .into(),
        );
    }
    let repo = Repository::discover(&std::env::current_dir()?)?;
    if args.open_companion {
        return herdr_ui::open_companion(&args, options, repo.root());
    }
    let patch = args.patch.as_deref().map(read_bytes).transpose()?;
    let inspected = match &patch {
        Some(patch) => repo.review_patch(base(&args.base), patch)?,
        None => repo.review(base(&args.base), &args.paths)?,
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
    if !options.interactive() {
        let files: Vec<Value> = match &preview {
            Some(preview) => preview
                .candidates
                .iter()
                .map(|candidate| {
                    let before = inspected
                        .file(&candidate.path)
                        .and_then(|file| file.worktree.as_deref())
                        .unwrap_or_default();
                    let mut value = diff_value(
                        before,
                        candidate.bytes.as_deref().unwrap_or_default(),
                        &candidate.path,
                        options.whitespace.into(),
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
                    value["path"] = json!(file.path.to_string_lossy());
                    value["path_bytes"] = path_value(&file.path);
                    value["deleted"] = json!(file.worktree.is_none());
                    value
                })
                .collect(),
        };
        let different = files
            .iter()
            .any(|file| file["equal"] != true || file["deleted"] == true);
        print_value(
            &json!({"base": args.base, "patch_preview": preview.is_some(), "files": files}),
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
    let watch = FileWatch::new(&[repo.root()], true)?;
    let language = LanguageUi::new(options, repo.root())?;
    let herdr = HerdrUi::new(options)?;
    let socket = args
        .socket
        .as_deref()
        .map(|path| SocketUi::new(path, repo.root(), &inspected, &snapshot))
        .transpose()?;
    let mut host = RepositoryHost {
        repo,
        inspected: Arc::new(inspected),
        args,
        options,
        selected: 0,
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
        socket_candidates: VecDeque::new(),
        loading: None,
        needs_refresh: false,
        background: BackgroundDiff::new(),
        pending_review: None,
        pending_generation: 0,
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

struct RepositoryHost<'a> {
    repo: Repository,
    inspected: Arc<Review>,
    args: ReviewArgs,
    options: &'a Options,
    selected: usize,
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
    socket_candidates: VecDeque<chvrn_integrations::socket::PatchCandidate>,
    loading: Option<std::sync::mpsc::Receiver<Result<Option<RepositoryRefresh>>>>,
    needs_refresh: bool,
    background: BackgroundDiff,
    pending_review: Option<Arc<Review>>,
    pending_generation: u64,
    refresh_conflict: bool,
    finished: Option<u8>,
}

impl RepositoryHost<'_> {
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

    fn session(&self) -> Result<ReviewSession> {
        if self.paths.is_empty() {
            let mut session = ReviewSession::two_way("", "");
            session.set_read_only(Pane::Left, true);
            session.set_read_only(Pane::Right, true);
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
        session.set_paths(&path, &path);
        session.set_whitespace_policy(self.options.whitespace.into());
        session.set_read_only(Pane::Left, true);
        session.set_read_only(Pane::Right, binary || self.preview.is_some());
        session.set_message(format!(
            "{}/{} {} | Ctrl-N/P files; S stage; x reject; s accept; c comment{}",
            self.selected + 1,
            self.paths().len(),
            path.display(),
            if self.preview.is_some() {
                " | PATCH PREVIEW: no disk changes until all files accepted"
            } else {
                ""
            }
        ));
        Ok(session)
    }

    fn reload(&mut self, session: &mut ReviewSession) -> Result<()> {
        let path = self.current_path().ok();
        self.inspected = Arc::new(self.repo.review(base(&self.args.base), &self.args.paths)?);
        self.reset_paths();
        self.invalidate()?;
        self.selected = path
            .and_then(|path| self.paths.iter().position(|candidate| *candidate == path))
            .unwrap_or(0);
        if self.paths.is_empty() {
            *session = ReviewSession::two_way("", "");
            session.set_read_only(Pane::Left, true);
            session.set_read_only(Pane::Right, true);
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
            herdr.invalidate(&self.snapshot);
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
                herdr.invalidate(&self.snapshot);
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
        if !rebuild && session.accepted_generation() != self.pending_generation {
            return Err("the latest repository refresh is still computing; no snapshot authority was changed".into());
        }
        let old_path = self.current_path().ok();
        if let Some(review) = self.pending_review.take() {
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
        let path = self
            .repo
            .root()
            .join(self.current_path().unwrap_or_default());
        self.language.tick(session, &path)?;
        if self.language.viewing_definition() {
            return Ok(());
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
                            self.pending_generation = request.generation();
                            self.background.request(request);
                        } else {
                            self.pending_generation = 0;
                            self.refresh_conflict = session.is_dirty();
                        }
                        self.pending_review = Some(refresh.incoming);
                        if self.pending_generation == 0 && !self.refresh_conflict {
                            self.publish_refresh(session, true)?;
                        }
                    }
                }
            }
        }
        if self.needs_refresh && self.loading.is_none() {
            self.needs_refresh = false;
            let root = self.repo.root().to_path_buf();
            let base = base(&self.args.base);
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
            let generation = completion.generation();
            let outcome = session.handle(ReviewInput::DiffReady(completion));
            if generation == self.pending_generation {
                if outcome == ReviewOutcome::RefreshConflict {
                    self.refresh_conflict = true;
                    session.set_message("External edits conflict with local edits. R discards local edits and reloads; submission is blocked");
                } else if session.accepted_generation() == generation {
                    self.publish_refresh(session, false)?;
                }
            }
        }
        Ok(())
    }

    fn input(&mut self, session: &mut ReviewSession, event: &Event) -> Result<bool> {
        let path = self
            .repo
            .root()
            .join(self.current_path().unwrap_or_default());
        if self.language.input(session, &path, event)? {
            return Ok(true);
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
        if matches!(key.code, KeyCode::Char('q'))
            && self.herdr.as_ref().is_some_and(|herdr| herdr.pending)
        {
            return Err("Submitted feedback is still pending. Resolve the agent's own prompt separately; chvrn will not approve it or silently discard feedback".into());
        }
        if matches!(key.code, KeyCode::Char('S' | 'x' | 'v'))
            && self.herdr.as_ref().is_some_and(|herdr| herdr.pending)
        {
            return Err(
                "review mutations are blocked while submitted feedback awaits delivery".into(),
            );
        }
        if key.code == KeyCode::Char('R') && self.refresh_conflict {
            if self.pending_generation != 0 {
                session.handle(ReviewInput::DiscardAndReload);
            }
            self.publish_refresh(session, self.pending_generation == 0)?;
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
            *session = self.session()?;
            return Ok(true);
        }
        if key.code == KeyCode::Char('x') && self.preview.is_some() {
            let preview = self.preview.take().ok_or("patch preview is unavailable")?;
            if let Some(snapshot) = preview.socket_snapshot {
                self.socket
                    .as_ref()
                    .ok_or("socket is unavailable")?
                    .decision(&snapshot, false)?;
                self.reload(session)?;
                session.set_message("Patch candidate explicitly declined; files unchanged");
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
            *session = self.session()?;
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
                *session = self.session()?;
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
                let mode = Some(file.mode.unwrap_or(0o100644));
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
                *session = self.session()?;
                return Ok(false);
            }
        }
        if let Some(preview) = self.preview.take() {
            if let Err(error) = self.repo.import_patch(&self.inspected, &preview.patch) {
                self.preview = Some(preview);
                return Err(error.into());
            }
            if let Some(snapshot) = &preview.socket_snapshot {
                self.socket
                    .as_ref()
                    .ok_or("socket is unavailable")?
                    .decision(snapshot, true)
                    .map_err(|error| {
                        format!(
                            "Patch was applied, but its socket receipt was not confirmed: {error}"
                        )
                    })?;
            }
            self.checked_transition(&preview.candidates)?;
            if preview.socket_snapshot.is_some() {
                self.decisions.clear();
                self.accepted_paths.clear();
                self.selected = 0;
                *session = self.session()?;
                session.set_message(
                    "Patch candidate explicitly accepted and applied; accepted receipt confirmed",
                );
                return Ok(false);
            }
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
