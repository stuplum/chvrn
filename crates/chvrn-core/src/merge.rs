use std::ops::Range;

use similar::{Algorithm, DiffTag, capture_diff_slices};
use unicode_segmentation::UnicodeSegmentation;

use crate::TextSnapshot;
use crate::diff::{exact_hunks, line_offset, lines};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConflictId(pub usize);

pub struct Conflict {
    pub id: ConflictId,
    pub base_lines: Range<usize>,
    pub ours_lines: Range<usize>,
    pub theirs_lines: Range<usize>,
}

pub struct PreviewConflict {
    pub id: ConflictId,
    pub result_chars: Range<usize>,
    pub result_lines: Range<usize>,
}

pub enum ConflictResolution {
    Ours,
    Theirs,
    Both,
    Manual(String),
}

#[derive(Debug, PartialEq, Eq)]
pub enum ResolveError {
    UnknownConflict,
    BinaryInput,
}

#[derive(Clone, Copy)]
enum Side {
    Ours,
    Theirs,
}

struct Edit {
    base: Range<usize>,
    side_lines: Range<usize>,
    text: String,
    side: Side,
}

enum PieceContent {
    Fixed(String),
    Conflict {
        id: ConflictId,
        ours: String,
        theirs: String,
        resolution: Option<ConflictResolution>,
    },
}

struct Piece {
    base: Range<usize>,
    content: PieceContent,
}

pub struct Merge {
    base: TextSnapshot,
    pieces: Vec<Piece>,
    conflicts: Vec<Conflict>,
    preview: TextSnapshot,
    unresolved: Vec<PreviewConflict>,
}

fn side_edits(base: &TextSnapshot, side: &TextSnapshot, side_name: Side) -> Vec<Edit> {
    let changes = exact_hunks(base, side);
    let side_lines = lines(side.text());
    changes
        .into_iter()
        .map(|(base_lines, side_range)| {
            let start = line_offset(&side_lines, side_range.start, side.as_bytes().len());
            let end = line_offset(&side_lines, side_range.end, side.as_bytes().len());
            Edit {
                base: base_lines,
                side_lines: side_range,
                text: side.text()[start..end].to_owned(),
                side: side_name,
            }
        })
        .collect()
}

fn overlaps(left: &Range<usize>, right: &Range<usize>) -> bool {
    if left.is_empty() {
        if right.is_empty() {
            left.start == right.start
        } else {
            right.start < left.start && left.start < right.end
        }
    } else if right.is_empty() {
        left.start < right.start && right.start < left.end
    } else {
        left.start < right.end && right.start < left.end
    }
}

struct InlineEdit {
    base: Range<usize>,
    replacement: String,
}

fn inline_edits(base: &[&str], side: &[&str]) -> Vec<InlineEdit> {
    let mut changes: Vec<InlineEdit> = Vec::new();
    for operation in capture_diff_slices(Algorithm::Myers, base, side) {
        if operation.tag() == DiffTag::Equal {
            continue;
        }
        let old = operation.old_range();
        let replacement = side[operation.new_range()].concat();
        if let Some(last) = changes.last_mut() {
            if last.base.end == old.start {
                last.base.end = old.end;
                last.replacement.push_str(&replacement);
                continue;
            }
        }
        changes.push(InlineEdit {
            base: old,
            replacement,
        });
    }
    changes
}

fn combine_nonoverlapping_text(base: &str, ours: &str, theirs: &str) -> Option<String> {
    let base_parts: Vec<_> = base.graphemes(true).collect();
    let ours_parts: Vec<_> = ours.graphemes(true).collect();
    let theirs_parts: Vec<_> = theirs.graphemes(true).collect();
    let mut changes = inline_edits(&base_parts, &ours_parts);
    for incoming in inline_edits(&base_parts, &theirs_parts) {
        let mut identical = false;
        for existing in &changes {
            if overlaps(&existing.base, &incoming.base) {
                if existing.base == incoming.base && existing.replacement == incoming.replacement {
                    identical = true;
                } else {
                    return None;
                }
            }
        }
        if !identical {
            changes.push(incoming);
        }
    }
    changes.sort_by_key(|edit| (edit.base.start, !edit.base.is_empty()));
    let mut output = String::with_capacity(ours.len().max(theirs.len()));
    let mut position = 0;
    for edit in changes {
        for part in &base_parts[position..edit.base.start] {
            output.push_str(part);
        }
        output.push_str(&edit.replacement);
        position = edit.base.end;
    }
    for part in &base_parts[position..] {
        output.push_str(part);
    }
    Some(output)
}

fn proposal(
    base: &TextSnapshot,
    base_lines: &[crate::diff::Line],
    scope: &Range<usize>,
    edits: &[&Edit],
) -> String {
    let begin = line_offset(base_lines, scope.start, base.as_bytes().len());
    let end = line_offset(base_lines, scope.end, base.as_bytes().len());
    let capacity = end - begin + edits.iter().map(|edit| edit.text.len()).sum::<usize>();
    let mut output = String::with_capacity(capacity);
    let mut position = scope.start;
    for edit in edits {
        let next = line_offset(base_lines, edit.base.start, base.as_bytes().len());
        let previous = line_offset(base_lines, position, base.as_bytes().len());
        output.push_str(&base.text()[previous..next]);
        output.push_str(&edit.text);
        position = edit.base.end;
    }
    let previous = line_offset(base_lines, position, base.as_bytes().len());
    output.push_str(&base.text()[previous..end]);
    output
}

fn projected_lines(scope: &Range<usize>, edits: &[&Edit]) -> Range<usize> {
    let first = edits.first().unwrap();
    let last = edits.last().unwrap();
    let start = first.side_lines.start - (first.base.start - scope.start);
    let end = last.side_lines.end + (scope.end - last.base.end);
    start..end
}

fn append(output: &mut String, char_length: &mut usize, text: &str) {
    output.push_str(text);
    *char_length += text.chars().count();
}

impl Merge {
    pub fn three_way(base: &TextSnapshot, ours: &TextSnapshot, theirs: &TextSnapshot) -> Self {
        let mut edits = side_edits(base, ours, Side::Ours);
        edits.extend(side_edits(base, theirs, Side::Theirs));
        edits.sort_by_key(|edit| (edit.base.start, !edit.base.is_empty()));
        let base_lines = lines(base.text());
        let mut pieces = Vec::new();
        let mut conflicts = Vec::new();
        let mut group: Vec<&Edit> = Vec::new();
        for edit in &edits {
            if !group.is_empty()
                && !group
                    .iter()
                    .any(|member| overlaps(&member.base, &edit.base))
            {
                Self::push_group(base, &base_lines, &group, &mut pieces, &mut conflicts);
                group.clear();
            }
            group.push(edit);
        }
        if !group.is_empty() {
            Self::push_group(base, &base_lines, &group, &mut pieces, &mut conflicts);
        }
        let mut merge = Self {
            base: base.clone(),
            pieces,
            conflicts,
            preview: base.clone(),
            unresolved: Vec::new(),
        };
        merge.rebuild_preview();
        merge
    }

    fn push_group(
        base: &TextSnapshot,
        base_lines: &[crate::diff::Line],
        group: &[&Edit],
        pieces: &mut Vec<Piece>,
        conflicts: &mut Vec<Conflict>,
    ) {
        let scope = group.iter().map(|edit| edit.base.start).min().unwrap()
            ..group.iter().map(|edit| edit.base.end).max().unwrap();
        let ours: Vec<_> = group
            .iter()
            .copied()
            .filter(|edit| matches!(edit.side, Side::Ours))
            .collect();
        let theirs: Vec<_> = group
            .iter()
            .copied()
            .filter(|edit| matches!(edit.side, Side::Theirs))
            .collect();
        let ours_text = proposal(base, base_lines, &scope, &ours);
        let theirs_text = proposal(base, base_lines, &scope, &theirs);
        let content = if ours.is_empty() {
            PieceContent::Fixed(theirs_text)
        } else if theirs.is_empty() || ours_text == theirs_text {
            PieceContent::Fixed(ours_text)
        } else {
            let combined = if ours.len() == 1 && theirs.len() == 1 {
                let start = line_offset(base_lines, scope.start, base.as_bytes().len());
                let end = line_offset(base_lines, scope.end, base.as_bytes().len());
                combine_nonoverlapping_text(&base.text()[start..end], &ours_text, &theirs_text)
            } else {
                None
            };
            if let Some(combined) = combined {
                PieceContent::Fixed(combined)
            } else {
                let id = ConflictId(conflicts.len());
                conflicts.push(Conflict {
                    id,
                    base_lines: scope.clone(),
                    ours_lines: projected_lines(&scope, &ours),
                    theirs_lines: projected_lines(&scope, &theirs),
                });
                PieceContent::Conflict {
                    id,
                    ours: ours_text,
                    theirs: theirs_text,
                    resolution: None,
                }
            }
        };
        pieces.push(Piece {
            base: scope,
            content,
        });
    }

    fn rebuild_preview(&mut self) {
        let base_lines = lines(self.base.text());
        let mut output = String::with_capacity(self.base.as_bytes().len());
        let mut unresolved_ranges = Vec::new();
        let mut position = 0;
        let mut chars = 0;
        for piece in &self.pieces {
            let start = line_offset(&base_lines, position, self.base.as_bytes().len());
            let end = line_offset(&base_lines, piece.base.start, self.base.as_bytes().len());
            append(&mut output, &mut chars, &self.base.text()[start..end]);
            match &piece.content {
                PieceContent::Fixed(text) => append(&mut output, &mut chars, text),
                PieceContent::Conflict {
                    id,
                    ours,
                    theirs,
                    resolution,
                } => {
                    let byte_start = output.len();
                    let char_start = chars;
                    match resolution {
                        None | Some(ConflictResolution::Ours) => {
                            append(&mut output, &mut chars, ours)
                        }
                        Some(ConflictResolution::Theirs) => append(&mut output, &mut chars, theirs),
                        Some(ConflictResolution::Both) => {
                            append(&mut output, &mut chars, ours);
                            append(&mut output, &mut chars, theirs);
                        }
                        Some(ConflictResolution::Manual(text)) => {
                            append(&mut output, &mut chars, text)
                        }
                    }
                    if resolution.is_none() {
                        unresolved_ranges.push((*id, byte_start..output.len(), char_start..chars));
                    }
                }
            }
            position = piece.base.end;
        }
        let start = line_offset(&base_lines, position, self.base.as_bytes().len());
        append(&mut output, &mut chars, &self.base.text()[start..]);
        let preview_lines = lines(&output);
        self.unresolved = unresolved_ranges
            .into_iter()
            .map(|(id, bytes, result_chars)| {
                let first = if bytes.is_empty()
                    && bytes.start == output.len()
                    && (output.ends_with('\n') || output.ends_with('\r'))
                {
                    preview_lines.len()
                } else {
                    preview_lines
                        .partition_point(|line| line.full.start <= bytes.start)
                        .saturating_sub(1)
                };
                let last = if bytes.is_empty() {
                    first
                } else {
                    preview_lines.partition_point(|line| line.full.start < bytes.end)
                };
                PreviewConflict {
                    id,
                    result_chars,
                    result_lines: first..last,
                }
            })
            .collect();
        self.preview = TextSnapshot::from_owned(output);
    }

    pub fn conflicts(&self) -> &[Conflict] {
        &self.conflicts
    }

    pub fn preview(&self) -> TextSnapshot {
        self.preview.clone()
    }

    pub fn preview_conflicts(&self) -> Vec<PreviewConflict> {
        self.unresolved
            .iter()
            .map(|conflict| PreviewConflict {
                id: conflict.id,
                result_chars: conflict.result_chars.clone(),
                result_lines: conflict.result_lines.clone(),
            })
            .collect()
    }

    pub fn result(&self) -> Option<TextSnapshot> {
        self.unresolved.is_empty().then(|| self.preview.clone())
    }

    pub fn resolve(
        &mut self,
        id: ConflictId,
        choice: ConflictResolution,
    ) -> Result<(), ResolveError> {
        if matches!(&choice, ConflictResolution::Manual(text) if text.as_bytes().contains(&0)) {
            return Err(ResolveError::BinaryInput);
        }
        let Some(piece) = self.pieces.iter_mut().find(|piece| {
            matches!(&piece.content, PieceContent::Conflict { id: current, .. } if *current == id)
        }) else {
            return Err(ResolveError::UnknownConflict);
        };
        if let PieceContent::Conflict { resolution, .. } = &mut piece.content {
            *resolution = Some(choice);
        }
        self.rebuild_preview();
        Ok(())
    }
}
