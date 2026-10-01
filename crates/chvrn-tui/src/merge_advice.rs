use std::sync::Arc;

use chvrn_core::{
    edit::CapturedText,
    merge::ConflictId,
    merge_advice::{MergeAdviceChoice, MergeAdviceInput, MergeAdviceSource, MergeAdviceSuggestion},
};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};

use crate::{
    ReviewOutcome,
    session::{Mode, ResolutionChoice, ReviewSession},
};

#[derive(Clone)]
pub struct MergeAdviceRequest {
    session: Arc<()>,
    authority: Arc<()>,
    conflict: ConflictId,
    result: CapturedText,
    revision: u64,
    input: MergeAdviceInput,
}

impl MergeAdviceRequest {
    pub fn input(&self) -> &MergeAdviceInput {
        &self.input
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum MergeAdviceError {
    Disabled,
    Unavailable,
    Busy,
}

#[derive(Default)]
pub(crate) struct MergeAdviceState {
    session: Arc<()>,
    enabled: bool,
    revision: u64,
    pending: Option<Arc<()>>,
    pub(crate) dialog: Option<MergeAdviceDialog>,
}

pub(crate) struct MergeAdviceDialog {
    request: MergeAdviceRequest,
    pub(crate) suggestion: MergeAdviceSuggestion,
}

impl ReviewSession {
    pub fn set_merge_advice_enabled(&mut self, enabled: bool) {
        if !enabled {
            self.cancel_merge_advice();
        }
        self.merge_advice.enabled = enabled;
        if self.help {
            self.prepare_help();
        }
    }

    pub fn begin_merge_advice(&mut self) -> Result<MergeAdviceRequest, MergeAdviceError> {
        if !self.merge_advice.enabled {
            return Err(MergeAdviceError::Disabled);
        }
        if self.merge_advice.pending.is_some() {
            return Err(MergeAdviceError::Busy);
        }
        if !self.can_begin_merge_advice() {
            return Err(MergeAdviceError::Unavailable);
        }
        let Mode::ThreeWay {
            base,
            ours,
            theirs,
            result,
            conflicts,
            merge,
            ..
        } = &self.mode
        else {
            return Err(MergeAdviceError::Unavailable);
        };
        let region = &conflicts[self.selected.ok_or(MergeAdviceError::Unavailable)?];
        let conflict = merge
            .conflicts()
            .iter()
            .find(|conflict| conflict.id == region.id)
            .ok_or(MergeAdviceError::Unavailable)?;
        let authority = Arc::new(());
        let request = MergeAdviceRequest {
            session: Arc::clone(&self.merge_advice.session),
            authority: Arc::clone(&authority),
            conflict: region.id,
            result: result.buffer.capture(),
            revision: self.merge_advice.revision,
            input: MergeAdviceInput {
                base: MergeAdviceSource {
                    snapshot: base.clone(),
                    lines: conflict.base_lines.clone(),
                },
                ours: MergeAdviceSource {
                    snapshot: ours.snapshot.clone(),
                    lines: region.ours.clone(),
                },
                theirs: MergeAdviceSource {
                    snapshot: theirs.snapshot.clone(),
                    lines: region.theirs.clone(),
                },
            },
        };
        self.merge_advice.pending = Some(authority);
        self.message.clear();
        Ok(request)
    }

    pub fn receive_merge_advice(
        &mut self,
        request: MergeAdviceRequest,
        reply: Result<MergeAdviceSuggestion, String>,
    ) -> bool {
        if !self
            .merge_advice
            .pending
            .as_ref()
            .is_some_and(|pending| Arc::ptr_eq(pending, &request.authority))
        {
            return false;
        }
        self.merge_advice.pending = None;
        if self.is_review_modal() || !self.merge_advice_matches(&request) {
            return false;
        }
        match reply {
            Ok(suggestion) => {
                self.message.clear();
                self.merge_advice.dialog = Some(MergeAdviceDialog {
                    request,
                    suggestion,
                });
            }
            Err(message) => self.message = message,
        }
        true
    }

    pub fn cancel_merge_advice(&mut self) {
        self.merge_advice.revision = self
            .merge_advice
            .revision
            .checked_add(1)
            .expect("merge advice revision overflow");
        self.merge_advice.pending = None;
        self.merge_advice.dialog = None;
    }

    pub fn is_review_modal(&self) -> bool {
        self.help
            || self.confirming_discard
            || self.confirming_merge
            || self.merge_advice.dialog.is_some()
    }

    pub(crate) fn merge_advice_eligible(&self) -> bool {
        self.merge_advice.enabled
            && !self.editing
            && !self.local_pending
            && !self.confirming_discard
            && !self.confirming_merge
            && !self.refresh_conflict
            && matches!(&self.mode, Mode::ThreeWay { conflicts, .. }
                if self.selected.and_then(|index| conflicts.get(index)).is_some_and(|region| !region.changed))
    }

    pub(crate) fn can_begin_merge_advice(&self) -> bool {
        self.merge_advice_eligible()
            && !self.is_review_modal()
            && self.merge_advice.pending.is_none()
    }

    fn merge_advice_matches(&self, request: &MergeAdviceRequest) -> bool {
        self.merge_advice_eligible()
            && Arc::ptr_eq(&self.merge_advice.session, &request.session)
            && self.merge_advice.revision == request.revision
            && matches!(&self.mode, Mode::ThreeWay { conflicts, result, .. }
                if request.result.same_identity(&result.snapshot)
                    && self.selected.and_then(|index| conflicts.get(index)).is_some_and(|region| region.id == request.conflict))
    }

    pub(crate) fn handle_merge_advice_key(&mut self, event: KeyEvent) -> ReviewOutcome {
        if event.kind != KeyEventKind::Press || !event.modifiers.is_empty() {
            return ReviewOutcome::Continue;
        }
        match event.code {
            KeyCode::Esc | KeyCode::Char('q') => self.cancel_merge_advice(),
            KeyCode::Enter => {
                let Some(dialog) = self.merge_advice.dialog.as_ref() else {
                    return ReviewOutcome::Continue;
                };
                if !self.merge_advice_matches(&dialog.request) {
                    self.cancel_merge_advice();
                    return ReviewOutcome::Continue;
                }
                let choice = match dialog.suggestion.choice {
                    MergeAdviceChoice::Ours => ResolutionChoice::Ours,
                    MergeAdviceChoice::Theirs => ResolutionChoice::Theirs,
                    MergeAdviceChoice::LeaveUnresolved => return ReviewOutcome::Continue,
                };
                let index = self.selected.expect("validated selected conflict");
                self.cancel_merge_advice();
                self.resolve_conflict(index, choice);
            }
            _ => {}
        }
        ReviewOutcome::Continue
    }
}
