use std::ops::Range;

use chvrn_core::{
    diff::{ApplyDirection, WhitespacePolicy},
    merge::ConflictResolution,
};
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::{
    Pane, ReviewInput, ReviewOutcome,
    render::{GutterAction, InsertionPosition},
    session::{ConflictRegion, DiffCompletion, Mode, ResolutionChoice, ReviewSession, TextPane},
    text,
};

#[derive(Debug, Eq, PartialEq)]
pub enum ReviewEditError {
    ReadOnly,
    UnresolvedConflicts,
    InvalidText,
}

impl ReviewSession {
    pub fn handle(&mut self, input: ReviewInput) -> ReviewOutcome {
        match input {
            ReviewInput::Key(event)
                if matches!(event.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
            {
                self.handle_key(event)
            }
            ReviewInput::Key(_) => ReviewOutcome::Continue,
            ReviewInput::Mouse(event) => self.handle_mouse(event),
            ReviewInput::Paste(text) => {
                if self.editing {
                    self.insert_text(&text);
                }
                ReviewOutcome::Continue
            }
            ReviewInput::Resize { width, height } => {
                if self.width != width || self.height != height {
                    self.width = width;
                    self.height = height;
                    self.keep_cursor_visible();
                    self.keep_horizontal_visible();
                }
                ReviewOutcome::Continue
            }
            ReviewInput::DiffReady(completion) => self.receive_diff(completion),
            ReviewInput::ConfirmDiscard => {
                if self.confirming_discard {
                    ReviewOutcome::Quit
                } else {
                    ReviewOutcome::Continue
                }
            }
            ReviewInput::DiscardAndReload => {
                if self.refresh_conflict {
                    if let Some(completion) = self.pending.take() {
                        self.accept_diff(completion);
                    }
                }
                ReviewOutcome::Continue
            }
        }
    }

    pub fn go_to(&mut self, pane: Pane, line: usize, grapheme: usize) {
        let actual_line = line.min(self.pane(pane).buffer.line_count().saturating_sub(1));
        let maximum = text::buffer_line_content(&self.pane(pane).buffer, actual_line)
            .graphemes(true)
            .count();
        self.focus = pane;
        self.column = grapheme.min(maximum);
        self.aligned_row = self.row_for_line(pane, actual_line);
        let offset =
            text::buffer_position_offset(&self.pane(pane).buffer, actual_line, self.column);
        let _ = self.pane_mut(pane).buffer.set_cursor(offset);
        self.keep_cursor_visible();
        self.keep_horizontal_visible();
    }

    pub fn replace_pane_text(&mut self, pane: Pane, text: &str) -> Result<(), ReviewEditError> {
        if self.pane(pane).read_only {
            return Err(ReviewEditError::ReadOnly);
        }
        if matches!(pane, Pane::Result) && self.unresolved_conflicts() != 0 {
            return Err(ReviewEditError::UnresolvedConflicts);
        }
        if text.contains('\0') {
            return Err(ReviewEditError::InvalidText);
        }
        let current = self.pane(pane).buffer.len_chars();
        let previous = if pane == Pane::Result {
            self.merge_metadata()
        } else {
            None
        };
        self.pane_mut(pane)
            .buffer
            .replace(0..current, text)
            .map_err(|_| ReviewEditError::InvalidText)?;
        self.record_merge_edit(previous);
        self.record_action(pane);
        self.focus = pane;
        self.refresh_after_edit(pane);
        Ok(())
    }

    fn receive_diff(&mut self, completion: DiffCompletion) -> ReviewOutcome {
        if completion.generation != self.latest_generation || completion.policy != self.whitespace {
            return ReviewOutcome::Continue;
        }
        if self.changed {
            self.pending = Some(completion);
            self.refresh_conflict = true;
            return ReviewOutcome::RefreshConflict;
        }
        self.accept_diff(completion);
        ReviewOutcome::Continue
    }

    fn accept_diff(&mut self, completion: DiffCompletion) {
        let DiffCompletion {
            generation,
            left,
            right,
            diff,
            rows,
            projection,
            left_syntax,
            right_syntax,
            ..
        } = completion;
        let (left_language, right_language, left_read_only, right_read_only) = match &self.mode {
            Mode::TwoWay { left, right, .. } => (
                left.language,
                right.language,
                left.read_only,
                right.read_only,
            ),
            Mode::ThreeWay { .. } => return,
        };
        let mut left = TextPane::new(left, left_read_only);
        let mut right = TextPane::new(right, right_read_only);
        left.language = left_language;
        right.language = right_language;
        left.syntax = left_syntax;
        right.syntax = right_syntax;
        let has_hunk = !diff.hunks().is_empty();
        self.mode = Mode::TwoWay { left, right, diff };
        self.rows = rows;
        self.projection = projection;
        self.selected = has_hunk.then_some(0);
        self.aligned_row = 0;
        self.column = 0;
        self.scroll = 0;
        self.changed = false;
        self.local_generation = self
            .local_generation
            .checked_add(1)
            .expect("local diff generation overflow");
        self.local_pending = false;
        self.accepted_generation = generation;
        self.refresh_conflict = false;
        self.pending = None;
        self.editing = false;
        self.undo_actions.clear();
        self.redo_actions.clear();
    }

    fn handle_key(&mut self, event: KeyEvent) -> ReviewOutcome {
        if self.confirming_discard {
            if event.code == KeyCode::Esc {
                self.confirming_discard = false;
            }
            return ReviewOutcome::Continue;
        }
        if self.help {
            if matches!(event.code, KeyCode::Esc | KeyCode::Char('?')) {
                self.help = false;
            }
            return ReviewOutcome::Continue;
        }
        if self.editing {
            return self.handle_edit_key(event);
        }
        if event.modifiers.contains(KeyModifiers::CONTROL) {
            match event.code {
                KeyCode::Char('r') => self.redo(),
                KeyCode::Char('d') => {
                    self.scroll_by(usize::from(self.height.saturating_sub(3)) / 2, true)
                }
                KeyCode::Char('b') => {
                    self.scroll_by(usize::from(self.height.saturating_sub(3)), false)
                }
                _ => {}
            }
            return ReviewOutcome::Continue;
        }
        if event.modifiers.contains(KeyModifiers::SHIFT)
            && matches!(event.code, KeyCode::Left | KeyCode::Right)
        {
            self.shift_horizontal(event.code == KeyCode::Right);
            return ReviewOutcome::Continue;
        }
        match event.code {
            KeyCode::Char('s') => return self.submit(),
            KeyCode::Char('q') => {
                if self.changed {
                    self.confirming_discard = true;
                    return ReviewOutcome::DiscardRequired;
                }
                return ReviewOutcome::Quit;
            }
            KeyCode::Char('?') => self.help = true,
            KeyCode::Char('i') => self.enter_edit(),
            KeyCode::Char('a') => self.apply_hunk(),
            KeyCode::Char('u') => self.undo(),
            KeyCode::Char(']') => self.select_hunk(true),
            KeyCode::Char('[') => self.select_hunk(false),
            KeyCode::Char('j') | KeyCode::Down => self.move_row(true),
            KeyCode::Char('k') | KeyCode::Up => self.move_row(false),
            KeyCode::Char('h') | KeyCode::Left => self.move_column(false),
            KeyCode::Char('l') | KeyCode::Right => self.move_column(true),
            KeyCode::Home => {
                self.column = 0;
                self.keep_cursor_visible();
                self.keep_horizontal_visible();
            }
            KeyCode::End => {
                let (line, _) = self.position_for_row(self.focus, self.aligned_row);
                self.column = text::buffer_line_content(&self.pane(self.focus).buffer, line)
                    .graphemes(true)
                    .count();
                self.keep_horizontal_visible();
            }
            KeyCode::PageDown => {
                self.scroll_by(usize::from(self.height.saturating_sub(3)).max(1), true)
            }
            KeyCode::PageUp => {
                self.scroll_by(usize::from(self.height.saturating_sub(3)).max(1), false)
            }
            KeyCode::Tab | KeyCode::BackTab => self.switch_focus(),
            KeyCode::Char('w') => self.cycle_whitespace(),
            KeyCode::Char('o' | 't' | 'b' | 'r') => self.choose_conflict(event.code),
            _ => {}
        }
        ReviewOutcome::Continue
    }

    fn handle_edit_key(&mut self, event: KeyEvent) -> ReviewOutcome {
        if event.code == KeyCode::Esc {
            self.editing = false;
            return ReviewOutcome::Continue;
        }
        if event.modifiers.contains(KeyModifiers::CONTROL) {
            match event.code {
                KeyCode::Char('z') => self.undo(),
                KeyCode::Char('r') => self.redo(),
                _ => {}
            }
            return ReviewOutcome::Continue;
        }
        match event.code {
            KeyCode::Char(ch) => self.insert_text(&ch.to_string()),
            KeyCode::Enter => self.insert_text("\n"),
            KeyCode::Tab => self.insert_text("\t"),
            KeyCode::Backspace => self.delete_grapheme(true),
            KeyCode::Delete => self.delete_grapheme(false),
            KeyCode::Left => self.move_edit_cursor(false),
            KeyCode::Right => self.move_edit_cursor(true),
            KeyCode::Up => self.move_edit_line(false),
            KeyCode::Down => self.move_edit_line(true),
            KeyCode::Home => self.move_edit_edge(false),
            KeyCode::End => self.move_edit_edge(true),
            _ => {}
        }
        ReviewOutcome::Continue
    }

    fn cycle_whitespace(&mut self) {
        self.set_whitespace_policy(match self.whitespace {
            WhitespacePolicy::Exact => WhitespacePolicy::IgnoreEdge,
            WhitespacePolicy::IgnoreEdge => WhitespacePolicy::IgnoreAll,
            WhitespacePolicy::IgnoreAll => WhitespacePolicy::IgnoreBlankLines,
            WhitespacePolicy::IgnoreBlankLines => WhitespacePolicy::Exact,
        });
    }

    fn switch_focus(&mut self) {
        self.focus = match self.focus {
            Pane::Left => Pane::Right,
            Pane::Right => Pane::Left,
            Pane::Ours => Pane::Result,
            Pane::Result => Pane::Theirs,
            Pane::Theirs => Pane::Ours,
        };
        let (line, column) = self.position_for_row(self.focus, self.aligned_row);
        self.aligned_row = self.row_for_line(self.focus, line);
        self.column = column;
        self.keep_cursor_visible();
        if !self.local_pending {
            self.keep_horizontal_visible();
        }
    }

    fn move_row(&mut self, forward: bool) {
        if self.local_pending {
            let limit = self.pane(self.focus).buffer.line_count();
            self.aligned_row = if forward {
                self.aligned_row
                    .saturating_add(1)
                    .min(limit.saturating_sub(1))
            } else {
                self.aligned_row.saturating_sub(1)
            };
        } else {
            let projected = self.projected_rows(self.focus);
            let index = projected
                .binary_search(&self.aligned_row)
                .unwrap_or_else(|index| index.min(projected.len().saturating_sub(1)));
            let next = if forward {
                index
                    .saturating_add(1)
                    .min(projected.len().saturating_sub(1))
            } else {
                index.saturating_sub(1)
            };
            if let Some(&row) = projected.get(next) {
                self.aligned_row = row;
                if let Some(hunk) = self.rows[row].hunk {
                    self.selected = Some(hunk);
                }
            }
        }
        self.keep_cursor_visible();
    }

    fn move_column(&mut self, forward: bool) {
        let (line, _) = self.position_for_row(self.focus, self.aligned_row);
        let maximum = text::buffer_line_content(&self.pane(self.focus).buffer, line)
            .graphemes(true)
            .count();
        self.column = if forward {
            (self.column + 1).min(maximum)
        } else {
            self.column.saturating_sub(1)
        };
        self.keep_horizontal_visible();
    }

    fn scroll_by(&mut self, amount: usize, forward: bool) {
        if self.local_pending {
            let limit = self.pane(self.focus).buffer.line_count();
            self.aligned_row = if forward {
                self.aligned_row
                    .saturating_add(amount)
                    .min(limit.saturating_sub(1))
            } else {
                self.aligned_row.saturating_sub(amount)
            };
        } else {
            let projected = self.projected_rows(self.focus);
            let index = projected
                .binary_search(&self.aligned_row)
                .unwrap_or_else(|index| index.min(projected.len().saturating_sub(1)));
            let next = if forward {
                index
                    .saturating_add(amount)
                    .min(projected.len().saturating_sub(1))
            } else {
                index.saturating_sub(amount)
            };
            if let Some(&row) = projected.get(next) {
                self.aligned_row = row;
            }
        }
        self.keep_cursor_visible();
    }

    fn select_hunk(&mut self, forward: bool) {
        let Some(current) = self.selected else {
            return;
        };
        let next = if forward {
            (current + 1).min(self.hunk_count() - 1)
        } else {
            current.saturating_sub(1)
        };
        self.selected = Some(next);
        if let Some(row) = self.rows.iter().position(|row| row.hunk == Some(next)) {
            self.aligned_row = row;
            self.column = 0;
            self.keep_cursor_visible();
        }
    }

    fn enter_edit(&mut self) {
        if self.pane(self.focus).read_only {
            self.message = "This pane is read-only".to_owned();
            return;
        }
        let (line, column) = self.position_for_row(self.focus, self.aligned_row);
        let offset = text::buffer_position_offset(&self.pane(self.focus).buffer, line, column);
        if self.pane_mut(self.focus).buffer.set_cursor(offset).is_ok() {
            self.editing = true;
            self.column = column;
        }
    }

    fn insert_text(&mut self, inserted: &str) {
        if inserted.is_empty() || self.pane(self.focus).read_only || inserted.contains('\0') {
            return;
        }
        let start = self.pane(self.focus).buffer.cursor();
        let edited_resolution = if self.focus == Pane::Result {
            self.resolution_at_position(start)
        } else {
            None
        };
        let previous = if self.focus == Pane::Result {
            self.merge_metadata()
        } else {
            None
        };
        if self.pane_mut(self.focus).buffer.insert(inserted).is_ok() {
            self.record_merge_edit(previous);
            self.record_action(self.focus);
            self.adjust_merge_regions(
                self.focus,
                start..start,
                inserted.chars().count(),
                true,
                edited_resolution,
            );
            self.refresh_after_edit(self.focus);
        }
    }

    fn delete_grapheme(&mut self, backward: bool) {
        if self.pane(self.focus).read_only {
            return;
        }
        let current = self.pane(self.focus).buffer.cursor();
        let buffer = &self.pane(self.focus).buffer;
        let range = if backward {
            text::buffer_previous_grapheme(buffer, current)..current
        } else {
            current..text::buffer_next_grapheme(buffer, current)
        };
        if range.is_empty() {
            return;
        }
        let previous = if self.focus == Pane::Result {
            self.merge_metadata()
        } else {
            None
        };
        if self
            .pane_mut(self.focus)
            .buffer
            .delete(range.clone())
            .is_ok()
        {
            self.record_merge_edit(previous);
            self.record_action(self.focus);
            self.adjust_merge_regions(self.focus, range, 0, true, None);
            self.refresh_after_edit(self.focus);
        }
    }

    fn move_edit_cursor(&mut self, right: bool) {
        let current = self.pane(self.focus).buffer.cursor();
        let buffer = &self.pane(self.focus).buffer;
        let target = if right {
            text::buffer_next_grapheme(buffer, current)
        } else {
            text::buffer_previous_grapheme(buffer, current)
        };
        self.set_edit_cursor(target);
    }

    fn move_edit_line(&mut self, down: bool) {
        let buffer = &self.pane(self.focus).buffer;
        let (line, column) = text::buffer_cursor_position(buffer, buffer.cursor());
        let target_line = if down {
            line.saturating_add(1)
        } else {
            line.saturating_sub(1)
        };
        let target = text::buffer_position_offset(buffer, target_line, column);
        self.set_edit_cursor(target);
    }

    fn move_edit_edge(&mut self, end: bool) {
        let buffer = &self.pane(self.focus).buffer;
        let (line, _) = text::buffer_cursor_position(buffer, buffer.cursor());
        let column = if end {
            text::buffer_line_content(buffer, line)
                .graphemes(true)
                .count()
        } else {
            0
        };
        let target = text::buffer_position_offset(buffer, line, column);
        self.set_edit_cursor(target);
    }

    fn set_edit_cursor(&mut self, offset: usize) {
        if self.pane_mut(self.focus).buffer.set_cursor(offset).is_ok() {
            let (line, column) =
                text::buffer_cursor_position(&self.pane(self.focus).buffer, offset);
            self.column = column;
            self.aligned_row = self.row_for_line(self.focus, line);
            self.keep_cursor_visible();
            if !self.local_pending {
                self.keep_horizontal_visible();
            }
        }
    }

    fn undo(&mut self) {
        let Some(pane) = self.undo_actions.pop() else {
            return;
        };
        let previous = if pane == Pane::Result {
            self.merge_metadata()
        } else {
            None
        };
        if !self.pane_mut(pane).buffer.undo() {
            self.undo_actions.push(pane);
            return;
        }
        if let (
            Mode::ThreeWay {
                undo_meta,
                redo_meta,
                ..
            },
            Some(previous),
        ) = (&mut self.mode, previous)
        {
            if let Some(restored) = undo_meta.pop() {
                redo_meta.push(previous);
                self.restore_merge_metadata(restored);
            }
        }
        self.redo_actions.push(pane);
        self.focus = pane;
        self.refresh_after_edit(pane);
    }

    fn redo(&mut self) {
        let Some(pane) = self.redo_actions.pop() else {
            return;
        };
        let previous = if pane == Pane::Result {
            self.merge_metadata()
        } else {
            None
        };
        if !self.pane_mut(pane).buffer.redo() {
            self.redo_actions.push(pane);
            return;
        }
        if let (
            Mode::ThreeWay {
                undo_meta,
                redo_meta,
                ..
            },
            Some(previous),
        ) = (&mut self.mode, previous)
        {
            if let Some(restored) = redo_meta.pop() {
                undo_meta.push(previous);
                self.restore_merge_metadata(restored);
            }
        }
        self.undo_actions.push(pane);
        self.focus = pane;
        self.refresh_after_edit(pane);
    }

    fn apply_hunk(&mut self) {
        if self.local_pending {
            self.message = "Waiting for the edited diff before applying a hunk".to_owned();
            return;
        }
        let Some(index) = self.selected else {
            return;
        };
        let (target, range, replacement, accepted) = match &self.mode {
            Mode::TwoWay { left, right, diff } => {
                let Some(hunk) = diff.hunks().get(index) else {
                    return;
                };
                let (target, source, destination, source_lines, destination_lines, direction) =
                    if self.focus == Pane::Left {
                        (
                            Pane::Right,
                            left,
                            right,
                            &hunk.left_lines,
                            &hunk.right_lines,
                            ApplyDirection::LeftToRight,
                        )
                    } else {
                        (
                            Pane::Left,
                            right,
                            left,
                            &hunk.right_lines,
                            &hunk.left_lines,
                            ApplyDirection::RightToLeft,
                        )
                    };
                if destination.read_only {
                    self.message = "Destination pane is read-only".to_owned();
                    return;
                }
                let accepted = diff.apply_hunk(hunk, &destination.snapshot, direction);
                let range =
                    text::line_range_chars(destination.snapshot.text(), destination_lines.clone());
                let source_range =
                    text::line_range_chars(source.snapshot.text(), source_lines.clone());
                let replacement =
                    text::slice_chars(source.snapshot.text(), source_range).to_owned();
                (target, range, replacement, accepted)
            }
            Mode::ThreeWay { .. } => return,
        };
        if accepted.is_err() {
            self.message = "Selected hunk belongs to a stale snapshot".to_owned();
            return;
        }
        if self
            .pane_mut(target)
            .buffer
            .replace(range, &replacement)
            .is_err()
        {
            self.message = "Cannot apply this hunk to the destination".to_owned();
            return;
        }
        self.record_action(target);
        self.refresh_after_edit(target);
    }

    fn choose_conflict(&mut self, key: KeyCode) {
        let Some(index) = self.selected else {
            return;
        };
        let before = self.merge_metadata();
        let Mode::ThreeWay {
            merge,
            result,
            conflicts,
            resolutions,
            resolved,
            ..
        } = &mut self.mode
        else {
            return;
        };
        let Some(region) = conflicts.get(index) else {
            return;
        };
        let id = region.id;
        let range = region.chars.clone();
        let ours = region.ours.clone();
        let theirs = region.theirs.clone();
        let choice = match key {
            KeyCode::Char('o') => ResolutionChoice::Ours,
            KeyCode::Char('t') => ResolutionChoice::Theirs,
            KeyCode::Char('b') => ResolutionChoice::Both,
            KeyCode::Char('r') if region.changed => ResolutionChoice::Manual(
                text::slice_chars(&result.buffer.text(), range.clone()).to_owned(),
            ),
            KeyCode::Char('r') => {
                self.message = "Edit the result before choosing it".to_owned();
                return;
            }
            _ => return,
        };
        let old_preview = merge.preview();
        let Some(core_span) = merge
            .preview_conflicts()
            .into_iter()
            .find(|span| span.id == id)
        else {
            return;
        };
        let resolution = match &choice {
            ResolutionChoice::Ours => ConflictResolution::Ours,
            ResolutionChoice::Theirs => ConflictResolution::Theirs,
            ResolutionChoice::Both => ConflictResolution::Both,
            ResolutionChoice::Manual(text) => ConflictResolution::Manual(text.clone()),
        };
        if merge.resolve(id, resolution).is_err() {
            return;
        }
        let updated = merge.preview();
        let replacement = if let ResolutionChoice::Manual(text) = &choice {
            text.clone()
        } else {
            let suffix = old_preview.text().chars().count() - core_span.result_chars.end;
            let end = updated.text().chars().count() - suffix;
            text::slice_chars(updated.text(), core_span.result_chars.start..end).to_owned()
        };
        if result.buffer.replace(range.clone(), &replacement).is_err() {
            self.message = "Cannot replace the selected conflict".to_owned();
            self.restore_merge_metadata(before.expect("merge metadata"));
            return;
        }
        let replacement_chars = replacement.chars().count();
        shift_following_conflicts(conflicts, index, range.clone(), replacement_chars);
        for region in resolved.iter_mut() {
            shift_tracked_range(&mut region.result, range.clone(), replacement_chars);
        }
        conflicts.remove(index);
        resolutions.push((id, choice));
        resolved.push(crate::session::ResolvedConflict {
            id,
            result: range.start..range.start + replacement_chars,
            ours,
            theirs,
        });
        self.record_merge_edit(before);
        self.record_action(Pane::Result);
        self.refresh_after_edit(Pane::Result);
        if self.selected.is_some() {
            self.select_hunk(false);
        }
    }

    fn insert_resolved_source(
        &mut self,
        id: chvrn_core::merge::ConflictId,
        source: Pane,
        position: InsertionPosition,
    ) {
        let Mode::ThreeWay {
            ours,
            result,
            theirs,
            resolved,
            ..
        } = &self.mode
        else {
            return;
        };
        let Some(region) = resolved.iter().find(|region| region.id == id) else {
            return;
        };
        let (source, lines) = match source {
            Pane::Ours => (ours, region.ours.clone()),
            Pane::Theirs => (theirs, region.theirs.clone()),
            _ => return,
        };
        let source_chars = text::line_range_chars(source.snapshot.text(), lines);
        let inserted = text::slice_chars(source.snapshot.text(), source_chars).to_owned();
        if inserted.is_empty() {
            return;
        }
        let offset = match position {
            InsertionPosition::Before => region.result.start,
            InsertionPosition::After => region.result.end,
        };
        if offset > result.buffer.len_chars() {
            return;
        }
        let before = self.merge_metadata();
        if self
            .pane_mut(Pane::Result)
            .buffer
            .replace(offset..offset, &inserted)
            .is_err()
        {
            self.message = "Cannot insert the selected source block".to_owned();
            return;
        }
        self.record_merge_edit(before);
        self.record_action(Pane::Result);
        self.adjust_merge_regions(
            Pane::Result,
            offset..offset,
            inserted.chars().count(),
            false,
            Some(id),
        );
        self.refresh_after_edit(Pane::Result);
    }

    fn resolution_at_position(&self, position: usize) -> Option<chvrn_core::merge::ConflictId> {
        let Mode::ThreeWay { resolved, .. } = &self.mode else {
            return None;
        };
        resolved
            .iter()
            .find(|region| region.result.start == position)
            .or_else(|| {
                resolved
                    .iter()
                    .find(|region| region.result.start < position && position < region.result.end)
            })
            .or_else(|| resolved.iter().find(|region| region.result.end == position))
            .map(|region| region.id)
    }

    fn adjust_merge_regions(
        &mut self,
        pane: Pane,
        range: Range<usize>,
        replacement_chars: usize,
        edit_selected_conflict: bool,
        expanded_resolution: Option<chvrn_core::merge::ConflictId>,
    ) {
        if pane != Pane::Result {
            return;
        }
        let Mode::ThreeWay {
            conflicts,
            resolved,
            ..
        } = &mut self.mode
        else {
            return;
        };
        let selected = self.selected;
        for (index, region) in conflicts.iter_mut().enumerate() {
            if range.start == range.end
                && range.start == region.chars.start
                && selected == Some(index)
                && edit_selected_conflict
            {
                region.chars.end += replacement_chars;
                region.changed = true;
            } else if range.end <= region.chars.start {
                region.chars.start = shifted(
                    region.chars.start,
                    range.end - range.start,
                    replacement_chars,
                );
                region.chars.end =
                    shifted(region.chars.end, range.end - range.start, replacement_chars);
            } else if range.start >= region.chars.end {
                if range.start == region.chars.end
                    && selected == Some(index)
                    && range.is_empty()
                    && edit_selected_conflict
                {
                    region.chars.end += replacement_chars;
                    region.changed = true;
                }
            } else {
                region.chars.start = region.chars.start.min(range.start);
                region.chars.end = shifted(
                    region.chars.end.max(range.end),
                    range.end - range.start,
                    replacement_chars,
                );
                region.changed = true;
            }
        }
        for region in resolved {
            if expanded_resolution == Some(region.id) && range.is_empty() {
                region.result.end = region.result.end.saturating_add(replacement_chars);
            } else if range.end <= region.result.start {
                region.result.start = shifted(
                    region.result.start,
                    range.end - range.start,
                    replacement_chars,
                );
                region.result.end = shifted(
                    region.result.end,
                    range.end - range.start,
                    replacement_chars,
                );
            } else if range.start < region.result.end {
                region.result.start = region.result.start.min(range.start);
                region.result.end = shifted(
                    region.result.end.max(range.end),
                    range.end - range.start,
                    replacement_chars,
                );
            }
        }
    }

    fn handle_mouse(&mut self, event: MouseEvent) -> ReviewOutcome {
        match event.kind {
            MouseEventKind::ScrollDown => self.scroll_by(3, true),
            MouseEventKind::ScrollUp => self.scroll_by(3, false),
            MouseEventKind::ScrollLeft => self.shift_horizontal(false),
            MouseEventKind::ScrollRight => self.shift_horizontal(true),
            MouseEventKind::Down(MouseButton::Left | MouseButton::Right) => {
                if let Some(hit) = self.mouse_target(event.column, event.row) {
                    self.focus = hit.pane;
                    self.aligned_row = hit.row;
                    self.column = hit.column;
                    let clicked_hunk = if self.local_pending {
                        None
                    } else {
                        self.rows.get(hit.row).and_then(|row| row.hunk)
                    };
                    if let Some(hunk) = clicked_hunk {
                        self.selected = Some(hunk);
                    }
                    self.keep_cursor_visible();
                    if event.kind == MouseEventKind::Down(MouseButton::Left) {
                        if let Some(action) = hit.action {
                            match action {
                                GutterAction::ApplyChoice { hunk } => {
                                    self.selected = Some(hunk);
                                    match hit.pane {
                                        Pane::Left | Pane::Right => self.apply_hunk(),
                                        Pane::Ours => self.choose_conflict(KeyCode::Char('o')),
                                        Pane::Theirs => self.choose_conflict(KeyCode::Char('t')),
                                        Pane::Result => {}
                                    }
                                }
                                GutterAction::InsertResolved { id, position } => {
                                    self.insert_resolved_source(id, hit.pane, position)
                                }
                            }
                            return ReviewOutcome::Continue;
                        }
                    } else if hit.gutter
                        && event.kind == MouseEventKind::Down(MouseButton::Right)
                        && !self.local_pending
                    {
                        let action_hunk = match hit.action {
                            Some(GutterAction::ApplyChoice { hunk }) => Some(hunk),
                            Some(GutterAction::InsertResolved { .. }) => None,
                            None => clicked_hunk,
                        };
                        if let Some(hunk) = action_hunk {
                            self.selected = Some(hunk);
                            match hit.pane {
                                Pane::Left | Pane::Right => self.apply_hunk(),
                                Pane::Ours => self.choose_conflict(KeyCode::Char('o')),
                                Pane::Theirs => self.choose_conflict(KeyCode::Char('t')),
                                Pane::Result => {}
                            }
                            return ReviewOutcome::Continue;
                        }
                    }
                    if self.pane(self.focus).read_only {
                        self.editing = false;
                    }
                    if self.editing {
                        let (line, column) = self.position_for_row(self.focus, self.aligned_row);
                        let offset = text::buffer_position_offset(
                            &self.pane(self.focus).buffer,
                            line,
                            column,
                        );
                        let _ = self.pane_mut(self.focus).buffer.set_cursor(offset);
                    }
                }
            }
            _ => {}
        }
        ReviewOutcome::Continue
    }

    fn shift_horizontal(&mut self, forward: bool) {
        let offset = self.horizontal.entry(self.focus).or_default();
        *offset = if forward {
            offset.saturating_add(3)
        } else {
            offset.saturating_sub(3)
        };
    }

    pub(crate) fn keep_horizontal_visible(&mut self) {
        if self.local_pending {
            return;
        }
        let target = self
            .rows
            .get(self.aligned_row)
            .and_then(|row| row.line(self.focus))
            .map_or(0, |line| line.cells_before(self.column));
        let visible = self.pane_content_width(self.focus).max(1);
        let offset = self.horizontal.entry(self.focus).or_default();
        if target < *offset {
            *offset = target;
        }
        if target >= *offset + visible {
            *offset = target + 1 - visible;
        }
    }
}

fn shifted(position: usize, removed: usize, inserted: usize) -> usize {
    if inserted >= removed {
        position.saturating_add(inserted - removed)
    } else {
        position.saturating_sub(removed - inserted)
    }
}

fn shift_tracked_range(tracked: &mut Range<usize>, replaced: Range<usize>, inserted: usize) {
    let removed = replaced.end - replaced.start;
    if replaced.end <= tracked.start {
        tracked.start = shifted(tracked.start, removed, inserted);
        tracked.end = shifted(tracked.end, removed, inserted);
    } else if replaced.start < tracked.end {
        tracked.start = tracked.start.min(replaced.start);
        tracked.end = shifted(tracked.end.max(replaced.end), removed, inserted);
    }
}

fn shift_following_conflicts(
    conflicts: &mut [ConflictRegion],
    selected: usize,
    replaced: Range<usize>,
    inserted: usize,
) {
    for (index, region) in conflicts.iter_mut().enumerate() {
        if index != selected && region.chars.start >= replaced.end {
            region.chars.start =
                shifted(region.chars.start, replaced.end - replaced.start, inserted);
            region.chars.end = shifted(region.chars.end, replaced.end - replaced.start, inserted);
        }
    }
}
