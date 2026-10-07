use std::{
    collections::HashMap,
    ops::Range,
    path::{Path, PathBuf},
    sync::{
        Arc, LazyLock,
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
};

use chvrn_core::{
    TextSnapshot,
    diff::{AlignedRow, Diff, Hunk, RenderedLine, WhitespacePolicy, intraline_spans},
    edit::{CapturedText, TextBuffer},
    merge::{ConflictId, ConflictResolution, Merge},
    syntax::{HighlightSpan, SyntaxCatalog},
};
use unicode_segmentation::UnicodeSegmentation;

use crate::{Cursor, Pane, ReviewOutcome, ReviewSubmission, Theme, render::RenderPalette, text};

#[derive(Clone, Default)]
pub(crate) struct SyntaxSource {
    pub(crate) path: PathBuf,
}

#[derive(Default)]
pub(crate) struct SyntaxPaint {
    pub(crate) spans: Vec<HighlightSpan>,
    pub(crate) error: Option<String>,
}

impl SyntaxSource {
    fn highlight(&self, snapshot: &TextSnapshot) -> SyntaxPaint {
        static CATALOGUE: LazyLock<Result<SyntaxCatalog, String>> =
            LazyLock::new(|| SyntaxCatalog::configured().map_err(|error| error.to_string()));
        match CATALOGUE
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|catalogue| {
                catalogue
                    .highlight(&self.path, snapshot)
                    .map_err(|error| error.to_string())
            }) {
            Ok(spans) => SyntaxPaint { spans, error: None },
            Err(error) => SyntaxPaint {
                spans: Vec::new(),
                error: Some(error),
            },
        }
    }
}

pub struct DiffRequest {
    generation: u64,
    left: TextSnapshot,
    right: TextSnapshot,
    policy: WhitespacePolicy,
    left_source: SyntaxSource,
    right_source: SyntaxSource,
}

pub struct DiffCompletion {
    pub(crate) generation: u64,
    pub(crate) left: TextSnapshot,
    pub(crate) right: TextSnapshot,
    pub(crate) diff: Diff,
    pub(crate) policy: WhitespacePolicy,
    pub(crate) left_syntax: SyntaxPaint,
    pub(crate) rows: Vec<ViewRow>,
    pub(crate) projection: [PaneProjection; 5],
    pub(crate) right_syntax: SyntaxPaint,
}

impl DiffRequest {
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn compute(self) -> DiffCompletion {
        let left = self.left;
        let right = self.right;
        let diff = Diff::between(&left, &right, self.policy);
        let rows = two_way_rows(&diff, &left, &right);
        let projection = two_way_projection(&rows, &diff);
        let left_syntax = self.left_source.highlight(&left);
        let right_syntax = self.right_source.highlight(&right);
        DiffCompletion {
            generation: self.generation,
            left,
            right,
            diff,
            projection,
            rows,
            policy: self.policy,
            left_syntax,
            right_syntax,
        }
    }
}

impl DiffCompletion {
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

const SYNC_DIFF_LIMIT_BYTES: usize = 64 * 1024;

enum LocalWork {
    TwoWay {
        generation: u64,
        left: CapturedText,
        right: CapturedText,
        policy: WhitespacePolicy,
        left_source: SyntaxSource,
        right_source: SyntaxSource,
    },
    ThreeWay {
        generation: u64,
        ours: CapturedText,
        result: CapturedText,
        theirs: CapturedText,
        conflicts: Vec<ConflictRegion>,
        resolved: Vec<ResolvedConflict>,
        policy: WhitespacePolicy,
        sources: [SyntaxSource; 3],
    },
}

enum LocalComputed {
    TwoWay(DiffCompletion),
    ThreeWay {
        ours: TextSnapshot,
        result: TextSnapshot,
        theirs: TextSnapshot,
        rows: Vec<ViewRow>,
        projection: [PaneProjection; 5],
        syntax: [SyntaxPaint; 3],
    },
}

struct LocalResult {
    generation: u64,
    computed: LocalComputed,
}

impl LocalWork {
    fn compute(self) -> LocalResult {
        match self {
            Self::TwoWay {
                generation,
                left,
                right,
                policy,
                left_source,
                right_source,
            } => {
                let request = DiffRequest {
                    generation,
                    left: left.snapshot(),
                    right: right.snapshot(),
                    policy,
                    left_source,
                    right_source,
                };
                LocalResult {
                    generation,
                    computed: LocalComputed::TwoWay(request.compute()),
                }
            }
            Self::ThreeWay {
                generation,
                ours,
                result,
                theirs,
                conflicts,
                resolved,
                policy,
                sources,
            } => {
                let ours = ours.snapshot();
                let result = result.snapshot();
                let theirs = theirs.snapshot();
                let panes = [
                    TextPane::new(ours.clone(), true),
                    TextPane::new(result.clone(), false),
                    TextPane::new(theirs.clone(), true),
                ];
                let (rows, left_bands, right_bands, left_actions, right_actions) = three_way_rows(
                    &panes[0], &panes[1], &panes[2], &conflicts, &resolved, policy,
                );
                let projection = three_way_projection(
                    &rows,
                    left_bands,
                    right_bands,
                    left_actions,
                    right_actions,
                );
                let [ours_source, result_source, theirs_source] = sources;
                let syntax = [
                    ours_source.highlight(&ours),
                    result_source.highlight(&result),
                    theirs_source.highlight(&theirs),
                ];
                LocalResult {
                    generation,
                    computed: LocalComputed::ThreeWay {
                        ours,
                        result,
                        theirs,
                        rows,
                        projection,
                        syntax,
                    },
                }
            }
        }
    }
}

struct LocalWorker {
    requests: SyncSender<LocalWork>,
    results: Receiver<LocalResult>,
    queued: Option<LocalWork>,
}

impl LocalWorker {
    fn new() -> Self {
        let (requests, input) = mpsc::sync_channel::<LocalWork>(1);
        let (output, results) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(mut work) = input.recv() {
                while let Ok(newer) = input.try_recv() {
                    work = newer;
                }
                if output.send(work.compute()).is_err() {
                    break;
                }
            }
        });
        Self {
            requests,
            results,
            queued: None,
        }
    }

    fn flush(&mut self) {
        if let Some(work) = self.queued.take() {
            if let Err(TrySendError::Full(work)) = self.requests.try_send(work) {
                self.queued = Some(work);
            }
        }
    }

    fn request(&mut self, work: LocalWork) {
        self.queued = Some(work);
        self.flush();
    }

    fn latest(&mut self) -> Option<LocalResult> {
        self.flush();
        self.results.try_iter().last()
    }
}

pub(crate) struct TextPane {
    pub(crate) buffer: TextBuffer,
    pub(crate) snapshot: TextSnapshot,
    pub(crate) syntax: SyntaxPaint,
    pub(crate) syntax_source: SyntaxSource,
    pub(crate) read_only: bool,
}

impl TextPane {
    pub(crate) fn new(snapshot: TextSnapshot, read_only: bool) -> Self {
        Self {
            buffer: TextBuffer::new(snapshot.clone()),
            snapshot,
            syntax: SyntaxPaint::default(),
            syntax_source: SyntaxSource::default(),
            read_only,
        }
    }

    pub(crate) fn refresh(&mut self) {
        self.snapshot = self.buffer.snapshot();
        self.update_syntax();
    }

    pub(crate) fn update_syntax(&mut self) {
        self.syntax = self.syntax_source.highlight(&self.snapshot);
    }
}

#[derive(Clone)]
pub(crate) enum ResolutionChoice {
    Ours,
    Theirs,
    Both,
    Manual(String),
}

#[derive(Clone)]
pub(crate) struct MergeMetadata {
    pub(crate) conflicts: Vec<ConflictRegion>,
    pub(crate) resolutions: Vec<(ConflictId, ResolutionChoice)>,
    pub(crate) resolved: Vec<ResolvedConflict>,
}

#[derive(Clone)]
pub(crate) struct ConflictRegion {
    pub(crate) id: ConflictId,
    pub(crate) chars: Range<usize>,
    pub(crate) ours: Range<usize>,
    pub(crate) theirs: Range<usize>,
    pub(crate) changed: bool,
}

#[derive(Clone)]
pub(crate) struct ResolvedConflict {
    pub(crate) id: ConflictId,
    pub(crate) result: Range<usize>,
    pub(crate) ours: Range<usize>,
    pub(crate) theirs: Range<usize>,
    pub(crate) ours_accepted: bool,
    pub(crate) theirs_accepted: bool,
}

pub(crate) enum Mode {
    TwoWay {
        left: TextPane,
        right: TextPane,
        diff: Diff,
    },
    ThreeWay {
        base: TextSnapshot,
        ours: TextPane,
        result: TextPane,
        theirs: TextPane,
        merge: Merge,
        conflicts: Vec<ConflictRegion>,
        resolutions: Vec<(ConflictId, ResolutionChoice)>,
        resolved: Vec<ResolvedConflict>,
        undo_meta: Vec<MergeMetadata>,
        redo_meta: Vec<MergeMetadata>,
    },
}

#[derive(Clone, Copy)]
pub(crate) struct DisplayStop {
    pub(crate) byte: usize,
    pub(crate) scalar: usize,
    pub(crate) cells: usize,
    pub(crate) grapheme: usize,
}

#[derive(Clone)]
pub(crate) struct ViewLine {
    pub(crate) number: usize,
    pub(crate) text: String,
    pub(crate) byte_start: usize,
    pub(crate) changed: Vec<Range<usize>>,
    pub(crate) stops: Arc<[DisplayStop]>,
}

impl ViewLine {
    pub(crate) fn new(number: usize, text: String, byte_start: usize) -> Self {
        let mut stops = vec![DisplayStop {
            byte: 0,
            scalar: 0,
            cells: 0,
            grapheme: 0,
        }];
        let mut scalar = 0;
        let mut cells = 0;
        for (index, (byte, grapheme)) in text.grapheme_indices(true).enumerate() {
            if index > 0 && index % 128 == 0 {
                stops.push(DisplayStop {
                    byte,
                    scalar,
                    cells,
                    grapheme: index,
                });
            }
            scalar += grapheme.chars().count();
            cells += text::display_cell_width(grapheme, cells);
        }
        Self {
            number,
            text,
            byte_start,
            changed: Vec::new(),
            stops: stops.into(),
        }
    }

    pub(crate) fn stop_at_cell(&self, cell: usize) -> DisplayStop {
        self.stops[self
            .stops
            .partition_point(|stop| stop.cells <= cell)
            .saturating_sub(1)]
    }

    pub(crate) fn stop_at_grapheme(&self, column: usize) -> DisplayStop {
        self.stops[self
            .stops
            .partition_point(|stop| stop.grapheme <= column)
            .saturating_sub(1)]
    }

    pub(crate) fn cells_before(&self, column: usize) -> usize {
        let stop = self.stop_at_grapheme(column);
        self.text[stop.byte..]
            .graphemes(true)
            .take(column - stop.grapheme)
            .fold(stop.cells, |cells, grapheme| {
                cells + text::display_cell_width(grapheme, cells)
            })
    }
}

#[derive(Clone, Default)]
pub(crate) struct ViewRow {
    pub(crate) left: Option<ViewLine>,
    pub(crate) right: Option<ViewLine>,
    pub(crate) ours: Option<ViewLine>,
    pub(crate) result: Option<ViewLine>,
    pub(crate) theirs: Option<ViewLine>,
    pub(crate) hunk: Option<usize>,
}

impl ViewRow {
    pub(crate) fn line(&self, pane: Pane) -> Option<&ViewLine> {
        match pane {
            Pane::Left => self.left.as_ref(),
            Pane::Right => self.right.as_ref(),
            Pane::Ours => self.ours.as_ref(),
            Pane::Result => self.result.as_ref(),
            Pane::Theirs => self.theirs.as_ref(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ChangeKind {
    Modified,
    Added,
    Removed,
    Conflict,
    Resolved,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ChangeBand {
    pub(crate) left: Range<usize>,
    pub(crate) right: Range<usize>,
    pub(crate) hunk: Option<usize>,
    pub(crate) resolved: Option<ConflictId>,
    pub(crate) kind: ChangeKind,
}

#[derive(Default)]
pub(crate) struct PaneProjection {
    pub(crate) lines: Vec<usize>,
    pub(crate) bands: Vec<ChangeBand>,
    pub(crate) actions: Vec<ChangeBand>,
}

#[derive(Clone, Copy)]
pub(crate) struct HistoryAction {
    pub(crate) pane: Pane,
    pub(crate) has_text_history: bool,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum RepositoryReviewMode {
    Index,
    Revision,
    PatchPreview,
}

pub(crate) struct RepositoryReview {
    pub(crate) mode: RepositoryReviewMode,
    pub(crate) position: String,
}

pub struct ReviewSession {
    pub(crate) mode: Mode,
    pub(crate) rows: Vec<ViewRow>,
    pub(crate) projection: [PaneProjection; 5],
    pub(crate) focus: Pane,
    pub(crate) aligned_row: usize,
    pub(crate) column: usize,
    pub(crate) selected: Option<usize>,
    pub(crate) scroll: usize,
    pub(crate) horizontal: HashMap<Pane, usize>,
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) editing: bool,
    pub(crate) help: bool,
    pub(crate) help_lines: Vec<String>,
    pub(crate) help_scroll: usize,
    pub(crate) confirming_discard: bool,
    pub(crate) confirming_merge: bool,
    pub(crate) changed: bool,
    pub(crate) refresh_conflict: bool,
    pub(crate) pending: Option<DiffCompletion>,
    pub(crate) latest_generation: u64,
    pub(crate) accepted_generation: u64,
    pub(crate) message: String,
    pub(crate) repository_review: Option<RepositoryReview>,
    pub(crate) left_path: Option<PathBuf>,
    pub(crate) right_path: Option<PathBuf>,
    pub(crate) output_path: Option<PathBuf>,
    pub(crate) whitespace: WhitespacePolicy,
    pub(crate) undo_actions: Vec<HistoryAction>,
    pub(crate) redo_actions: Vec<HistoryAction>,
    pub(crate) local_generation: u64,
    pub(crate) local_pending: bool,
    local_worker: Option<LocalWorker>,
    pub(crate) merge_advice: crate::merge_advice::MergeAdviceState,
    pub(crate) duplicate_additions: crate::duplicate_additions::DuplicateAdditions,
    theme: Arc<Theme>,
    pub(crate) palette: RenderPalette,
}

impl ReviewSession {
    pub fn two_way(left: &str, right: &str) -> Self {
        let left = snapshot(left);
        let right = snapshot(right);
        let diff = Diff::between(&left, &right, WhitespacePolicy::Exact);
        let theme = Arc::new(Theme::default());
        let mut session = Self {
            mode: Mode::TwoWay {
                left: TextPane::new(left, false),
                right: TextPane::new(right, false),
                diff,
            },
            rows: Vec::new(),
            projection: std::array::from_fn(|_| PaneProjection::default()),
            focus: Pane::Left,
            aligned_row: 0,
            column: 0,
            selected: None,
            scroll: 0,
            horizontal: HashMap::new(),
            width: 80,
            height: 24,
            editing: false,
            help: false,
            help_lines: Vec::new(),
            help_scroll: 0,
            confirming_discard: false,
            confirming_merge: false,
            changed: false,
            refresh_conflict: false,
            pending: None,
            latest_generation: 0,
            accepted_generation: 0,
            message: String::new(),
            repository_review: None,
            left_path: None,
            right_path: None,
            output_path: None,
            whitespace: WhitespacePolicy::Exact,
            undo_actions: Vec::new(),
            redo_actions: Vec::new(),
            local_generation: 0,
            local_pending: false,
            local_worker: None,
            merge_advice: crate::merge_advice::MergeAdviceState::default(),
            duplicate_additions: crate::duplicate_additions::DuplicateAdditions::default(),
            palette: RenderPalette::compile(&theme),
            theme,
        };
        session.rebuild_rows();
        session
    }

    pub fn three_way(base: &str, ours: &str, theirs: &str) -> Self {
        let base = snapshot(base);
        let ours = snapshot(ours);
        let theirs = snapshot(theirs);
        let merge = Merge::three_way(&base, &ours, &theirs);
        let result = merge.preview();
        let conflicts = merge
            .preview_conflicts()
            .into_iter()
            .map(|span| {
                let conflict = merge
                    .conflicts()
                    .iter()
                    .find(|conflict| conflict.id == span.id)
                    .expect("preview conflict must retain its source provenance");
                ConflictRegion {
                    id: span.id,
                    chars: span.result_chars,
                    ours: conflict.ours_lines.clone(),
                    theirs: conflict.theirs_lines.clone(),
                    changed: false,
                }
            })
            .collect();
        let theme = Arc::new(Theme::default());
        let duplicate_additions =
            crate::duplicate_additions::DuplicateAdditions::new(&base, &ours, &theirs);
        let mut session = Self {
            mode: Mode::ThreeWay {
                base,
                ours: TextPane::new(ours, true),
                result: TextPane::new(result, false),
                theirs: TextPane::new(theirs, true),
                merge,
                conflicts,
                resolutions: Vec::new(),
                resolved: Vec::new(),
                undo_meta: Vec::new(),
                redo_meta: Vec::new(),
            },
            rows: Vec::new(),
            projection: std::array::from_fn(|_| PaneProjection::default()),
            focus: Pane::Result,
            aligned_row: 0,
            column: 0,
            selected: None,
            scroll: 0,
            horizontal: HashMap::new(),
            width: 80,
            height: 24,
            editing: false,
            help: false,
            help_lines: Vec::new(),
            help_scroll: 0,
            confirming_discard: false,
            confirming_merge: false,
            changed: false,
            refresh_conflict: false,
            pending: None,
            latest_generation: 0,
            accepted_generation: 0,
            message: String::new(),
            repository_review: None,
            left_path: None,
            right_path: None,
            output_path: None,
            whitespace: WhitespacePolicy::Exact,
            undo_actions: Vec::new(),
            redo_actions: Vec::new(),
            local_generation: 0,
            local_pending: false,
            local_worker: None,
            merge_advice: crate::merge_advice::MergeAdviceState::default(),
            duplicate_additions,
            palette: RenderPalette::compile(&theme),
            theme,
        };
        session.rebuild_rows();
        session
    }

    pub fn set_theme(&mut self, theme: Arc<Theme>) {
        self.palette = RenderPalette::compile(&theme);
        self.theme = theme;
    }

    pub fn theme(&self) -> &Arc<Theme> {
        &self.theme
    }

    pub fn focus(&self) -> Pane {
        self.focus
    }
    pub fn is_confirming_merge(&self) -> bool {
        self.confirming_merge
    }
    pub fn is_editing(&self) -> bool {
        self.editing
    }
    pub fn is_dirty(&self) -> bool {
        self.changed
    }
    pub fn is_local_diff_pending(&self) -> bool {
        self.local_pending
    }
    pub fn is_read_only(&self, pane: Pane) -> bool {
        self.pane(pane).read_only
    }
    pub fn accepted_generation(&self) -> u64 {
        self.accepted_generation
    }
    pub fn selected_hunk(&self) -> Option<usize> {
        if self.local_pending {
            None
        } else {
            self.selected
        }
    }
    pub fn hunk_count(&self) -> usize {
        if self.local_pending {
            return 0;
        }
        match &self.mode {
            Mode::TwoWay { diff, .. } => diff.hunks().len(),
            Mode::ThreeWay { conflicts, .. } => conflicts.len(),
        }
    }
    pub fn unresolved_conflicts(&self) -> usize {
        match &self.mode {
            Mode::ThreeWay { conflicts, .. } => conflicts.len(),
            Mode::TwoWay { .. } => 0,
        }
    }
    pub fn selected_hunk_ranges(&self) -> Option<(Range<usize>, Range<usize>)> {
        if self.local_pending {
            return None;
        }
        let Mode::TwoWay { diff, .. } = &self.mode else {
            return None;
        };
        let hunk = diff.hunks().get(self.selected?)?;
        Some((hunk.left_lines.clone(), hunk.right_lines.clone()))
    }
    pub fn pane_text(&self, pane: Pane) -> String {
        self.pane(pane).buffer.text()
    }
    pub fn pane_snapshot(&self, pane: Pane) -> TextSnapshot {
        self.pane(pane).buffer.snapshot()
    }
    pub fn pane_capture(&self, pane: Pane) -> CapturedText {
        self.pane(pane).buffer.capture()
    }
    pub fn pane_matches_snapshot(&self, pane: Pane, snapshot: &TextSnapshot) -> bool {
        self.pane(pane).buffer.capture().same_identity(snapshot)
    }

    pub fn cursor(&self) -> Cursor {
        let (line, grapheme) = self.position_for_row(self.focus, self.aligned_row);
        Cursor {
            pane: self.focus,
            aligned_row: self.aligned_row,
            line,
            grapheme,
        }
    }

    pub fn set_message(&mut self, message: impl Into<String>) {
        self.message = message.into();
    }

    pub fn set_repository_review(&mut self, mode: RepositoryReviewMode, position: String) {
        self.repository_review = Some(RepositoryReview { mode, position });
        if self.help {
            self.prepare_help();
        }
    }

    pub fn set_paths(&mut self, left: &Path, right: &Path) {
        self.left_path = Some(left.to_path_buf());
        self.right_path = Some(right.to_path_buf());
        match &mut self.mode {
            Mode::TwoWay {
                left: lhs,
                right: rhs,
                ..
            } => {
                lhs.syntax_source.path = left.to_path_buf();
                rhs.syntax_source.path = right.to_path_buf();
                if !self.local_pending {
                    lhs.update_syntax();
                    rhs.update_syntax();
                }
            }
            Mode::ThreeWay {
                ours,
                result,
                theirs,
                ..
            } => {
                ours.syntax_source.path = left.to_path_buf();
                result.syntax_source.path = right.to_path_buf();
                theirs.syntax_source.path = right.to_path_buf();
                if !self.local_pending {
                    ours.update_syntax();
                    result.update_syntax();
                    theirs.update_syntax();
                }
            }
        }
        if self.local_pending {
            self.refresh_alignment();
        }
        if self.help {
            self.prepare_help();
        }
    }

    pub fn set_output_path(&mut self, path: &Path) {
        self.output_path = Some(path.to_path_buf());
        if self.help {
            self.prepare_help();
        }
    }

    pub fn set_read_only(&mut self, pane: Pane, read_only: bool) {
        if matches!(pane, Pane::Ours | Pane::Theirs) {
            return;
        }
        self.pane_mut(pane).read_only = read_only;
    }

    pub fn whitespace_policy(&self) -> WhitespacePolicy {
        self.whitespace
    }
    pub fn set_whitespace_policy(&mut self, policy: WhitespacePolicy) {
        if self.whitespace == policy {
            return;
        }
        self.whitespace = policy;
        self.refresh_alignment();
    }

    pub fn request_diff(&mut self, left: &str, right: &str) -> DiffRequest {
        self.request_diff_snapshots(snapshot(left), snapshot(right))
    }

    pub fn request_diff_snapshots(
        &mut self,
        left: TextSnapshot,
        right: TextSnapshot,
    ) -> DiffRequest {
        assert!(
            matches!(&self.mode, Mode::TwoWay { .. }),
            "background refresh requires a two-way review"
        );
        self.latest_generation = self
            .latest_generation
            .checked_add(1)
            .expect("diff generation overflow");
        self.pending = None;
        DiffRequest {
            generation: self.latest_generation,
            left,
            right,
            policy: self.whitespace,
            left_source: self.pane(Pane::Left).syntax_source.clone(),
            right_source: self.pane(Pane::Right).syntax_source.clone(),
        }
    }

    pub(crate) fn pane(&self, pane: Pane) -> &TextPane {
        match (&self.mode, pane) {
            (Mode::TwoWay { left, .. }, Pane::Left) => left,
            (Mode::TwoWay { right, .. }, Pane::Right) => right,
            (Mode::ThreeWay { ours, .. }, Pane::Ours) => ours,
            (Mode::ThreeWay { result, .. }, Pane::Result) => result,
            (Mode::ThreeWay { theirs, .. }, Pane::Theirs) => theirs,
            _ => panic!("pane is not present in this review mode"),
        }
    }

    pub(crate) fn pane_mut(&mut self, pane: Pane) -> &mut TextPane {
        match (&mut self.mode, pane) {
            (Mode::TwoWay { left, .. }, Pane::Left) => left,
            (Mode::TwoWay { right, .. }, Pane::Right) => right,
            (Mode::ThreeWay { ours, .. }, Pane::Ours) => ours,
            (Mode::ThreeWay { result, .. }, Pane::Result) => result,
            (Mode::ThreeWay { theirs, .. }, Pane::Theirs) => theirs,
            _ => panic!("pane is not present in this review mode"),
        }
    }

    pub(crate) fn projected_rows(&self, pane: Pane) -> &[usize] {
        &self.projection[pane as usize].lines
    }

    pub(crate) fn pane_top(&self, pane: Pane) -> usize {
        if self.local_pending {
            return self.scroll;
        }
        self.projected_rows(pane)
            .partition_point(|&row| row < self.scroll)
    }

    pub(crate) fn pane_row(&self, pane: Pane, offset: usize) -> Option<usize> {
        if self.local_pending {
            return Some(self.scroll.saturating_add(offset));
        }
        self.projected_rows(pane)
            .get(self.pane_top(pane).saturating_add(offset))
            .copied()
    }

    pub(crate) fn change_bands(&self, left: Pane, right: Pane) -> &[ChangeBand] {
        match (left, right) {
            (Pane::Left, Pane::Right)
            | (Pane::Ours, Pane::Result)
            | (Pane::Result, Pane::Theirs) => &self.projection[left as usize].bands,
            _ => &[],
        }
    }

    pub(crate) fn action_bands(&self, left: Pane, right: Pane) -> &[ChangeBand] {
        match (left, right) {
            (Pane::Left, Pane::Right)
            | (Pane::Ours, Pane::Result)
            | (Pane::Result, Pane::Theirs) => &self.projection[left as usize].actions,
            _ => &[],
        }
    }

    pub(crate) fn position_for_row(&self, pane: Pane, row: usize) -> (usize, usize) {
        if self.local_pending {
            let line = row.min(self.pane(pane).buffer.line_count().saturating_sub(1));
            return (line, self.column);
        }
        if let Some(line) = self.rows.get(row).and_then(|row| row.line(pane)) {
            return (
                line.number,
                self.column.min(line.text.graphemes(true).count()),
            );
        }
        let projected = self.projected_rows(pane);
        let next = projected.partition_point(|&candidate| candidate <= row);
        if let Some(line) = projected
            .get(next)
            .and_then(|&next_row| self.rows[next_row].line(pane))
        {
            return (line.number, 0);
        }
        text::buffer_cursor_position(&self.pane(pane).buffer, self.pane(pane).buffer.len_chars())
    }

    pub(crate) fn row_for_line(&self, pane: Pane, line: usize) -> usize {
        if self.local_pending {
            return line;
        }
        self.projected_rows(pane)
            .get(line)
            .copied()
            .unwrap_or_else(|| self.rows.len().saturating_sub(1))
    }

    pub(crate) fn rebuild_rows(&mut self) {
        let (rows, projection) = match &mut self.mode {
            Mode::TwoWay { left, right, diff } => {
                *diff = Diff::between(&left.snapshot, &right.snapshot, self.whitespace);
                let rows = two_way_rows(diff, &left.snapshot, &right.snapshot);
                let projection = two_way_projection(&rows, diff);
                (rows, projection)
            }
            Mode::ThreeWay {
                ours,
                result,
                theirs,
                conflicts,
                resolved,
                ..
            } => {
                let (rows, left_bands, right_bands, left_actions, right_actions) =
                    three_way_rows(ours, result, theirs, conflicts, resolved, self.whitespace);
                let projection = three_way_projection(
                    &rows,
                    left_bands,
                    right_bands,
                    left_actions,
                    right_actions,
                );
                (rows, projection)
            }
        };
        self.rows = rows;
        self.projection = projection;
        self.refresh_duplicate_additions();
        let count = self.hunk_count();
        self.selected = if count == 0 {
            None
        } else {
            Some(self.selected.unwrap_or(0).min(count - 1))
        };
        self.aligned_row = self.aligned_row.min(self.rows.len().saturating_sub(1));
    }

    pub(crate) fn refresh_after_edit(&mut self, pane: Pane) {
        self.cancel_merge_advice();
        self.changed = true;
        self.refresh_alignment();
        if self.local_pending {
            let buffer = &self.pane(pane).buffer;
            let position = buffer.cursor();
            let line = buffer
                .char_to_line(position)
                .expect("editor cursor must remain valid");
            let start = buffer.line_to_char(line).expect("editor line must exist");
            let prefix = buffer
                .slice_chars(start..position)
                .expect("editor cursor must remain valid");
            self.column = prefix.graphemes(true).count();
            self.aligned_row = line;
        } else {
            let text = self.pane(pane).snapshot.text();
            let (line, column) = text::cursor_position(text, self.pane(pane).buffer.cursor());
            self.column = column;
            self.aligned_row = self.row_for_line(pane, line);
        }
        self.keep_cursor_visible();
        if !self.local_pending {
            self.keep_horizontal_visible();
        }
    }

    fn refresh_alignment(&mut self) {
        self.local_generation = self
            .local_generation
            .checked_add(1)
            .expect("local diff generation overflow");
        let size = match &self.mode {
            Mode::TwoWay { left, right, .. } => left
                .buffer
                .len_bytes()
                .saturating_add(right.buffer.len_bytes()),
            Mode::ThreeWay {
                ours,
                result,
                theirs,
                ..
            } => ours
                .buffer
                .len_bytes()
                .saturating_add(result.buffer.len_bytes())
                .saturating_add(theirs.buffer.len_bytes()),
        };
        if size <= SYNC_DIFF_LIMIT_BYTES {
            self.local_pending = false;
            match &mut self.mode {
                Mode::TwoWay { left, right, .. } => {
                    left.refresh();
                    right.refresh();
                }
                Mode::ThreeWay {
                    ours,
                    result,
                    theirs,
                    ..
                } => {
                    ours.refresh();
                    result.refresh();
                    theirs.refresh();
                }
            }
            self.rebuild_rows();
            return;
        }
        let generation = self.local_generation;
        let work = match &self.mode {
            Mode::TwoWay { left, right, .. } => LocalWork::TwoWay {
                generation,
                left: left.buffer.capture(),
                right: right.buffer.capture(),
                policy: self.whitespace,
                left_source: left.syntax_source.clone(),
                right_source: right.syntax_source.clone(),
            },
            Mode::ThreeWay {
                ours,
                result,
                theirs,
                conflicts,
                resolved,
                ..
            } => LocalWork::ThreeWay {
                generation,
                ours: ours.buffer.capture(),
                result: result.buffer.capture(),
                theirs: theirs.buffer.capture(),
                conflicts: conflicts.clone(),
                resolved: resolved.clone(),
                policy: self.whitespace,
                sources: [
                    ours.syntax_source.clone(),
                    result.syntax_source.clone(),
                    theirs.syntax_source.clone(),
                ],
            },
        };
        self.local_pending = true;
        self.selected = None;
        self.local_worker
            .get_or_insert_with(LocalWorker::new)
            .request(work);
    }

    pub fn poll_background(&mut self) {
        let Some(completion) = self.local_worker.as_mut().and_then(LocalWorker::latest) else {
            return;
        };
        if completion.generation != self.local_generation || !self.local_pending {
            return;
        }
        match completion.computed {
            LocalComputed::TwoWay(completion) => {
                let Mode::TwoWay { left, right, diff } = &mut self.mode else {
                    return;
                };
                if !left.buffer.capture().same_identity(&completion.left)
                    || !right.buffer.capture().same_identity(&completion.right)
                {
                    return;
                }
                left.snapshot = completion.left;
                right.snapshot = completion.right;
                left.syntax = completion.left_syntax;
                right.syntax = completion.right_syntax;
                *diff = completion.diff;
                self.rows = completion.rows;
                self.projection = completion.projection;
            }
            LocalComputed::ThreeWay {
                ours,
                result,
                theirs,
                rows,
                projection,
                syntax,
            } => {
                let Mode::ThreeWay {
                    ours: current_ours,
                    result: current_result,
                    theirs: current_theirs,
                    ..
                } = &mut self.mode
                else {
                    return;
                };
                if !current_ours.buffer.capture().same_identity(&ours)
                    || !current_result.buffer.capture().same_identity(&result)
                    || !current_theirs.buffer.capture().same_identity(&theirs)
                {
                    return;
                }
                current_ours.snapshot = ours;
                current_result.snapshot = result;
                current_theirs.snapshot = theirs;
                let [ours_syntax, result_syntax, theirs_syntax] = syntax;
                current_ours.syntax = ours_syntax;
                current_result.syntax = result_syntax;
                current_theirs.syntax = theirs_syntax;
                self.rows = rows;
                self.projection = projection;
            }
        }
        self.refresh_duplicate_additions();
        let line = self.aligned_row;
        self.local_pending = false;
        self.selected = if self.hunk_count() == 0 {
            None
        } else {
            Some(0)
        };
        self.aligned_row = self.row_for_line(self.focus, line);
        self.keep_cursor_visible();
        self.keep_horizontal_visible();
    }

    pub(crate) fn keep_cursor_visible(&mut self) {
        let visible = usize::from(self.height.saturating_sub(3)).max(1);
        if self.local_pending {
            if self.aligned_row < self.scroll {
                self.scroll = self.aligned_row;
            } else if self.aligned_row >= self.scroll.saturating_add(visible) {
                self.scroll = self.aligned_row + 1 - visible;
            }
            return;
        }
        let projected = self.projected_rows(self.focus);
        let Ok(index) = projected.binary_search(&self.aligned_row) else {
            return;
        };
        let top = self.pane_top(self.focus);
        if index < top {
            self.scroll = self.aligned_row;
        } else if index >= top.saturating_add(visible) {
            self.scroll = projected[index + 1 - visible];
        }
    }

    pub(crate) fn record_action(&mut self, pane: Pane, has_text_history: bool) {
        self.undo_actions.push(HistoryAction {
            pane,
            has_text_history,
        });
        self.redo_actions.clear();
    }

    pub(crate) fn merge_metadata(&self) -> Option<MergeMetadata> {
        let Mode::ThreeWay {
            conflicts,
            resolutions,
            resolved,
            ..
        } = &self.mode
        else {
            return None;
        };
        Some(MergeMetadata {
            conflicts: conflicts.clone(),
            resolutions: resolutions.clone(),
            resolved: resolved.clone(),
        })
    }

    pub(crate) fn record_merge_edit(&mut self, before: Option<MergeMetadata>) {
        let (
            Mode::ThreeWay {
                undo_meta,
                redo_meta,
                ..
            },
            Some(before),
        ) = (&mut self.mode, before)
        else {
            return;
        };
        undo_meta.push(before);
        redo_meta.clear();
    }

    pub(crate) fn restore_merge_metadata(&mut self, metadata: MergeMetadata) {
        let Mode::ThreeWay {
            base,
            ours,
            theirs,
            merge,
            conflicts,
            resolutions,
            resolved,
            ..
        } = &mut self.mode
        else {
            return;
        };
        let mut restored = Merge::three_way(base, &ours.snapshot, &theirs.snapshot);
        for (id, choice) in &metadata.resolutions {
            let choice = match choice {
                ResolutionChoice::Ours => ConflictResolution::Ours,
                ResolutionChoice::Theirs => ConflictResolution::Theirs,
                ResolutionChoice::Both => ConflictResolution::Both,
                ResolutionChoice::Manual(text) => ConflictResolution::Manual(text.clone()),
            };
            restored
                .resolve(*id, choice)
                .expect("reviewed conflict identity must remain stable");
        }
        *merge = restored;
        *conflicts = metadata.conflicts;
        *resolutions = metadata.resolutions;
        *resolved = metadata.resolved;
    }

    pub(crate) fn submit(&mut self) -> ReviewOutcome {
        if self.refresh_conflict {
            return ReviewOutcome::RefreshConflict;
        }
        if self.local_pending {
            return ReviewOutcome::LocalDiffPending;
        }
        if self.unresolved_conflicts() > 0 {
            return ReviewOutcome::UnresolvedConflicts(self.unresolved_conflicts());
        }
        if matches!(&self.mode, Mode::ThreeWay { .. }) && !self.confirming_merge {
            self.confirming_merge = true;
            self.message.clear();
            return ReviewOutcome::Continue;
        }
        self.confirming_merge = false;
        match &self.mode {
            Mode::TwoWay { left, right, .. } => ReviewOutcome::Submitted(ReviewSubmission {
                left: left.buffer.text(),
                right: right.buffer.text(),
                result: None,
            }),
            Mode::ThreeWay {
                ours,
                result,
                theirs,
                ..
            } => ReviewOutcome::Submitted(ReviewSubmission {
                left: ours.buffer.text(),
                right: theirs.buffer.text(),
                result: Some(result.buffer.text()),
            }),
        }
    }
}

fn snapshot(text: &str) -> TextSnapshot {
    TextSnapshot::from_bytes(text.as_bytes()).expect("review text must be UTF-8 without NUL bytes")
}

fn view_line(line: &RenderedLine, spans: &[text::LineSpan], text_length: usize) -> ViewLine {
    let number = line.number.saturating_sub(1);
    let byte_start = spans
        .get(number)
        .map_or(text_length, |span| span.start_byte);
    ViewLine::new(number, line.text.clone(), byte_start)
}

fn annotate_pair(left: &mut Option<ViewLine>, right: &mut Option<ViewLine>) {
    if let (Some(left), Some(right)) = (left, right) {
        if left.text == right.text {
            return;
        }
        for change in intraline_spans(&left.text, &right.text) {
            left.changed.push(change.left);
            right.changed.push(change.right);
        }
        left.changed.sort_unstable_by_key(|span| span.start);
        right.changed.sort_unstable_by_key(|span| span.start);
        left.changed.dedup_by(|current, previous| {
            if current.start > previous.end {
                return false;
            }
            previous.end = previous.end.max(current.end);
            true
        });
        right.changed.dedup_by(|current, previous| {
            if current.start > previous.end {
                return false;
            }
            previous.end = previous.end.max(current.end);
            true
        });
    }
}

fn row_hunk(row: &AlignedRow, hunks: &[Hunk], current: &mut usize) -> Option<usize> {
    while let Some(hunk) = hunks.get(*current) {
        let left_after = row
            .left
            .as_ref()
            .is_none_or(|line| line.number.saturating_sub(1) >= hunk.left_lines.end);
        let right_after = row
            .right
            .as_ref()
            .is_none_or(|line| line.number.saturating_sub(1) >= hunk.right_lines.end);
        if left_after && right_after {
            *current += 1;
        } else {
            break;
        }
    }
    let hunk = hunks.get(*current)?;
    if row
        .left
        .as_ref()
        .is_some_and(|line| hunk.left_lines.contains(&line.number.saturating_sub(1)))
        || row
            .right
            .as_ref()
            .is_some_and(|line| hunk.right_lines.contains(&line.number.saturating_sub(1)))
    {
        Some(*current)
    } else {
        None
    }
}

fn terminal_view(snapshot: &TextSnapshot, spans: &[text::LineSpan]) -> Option<ViewLine> {
    let source = snapshot.text();
    if !source.is_empty() && !source.ends_with('\n') && !source.ends_with('\r') {
        return None;
    }
    Some(ViewLine::new(
        spans.len().saturating_sub(1),
        String::new(),
        source.len(),
    ))
}

fn two_way_rows(diff: &Diff, left: &TextSnapshot, right: &TextSnapshot) -> Vec<ViewRow> {
    let left_spans = text::lines(left.text());
    let right_spans = text::lines(right.text());
    let mut current_hunk = 0;
    let mut rows: Vec<_> = diff
        .rows()
        .iter()
        .map(|row| {
            let mut lhs = row
                .left
                .as_ref()
                .map(|line| view_line(line, &left_spans, left.text().len()));
            let mut rhs = row
                .right
                .as_ref()
                .map(|line| view_line(line, &right_spans, right.text().len()));
            annotate_pair(&mut lhs, &mut rhs);
            ViewRow {
                left: lhs,
                right: rhs,
                hunk: row_hunk(row, diff.hunks(), &mut current_hunk),
                ..ViewRow::default()
            }
        })
        .collect();
    let left_end = terminal_view(left, &left_spans);
    let right_end = terminal_view(right, &right_spans);
    if left_end.is_some() || right_end.is_some() {
        rows.push(ViewRow {
            left: left_end,
            right: right_end,
            ..ViewRow::default()
        });
    }
    rows
}

fn pivot_rows(
    diff: &Diff,
    pivot_right: bool,
    count: usize,
    other: &TextSnapshot,
) -> (Vec<Vec<ViewLine>>, Vec<Option<ViewLine>>) {
    let other_spans = text::lines(other.text());
    let mut before = vec![Vec::new(); count + 1];
    let mut paired = vec![None; count];
    let mut next = 0;
    for row in diff.rows() {
        let pivot = if pivot_right { &row.right } else { &row.left };
        let side = if pivot_right { &row.left } else { &row.right };
        if let Some(pivot) = pivot {
            next = pivot.number.saturating_sub(1).min(count.saturating_sub(1));
            if let Some(side) = side {
                paired[next] = Some(view_line(side, &other_spans, other.text().len()));
            }
            next += 1;
        } else if let Some(side) = side {
            before[next.min(count)].push(view_line(side, &other_spans, other.text().len()));
        }
    }
    (before, paired)
}

fn three_way_rows(
    ours: &TextPane,
    result: &TextPane,
    theirs: &TextPane,
    conflicts: &[ConflictRegion],
    resolved: &[ResolvedConflict],
    policy: WhitespacePolicy,
) -> (
    Vec<ViewRow>,
    Vec<ChangeBand>,
    Vec<ChangeBand>,
    Vec<ChangeBand>,
    Vec<ChangeBand>,
) {
    let left_diff = Diff::between(&ours.snapshot, &result.snapshot, policy);
    let right_diff = Diff::between(&result.snapshot, &theirs.snapshot, policy);
    let result_lines = text::lines(result.snapshot.text());
    let count = result_lines.len()
        - usize::from(
            result.snapshot.text().ends_with('\n') || result.snapshot.text().ends_with('\r'),
        );
    let (mut ours_before, mut ours_paired) = pivot_rows(&left_diff, true, count, &ours.snapshot);
    let (mut theirs_before, mut theirs_paired) =
        pivot_rows(&right_diff, false, count, &theirs.snapshot);
    let mut rows = Vec::new();
    for index in 0..=count {
        let ours_extra = std::mem::take(&mut ours_before[index]);
        let theirs_extra = std::mem::take(&mut theirs_before[index]);
        for offset in 0..ours_extra.len().max(theirs_extra.len()) {
            rows.push(ViewRow {
                ours: ours_extra.get(offset).cloned(),
                theirs: theirs_extra.get(offset).cloned(),
                ..ViewRow::default()
            });
        }
        if index == count {
            break;
        }
        let line = result_lines[index];
        let mut view = ViewRow {
            ours: ours_paired[index].take(),
            result: Some(ViewLine::new(
                index,
                result.snapshot.text()[line.start_byte..line.content_end_byte].to_owned(),
                line.start_byte,
            )),
            theirs: theirs_paired[index].take(),
            ..ViewRow::default()
        };
        annotate_pair(&mut view.ours, &mut view.result);
        annotate_pair(&mut view.theirs, &mut view.result);
        rows.push(view);
    }
    let ours_end = terminal_view(&ours.snapshot, &text::lines(ours.snapshot.text()));
    let result_end = terminal_view(&result.snapshot, &result_lines);
    let theirs_end = terminal_view(&theirs.snapshot, &text::lines(theirs.snapshot.text()));
    if ours_end.is_some() || result_end.is_some() || theirs_end.is_some() {
        rows.push(ViewRow {
            ours: ours_end,
            result: result_end,
            theirs: theirs_end,
            ..ViewRow::default()
        });
    }
    for row in &mut rows {
        if let Some(result_line) = &row.result {
            let start = result_lines[result_line.number].start_char;
            let end = result_lines[result_line.number].end_char;
            row.hunk = conflicts.iter().position(|region| {
                (region.chars.start < end && region.chars.end > start)
                    || (region.chars.is_empty() && region.chars.start == start)
            });
        }
    }
    for (index, region) in conflicts.iter().enumerate() {
        if rows.iter().any(|row| row.hunk == Some(index)) {
            continue;
        }
        let (line, _) = text::cursor_position(result.snapshot.text(), region.chars.start);
        let row = rows
            .iter()
            .position(|row| row.result.as_ref().is_some_and(|view| view.number >= line))
            .unwrap_or_else(|| rows.len().saturating_sub(1));
        if rows.is_empty() {
            rows.push(ViewRow::default());
        }
        rows[row].hunk = Some(index);
    }
    let mut left_bands = bands_for_diff(&left_diff, false);
    let mut right_bands = bands_for_diff(&right_diff, false);
    mark_resolved(&mut left_bands, resolved, Pane::Ours);
    mark_resolved(&mut right_bands, resolved, Pane::Theirs);
    let (left_actions, right_actions) =
        three_way_action_bands(result.snapshot.text(), conflicts, resolved);
    (rows, left_bands, right_bands, left_actions, right_actions)
}

fn bands_for_diff(diff: &Diff, actionable: bool) -> Vec<ChangeBand> {
    diff.hunks()
        .iter()
        .enumerate()
        .map(|(index, hunk)| ChangeBand {
            left: hunk.left_lines.clone(),
            right: hunk.right_lines.clone(),
            hunk: actionable.then_some(index),
            resolved: None,
            kind: if hunk.left_lines.is_empty() {
                ChangeKind::Added
            } else if hunk.right_lines.is_empty() {
                ChangeKind::Removed
            } else {
                ChangeKind::Modified
            },
        })
        .collect()
}

fn projected_lines(rows: &[ViewRow], pane: Pane) -> Vec<usize> {
    rows.iter()
        .enumerate()
        .filter_map(|(index, row)| row.line(pane).map(|_| index))
        .collect()
}

fn two_way_projection(rows: &[ViewRow], diff: &Diff) -> [PaneProjection; 5] {
    let mut projections: [PaneProjection; 5] = std::array::from_fn(|_| PaneProjection::default());
    projections[Pane::Left as usize].lines = projected_lines(rows, Pane::Left);
    let bands = bands_for_diff(diff, true);
    projections[Pane::Left as usize].actions = bands.clone();
    projections[Pane::Left as usize].bands = bands;
    projections[Pane::Right as usize].lines = projected_lines(rows, Pane::Right);
    projections
}

fn three_way_projection(
    rows: &[ViewRow],
    left_bands: Vec<ChangeBand>,
    right_bands: Vec<ChangeBand>,
    left_actions: Vec<ChangeBand>,
    right_actions: Vec<ChangeBand>,
) -> [PaneProjection; 5] {
    let mut projections: [PaneProjection; 5] = std::array::from_fn(|_| PaneProjection::default());
    projections[Pane::Ours as usize].lines = projected_lines(rows, Pane::Ours);
    projections[Pane::Ours as usize].bands = left_bands;
    projections[Pane::Ours as usize].actions = left_actions;
    projections[Pane::Result as usize].lines = projected_lines(rows, Pane::Result);
    projections[Pane::Result as usize].bands = right_bands;
    projections[Pane::Result as usize].actions = right_actions;
    projections[Pane::Theirs as usize].lines = projected_lines(rows, Pane::Theirs);
    projections
}

fn result_line_range(text: &str, chars: &Range<usize>) -> Range<usize> {
    let (start, _) = text::cursor_position(text, chars.start);
    if chars.is_empty() {
        return start..start;
    }
    let (end, _) = text::cursor_position(text, chars.end.saturating_sub(1));
    start..end + 1
}

fn three_way_action_bands(
    result: &str,
    conflicts: &[ConflictRegion],
    resolved: &[ResolvedConflict],
) -> (Vec<ChangeBand>, Vec<ChangeBand>) {
    let mut left = Vec::with_capacity(conflicts.len() + resolved.len());
    let mut right = Vec::with_capacity(conflicts.len() + resolved.len());
    for (index, region) in conflicts.iter().enumerate() {
        let result_range = result_line_range(result, &region.chars);
        left.push(ChangeBand {
            left: region.ours.clone(),
            right: result_range.clone(),
            hunk: Some(index),
            resolved: None,
            kind: ChangeKind::Conflict,
        });
        right.push(ChangeBand {
            left: result_range,
            right: region.theirs.clone(),
            hunk: Some(index),
            resolved: None,
            kind: ChangeKind::Conflict,
        });
    }
    for region in resolved {
        let ours_pending = !region.ours_accepted && !region.ours.is_empty();
        let theirs_pending = !region.theirs_accepted && !region.theirs.is_empty();
        if !ours_pending && !theirs_pending {
            continue;
        }
        let result_range = result_line_range(result, &region.result);
        if ours_pending {
            left.push(ChangeBand {
                left: region.ours.clone(),
                right: result_range.clone(),
                hunk: None,
                resolved: Some(region.id),
                kind: ChangeKind::Resolved,
            });
        }
        if theirs_pending {
            right.push(ChangeBand {
                left: result_range,
                right: region.theirs.clone(),
                hunk: None,
                resolved: Some(region.id),
                kind: ChangeKind::Resolved,
            });
        }
    }
    let key = |band: &ChangeBand| {
        (
            band.left.start.min(band.right.start),
            usize::from(band.hunk.is_none()),
            band.hunk
                .unwrap_or_else(|| band.resolved.map_or(usize::MAX, |id| id.0)),
        )
    };
    left.sort_by_key(|band| key(band));
    right.sort_by_key(|band| key(band));
    (left, right)
}

fn mark_resolved(bands: &mut [ChangeBand], resolved: &[ResolvedConflict], source: Pane) {
    for band in bands {
        if band.kind == ChangeKind::Conflict {
            continue;
        }
        let source_range = if source == Pane::Ours {
            &band.left
        } else {
            &band.right
        };
        if resolved.iter().any(|conflict| {
            let conflict_range = if source == Pane::Ours {
                &conflict.ours
            } else {
                &conflict.theirs
            };
            source_range.start < conflict_range.end && conflict_range.start < source_range.end
                || source_range.is_empty() && source_range.start == conflict_range.start
        }) {
            band.kind = ChangeKind::Resolved;
        }
    }
}
