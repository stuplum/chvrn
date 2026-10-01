use std::ops::Range;

use crate::TextSnapshot;

#[derive(Clone, Debug)]
pub struct MergeAdviceSource {
    pub snapshot: TextSnapshot,
    pub lines: Range<usize>,
}

#[derive(Clone, Debug)]
pub struct MergeAdviceInput {
    pub base: MergeAdviceSource,
    pub ours: MergeAdviceSource,
    pub theirs: MergeAdviceSource,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MergeAdviceChoice {
    Ours,
    Theirs,
    LeaveUnresolved,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MergeAdviceSuggestion {
    pub choice: MergeAdviceChoice,
    pub confidence: f64,
    pub model: String,
}
