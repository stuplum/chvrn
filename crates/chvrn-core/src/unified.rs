use std::fmt;
use std::ops::Range;

#[derive(Debug)]
pub struct UnifiedPatch {
    pub files: Vec<PatchFile>,
    source: String,
}

#[derive(Debug)]
pub struct PatchFile {
    pub old_path: Option<String>,
    pub new_path: Option<String>,
    pub metadata: Vec<String>,
    pub hunks: Vec<PatchHunk>,
    pub binary: bool,
}

#[derive(Debug)]
pub struct PatchHunk {
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
    pub heading: String,
    pub lines: Vec<PatchLine>,
}

#[derive(Debug)]
pub struct PatchLine {
    pub kind: PatchLineKind,
    pub text: Range<usize>,
    pub old_number: Option<usize>,
    pub new_number: Option<usize>,
    pub no_newline: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PatchLineKind {
    Context,
    Removed,
    Added,
}

#[derive(Debug)]
pub struct PatchError {
    line: usize,
    reason: &'static str,
}

impl PatchError {
    fn new(line: usize, reason: &'static str) -> Self {
        Self { line, reason }
    }
}

impl fmt::Display for PatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid unified diff at line {}: {}",
            self.line, self.reason
        )
    }
}

impl std::error::Error for PatchError {}

impl UnifiedPatch {
    pub fn parse(input: String) -> Result<Self, PatchError> {
        let source = normalize(input)?;
        let mut parser = Parser {
            source: &source,
            offset: 0,
            line: 1,
        };
        let mut files = Vec::new();
        while let Some(line) = parser.peek() {
            files.push(parser.file(line)?);
        }
        Ok(Self { files, source })
    }

    pub fn text(&self, span: &Range<usize>) -> &str {
        &self.source[span.clone()]
    }
}

fn normalize(input: String) -> Result<String, PatchError> {
    let mut output = None;
    let mut copied = 0;
    let mut offset = 0;
    let mut line = 1;
    while offset < input.len() {
        let ch = input[offset..].chars().next().unwrap();
        if ch == '\u{1b}' {
            let bytes = input.as_bytes();
            let mut end = offset + 1;
            if bytes.get(end) != Some(&b'[') {
                return Err(PatchError::new(line, "unsupported terminal escape"));
            }
            end += 1;
            while bytes
                .get(end)
                .is_some_and(|b| b.is_ascii_digit() || *b == b';' || *b == b':')
            {
                end += 1;
            }
            if bytes.get(end) != Some(&b'm') {
                return Err(PatchError::new(line, "unsupported terminal escape"));
            }
            output
                .get_or_insert_with(|| String::with_capacity(input.len()))
                .push_str(&input[copied..offset]);
            offset = end + 1;
            copied = offset;
            continue;
        }
        if ch.is_control()
            && ch != '\t'
            && ch != '\n'
            && !(ch == '\r' && input.as_bytes().get(offset + 1) == Some(&b'\n'))
        {
            return Err(PatchError::new(line, "unsupported control character"));
        }
        if ch == '\n' {
            line += 1;
        }
        offset += ch.len_utf8();
    }
    if let Some(mut output) = output {
        output.push_str(&input[copied..]);
        Ok(output)
    } else {
        Ok(input)
    }
}

#[derive(Clone, Copy)]
struct SourceLine<'a> {
    text: &'a str,
    start: usize,
    number: usize,
    next: usize,
}

struct Parser<'a> {
    source: &'a str,
    offset: usize,
    line: usize,
}

#[derive(Default)]
struct Metadata {
    old_mode: bool,
    new_mode: bool,
    added: bool,
    deleted: bool,
    rename_from: bool,
    rename_to: bool,
    copy_from: bool,
    copy_to: bool,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<SourceLine<'a>> {
        if self.offset == self.source.len() {
            return None;
        }
        let rest = &self.source[self.offset..];
        let length = rest.find('\n').unwrap_or(rest.len());
        let text = &rest[..length];
        Some(SourceLine {
            text: text.strip_suffix('\r').unwrap_or(text),
            start: self.offset,
            number: self.line,
            next: self.offset + length + usize::from(length < rest.len()),
        })
    }

    fn advance(&mut self) {
        if let Some(line) = self.peek() {
            self.offset = line.next;
            self.line += 1;
        }
    }

    fn file(&mut self, first: SourceLine<'_>) -> Result<PatchFile, PatchError> {
        let first_number = first.number;
        let git = first.text.starts_with("diff --git ");
        let (old_path, new_path, prefixed) = if git {
            let (old, new) = git_paths(&first.text[11..], first.number)?;
            let prefixed = old.starts_with("a/") && new.starts_with("b/");
            self.advance();
            (
                strip_path(old, if prefixed { "a/" } else { "" }),
                strip_path(new, if prefixed { "b/" } else { "" }),
                prefixed,
            )
        } else if first.text.starts_with("--- ") {
            (None, None, false)
        } else {
            return Err(PatchError::new(
                first.number,
                "expected file header; combined diffs are not supported",
            ));
        };
        let mut file = PatchFile {
            old_path,
            new_path,
            metadata: Vec::new(),
            hunks: Vec::new(),
            binary: false,
        };
        let mut metadata = Metadata::default();
        let mut headers = false;
        let mut old_ended = None;
        let mut new_ended = None;
        while let Some(line) = self.peek() {
            if line.text.starts_with("diff --git ") {
                break;
            }
            if line.text.starts_with("--- ") {
                if headers {
                    if !git {
                        break;
                    }
                    return Err(PatchError::new(line.number, "duplicate file headers"));
                }
                file.old_path = header_path(
                    &line.text[4..],
                    if prefixed { "a/" } else { "" },
                    if metadata.rename_from || metadata.copy_from {
                        file.old_path.as_deref()
                    } else {
                        None
                    },
                    line.number,
                )?;
                self.advance();
                let next = self
                    .peek()
                    .ok_or_else(|| PatchError::new(self.line, "missing +++ file header"))?;
                let path = next
                    .text
                    .strip_prefix("+++ ")
                    .ok_or_else(|| PatchError::new(next.number, "expected +++ file header"))?;
                file.new_path = header_path(
                    path,
                    if prefixed { "b/" } else { "" },
                    if metadata.rename_to || metadata.copy_to {
                        file.new_path.as_deref()
                    } else {
                        None
                    },
                    next.number,
                )?;
                if file.old_path.is_none() && file.new_path.is_none() {
                    return Err(PatchError::new(
                        next.number,
                        "both file paths are /dev/null",
                    ));
                }
                headers = true;
                self.advance();
            } else if line.text.starts_with("@@ ") {
                if !headers || file.binary {
                    return Err(PatchError::new(
                        line.number,
                        "hunk requires text file headers",
                    ));
                }
                let hunk = self.hunk(&mut old_ended, &mut new_ended)?;
                if let Some(previous) = file.hunks.last() {
                    if position(hunk.old_start, hunk.old_count)
                        < range_end(previous.old_start, previous.old_count)
                        || position(hunk.new_start, hunk.new_count)
                            < range_end(previous.new_start, previous.new_count)
                    {
                        return Err(PatchError::new(
                            self.line,
                            "overlapping or out-of-order hunks",
                        ));
                    }
                }
                if (file.old_path.is_none() && hunk.old_count != 0)
                    || (file.new_path.is_none() && hunk.new_count != 0)
                {
                    return Err(PatchError::new(
                        self.line,
                        "hunk consumes lines from /dev/null",
                    ));
                }
                file.hunks.push(hunk);
            } else if git && !headers && !file.binary && line.text == "GIT binary patch" {
                file.binary = true;
                file.metadata.push(line.text.to_owned());
                self.advance();
                self.binary_payload()?;
            } else if git
                && !headers
                && !file.binary
                && line.text.starts_with("Binary files ")
                && line.text.ends_with(" differ")
            {
                file.binary = true;
                file.metadata.push(line.text.to_owned());
                self.advance();
            } else if git && !headers && !file.binary {
                parse_metadata(&mut file, &mut metadata, line.text, line.number)?;
                file.metadata.push(line.text.to_owned());
                self.advance();
            } else {
                return Err(PatchError::new(
                    line.number,
                    "unexpected content outside a hunk",
                ));
            }
        }
        if metadata.old_mode != metadata.new_mode
            || metadata.rename_from != metadata.rename_to
            || metadata.copy_from != metadata.copy_to
            || (metadata.added && metadata.deleted)
            || (metadata.rename_from && metadata.copy_from)
            || (metadata.added && file.old_path.is_some())
            || (metadata.deleted && file.new_path.is_some())
        {
            return Err(PatchError::new(
                first_number,
                "incomplete or conflicting file metadata",
            ));
        }
        if headers && file.hunks.is_empty() {
            return Err(PatchError::new(self.line, "file headers have no hunks"));
        }
        if !file.binary
            && file.hunks.is_empty()
            && !(metadata.old_mode
                || metadata.added
                || metadata.deleted
                || metadata.rename_from
                || metadata.copy_from)
        {
            return Err(PatchError::new(first_number, "file contains no change"));
        }
        Ok(file)
    }

    fn hunk(
        &mut self,
        old_ended: &mut Option<usize>,
        new_ended: &mut Option<usize>,
    ) -> Result<PatchHunk, PatchError> {
        let header = self.peek().unwrap();
        let mut hunk = hunk_header(header.text, header.number)?;
        if old_ended.is_some_and(|last| hunk.old_count != 0 || hunk.old_start > last)
            || new_ended.is_some_and(|last| hunk.new_count != 0 || hunk.new_start > last)
        {
            return Err(PatchError::new(
                header.number,
                "hunk extends beyond a marked final line",
            ));
        }
        self.advance();
        let mut old_used = 0;
        let mut new_used = 0;
        loop {
            let Some(line) = self.peek() else {
                break;
            };
            if line.text == "\\ No newline at end of file" {
                let previous = hunk.lines.last_mut().ok_or_else(|| {
                    PatchError::new(line.number, "newline marker has no preceding line")
                })?;
                if previous.no_newline {
                    return Err(PatchError::new(line.number, "duplicate newline marker"));
                }
                previous.no_newline = true;
                if previous.old_number.is_some() {
                    *old_ended = previous.old_number;
                }
                if previous.new_number.is_some() {
                    *new_ended = previous.new_number;
                }
                self.advance();
                continue;
            }
            if old_used == hunk.old_count && new_used == hunk.new_count {
                break;
            }
            let kind = match line.text.as_bytes().first() {
                Some(b' ') => PatchLineKind::Context,
                Some(b'-') => PatchLineKind::Removed,
                Some(b'+') => PatchLineKind::Added,
                _ => return Err(PatchError::new(line.number, "expected hunk body line")),
            };
            let old_number = if kind != PatchLineKind::Added {
                if old_used == hunk.old_count || old_ended.is_some() {
                    return Err(PatchError::new(
                        line.number,
                        "old side exceeds its count or final line",
                    ));
                }
                let number = hunk
                    .old_start
                    .checked_add(old_used)
                    .ok_or_else(|| PatchError::new(line.number, "old line number overflow"))?;
                old_used += 1;
                Some(number)
            } else {
                None
            };
            let new_number = if kind != PatchLineKind::Removed {
                if new_used == hunk.new_count || new_ended.is_some() {
                    return Err(PatchError::new(
                        line.number,
                        "new side exceeds its count or final line",
                    ));
                }
                let number = hunk
                    .new_start
                    .checked_add(new_used)
                    .ok_or_else(|| PatchError::new(line.number, "new line number overflow"))?;
                new_used += 1;
                Some(number)
            } else {
                None
            };
            hunk.lines.push(PatchLine {
                kind,
                text: line.start + 1..line.start + line.text.len(),
                old_number,
                new_number,
                no_newline: false,
            });
            self.advance();
        }
        if old_used != hunk.old_count || new_used != hunk.new_count {
            return Err(PatchError::new(self.line, "truncated hunk body"));
        }
        Ok(hunk)
    }

    fn binary_payload(&mut self) -> Result<(), PatchError> {
        let mut sections = 0;
        loop {
            let Some(header) = self.peek() else {
                break;
            };
            if header.text.starts_with("diff --git ") {
                break;
            }
            let count = header
                .text
                .strip_prefix("literal ")
                .or_else(|| header.text.strip_prefix("delta "))
                .ok_or_else(|| {
                    PatchError::new(header.number, "expected binary literal or delta header")
                })?;
            number(count, header.number)?;
            self.advance();
            let mut lines = 0;
            let mut terminated = false;
            while let Some(line) = self.peek() {
                if line.text.is_empty() {
                    terminated = true;
                    self.advance();
                    break;
                }
                if line.text.starts_with("diff --git ") {
                    break;
                }
                let bytes = line.text.as_bytes();
                let decoded = match bytes.first() {
                    Some(b'A'..=b'Z') => usize::from(bytes[0] - b'A') + 1,
                    Some(b'a'..=b'z') => usize::from(bytes[0] - b'a') + 27,
                    _ => return Err(PatchError::new(line.number, "invalid binary payload line")),
                };
                const ALPHABET: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz!#$%&()*+-;<=>?@^_`{|}~";
                if bytes.len() != 1 + decoded.div_ceil(4) * 5
                    || !bytes[1..].iter().all(|byte| ALPHABET.contains(byte))
                {
                    return Err(PatchError::new(
                        line.number,
                        "invalid binary payload encoding",
                    ));
                }
                lines += 1;
                self.advance();
            }
            if lines == 0 || !terminated {
                return Err(PatchError::new(self.line, "truncated binary payload"));
            }
            sections += 1;
        }
        if sections == 0 {
            return Err(PatchError::new(self.line, "missing binary payload"));
        }
        Ok(())
    }
}

fn number(text: &str, line: usize) -> Result<usize, PatchError> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(PatchError::new(line, "invalid decimal number"));
    }
    text.parse()
        .map_err(|_| PatchError::new(line, "number exceeds supported range"))
}

fn position(start: usize, count: usize) -> usize {
    if count == 0 { start } else { start - 1 }
}

fn range_end(start: usize, count: usize) -> usize {
    position(start, count) + count
}

fn hunk_range(text: &str, prefix: char, line: usize) -> Result<(usize, usize), PatchError> {
    let text = text
        .strip_prefix(prefix)
        .ok_or_else(|| PatchError::new(line, "invalid hunk range"))?;
    let (start, count) = match text.split_once(',') {
        Some((start, count)) => (number(start, line)?, number(count, line)?),
        None => (number(text, line)?, 1),
    };
    if count != 0 && start == 0 {
        return Err(PatchError::new(line, "nonempty hunk range starts at zero"));
    }
    if position(start, count).checked_add(count).is_none() {
        return Err(PatchError::new(line, "hunk range overflow"));
    }
    Ok((start, count))
}

fn hunk_header(text: &str, line: usize) -> Result<PatchHunk, PatchError> {
    let text = text
        .strip_prefix("@@ ")
        .ok_or_else(|| PatchError::new(line, "invalid hunk header"))?;
    let (old, rest) = text
        .split_once(' ')
        .ok_or_else(|| PatchError::new(line, "missing new hunk range"))?;
    let (new, suffix) = rest
        .split_once(" @@")
        .ok_or_else(|| PatchError::new(line, "missing hunk header terminator"))?;
    let heading = if suffix.is_empty() {
        ""
    } else {
        suffix
            .strip_prefix(' ')
            .ok_or_else(|| PatchError::new(line, "invalid hunk heading"))?
    };
    let (old_start, old_count) = hunk_range(old, '-', line)?;
    let (new_start, new_count) = hunk_range(new, '+', line)?;
    if old_count == 0 && new_count == 0 {
        return Err(PatchError::new(line, "empty hunk"));
    }
    Ok(PatchHunk {
        old_start,
        old_count,
        new_start,
        new_count,
        heading: heading.to_owned(),
        lines: Vec::new(),
    })
}

fn quoted_path(text: &str, line: usize) -> Result<(String, &str), PatchError> {
    let bytes = text.as_bytes();
    let mut output = Vec::new();
    let mut index = 1;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => {
                let path = String::from_utf8(output)
                    .map_err(|_| PatchError::new(line, "path is not valid UTF-8"))?;
                if path.is_empty() || path.contains('\0') {
                    return Err(PatchError::new(
                        line,
                        "invalid empty or NUL-containing path",
                    ));
                }
                return Ok((path, &text[index + 1..]));
            }
            b'\\' => {
                index += 1;
                let escape = *bytes
                    .get(index)
                    .ok_or_else(|| PatchError::new(line, "truncated path escape"))?;
                let value = match escape {
                    b'a' => 7,
                    b'b' => 8,
                    b't' => b'\t',
                    b'n' => b'\n',
                    b'v' => 11,
                    b'f' => 12,
                    b'r' => b'\r',
                    b'\\' => b'\\',
                    b'"' => b'"',
                    b'0'..=b'3' => {
                        let second = *bytes
                            .get(index + 1)
                            .filter(|byte| matches!(**byte, b'0'..=b'7'))
                            .ok_or_else(|| PatchError::new(line, "invalid octal path escape"))?;
                        let third = *bytes
                            .get(index + 2)
                            .filter(|byte| matches!(**byte, b'0'..=b'7'))
                            .ok_or_else(|| PatchError::new(line, "invalid octal path escape"))?;
                        index += 2;
                        (escape - b'0') * 64 + (second - b'0') * 8 + (third - b'0')
                    }
                    _ => return Err(PatchError::new(line, "invalid quoted path escape")),
                };
                output.push(value);
            }
            byte => output.push(byte),
        }
        index += 1;
    }
    Err(PatchError::new(line, "unterminated quoted path"))
}

fn path_value(text: &str, line: usize) -> Result<String, PatchError> {
    if text.starts_with('"') {
        let (path, rest) = quoted_path(text, line)?;
        if !rest.is_empty() {
            return Err(PatchError::new(
                line,
                "unexpected content after quoted path",
            ));
        }
        Ok(path)
    } else if text.is_empty() {
        Err(PatchError::new(line, "empty file path"))
    } else {
        Ok(text.to_owned())
    }
}

fn strip_path(mut path: String, prefix: &str) -> Option<String> {
    if path == "/dev/null" {
        None
    } else {
        if path.starts_with(prefix) {
            path.drain(..prefix.len());
        }
        Some(path)
    }
}

fn header_path(
    text: &str,
    prefix: &str,
    authoritative: Option<&str>,
    line: usize,
) -> Result<Option<String>, PatchError> {
    let path = if text.starts_with('"') {
        let (path, rest) = quoted_path(text, line)?;
        if !rest.is_empty() && !rest.starts_with('\t') {
            return Err(PatchError::new(line, "unexpected content after file path"));
        }
        path
    } else {
        path_value(text.split('\t').next().unwrap(), line)?
    };
    let prefix = if authoritative == Some(path.as_str()) {
        ""
    } else {
        prefix
    };
    Ok(strip_path(path, prefix))
}

fn git_paths(text: &str, line: usize) -> Result<(String, String), PatchError> {
    let (old, new) = if text.starts_with('"') {
        let (old, rest) = quoted_path(text, line)?;
        let rest = rest
            .strip_prefix(' ')
            .ok_or_else(|| PatchError::new(line, "missing new Git path"))?;
        (old, path_value(rest, line)?)
    } else {
        let split = text
            .match_indices(' ')
            .find(|(index, _)| text[..*index] == text[*index + 1..])
            .or_else(|| {
                text.match_indices(" b/").find(|(index, _)| {
                    text[..*index].strip_prefix("a/") == text[*index + 1..].strip_prefix("b/")
                })
            })
            .or_else(|| text.match_indices(" b/").next())
            .or_else(|| text.match_indices(" \"").next())
            .or_else(|| text.match_indices(' ').next())
            .ok_or_else(|| PatchError::new(line, "missing Git path pair"))?
            .0;
        (
            path_value(&text[..split], line)?,
            path_value(&text[split + 1..], line)?,
        )
    };
    Ok((old, new))
}

fn parse_metadata(
    file: &mut PatchFile,
    state: &mut Metadata,
    text: &str,
    line: usize,
) -> Result<(), PatchError> {
    for (prefix, flag) in [
        ("old mode ", &mut state.old_mode),
        ("new mode ", &mut state.new_mode),
        ("new file mode ", &mut state.added),
        ("deleted file mode ", &mut state.deleted),
    ] {
        if let Some(mode) = text.strip_prefix(prefix) {
            if *flag || mode.len() != 6 || !mode.bytes().all(|byte| matches!(byte, b'0'..=b'7')) {
                return Err(PatchError::new(line, "invalid or duplicate file mode"));
            }
            *flag = true;
            if prefix == "new file mode " {
                file.old_path = None;
            }
            if prefix == "deleted file mode " {
                file.new_path = None;
            }
            return Ok(());
        }
    }
    for (prefix, flag, path) in [
        ("rename from ", &mut state.rename_from, &mut file.old_path),
        ("rename to ", &mut state.rename_to, &mut file.new_path),
    ] {
        if let Some(value) = text.strip_prefix(prefix) {
            if *flag {
                return Err(PatchError::new(line, "duplicate rename metadata"));
            }
            *flag = true;
            *path = Some(path_value(value, line)?);
            return Ok(());
        }
    }
    for (prefix, flag, path) in [
        ("copy from ", &mut state.copy_from, &mut file.old_path),
        ("copy to ", &mut state.copy_to, &mut file.new_path),
    ] {
        if let Some(value) = text.strip_prefix(prefix) {
            if *flag {
                return Err(PatchError::new(line, "duplicate copy metadata"));
            }
            *flag = true;
            *path = Some(path_value(value, line)?);
            return Ok(());
        }
    }
    if let Some(value) = text
        .strip_prefix("similarity index ")
        .or_else(|| text.strip_prefix("dissimilarity index "))
    {
        let percent = value
            .strip_suffix('%')
            .ok_or_else(|| PatchError::new(line, "invalid similarity percentage"))?;
        if number(percent, line)? > 100 {
            return Err(PatchError::new(line, "invalid similarity percentage"));
        }
        return Ok(());
    }
    if let Some(value) = text.strip_prefix("index ") {
        let (hashes, mode) = value
            .split_once(' ')
            .map_or((value, None), |(hashes, mode)| (hashes, Some(mode)));
        let (old, new) = hashes
            .split_once("..")
            .ok_or_else(|| PatchError::new(line, "invalid index metadata"))?;
        if old.is_empty()
            || new.is_empty()
            || !old
                .bytes()
                .chain(new.bytes())
                .all(|byte| byte.is_ascii_hexdigit())
            || mode.is_some_and(|mode| {
                mode.len() != 6 || !mode.bytes().all(|byte| matches!(byte, b'0'..=b'7'))
            })
        {
            return Err(PatchError::new(line, "invalid index metadata"));
        }
        return Ok(());
    }
    Err(PatchError::new(
        line,
        "unrecognised metadata or unsupported combined diff",
    ))
}
