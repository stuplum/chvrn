mod bundled_themes;
mod duplicate_additions;
mod input;
mod merge_advice;
mod pager;
mod render;
mod session;
mod text;
mod theme;

use crossterm::event::{KeyEvent, MouseEvent};

pub use chvrn_core::diff::WhitespacePolicy;
pub use chvrn_core::structural::Language;
pub use input::ReviewEditError;
pub use merge_advice::{MergeAdviceError, MergeAdviceRequest};
pub use pager::PagerSession;
pub use session::{
    DiffCompletion, DiffRequest, DiffRequestId, RepositoryReviewMode, ReviewSession,
};
pub use theme::{BUNDLED_THEMES, Theme, ThemeError};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
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
    RefreshConflict,
    RefreshApplied,
    RefreshSuperseded,
    LocalDiffPending,
    UnresolvedConflicts(usize),
    Submitted(ReviewSubmission),
    Quit,
}
