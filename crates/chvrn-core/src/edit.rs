use std::cell::RefCell;
use std::ops::Range;
use std::sync::Arc;

use ropey::Rope;

use crate::TextSnapshot;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorMotion {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, PartialEq, Eq)]
pub enum EditError {
    OutOfBounds,
    InvalidLineEndingBoundary,
    BinaryInput,
}

struct HistoryState {
    rope: Rope,
    cursor: usize,
    desired_column: Option<usize>,
}

#[derive(Clone)]
pub struct CapturedText {
    rope: Rope,
    identity: Arc<()>,
}

impl CapturedText {
    pub fn same_identity(&self, snapshot: &TextSnapshot) -> bool {
        Arc::ptr_eq(&self.identity, &snapshot.identity)
    }

    pub fn snapshot(self) -> TextSnapshot {
        TextSnapshot::from_owned_with_identity(self.rope.to_string(), self.identity)
    }
}

pub struct TextBuffer {
    rope: Rope,
    identity: Arc<()>,
    cursor: usize,
    desired_column: Option<usize>,
    undo: Vec<HistoryState>,
    redo: Vec<HistoryState>,
    cached: RefCell<Option<TextSnapshot>>,
}

impl TextBuffer {
    pub fn new(snapshot: TextSnapshot) -> Self {
        let identity = snapshot.identity.clone();
        let rope = Rope::from_str(snapshot.text());
        Self {
            rope,
            identity,
            cursor: 0,
            desired_column: None,
            undo: Vec::new(),
            redo: Vec::new(),
            cached: RefCell::new(Some(snapshot)),
        }
    }

    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    pub fn capture(&self) -> CapturedText {
        CapturedText {
            rope: self.rope.clone(),
            identity: self.identity.clone(),
        }
    }

    pub fn len_bytes(&self) -> usize {
        self.rope.len_bytes()
    }

    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    pub fn line_count(&self) -> usize {
        self.rope.len_lines()
    }

    pub fn char_to_line(&self, char_offset: usize) -> Result<usize, EditError> {
        self.validate_offset(char_offset)?;
        Ok(self.rope.char_to_line(char_offset))
    }

    pub fn line_to_char(&self, line: usize) -> Option<usize> {
        (line < self.rope.len_lines()).then(|| self.rope.line_to_char(line))
    }

    pub fn line_text(&self, line: usize) -> Option<String> {
        (line < self.rope.len_lines()).then(|| self.rope.line(line).to_string())
    }

    pub fn slice_chars(&self, chars: Range<usize>) -> Result<String, EditError> {
        if chars.start > chars.end {
            return Err(EditError::OutOfBounds);
        }
        self.validate_offset(chars.start)?;
        self.validate_offset(chars.end)?;
        Ok(self.rope.slice(chars).to_string())
    }

    pub fn snapshot(&self) -> TextSnapshot {
        let mut cached = self.cached.borrow_mut();
        cached
            .get_or_insert_with(|| {
                TextSnapshot::from_owned_with_identity(self.rope.to_string(), self.identity.clone())
            })
            .clone()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    fn is_crlf_boundary(&self, offset: usize) -> bool {
        offset > 0
            && offset < self.rope.len_chars()
            && self.rope.char(offset - 1) == '\r'
            && self.rope.char(offset) == '\n'
    }

    fn validate_offset(&self, offset: usize) -> Result<(), EditError> {
        if offset > self.rope.len_chars() {
            Err(EditError::OutOfBounds)
        } else if self.is_crlf_boundary(offset) {
            Err(EditError::InvalidLineEndingBoundary)
        } else {
            Ok(())
        }
    }

    pub fn set_cursor(&mut self, char_offset: usize) -> Result<(), EditError> {
        self.validate_offset(char_offset)?;
        self.cursor = char_offset;
        self.desired_column = None;
        Ok(())
    }

    fn line_end(&self, line: usize) -> usize {
        let start = self.rope.line_to_char(line);
        let mut end = start + self.rope.line(line).len_chars();
        if end > start && self.rope.char(end - 1) == '\n' {
            end -= 1;
        }
        if end > start && self.rope.char(end - 1) == '\r' {
            end -= 1;
        }
        end
    }

    pub fn move_cursor(&mut self, motion: CursorMotion) -> bool {
        let next = match motion {
            CursorMotion::Left if self.cursor > 0 => {
                let previous = self.cursor - 1;
                if self.is_crlf_boundary(previous) {
                    previous - 1
                } else {
                    previous
                }
            }
            CursorMotion::Right if self.cursor < self.rope.len_chars() => {
                let following = self.cursor + 1;
                if self.is_crlf_boundary(following) {
                    following + 1
                } else {
                    following
                }
            }
            CursorMotion::Up | CursorMotion::Down => {
                let line = self.rope.char_to_line(self.cursor);
                let other = match motion {
                    CursorMotion::Up if line > 0 => line - 1,
                    CursorMotion::Down if line + 1 < self.rope.len_lines() => line + 1,
                    _ => return false,
                };
                let start = self.rope.line_to_char(line);
                let column = self.desired_column.unwrap_or(self.cursor - start);
                self.desired_column = Some(column);
                let other_start = self.rope.line_to_char(other);
                other_start + column.min(self.line_end(other) - other_start)
            }
            _ => return false,
        };
        self.cursor = next;
        if !matches!(motion, CursorMotion::Up | CursorMotion::Down) {
            self.desired_column = None;
        }
        true
    }

    pub fn insert(&mut self, text: &str) -> Result<(), EditError> {
        self.replace(self.cursor..self.cursor, text)
    }

    pub fn delete(&mut self, chars: Range<usize>) -> Result<(), EditError> {
        self.replace(chars, "")
    }

    pub fn replace(&mut self, chars: Range<usize>, text: &str) -> Result<(), EditError> {
        if chars.start > chars.end {
            return Err(EditError::OutOfBounds);
        }
        self.validate_offset(chars.start)?;
        self.validate_offset(chars.end)?;
        if text.as_bytes().contains(&0) {
            return Err(EditError::BinaryInput);
        }
        if chars.is_empty() && text.is_empty() {
            return Ok(());
        }
        self.undo.push(HistoryState {
            rope: self.rope.clone(),
            cursor: self.cursor,
            desired_column: self.desired_column,
        });
        self.redo.clear();
        self.rope.remove(chars.clone());
        self.rope.insert(chars.start, text);
        self.cursor = chars.start + text.chars().count();
        if self.is_crlf_boundary(self.cursor) {
            self.cursor += 1;
        }
        self.desired_column = None;
        self.cached.get_mut().take();
        self.identity = Arc::new(());
        Ok(())
    }

    pub fn undo(&mut self) -> bool {
        let Some(previous) = self.undo.pop() else {
            return false;
        };
        self.redo.push(HistoryState {
            rope: self.rope.clone(),
            cursor: self.cursor,
            desired_column: self.desired_column,
        });
        self.rope = previous.rope;
        self.cursor = previous.cursor;
        self.desired_column = previous.desired_column;
        self.cached.get_mut().take();
        self.identity = Arc::new(());
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        self.undo.push(HistoryState {
            rope: self.rope.clone(),
            cursor: self.cursor,
            desired_column: self.desired_column,
        });
        self.rope = next.rope;
        self.cursor = next.cursor;
        self.desired_column = next.desired_column;
        self.cached.get_mut().take();
        self.identity = Arc::new(());
        true
    }
}
