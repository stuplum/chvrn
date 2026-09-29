use std::borrow::Cow;
use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use similar::{Algorithm, DiffTag, capture_diff_slices};
use unicode_segmentation::UnicodeSegmentation;

use crate::TextSnapshot;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WhitespacePolicy {
    Exact,
    IgnoreEdge,
    IgnoreAll,
    IgnoreBlankLines,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyDirection {
    LeftToRight,
    RightToLeft,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ApplyError {
    StaleSnapshot,
    ForeignHunk,
}

pub struct RenderedLine {
    pub number: usize,
    pub text: String,
}

pub struct AlignedRow {
    pub left: Option<RenderedLine>,
    pub right: Option<RenderedLine>,
}

pub struct Hunk {
    pub left_lines: Range<usize>,
    pub right_lines: Range<usize>,
    owner: Arc<()>,
}

pub struct Diff {
    left: TextSnapshot,
    right: TextSnapshot,
    left_lines: Vec<Line>,
    right_lines: Vec<Line>,
    rows: Vec<AlignedRow>,
    hunks: Vec<Hunk>,
    identity: Arc<()>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct IntralineChange {
    pub left: Range<usize>,
    pub right: Range<usize>,
}

#[derive(Clone, Copy)]
pub(crate) struct Line {
    pub(crate) content: RangeMarker,
    pub(crate) full: RangeMarker,
}

#[derive(Clone, Copy)]
pub(crate) struct RangeMarker {
    pub(crate) start: usize,
    pub(crate) end: usize,
}

impl RangeMarker {
    fn as_range(self) -> Range<usize> {
        self.start..self.end
    }
}

pub(crate) fn lines(text: &str) -> Vec<Line> {
    let bytes = text.as_bytes();
    let mut result = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\r' || bytes[i] == b'\n' {
            let end = i;
            i += 1;
            if bytes[end] == b'\r' && i < bytes.len() && bytes[i] == b'\n' {
                i += 1;
            }
            result.push(Line {
                content: RangeMarker { start, end },
                full: RangeMarker { start, end: i },
            });
            start = i;
        } else {
            i += 1;
        }
    }
    if start < bytes.len() {
        result.push(Line {
            content: RangeMarker {
                start,
                end: bytes.len(),
            },
            full: RangeMarker {
                start,
                end: bytes.len(),
            },
        });
    }
    result
}

pub(crate) fn line_offset(lines: &[Line], line: usize, text_length: usize) -> usize {
    lines.get(line).map_or(text_length, |item| item.full.start)
}

#[derive(Hash, PartialEq, Eq, PartialOrd, Ord)]
struct LineKey<'a> {
    content: Cow<'a, str>,
    ending: &'a str,
}

fn horizontal(c: char) -> bool {
    c == ' ' || c == '\t'
}

fn key<'a>(text: &'a str, line: Line, policy: WhitespacePolicy) -> Option<LineKey<'a>> {
    let content = &text[line.content.as_range()];
    let ending = &text[line.content.end..line.full.end];
    if policy == WhitespacePolicy::IgnoreBlankLines && content.chars().all(horizontal) {
        return None;
    }
    let content = match policy {
        WhitespacePolicy::Exact | WhitespacePolicy::IgnoreBlankLines => Cow::Borrowed(content),
        WhitespacePolicy::IgnoreEdge => Cow::Borrowed(content.trim_matches(horizontal)),
        WhitespacePolicy::IgnoreAll => {
            if content.chars().any(horizontal) {
                Cow::Owned(content.chars().filter(|c| !horizontal(*c)).collect())
            } else {
                Cow::Borrowed(content)
            }
        }
    };
    Some(LineKey { content, ending })
}

fn patience_matches<'a>(left: &[&LineKey<'a>], right: &[&LineKey<'a>]) -> Vec<(usize, usize)> {
    let mut matches = Vec::new();
    let mut pending = vec![(0..left.len(), 0..right.len())];
    while let Some((old_range, new_range)) = pending.pop() {
        let old_slice = &left[old_range.clone()];
        let new_slice = &right[new_range.clone()];
        if old_slice.is_empty() || new_slice.is_empty() {
            continue;
        }
        let mut counts_left: HashMap<&LineKey<'_>, (usize, usize)> = HashMap::new();
        let mut counts_right: HashMap<&LineKey<'_>, (usize, usize)> = HashMap::new();
        for (index, item) in old_slice.iter().enumerate() {
            let entry = counts_left.entry(item).or_insert((0, index));
            entry.0 += 1;
        }
        for (index, item) in new_slice.iter().enumerate() {
            let entry = counts_right.entry(item).or_insert((0, index));
            entry.0 += 1;
        }
        let candidates: Vec<(usize, usize)> = old_slice
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                let (left_count, _) = counts_left.get(item)?;
                let (right_count, other) = counts_right.get(item)?;
                (*left_count == 1 && *right_count == 1).then_some((index, *other))
            })
            .collect();
        if candidates.is_empty() {
            for operation in capture_diff_slices(Algorithm::Myers, old_slice, new_slice) {
                if operation.tag() == DiffTag::Equal {
                    for (old, new) in operation.old_range().zip(operation.new_range()) {
                        matches.push((old_range.start + old, new_range.start + new));
                    }
                }
            }
            continue;
        }
        let mut tails: Vec<usize> = Vec::new();
        let mut previous = vec![None; candidates.len()];
        for (index, &(_, right_index)) in candidates.iter().enumerate() {
            let position =
                tails.partition_point(|&candidate| candidates[candidate].1 < right_index);
            if position > 0 {
                previous[index] = Some(tails[position - 1]);
            }
            if position == tails.len() {
                tails.push(index);
            } else {
                tails[position] = index;
            }
        }
        let mut anchors = Vec::with_capacity(tails.len());
        let mut index = tails.last().copied();
        while let Some(current) = index {
            anchors.push(candidates[current]);
            index = previous[current];
        }
        anchors.reverse();
        let (mut old_end, mut new_end) = (0, 0);
        for &(old, new) in &anchors {
            pending.push((
                old_range.start + old_end..old_range.start + old,
                new_range.start + new_end..new_range.start + new,
            ));
            matches.push((old_range.start + old, new_range.start + new));
            old_end = old + 1;
            new_end = new + 1;
        }
        pending.push((
            old_range.start + old_end..old_range.end,
            new_range.start + new_end..new_range.end,
        ));
    }
    matches.sort_unstable_by_key(|&(old, _)| old);
    matches
}

fn matching_lines(
    left: &TextSnapshot,
    right: &TextSnapshot,
    left_lines: &[Line],
    right_lines: &[Line],
    policy: WhitespacePolicy,
) -> Vec<(usize, usize)> {
    let left_keys: Vec<_> = left_lines
        .iter()
        .map(|&line| key(left.text(), line, policy))
        .collect();
    let right_keys: Vec<_> = right_lines
        .iter()
        .map(|&line| key(right.text(), line, policy))
        .collect();
    let left_indices: Vec<_> = left_keys
        .iter()
        .enumerate()
        .filter_map(|(i, value)| value.as_ref().map(|key| (i, key)))
        .collect();
    let right_indices: Vec<_> = right_keys
        .iter()
        .enumerate()
        .filter_map(|(i, value)| value.as_ref().map(|key| (i, key)))
        .collect();
    let left_refs: Vec<_> = left_indices.iter().map(|(_, key)| *key).collect();
    let right_refs: Vec<_> = right_indices.iter().map(|(_, key)| *key).collect();
    patience_matches(&left_refs, &right_refs)
        .into_iter()
        .map(|(a, b)| (left_indices[a].0, right_indices[b].0))
        .collect()
}

pub(crate) fn exact_hunks(
    left: &TextSnapshot,
    right: &TextSnapshot,
) -> Vec<(Range<usize>, Range<usize>)> {
    let left_lines = lines(left.text());
    let right_lines = lines(right.text());
    let matches = matching_lines(
        left,
        right,
        &left_lines,
        &right_lines,
        WhitespacePolicy::Exact,
    );
    let mut ranges = Vec::new();
    let mut left_start = 0;
    let mut right_start = 0;
    for (left_end, right_end) in matches
        .into_iter()
        .chain(std::iter::once((left_lines.len(), right_lines.len())))
    {
        if left_end != left_start || right_end != right_start {
            ranges.push((left_start..left_end, right_start..right_end));
        }
        left_start = left_end + 1;
        right_start = right_end + 1;
    }
    ranges
}

fn rendered(text: &str, line: Line, number: usize) -> RenderedLine {
    RenderedLine {
        number: number + 1,
        text: text[line.content.as_range()].to_owned(),
    }
}

impl Diff {
    pub fn between(left: &TextSnapshot, right: &TextSnapshot, policy: WhitespacePolicy) -> Self {
        let left_lines = lines(left.text());
        let right_lines = lines(right.text());
        let matched = matching_lines(left, right, &left_lines, &right_lines, policy);
        let identity = Arc::new(());
        let mut rows = Vec::with_capacity(left_lines.len().max(right_lines.len()));
        let mut hunks = Vec::new();
        let mut left_start = 0;
        let mut right_start = 0;
        for (left_end, right_end) in matched
            .into_iter()
            .chain(std::iter::once((left_lines.len(), right_lines.len())))
        {
            let different = if policy == WhitespacePolicy::IgnoreBlankLines {
                left_lines[left_start..left_end]
                    .iter()
                    .any(|line| key(left.text(), *line, policy).is_some())
                    || right_lines[right_start..right_end]
                        .iter()
                        .any(|line| key(right.text(), *line, policy).is_some())
            } else {
                left_end != left_start || right_end != right_start
            };
            if different {
                hunks.push(Hunk {
                    left_lines: left_start..left_end,
                    right_lines: right_start..right_end,
                    owner: identity.clone(),
                });
            }
            let length = (left_end - left_start).max(right_end - right_start);
            for step in 0..length {
                rows.push(AlignedRow {
                    left: left_lines
                        .get(left_start + step)
                        .filter(|_| left_start + step < left_end)
                        .map(|&line| rendered(left.text(), line, left_start + step)),
                    right: right_lines
                        .get(right_start + step)
                        .filter(|_| right_start + step < right_end)
                        .map(|&line| rendered(right.text(), line, right_start + step)),
                });
            }
            if left_end < left_lines.len() && right_end < right_lines.len() {
                rows.push(AlignedRow {
                    left: Some(rendered(left.text(), left_lines[left_end], left_end)),
                    right: Some(rendered(right.text(), right_lines[right_end], right_end)),
                });
                left_start = left_end + 1;
                right_start = right_end + 1;
            }
        }
        Self {
            left: left.clone(),
            right: right.clone(),
            left_lines,
            right_lines,
            rows,
            hunks,
            identity,
        }
    }

    pub fn rows(&self) -> &[AlignedRow] {
        &self.rows
    }

    pub fn hunks(&self) -> &[Hunk] {
        &self.hunks
    }

    pub fn apply_hunk(
        &self,
        hunk: &Hunk,
        current: &TextSnapshot,
        direction: ApplyDirection,
    ) -> Result<TextSnapshot, ApplyError> {
        if !Arc::ptr_eq(&self.identity, &hunk.owner) {
            return Err(ApplyError::ForeignHunk);
        }
        let (source, destination, source_lines, destination_lines, source_range, destination_range) =
            match direction {
                ApplyDirection::LeftToRight => (
                    &self.left,
                    &self.right,
                    &self.left_lines,
                    &self.right_lines,
                    &hunk.left_lines,
                    &hunk.right_lines,
                ),
                ApplyDirection::RightToLeft => (
                    &self.right,
                    &self.left,
                    &self.right_lines,
                    &self.left_lines,
                    &hunk.right_lines,
                    &hunk.left_lines,
                ),
            };
        if !current.same_identity(destination) {
            return Err(ApplyError::StaleSnapshot);
        }
        let source_start = line_offset(source_lines, source_range.start, source.as_bytes().len());
        let source_end = line_offset(source_lines, source_range.end, source.as_bytes().len());
        let destination_start = line_offset(
            destination_lines,
            destination_range.start,
            destination.as_bytes().len(),
        );
        let destination_end = line_offset(
            destination_lines,
            destination_range.end,
            destination.as_bytes().len(),
        );
        let mut result = String::with_capacity(
            destination.text().len() - (destination_end - destination_start) + source_end
                - source_start,
        );
        result.push_str(&destination.text()[..destination_start]);
        result.push_str(&source.text()[source_start..source_end]);
        result.push_str(&destination.text()[destination_end..]);
        Ok(TextSnapshot::from_owned(result))
    }
}

pub fn intraline_spans(left: &str, right: &str) -> Vec<IntralineChange> {
    let left_graphemes: Vec<_> = left.graphemes(true).collect();
    let right_graphemes: Vec<_> = right.graphemes(true).collect();
    let left_offsets: Vec<_> = std::iter::once(0)
        .chain(left_graphemes.iter().scan(0, |offset, grapheme| {
            *offset += grapheme.chars().count();
            Some(*offset)
        }))
        .collect();
    let right_offsets: Vec<_> = std::iter::once(0)
        .chain(right_graphemes.iter().scan(0, |offset, grapheme| {
            *offset += grapheme.chars().count();
            Some(*offset)
        }))
        .collect();
    let mut spans: Vec<IntralineChange> = Vec::new();
    for operation in capture_diff_slices(Algorithm::Myers, &left_graphemes, &right_graphemes) {
        if operation.tag() == DiffTag::Equal {
            continue;
        }
        let old = operation.old_range();
        let new = operation.new_range();
        let change = IntralineChange {
            left: left_offsets[old.start]..left_offsets[old.end],
            right: right_offsets[new.start]..right_offsets[new.end],
        };
        if let Some(last) = spans.last_mut() {
            if last.left.end == change.left.start && last.right.end == change.right.start {
                last.left.end = change.left.end;
                last.right.end = change.right.end;
                continue;
            }
        }
        spans.push(change);
    }
    spans
}
