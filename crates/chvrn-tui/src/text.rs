use chvrn_core::edit::TextBuffer;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy)]
pub(crate) struct LineSpan {
    pub(crate) start_char: usize,
    pub(crate) end_char: usize,
    pub(crate) start_byte: usize,
    pub(crate) content_end_byte: usize,
}

struct LineSpans<'a> {
    text: &'a str,
    chars: std::iter::Peekable<std::str::CharIndices<'a>>,
    start_byte: usize,
    start_char: usize,
    consumed: usize,
    finished: bool,
}

impl<'a> LineSpans<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            text,
            chars: text.char_indices().peekable(),
            start_byte: 0,
            start_char: 0,
            consumed: 0,
            finished: false,
        }
    }
}

impl Iterator for LineSpans<'_> {
    type Item = LineSpan;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        while let Some((byte, ch)) = self.chars.next() {
            self.consumed += 1;
            if ch == '\r' || ch == '\n' {
                let mut end_byte = byte + ch.len_utf8();
                if ch == '\r' && self.chars.peek().is_some_and(|(_, next)| *next == '\n') {
                    self.chars.next();
                    self.consumed += 1;
                    end_byte += 1;
                }
                let line = LineSpan {
                    start_char: self.start_char,
                    end_char: self.consumed,
                    start_byte: self.start_byte,
                    content_end_byte: byte,
                };
                self.start_byte = end_byte;
                self.start_char = self.consumed;
                return Some(line);
            }
        }
        self.finished = true;
        Some(LineSpan {
            start_char: self.start_char,
            end_char: self.consumed,
            start_byte: self.start_byte,
            content_end_byte: self.text.len(),
        })
    }
}

pub(crate) fn lines(text: &str) -> Vec<LineSpan> {
    LineSpans::new(text).collect()
}

pub(crate) fn line_range_chars(text: &str, lines_range: Range<usize>) -> Range<usize> {
    let mut start = None;
    let mut end = None;
    let mut eof = 0;
    for (index, span) in LineSpans::new(text).enumerate() {
        eof = span.end_char;
        if index == lines_range.start {
            start = Some(span.start_char);
        }
        if index == lines_range.end {
            end = Some(span.start_char);
            break;
        }
    }
    start.unwrap_or(eof)..end.unwrap_or(eof)
}

pub(crate) fn slice_chars(text: &str, chars: Range<usize>) -> &str {
    let start = text
        .char_indices()
        .nth(chars.start)
        .map_or(text.len(), |(byte, _)| byte);
    let end = text
        .char_indices()
        .nth(chars.end)
        .map_or(text.len(), |(byte, _)| byte);
    &text[start..end]
}

pub(crate) fn grapheme_offset(line: &str, column: usize) -> usize {
    line.graphemes(true)
        .take(column)
        .map(|grapheme| grapheme.chars().count())
        .sum()
}

pub(crate) fn display_cell_width(grapheme: &str, before: usize) -> usize {
    if grapheme == "\t" {
        4 - before % 4
    } else {
        UnicodeWidthStr::width(grapheme).max(1)
    }
}

pub(crate) fn grapheme_column(line: &str, chars: usize) -> usize {
    let mut consumed = 0;
    let mut column = 0;
    for grapheme in line.graphemes(true) {
        let width = grapheme.chars().count();
        if consumed + width > chars {
            break;
        }
        consumed += width;
        column += 1;
    }
    column
}

pub(crate) fn cursor_position(text: &str, offset: usize) -> (usize, usize) {
    let mut last = (
        0,
        LineSpan {
            start_char: 0,
            end_char: 0,
            start_byte: 0,
            content_end_byte: 0,
        },
    );
    for (index, span) in LineSpans::new(text).enumerate() {
        last = (index, span);
        if offset < span.end_char {
            break;
        }
    }
    let (index, span) = last;
    let content = &text[span.start_byte..span.content_end_byte];
    (
        index,
        grapheme_column(content, offset.saturating_sub(span.start_char)),
    )
}

pub(crate) fn buffer_line_content(buffer: &TextBuffer, line: usize) -> String {
    let mut text = buffer.line_text(line).unwrap_or_default();
    while text.ends_with('\n') || text.ends_with('\r') {
        text.pop();
    }
    text
}

pub(crate) fn buffer_position_offset(buffer: &TextBuffer, line: usize, column: usize) -> usize {
    let Some(start) = buffer.line_to_char(line) else {
        return buffer.len_chars();
    };
    start + grapheme_offset(&buffer_line_content(buffer, line), column)
}

pub(crate) fn buffer_cursor_position(buffer: &TextBuffer, offset: usize) -> (usize, usize) {
    let line = buffer
        .char_to_line(offset)
        .expect("editor cursor must remain valid");
    let start = buffer.line_to_char(line).expect("editor line must exist");
    let prefix = buffer
        .slice_chars(start..offset)
        .expect("editor cursor must remain valid");
    (line, prefix.graphemes(true).count())
}

pub(crate) fn buffer_previous_grapheme(buffer: &TextBuffer, offset: usize) -> usize {
    if offset == 0 {
        return 0;
    }
    let line = buffer
        .char_to_line(offset)
        .expect("editor cursor must remain valid");
    let start = buffer.line_to_char(line).expect("editor line must exist");
    let earliest = if offset == start {
        offset.saturating_sub(2)
    } else {
        start
    };
    let mut window = earliest.max(offset.saturating_sub(1024));
    loop {
        let segment = buffer
            .slice_chars(window..offset)
            .expect("editor cursor must remain valid");
        let mut graphemes = segment.graphemes(true);
        let last = graphemes.next_back().expect("nonempty editor prefix");
        let previous = offset - last.chars().count();
        if previous > window || window == earliest {
            return previous;
        }
        window = earliest.max(window.saturating_sub(1024));
    }
}

pub(crate) fn buffer_next_grapheme(buffer: &TextBuffer, offset: usize) -> usize {
    if offset == buffer.len_chars() {
        return offset;
    }
    let line = buffer
        .char_to_line(offset)
        .expect("editor cursor must remain valid");
    let end = buffer.line_to_char(line + 1).unwrap_or(buffer.len_chars());
    let mut window = end.min(offset.saturating_add(1024));
    loop {
        let segment = match buffer.slice_chars(offset..window) {
            Ok(segment) => segment,
            Err(_) if window < end => {
                window += 1;
                continue;
            }
            Err(_) => return offset,
        };
        let first = segment
            .graphemes(true)
            .next()
            .expect("nonempty editor suffix");
        let next = offset + first.chars().count();
        if next < window || window == end {
            return next;
        }
        window = end.min(window.saturating_add(1024));
    }
}
