use chvrn_core::{
    merge::ConflictId,
    merge_advice::{MergeAdviceChoice, MergeAdviceSuggestion},
    structural::HighlightKind,
};
use ratatui::{
    Frame,
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Clear},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::{
    Pane, RepositoryReviewMode, WhitespacePolicy,
    session::{ChangeBand, ChangeKind, Mode, ReviewSession, ViewLine},
    text,
};

#[derive(Clone, Copy)]
pub(crate) struct MouseHit {
    pub(crate) pane: Pane,
    pub(crate) row: usize,
    pub(crate) column: usize,
    pub(crate) gutter: bool,
    pub(crate) action: Option<GutterAction>,
}

#[derive(Clone, Copy)]
pub(crate) enum GutterAction {
    ApplyChoice { hunk: usize },
    InsertResolved { id: ConflictId },
}

#[derive(Clone, Copy)]
struct PositionedAction {
    x: u16,
    y: u16,
    symbol: &'static str,
    hit: MouseHit,
}

#[derive(Clone, Copy)]
struct PaneArea {
    pane: Pane,
    outer: Rect,
    content: Rect,
    gutter: u16,
    overview_x: u16,
}

#[derive(Clone, Copy)]
struct Connector {
    area: Rect,
    left: Pane,
    right: Pane,
}

struct AdviceFooter {
    choice: &'static str,
    confidence: u8,
    parts: [Rect; 4],
    height: u16,
}

impl AdviceFooter {
    fn new(suggestion: &MergeAdviceSuggestion, width: u16) -> Self {
        let choice = match suggestion.choice {
            MergeAdviceChoice::Ours => "Ours",
            MergeAdviceChoice::Theirs => "Theirs",
            MergeAdviceChoice::LeaveUnresolved => "Leave unresolved",
        };
        let confidence = (suggestion.confidence * 100.0).round_ties_even() as u8;
        let confidence_width = match confidence {
            0..=9 => 13,
            10..=99 => 14,
            _ => 15,
        };
        let widths = [
            choice.len() as u16,
            confidence_width,
            if suggestion.choice == MergeAdviceChoice::LeaveUnresolved {
                0
            } else {
                13
            },
            12,
        ];
        let mut x = 0;
        let mut y = 0;
        let parts = std::array::from_fn(|index| {
            let part_width = widths[index].min(width);
            if part_width == 0 {
                return Rect::default();
            }
            let gap = if x == 0 {
                0
            } else if index == 1 {
                3
            } else {
                2
            };
            if part_width + gap > width.saturating_sub(x) {
                x = 0;
                y += 1;
            } else {
                x += gap;
            }
            let part = Rect::new(x, y, part_width, 1);
            x += part_width;
            part
        });
        Self {
            choice,
            confidence,
            parts,
            height: y + 1,
        }
    }
}

const SURFACE: Color = Color::Rgb(25, 27, 30);
const HEADING: Color = Color::Rgb(34, 37, 41);
const RAIL: Color = Color::Rgb(21, 23, 27);
const INK: Color = Color::Rgb(199, 204, 209);
const MUTED: Color = Color::Rgb(108, 115, 122);
const ACCENT: Color = Color::Rgb(112, 194, 210);
const MODIFIED: Color = Color::Rgb(222, 180, 106);
const CONNECTOR_WIDTH: u16 = 5;

fn wrap_help_line(lines: &mut Vec<String>, text: &str, width: usize) {
    if width == 0 {
        return;
    }
    let mut start = 0;
    let mut occupied = 0;
    for (index, grapheme) in text.grapheme_indices(true) {
        let next_width = grapheme.width();
        if occupied > 0 && occupied + next_width > width {
            lines.push(text[start..index].to_owned());
            start = index;
            occupied = 0;
        }
        occupied += next_width;
    }
    lines.push(text[start..].to_owned());
}

pub(crate) fn visible_pane_count(session: &ReviewSession, width: u16) -> usize {
    match (&session.mode, width) {
        (Mode::TwoWay { .. }, width) if width < 48 => 1,
        (Mode::ThreeWay { .. }, width) if width < 75 => 1,
        (Mode::TwoWay { .. }, _) => 2,
        (Mode::ThreeWay { .. }, _) => 3,
    }
}

fn pane_areas(session: &ReviewSession, area: Rect) -> ([PaneArea; 3], [Connector; 2], usize) {
    let footer_height = session.merge_advice.dialog.as_ref().map_or(1, |dialog| {
        AdviceFooter::new(&dialog.suggestion, area.width).height
    });
    let body = Rect::new(
        area.x,
        area.y.saturating_add(2),
        area.width,
        area.height.saturating_sub(2 + footer_height),
    );
    let count = visible_pane_count(session, area.width);
    let available = body
        .width
        .saturating_sub(CONNECTOR_WIDTH * (count as u16 - 1));
    let panes = match (&session.mode, count) {
        (_, 1) => [session.focus; 3],
        (Mode::TwoWay { .. }, _) => [Pane::Left, Pane::Right, Pane::Right],
        (Mode::ThreeWay { .. }, _) => [Pane::Ours, Pane::Result, Pane::Theirs],
    };
    let areas = std::array::from_fn(|index| {
        let slot = index.min(count);
        let start_offset = (u32::from(available) * slot as u32 / count as u32
            + u32::from(CONNECTOR_WIDTH) * slot as u32)
            .min(u32::from(body.width)) as u16;
        let end_offset = if index >= count {
            start_offset
        } else {
            (u32::from(available) * (slot as u32 + 1) / count as u32
                + u32::from(CONNECTOR_WIDTH) * slot as u32)
                .min(u32::from(body.width)) as u16
        };
        let outer = Rect::new(
            body.x.saturating_add(start_offset),
            body.y,
            end_offset - start_offset,
            body.height,
        );
        let gutter = outer.width.saturating_sub(2).min(5);
        let content = Rect::new(
            outer.x.saturating_add(gutter),
            outer.y,
            outer.width.saturating_sub(gutter + 1),
            outer.height,
        );
        PaneArea {
            pane: panes[index],
            outer,
            content,
            gutter,
            overview_x: outer.x.saturating_add(outer.width.saturating_sub(1)),
        }
    });
    let connectors = std::array::from_fn(|index| Connector {
        area: Rect::new(
            areas[index].outer.right(),
            body.y,
            CONNECTOR_WIDTH,
            body.height,
        ),
        left: panes[index],
        right: panes[index + 1],
    });
    (areas, connectors, count)
}

impl ReviewSession {
    pub(crate) fn pane_content_width(&self, pane: Pane) -> usize {
        let (panes, _, count) = pane_areas(self, Rect::new(0, 0, self.width, self.height));
        panes
            .iter()
            .take(count)
            .find(|area| area.pane == pane)
            .map_or(0, |area| usize::from(area.content.width))
    }

    pub fn render(&self, frame: &mut Frame<'_>) {
        let area = frame.area();
        if area.width == 0 || area.height == 0 {
            return;
        }
        let mode = match self.repository_review.as_ref().map(|review| review.mode) {
            Some(RepositoryReviewMode::Index) => "INDEX",
            Some(RepositoryReviewMode::Revision) => "REVISION",
            Some(RepositoryReviewMode::PatchPreview) => "PATCH PREVIEW",
            None => match &self.mode {
                Mode::TwoWay { .. } => "DIFF",
                Mode::ThreeWay { .. } => "MERGE",
            },
        };
        let language = if self.pane(self.focus).language.is_some() {
            "syntax"
        } else {
            "plain text"
        };
        let whitespace = match self.whitespace {
            WhitespacePolicy::Exact => "Exact",
            WhitespacePolicy::IgnoreEdge => "IgnoreEdge",
            WhitespacePolicy::IgnoreAll => "IgnoreAll",
            WhitespacePolicy::IgnoreBlankLines => "IgnoreBlankLines",
        };
        frame
            .buffer_mut()
            .set_style(area, Style::default().fg(INK).bg(SURFACE));
        frame.buffer_mut().set_style(
            Rect::new(area.x, area.y, area.width, 1),
            Style::default().fg(MUTED).bg(HEADING),
        );
        let quiet = Style::default().fg(MUTED).bg(HEADING);
        let normal = Style::default().fg(INK).bg(HEADING);
        let editing = if self.editing {
            normal.fg(ACCENT).add_modifier(Modifier::BOLD)
        } else {
            quiet
        };
        let modified = if self.changed {
            normal.fg(MODIFIED).add_modifier(Modifier::BOLD)
        } else {
            quiet
        };
        let mut x = area.x.saturating_add(1);
        for (label, style) in [
            ("chvrn  ", quiet),
            (
                self.repository_review
                    .as_ref()
                    .map_or("", |review| review.position.as_str()),
                quiet,
            ),
            (
                if self.repository_review.is_some() {
                    "  "
                } else {
                    ""
                },
                quiet,
            ),
            (mode, normal),
            ("  ", quiet),
            (if self.editing { "INSERT" } else { "REVIEW" }, editing),
            ("  ", quiet),
            (if self.changed { "modified" } else { "clean" }, modified),
            ("  ", quiet),
            (whitespace, quiet),
            ("  ", quiet),
            (language, quiet),
        ] {
            (x, _) = frame.buffer_mut().set_stringn(
                x,
                area.y,
                label,
                usize::from(area.right().saturating_sub(x)),
                style,
            );
        }
        let (panes, connectors, count) = pane_areas(self, area);
        if area.height > 1 {
            for pane_area in panes.iter().take(count) {
                frame.buffer_mut().set_style(
                    Rect::new(pane_area.outer.x, area.y + 1, pane_area.outer.width, 1),
                    Style::default().bg(HEADING),
                );
                let style = Style::default()
                    .fg(if pane_area.pane == self.focus {
                        INK
                    } else {
                        MUTED
                    })
                    .bg(HEADING);
                if pane_area.pane == self.focus {
                    frame
                        .buffer_mut()
                        .set_stringn(pane_area.outer.x, area.y + 1, "▸", 1, style);
                }
                frame.buffer_mut().set_stringn(
                    pane_area.outer.x.saturating_add(2),
                    area.y + 1,
                    pane_name(pane_area.pane),
                    usize::from(pane_area.outer.width.saturating_sub(2)),
                    style,
                );
            }
        }
        for pane_area in panes.iter().take(count) {
            self.render_pane(frame, pane_area);
        }
        if !self.local_pending {
            for connector in connectors.iter().take(count - 1) {
                self.render_connector(frame, connector);
            }
        }
        if area.height >= 2 {
            self.render_footer(frame);
        }
        if self.help {
            self.render_help(frame);
        }
    }

    fn render_footer(&self, frame: &mut Frame<'_>) {
        let screen = frame.area();
        if let Some(dialog) = &self.merge_advice.dialog {
            self.render_merge_advice_footer(
                frame,
                &AdviceFooter::new(&dialog.suggestion, screen.width),
            );
            return;
        }
        let area = Rect::new(screen.x, screen.bottom() - 1, screen.width, 1);
        let normal = Style::default().fg(INK).bg(HEADING);
        let key_style = normal.fg(ACCENT).add_modifier(Modifier::BOLD);
        let buffer = frame.buffer_mut();
        buffer.set_style(area, normal);
        let message = if self.confirming_discard {
            "Unsaved changes. y discards and quits; any other key returns"
        } else if self.refresh_conflict {
            "External changes conflict with local edits. R discards local edits and reloads; submit is blocked"
        } else if self.local_pending {
            "Updating edited diff; hunk apply and submit wait for the latest alignment"
        } else if self.confirming_merge {
            "[y] Write merge  [n/Esc] Review"
        } else {
            &self.message
        };
        if !message.is_empty() {
            buffer.set_stringn(
                area.x,
                area.y,
                message,
                usize::from(area.width),
                normal.fg(MODIFIED),
            );
            return;
        }
        let review = !self.editing;
        let repository_mode = self.repository_review.as_ref().map(|review| review.mode);
        let repository_hunk = repository_mode.is_some()
            && review
            && self.selected.is_some()
            && !self.is_dirty()
            && matches!(&self.mode, Mode::TwoWay { right, .. } if !right.read_only);
        let path = match (&self.mode, self.focus) {
            (Mode::TwoWay { .. }, Pane::Left) => self.left_path.as_deref(),
            (Mode::TwoWay { .. }, _) => self.right_path.as_deref(),
            (Mode::ThreeWay { .. }, _) => self.output_path.as_deref(),
        };
        let filename = path.map(|path| {
            path.file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy()
        });
        let conflict = match &self.mode {
            Mode::ThreeWay { conflicts, .. } => {
                self.selected.and_then(|index| conflicts.get(index))
            }
            Mode::TwoWay { .. } => None,
        };
        let can_copy = match &self.mode {
            Mode::TwoWay { left, right, .. } => {
                self.selected.is_some()
                    && !if self.focus == Pane::Left {
                        right.read_only
                    } else {
                        left.read_only
                    }
            }
            Mode::ThreeWay { .. } => false,
        };
        let shortcuts = [
            ("[Esc]", "Review", self.editing, true),
            (
                "[Ctrl-Z]",
                "Undo",
                self.editing && !self.undo_actions.is_empty(),
                false,
            ),
            (
                "[Ctrl-N/P]",
                "Files",
                review && repository_mode.is_some(),
                false,
            ),
            (
                "[S]",
                "Stage",
                repository_hunk && repository_mode == Some(RepositoryReviewMode::Index),
                false,
            ),
            (
                "[x]",
                "Restore",
                repository_hunk && repository_mode == Some(RepositoryReviewMode::Revision),
                false,
            ),
            (
                "[x]",
                "Decline",
                review && repository_mode == Some(RepositoryReviewMode::PatchPreview),
                false,
            ),
            (
                "[m]",
                "Merge",
                review
                    && matches!(
                        repository_mode,
                        Some(RepositoryReviewMode::Index | RepositoryReviewMode::Revision)
                    ),
                false,
            ),
            ("[c]", "Comment", review && repository_mode.is_some(), false),
            ("[J]", "Suggest", self.can_begin_merge_advice(), true),
            ("[o]", "Ours", review && conflict.is_some(), false),
            ("[t]", "Theirs", review && conflict.is_some(), false),
            ("[b]", "Both", review && conflict.is_some(), false),
            (
                "[r]",
                "Manual",
                review && conflict.is_some_and(|region| region.changed),
                false,
            ),
            ("[a]", "Copy", review && can_copy, false),
            (
                "[i]",
                "Edit",
                review && !self.pane(self.focus).read_only,
                false,
            ),
            (
                "[u]",
                "Undo",
                review && !self.undo_actions.is_empty(),
                false,
            ),
            ("[Ctrl-R]", "Redo", !self.redo_actions.is_empty(), false),
            (
                "[s]",
                if repository_mode.is_some() {
                    "Accept"
                } else {
                    "Save"
                },
                review,
                true,
            ),
            ("[q]", "Quit", review, true),
            ("[?]", "Help", review, true),
            ("[Tab]", "Next pane", review, false),
            ("[Shift-Tab]", "Previous pane", review, false),
            ("[ / ]", "Hunks", review && self.selected.is_some(), false),
        ];
        let mandatory_width: usize = shortcuts
            .iter()
            .filter(|(_, _, enabled, essential)| *enabled && *essential)
            .map(|(key, label, _, _)| key.len() + label.len() + 3)
            .sum();
        let compact = usize::from(area.width) < mandatory_width.saturating_sub(2);
        let filename_width = filename.as_ref().map_or(0, |name| name.width());
        let filename_reserved = filename
            .as_ref()
            .map_or(0, |_| filename_width.saturating_add(2).min(26))
            .min(usize::from(area.width).saturating_sub(mandatory_width));
        let controls_right = area.right().saturating_sub(filename_reserved as u16);
        let mut reserved: usize = shortcuts
            .iter()
            .filter(|(_, _, enabled, essential)| *enabled && *essential)
            .map(|(key, label, _, _)| key.len() + if compact { 2 } else { label.len() + 3 })
            .sum();
        let mut x = area.x;
        for (key, label, enabled, essential) in shortcuts {
            if !enabled {
                continue;
            }
            let label = if compact { "" } else { label };
            let width = key.len() + if label.is_empty() { 0 } else { label.len() + 1 };
            if essential {
                reserved = reserved.saturating_sub(width + 2);
            }
            if width + reserved > usize::from(controls_right.saturating_sub(x)) {
                continue;
            }
            (x, _) = buffer.set_stringn(x, area.y, key, key.len(), key_style);
            if !label.is_empty() {
                (x, _) = buffer.set_stringn(x, area.y, " ", 1, normal);
                (x, _) = buffer.set_stringn(x, area.y, label, label.len(), normal);
            }
            x = x.saturating_add(2);
        }
        if let Some(name) = filename {
            let available = usize::from(area.right().saturating_sub(x));
            let style = normal.fg(MUTED);
            let width = filename_width;
            if width <= available {
                buffer.set_stringn(area.right() - width as u16, area.y, &name, width, style);
            } else if available >= 3 {
                let mut start = name.len();
                let mut width = 1;
                for (index, grapheme) in name.grapheme_indices(true).rev() {
                    let next_width = width + grapheme.width();
                    if next_width > available {
                        break;
                    }
                    start = index;
                    width = next_width;
                }
                let x = area.right() - width as u16;
                buffer.set_stringn(x, area.y, "…", 1, style);
                buffer.set_stringn(x + 1, area.y, &name[start..], width - 1, style);
            }
        }
    }

    fn render_pane(&self, frame: &mut Frame<'_>, pane_area: &PaneArea) {
        let pane = pane_area.pane;
        if pane_area.content.width == 0 || pane_area.content.height == 0 {
            return;
        }
        if self.local_pending {
            self.render_pending_pane(frame, pane_area);
            return;
        }
        let view = self.pane(pane);
        for offset in 0..usize::from(pane_area.content.height) {
            let Some(row_index) = self.pane_row(pane, offset) else {
                break;
            };
            let Some(line) = self.rows.get(row_index).and_then(|row| row.line(pane)) else {
                continue;
            };
            let projection_index = self.pane_top(pane) + offset;
            let region = self.pane_region(pane, projection_index);
            let background = region.map_or(SURFACE, |(kind, selected)| {
                region_color(kind, pane, selected)
            });
            let y = pane_area.content.y + offset as u16;
            frame.buffer_mut().set_style(
                Rect::new(
                    pane_area.outer.x,
                    y,
                    pane_area.outer.width.saturating_sub(1),
                    1,
                ),
                Style::default().fg(INK).bg(background),
            );
            render_line_number(
                frame.buffer_mut(),
                pane_area.outer.x,
                y,
                pane_area.gutter,
                line.number + 1,
                background,
            );
            let horizontal = *self.horizontal.get(&pane).unwrap_or(&0);
            render_line(
                frame.buffer_mut(),
                pane_area.content,
                y,
                line,
                &view.syntax,
                horizontal,
                background,
                region.map(|(kind, _)| kind),
            );
            if row_index == self.aligned_row && pane == self.focus {
                if let Some(x) = cursor_cell(pane_area.content, Some(line), self.column, horizontal)
                {
                    let cell = &mut frame.buffer_mut()[(x, y)];
                    cell.set_style(cell.style().add_modifier(Modifier::REVERSED));
                }
            }
        }
        self.render_overview(frame.buffer_mut(), pane_area);
    }

    fn render_pending_pane(&self, frame: &mut Frame<'_>, pane_area: &PaneArea) {
        let pane = pane_area.pane;
        let source = &self.pane(pane).buffer;
        for line_index in 0..usize::from(pane_area.content.height) {
            let number = self.scroll + line_index;
            let Some(start) = source.line_to_char(number) else {
                break;
            };
            let end = source
                .line_to_char(number + 1)
                .unwrap_or(source.len_chars());
            let y = pane_area.content.y + line_index as u16;
            render_line_number(
                frame.buffer_mut(),
                pane_area.outer.x,
                y,
                pane_area.gutter,
                number + 1,
                SURFACE,
            );
            let cursor = (pane == self.focus && number == self.aligned_row)
                .then_some(source.cursor().clamp(start, end));
            let first = cursor.map_or(start, |cursor| start.max(cursor.saturating_sub(64)));
            let mut last = end.min(first.saturating_add(512));
            let snippet = loop {
                match source.slice_chars(first..last) {
                    Ok(snippet) => break snippet,
                    Err(_) if last > first => last -= 1,
                    Err(_) => break String::new(),
                }
            };
            let mut snippet = snippet;
            while snippet.ends_with('\n') || snippet.ends_with('\r') {
                snippet.pop();
            }
            let clipped = first > start;
            let mut content = pane_area.content;
            if clipped && content.width > 0 {
                frame.buffer_mut().set_string(
                    content.x,
                    y,
                    "‹",
                    Style::default().fg(MUTED).bg(SURFACE),
                );
                content.x += 1;
                content.width -= 1;
            }
            let line = ViewLine::new(number, snippet, 0);
            render_line(frame.buffer_mut(), content, y, &line, &[], 0, SURFACE, None);
            if let Some(cursor) = cursor {
                let relative = cursor.saturating_sub(first).min(line.text.chars().count());
                let column = text::grapheme_column(&line.text, relative);
                if let Some(x) = cursor_cell(content, Some(&line), column, 0) {
                    let cell = &mut frame.buffer_mut()[(x, y)];
                    cell.set_style(cell.style().add_modifier(Modifier::REVERSED));
                }
            }
        }
        self.render_overview(frame.buffer_mut(), pane_area);
    }

    fn pane_region(&self, pane: Pane, index: usize) -> Option<(ChangeKind, bool)> {
        let pairs: &[(Pane, Pane)] = match &self.mode {
            Mode::TwoWay { .. } => &[(Pane::Left, Pane::Right)],
            Mode::ThreeWay { .. } => &[(Pane::Ours, Pane::Result), (Pane::Result, Pane::Theirs)],
        };
        pairs
            .iter()
            .filter(|(left, right)| pane == *left || pane == *right)
            .filter_map(|(left, right)| {
                band_at(self.change_bands(*left, *right), index, pane == *left)
            })
            .max_by_key(|band| kind_priority(band.kind))
            .map(|band| {
                (
                    band.kind,
                    band.hunk.is_some_and(|hunk| self.selected == Some(hunk)),
                )
            })
    }

    fn overview_region(&self, pane: Pane, first: usize, end: usize) -> Option<ChangeKind> {
        let pairs: &[(Pane, Pane)] = match &self.mode {
            Mode::TwoWay { .. } => &[(Pane::Left, Pane::Right)],
            Mode::ThreeWay { .. } => &[(Pane::Ours, Pane::Result), (Pane::Result, Pane::Theirs)],
        };
        pairs
            .iter()
            .filter(|(left, right)| pane == *left || pane == *right)
            .filter_map(|(left, right)| {
                band_in_range(self.change_bands(*left, *right), first, end, pane == *left)
            })
            .max_by_key(|band| kind_priority(band.kind))
            .map(|band| band.kind)
    }

    fn render_overview(&self, buffer: &mut Buffer, pane_area: &PaneArea) {
        let height = usize::from(pane_area.outer.height);
        if height == 0 {
            return;
        }
        let pane = pane_area.pane;
        let total = if self.local_pending {
            self.pane(pane).buffer.line_count()
        } else {
            self.projected_rows(pane).len()
        }
        .max(1);
        let top = if self.local_pending {
            self.scroll
        } else {
            self.pane_top(pane)
        };
        let viewport_start = top.saturating_mul(height) / total;
        let viewport_end = top
            .saturating_add(height)
            .min(total)
            .saturating_mul(height)
            .div_ceil(total);
        for offset in 0..height {
            let first = total.saturating_mul(offset).saturating_add(height - 1) / height;
            let end = total.saturating_mul(offset + 1).saturating_add(height - 1) / height;
            let kind = if !self.local_pending && end > first {
                self.overview_region(pane, first, end)
            } else {
                None
            };
            let in_viewport = (viewport_start..viewport_end).contains(&offset);
            let y = pane_area.outer.y + offset as u16;
            let (symbol, color) = match (kind, in_viewport) {
                (Some(kind), true) => ("█", brighten(band_color(kind), 40)),
                (Some(kind), false) => ("▐", band_color(kind)),
                (None, true) => ("█", Color::Rgb(91, 110, 125)),
                (None, false) => ("│", Color::Rgb(47, 51, 57)),
            };
            buffer.set_string(
                pane_area.overview_x,
                y,
                symbol,
                Style::default().fg(color).bg(RAIL),
            );
        }
    }

    fn render_connector(&self, frame: &mut Frame<'_>, connector: &Connector) {
        if connector.area.width == 0 || connector.area.height == 0 {
            return;
        }
        frame
            .buffer_mut()
            .set_style(connector.area, Style::default().fg(MUTED).bg(RAIL));
        let left_top = self.pane_top(connector.left);
        let right_top = self.pane_top(connector.right);
        let height = usize::from(connector.area.height);
        let bands = self.change_bands(connector.left, connector.right);
        let first =
            bands.partition_point(|band| band.left.end < left_top && band.right.end < right_top);
        let bands = &bands[first..];
        let end = bands.partition_point(|band| {
            band.left.start <= left_top.saturating_add(height)
                || band.right.start <= right_top.saturating_add(height)
        });
        let bands = &bands[..end];
        for band in bands {
            draw_connector_band(
                frame.buffer_mut(),
                connector.area,
                band,
                left_top,
                right_top,
            );
        }
        let action_bands = self.action_bands(connector.left, connector.right);
        for band in action_bands.iter().rev() {
            for source in [connector.right, connector.left] {
                if let Some(action) = self.band_action(connector, band, source) {
                    frame.buffer_mut().set_string(
                        action.x,
                        action.y,
                        action.symbol,
                        Style::default()
                            .fg(Color::Rgb(225, 220, 188))
                            .bg(band_color(band.kind))
                            .add_modifier(Modifier::BOLD),
                    );
                }
            }
        }
    }

    fn band_action(
        &self,
        connector: &Connector,
        band: &ChangeBand,
        source: Pane,
    ) -> Option<PositionedAction> {
        if self.local_pending {
            return None;
        }
        let (edge_x, y, row) = self.band_action_anchor(connector, band, source)?;
        let hit = |action| MouseHit {
            pane: source,
            row,
            column: 0,
            gutter: true,
            action: Some(action),
        };
        let choice = matches!(
            (&self.mode, connector.left, connector.right, source),
            (
                Mode::TwoWay { .. },
                Pane::Left,
                Pane::Right,
                Pane::Left | Pane::Right
            ) | (Mode::ThreeWay { .. }, Pane::Ours, Pane::Result, Pane::Ours)
                | (
                    Mode::ThreeWay { .. },
                    Pane::Result,
                    Pane::Theirs,
                    Pane::Theirs
                )
        ) && band.hunk.is_some();
        if choice {
            let hunk = band.hunk.expect("choice action must identify its hunk");
            let symbol = if source == connector.left { "»" } else { "«" };
            return Some(PositionedAction {
                x: edge_x,
                y,
                symbol,
                hit: hit(GutterAction::ApplyChoice { hunk }),
            });
        }
        let id = band.resolved?;
        let resolved_source = matches!(
            (connector.left, connector.right, source),
            (Pane::Ours, Pane::Result, Pane::Ours) | (Pane::Result, Pane::Theirs, Pane::Theirs)
        );
        if !resolved_source {
            return None;
        }
        Some(PositionedAction {
            x: edge_x,
            y,
            symbol: if source == connector.left {
                "↘"
            } else {
                "↙"
            },
            hit: hit(GutterAction::InsertResolved { id }),
        })
    }

    fn band_action_anchor(
        &self,
        connector: &Connector,
        band: &ChangeBand,
        source: Pane,
    ) -> Option<(u16, u16, usize)> {
        let (range, x) = if source == connector.left {
            (&band.left, connector.area.x)
        } else {
            (&band.right, connector.area.right().saturating_sub(1))
        };
        let top = self.pane_top(source);
        if range.is_empty() {
            let other = if source == connector.left {
                connector.right
            } else {
                connector.left
            };
            let other_index = if source == connector.left {
                band.right.start
            } else {
                band.left.start
            };
            let row = *self.projected_rows(other).get(other_index)?;
            let offset = range.start.checked_sub(top)?;
            if offset < usize::from(connector.area.height) {
                let y = connector.area.y + offset as u16;
                return Some((x, y, row));
            }
            return None;
        }
        let end = range
            .end
            .min(top.saturating_add(usize::from(connector.area.height)));
        for index in range.start.max(top)..end {
            let offset = index - top;
            let row = self.pane_row(source, offset)?;
            let y = connector.area.y + offset as u16;
            return Some((x, y, row));
        }
        None
    }

    pub(crate) fn prepare_help(&mut self) {
        self.help_lines.clear();
        let width = usize::from(self.width.min(100).saturating_sub(2));
        let guide = [
            "?/Esc: close help   Up/Down/PgUp/PgDn: scroll help",
            "Tab: next pane   Shift-Tab: previous pane",
            "j/k or arrows: rows   h/l: columns",
            "[ / ]: previous/next hunk",
            "i: edit   Esc: review   u: undo   Ctrl-R: redo",
            "Insert mode: type text; Ctrl-Z: undo; Ctrl-R: redo",
            match self.mode {
                Mode::TwoWay { .. } => "a or »/«: copy the focused source hunk",
                Mode::ThreeWay { .. } => "o/t/b: choose ours/theirs/both   r: accept manual edit",
            },
            match self.mode {
                Mode::TwoWay { .. } => "Both panes are editable unless marked read-only",
                Mode::ThreeWay { .. } => "»/«: choose source   ↘/↙: insert remaining source below",
            },
            "w: whitespace policy   PageUp/PageDown: scroll",
            match self.mode {
                Mode::TwoWay { .. } => "s: save/submit   q: quit without approval",
                Mode::ThreeWay { .. } => "s: confirm merge   y: write   n/Esc: review   q: quit",
            },
        ];
        for line in guide {
            wrap_help_line(&mut self.help_lines, line, width);
        }
        if self.merge_advice_eligible() {
            for line in [
                "J: ask Jev for a suggestion for the selected unresolved conflict",
                "Sends the complete base, ours and theirs conflict plus up to 20 lines before and after each to TypeSafe. Source context may contain secrets.",
                "Model confidence is informational, not a correctness guarantee. Enter applies a suggested side; Esc/q dismisses. Writing still requires confirmation.",
            ] {
                wrap_help_line(&mut self.help_lines, line, width);
            }
        }
        if let Some(review) = &self.repository_review {
            wrap_help_line(
                &mut self.help_lines,
                "Ctrl-N/P: next/previous file   c: comment   s: save and accept file",
                width,
            );
            let action = match review.mode {
                RepositoryReviewMode::Index => "S: stage the selected hunk immediately",
                RepositoryReviewMode::Revision => "x: restore the selected hunk immediately",
                RepositoryReviewMode::PatchPreview => {
                    "x: decline preview; no disk changes until all files are accepted"
                }
            };
            wrap_help_line(&mut self.help_lines, action, width);
            if review.mode != RepositoryReviewMode::PatchPreview {
                wrap_help_line(
                    &mut self.help_lines,
                    "m: open the selected unresolved Git text conflict in three-way merge; confirmed saves do not stage",
                    width,
                );
            }
        }
        let (left_label, right_label) = match self.mode {
            Mode::TwoWay { .. } => ("Left:", "Right:"),
            Mode::ThreeWay { .. } => ("Ours:", "Theirs:"),
        };
        for (label, path) in [
            (left_label, self.left_path.as_deref()),
            (right_label, self.right_path.as_deref()),
            ("Output:", self.output_path.as_deref()),
        ] {
            if let Some(path) = path {
                wrap_help_line(&mut self.help_lines, "", width);
                wrap_help_line(&mut self.help_lines, label, width);
                wrap_help_line(&mut self.help_lines, &path.to_string_lossy(), width);
            }
        }
        self.help_scroll = self.help_scroll.min(
            self.help_lines
                .len()
                .saturating_sub(usize::from(self.height.saturating_sub(2))),
        );
    }

    fn render_merge_advice_footer(&self, frame: &mut Frame<'_>, footer: &AdviceFooter) {
        let screen = frame.area();
        let height = footer.height.min(screen.height.saturating_sub(1));
        let area = Rect::new(screen.x, screen.bottom() - height, screen.width, height);
        let normal = Style::default().fg(INK).bg(HEADING);
        let confidence = format!("{}% confidence", footer.confidence);
        let values = [
            footer.choice,
            confidence.as_str(),
            "[Enter] Apply",
            "[Esc] Ignore",
        ];
        let buffer = frame.buffer_mut();
        buffer.set_style(area, normal);
        let hidden_rows = footer.height - height;
        for (index, (part, value)) in footer.parts.iter().zip(values).enumerate() {
            if part.width == 0 || part.y < hidden_rows {
                continue;
            }
            let x = area.x + part.x;
            let y = area.y + part.y - hidden_rows;
            let style = match index {
                1 => normal.fg(MUTED),
                2 | 3 => normal.fg(ACCENT).add_modifier(Modifier::BOLD),
                _ => normal,
            };
            if index == 1 && part.x >= 3 {
                buffer.set_stringn(x - 2, y, "·", 1, normal.fg(MUTED));
            }
            buffer.set_stringn(x, y, value, usize::from(part.width), style);
        }
    }

    fn render_help(&self, frame: &mut Frame<'_>) {
        let area = frame.area();
        let width = area.width.min(100);
        let height = usize::from(area.height).min(self.help_lines.len().saturating_add(2)) as u16;
        let box_area = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        let style = Style::default().fg(INK).bg(HEADING);
        frame.render_widget(Clear, box_area);
        let block = Block::default()
            .borders(Borders::ALL)
            .title("Help")
            .style(style);
        let inner = block.inner(box_area);
        frame.render_widget(block, box_area);
        let scroll = self.help_scroll.min(
            self.help_lines
                .len()
                .saturating_sub(usize::from(inner.height)),
        );
        for (row, line) in self
            .help_lines
            .iter()
            .skip(scroll)
            .take(usize::from(inner.height))
            .enumerate()
        {
            frame.buffer_mut().set_stringn(
                inner.x,
                inner.y + row as u16,
                line,
                usize::from(inner.width),
                style,
            );
        }
    }

    pub(crate) fn mouse_target(&self, x: u16, y: u16) -> Option<MouseHit> {
        let area = Rect::new(0, 0, self.width, self.height);
        let (panes, connectors, count) = pane_areas(self, area);
        if !self.local_pending {
            for connector in connectors.iter().take(count - 1) {
                if x < connector.area.x
                    || x >= connector.area.right()
                    || y < connector.area.y
                    || y >= connector.area.bottom()
                {
                    continue;
                }
                let bands = self.action_bands(connector.left, connector.right);
                for band in bands {
                    for source in [connector.left, connector.right] {
                        if let Some(action) = self.band_action(connector, band, source) {
                            if x == action.x && y == action.y {
                                return Some(action.hit);
                            }
                        }
                    }
                }
            }
        }
        let pane_area = panes.into_iter().take(count).find(|pane| {
            y >= pane.content.y
                && y < pane.content.bottom()
                && x >= pane.outer.x
                && x < pane.overview_x
        })?;
        let offset = usize::from(y - pane_area.content.y);
        let row = if self.local_pending {
            let line = self.scroll + offset;
            (line < self.pane(pane_area.pane).buffer.line_count()).then_some(line)?
        } else {
            self.pane_row(pane_area.pane, offset)?
        };
        let gutter = x < pane_area.content.x;
        let column = if self.local_pending || gutter {
            0
        } else {
            let line = self.rows.get(row)?.line(pane_area.pane)?;
            let horizontal = *self.horizontal.get(&pane_area.pane).unwrap_or(&0);
            grapheme_at_cell(
                line,
                horizontal.saturating_add(usize::from(x - pane_area.content.x)),
            )
        };
        Some(MouseHit {
            pane: pane_area.pane,
            row,
            column,
            gutter,
            action: None,
        })
    }
}

fn render_line_number(
    buffer: &mut Buffer,
    x: u16,
    y: u16,
    gutter: u16,
    number: usize,
    background: Color,
) {
    let width = usize::from(gutter.saturating_sub(1));
    if width == 0 {
        return;
    }
    let mut digits = [b' '; 20];
    let mut value = number;
    let mut cursor = digits.len();
    while value > 0 && cursor > 0 {
        cursor -= 1;
        digits[cursor] = b'0' + (value % 10) as u8;
        value /= 10;
    }
    let visible = std::str::from_utf8(&digits[digits.len() - width..]).expect("decimal digits");
    buffer.set_stringn(
        x,
        y,
        visible,
        width,
        Style::default().fg(MUTED).bg(background),
    );
}

fn pane_name(pane: Pane) -> &'static str {
    match pane {
        Pane::Left => "Left",
        Pane::Right => "Right",
        Pane::Ours => "Ours",
        Pane::Result => "Merged result",
        Pane::Theirs => "Theirs",
    }
}

fn draw_connector_band(
    buffer: &mut Buffer,
    area: Rect,
    band: &ChangeBand,
    left_top: usize,
    right_top: usize,
) {
    let left_start = band.left.start as i64 - left_top as i64;
    let right_start = band.right.start as i64 - right_top as i64;
    let left_end = band.left.end as i64 - left_top as i64;
    let right_end = band.right.end as i64 - right_top as i64;
    let first = left_start
        .min(right_start)
        .max(0)
        .min(i64::from(area.height));
    let last = left_end.max(right_end).max(0).min(i64::from(area.height));
    let steps = i64::from(area.width.saturating_sub(2)).max(1);
    let color = band_color(band.kind);
    for x in 0..area.width {
        let start = i64::from(x.saturating_sub(1)).min(steps);
        let end = i64::from(x).min(steps);
        let top_left = 2 * (left_start * (steps - start) + right_start * start);
        let top_right = 2 * (left_start * (steps - end) + right_start * end);
        let bottom_left = 2 * (left_end * (steps - start) + right_end * start);
        let bottom_right = 2 * (left_end * (steps - end) + right_end * end);
        let top = top_left.min(top_right).div_euclid(steps);
        let bottom = (bottom_left.max(bottom_right) + steps - 1).div_euclid(steps);
        for y in first..last {
            let top_half = y * 2 >= top && y * 2 < bottom;
            let bottom_half = y * 2 + 1 >= top && y * 2 + 1 < bottom;
            if !top_half && !bottom_half {
                continue;
            }
            let cell = &mut buffer[(area.x + x, area.y + y as u16)];
            let (mut upper, mut lower) = match cell.symbol() {
                "▀" => (cell.fg, cell.bg),
                "▄" => (cell.bg, cell.fg),
                _ => (cell.bg, cell.bg),
            };
            if top_half {
                upper = color;
            }
            if bottom_half {
                lower = color;
            }
            if upper == lower {
                cell.set_symbol(" ").set_style(Style::default().bg(upper));
            } else {
                cell.set_symbol("▀")
                    .set_style(Style::default().fg(upper).bg(lower));
            }
        }
    }
}

fn band_at(bands: &[ChangeBand], index: usize, left: bool) -> Option<&ChangeBand> {
    let upto =
        bands.partition_point(|band| (if left { &band.left } else { &band.right }).start <= index);
    let band = bands.get(upto.checked_sub(1)?)?;
    (if left { &band.left } else { &band.right })
        .contains(&index)
        .then_some(band)
}

fn band_in_range(
    bands: &[ChangeBand],
    first: usize,
    end: usize,
    left: bool,
) -> Option<&ChangeBand> {
    let upto =
        bands.partition_point(|band| (if left { &band.left } else { &band.right }).start < end);
    let band = bands.get(upto.checked_sub(1)?)?;
    let range = if left { &band.left } else { &band.right };
    (range.end > first || range.is_empty() && range.start >= first).then_some(band)
}

fn kind_priority(kind: ChangeKind) -> u8 {
    match kind {
        ChangeKind::Conflict => 5,
        ChangeKind::Resolved => 4,
        ChangeKind::Modified => 3,
        ChangeKind::Added | ChangeKind::Removed => 2,
    }
}

fn band_color(kind: ChangeKind) -> Color {
    match kind {
        ChangeKind::Modified => Color::Rgb(81, 122, 163),
        ChangeKind::Added => Color::Rgb(79, 151, 108),
        ChangeKind::Removed => Color::Rgb(154, 95, 78),
        ChangeKind::Conflict => Color::Rgb(183, 124, 65),
        ChangeKind::Resolved => Color::Rgb(95, 164, 140),
    }
}

fn brighten(color: Color, amount: u8) -> Color {
    match color {
        Color::Rgb(red, green, blue) => Color::Rgb(
            red.saturating_add(amount),
            green.saturating_add(amount),
            blue.saturating_add(amount),
        ),
        _ => color,
    }
}

fn region_color(kind: ChangeKind, pane: Pane, selected: bool) -> Color {
    let color = match (kind, pane) {
        (ChangeKind::Modified, Pane::Left | Pane::Ours) => Color::Rgb(68, 64, 65),
        (ChangeKind::Modified, _) => Color::Rgb(47, 65, 82),
        (ChangeKind::Added, _) => Color::Rgb(37, 66, 52),
        (ChangeKind::Removed, _) => Color::Rgb(71, 49, 48),
        (ChangeKind::Conflict, Pane::Result) => Color::Rgb(93, 67, 48),
        (ChangeKind::Conflict, _) => Color::Rgb(77, 58, 49),
        (ChangeKind::Resolved, _) => Color::Rgb(40, 68, 63),
    };
    if selected { brighten(color, 9) } else { color }
}

fn intraline_color(region: Option<ChangeKind>, background: Color) -> Color {
    brighten(
        background,
        if matches!(region, Some(ChangeKind::Conflict)) {
            34
        } else {
            26
        },
    )
}

fn render_line(
    buffer: &mut Buffer,
    area: Rect,
    y: u16,
    line: &ViewLine,
    syntax: &[chvrn_core::structural::HighlightSpan],
    offset: usize,
    background: Color,
    region: Option<ChangeKind>,
) {
    let stop = line.stop_at_cell(offset);
    let mut cell_position = stop.cells;
    let mut scalar_position = stop.scalar;
    for (byte, grapheme) in line.text[stop.byte..].grapheme_indices(true) {
        let width = text::display_cell_width(grapheme, cell_position);
        let current = scalar_position;
        scalar_position += grapheme.chars().count();
        let start = cell_position;
        cell_position += width;
        if cell_position <= offset {
            continue;
        }
        if start < offset {
            continue;
        }
        let visible_x = start - offset;
        if visible_x + width > usize::from(area.width) {
            break;
        }
        let absolute_byte = line.byte_start + stop.byte + byte;
        let prefix = syntax.partition_point(|span| span.bytes.start <= absolute_byte);
        let syntax_color = syntax[..prefix]
            .iter()
            .rev()
            .find(|span| absolute_byte < span.bytes.end)
            .map(|span| syntax_color(&span.kind))
            .unwrap_or(INK);
        let changed_prefix = line.changed.partition_point(|span| span.start <= current);
        let changed = changed_prefix > 0 && current < line.changed[changed_prefix - 1].end;
        let style = if changed {
            Style::default()
                .fg(syntax_color)
                .bg(intraline_color(region, background))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(syntax_color).bg(background)
        };
        let x = area.x.saturating_add(visible_x as u16);
        if grapheme == "\t" {
            buffer.set_string(x, y, "→", style);
            if width > 1 {
                buffer.set_stringn(x + 1, y, "   ", width - 1, style);
            }
        } else if grapheme.chars().any(char::is_control) || UnicodeWidthStr::width(grapheme) == 0 {
            buffer.set_string(x, y, "·", style);
        } else {
            buffer.set_string(x, y, grapheme, style);
        }
    }
}

fn syntax_color(kind: &HighlightKind) -> Color {
    match kind {
        HighlightKind::Keyword => Color::Rgb(207, 146, 111),
        HighlightKind::Identifier => Color::Rgb(194, 189, 223),
        HighlightKind::String => Color::Rgb(132, 183, 127),
        HighlightKind::Number => Color::Rgb(177, 177, 124),
        HighlightKind::Comment => Color::Rgb(115, 124, 120),
        HighlightKind::Type => Color::Rgb(139, 183, 196),
        HighlightKind::Function => Color::Rgb(195, 161, 210),
        HighlightKind::Punctuation => INK,
    }
}

fn grapheme_at_cell(line: &ViewLine, target: usize) -> usize {
    let stop = line.stop_at_cell(target);
    let mut cells = stop.cells;
    let mut column = stop.grapheme;
    for grapheme in line.text[stop.byte..].graphemes(true) {
        let width = text::display_cell_width(grapheme, cells);
        if cells + width > target {
            break;
        }
        cells += width;
        column += 1;
    }
    column
}

fn cursor_cell(area: Rect, line: Option<&ViewLine>, column: usize, offset: usize) -> Option<u16> {
    let line = line?;
    let cells = line.cells_before(column);
    let visible = cells.checked_sub(offset)?;
    if visible < usize::from(area.width) {
        Some(area.x + visible as u16)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steep_connectors_cover_every_row_between_their_endpoints() {
        let area = Rect::new(0, 0, 5, 24);
        let color = band_color(ChangeKind::Modified);
        for (left, right) in [(18..20, 2..4), (2..4, 18..20)] {
            let mut buffer = Buffer::empty(area);
            buffer.set_style(area, Style::default().bg(RAIL));
            let band = ChangeBand {
                left,
                right,
                hunk: None,
                resolved: None,
                kind: ChangeKind::Modified,
            };
            draw_connector_band(&mut buffer, area, &band, 0, 0);

            for y in 2..20 {
                assert!(
                    (0..5).any(|x| {
                        let cell = &buffer[(x, y)];
                        cell.bg == color || matches!(cell.symbol(), "▀" | "▄") && cell.fg == color
                    }),
                    "connector is disconnected at row {y}: {band:?}",
                );
            }
            assert!((0..5).all(|x| buffer[(x, 0)].bg == RAIL));
            assert!((0..5).all(|x| buffer[(x, 23)].bg == RAIL));
        }
    }

    #[test]
    fn adjacent_connectors_do_not_erase_previously_painted_half_cells() {
        let area = Rect::new(0, 0, 5, 4);
        let color = band_color(ChangeKind::Modified);
        let mut buffer = Buffer::empty(area);
        buffer.set_style(area, Style::default().bg(RAIL));
        for (left, right) in [(0..1, 0..2), (1..2, 2..3)] {
            let band = ChangeBand {
                left,
                right,
                hunk: None,
                resolved: None,
                kind: ChangeKind::Modified,
            };
            draw_connector_band(&mut buffer, area, &band, 0, 0);
        }

        for x in 0..5 {
            let cell = &buffer[(x, 1)];
            assert_eq!(cell.bg, color, "unpainted half at column {x}");
            if matches!(cell.symbol(), "▀" | "▄") {
                assert_eq!(cell.fg, color, "unpainted half at column {x}");
            }
        }
    }
}
