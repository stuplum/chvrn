use chvrn_core::{TextSnapshot, edit::TextBuffer};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::io::{Cursor, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

const MAX_HEADER_BYTES: usize = 8192;
const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LspError {
    Io(String),
    Protocol(String),
    InvalidPosition,
    StaleSnapshot,
    Unavailable,
}

impl std::fmt::Display for LspError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(message) | Self::Protocol(message) => write!(formatter, "{message}"),
            Self::InvalidPosition => write!(formatter, "invalid UTF-16 or UTF-8 text position"),
            Self::StaleSnapshot => write!(formatter, "document snapshot is stale"),
            Self::Unavailable => write!(formatter, "language server capability is unavailable"),
        }
    }
}

impl std::error::Error for LspError {}

impl From<std::io::Error> for LspError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    pub uri: String,
    pub text: String,
    pub version: i32,
    pub snapshot_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextPosition {
    pub line: u32,
    pub byte_column: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspLocation {
    pub uri: String,
    pub line: u32,
    pub utf16_column: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub message: String,
    pub start: TextPosition,
    pub end: TextPosition,
}

pub struct FormatRequest {
    pub inspected_snapshot_id: String,
    pub new_snapshot_id: String,
}

#[derive(Clone)]
pub struct SnapshotBound<T> {
    source: TextSnapshot,
    pub value: T,
}

impl<T> SnapshotBound<T> {
    pub fn source(&self) -> &TextSnapshot {
        &self.source
    }

    pub fn is_current(&self, current: &TextSnapshot) -> bool {
        self.source.same_identity(current)
    }

    pub fn into_current(self, current: &TextSnapshot) -> Result<T, LspError> {
        if !self.is_current(current) {
            return Err(LspError::StaleSnapshot);
        }
        Ok(self.value)
    }
}

impl SnapshotBound<Option<String>> {
    pub fn apply_to_buffer(
        &self,
        buffer: &mut TextBuffer,
    ) -> Result<Option<TextSnapshot>, LspError> {
        if !self.is_current(&buffer.snapshot()) {
            return Err(LspError::StaleSnapshot);
        }
        let Some(text) = self.value.as_deref() else {
            return Ok(None);
        };
        if self.source.text() == text {
            return Ok(None);
        }
        buffer
            .replace(0..self.source.text().chars().count(), text)
            .map_err(|error| LspError::Protocol(format!("format edit rejected: {error:?}")))?;
        Ok(Some(buffer.snapshot()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormattingOptions {
    pub tab_size: u32,
    pub insert_spaces: bool,
}

impl Default for FormattingOptions {
    fn default() -> Self {
        Self {
            tab_size: 4,
            insert_spaces: true,
        }
    }
}

pub struct LspSession<R: Read, W: Write> {
    reader: R,
    writer: W,
    document: Document,
    next_id: i64,
    diagnostics: Vec<Diagnostic>,
    bound_snapshot: Option<TextSnapshot>,
    diagnostics_snapshot: Option<TextSnapshot>,
    history: Vec<String>,
    used_snapshot_ids: HashSet<String>,
    formatting_options: FormattingOptions,
    desynchronised: bool,
}

impl<R: Read, W: Write> LspSession<R, W> {
    pub fn new(reader: R, writer: W, document: Document) -> Self {
        let used_snapshot_ids = HashSet::from([document.snapshot_id.clone()]);
        Self {
            reader,
            writer,
            document,
            next_id: 1,
            diagnostics: Vec::new(),
            bound_snapshot: None,
            diagnostics_snapshot: None,
            history: Vec::new(),
            used_snapshot_ids,
            formatting_options: FormattingOptions::default(),
            desynchronised: false,
        }
    }

    pub fn document(&self) -> &Document {
        &self.document
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    pub fn bind_snapshot(&mut self, snapshot: TextSnapshot) -> Result<(), LspError> {
        if snapshot.text() != self.document.text {
            return Err(LspError::StaleSnapshot);
        }
        if self
            .bound_snapshot
            .as_ref()
            .is_some_and(|bound| !bound.same_identity(&snapshot))
        {
            self.diagnostics.clear();
            self.diagnostics_snapshot = None;
        }
        self.bound_snapshot = Some(snapshot);
        Ok(())
    }

    pub fn diagnostics_for_snapshot(&self, current: &TextSnapshot) -> Option<&[Diagnostic]> {
        self.diagnostics_snapshot
            .as_ref()
            .filter(|source| source.same_identity(current))
            .map(|_| self.diagnostics.as_slice())
    }

    pub fn set_formatting_options(&mut self, options: FormattingOptions) -> Result<(), LspError> {
        if options.tab_size == 0 {
            return Err(LspError::Protocol(
                "format tab size must be positive".into(),
            ));
        }
        self.formatting_options = options;
        Ok(())
    }

    pub fn initialise(
        &mut self,
        root_uri: Option<&str>,
        language_id: &str,
    ) -> Result<Value, LspError> {
        let response = self.request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "clientInfo": { "name": "chvrn" },
                "rootUri": root_uri,
                "capabilities": {
                    "general": { "positionEncodings": ["utf-16"] },
                    "textDocument": {
                        "synchronization": { "dynamicRegistration": false, "didSave": false },
                        "hover": { "contentFormat": ["plaintext"] },
                        "definition": {},
                        "formatting": {}
                    }
                }
            }),
        )?;
        let encoding = response
            .pointer("/capabilities/positionEncoding")
            .and_then(Value::as_str)
            .unwrap_or("utf-16");
        if encoding != "utf-16" {
            return Err(LspError::Protocol(
                "language server selected an unsupported position encoding".into(),
            ));
        }
        self.notify("initialized", json!({}))?;
        self.notify(
            "textDocument/didOpen",
            json!({ "textDocument": {
            "uri": &self.document.uri,
            "languageId": language_id,
            "version": self.document.version,
            "text": &self.document.text
        } }),
        )?;
        Ok(response)
    }

    pub fn hover(&mut self, position: TextPosition) -> Result<Option<String>, LspError> {
        let wire = to_utf16(&self.document.text, position)?;
        let response = self.request(
            "textDocument/hover",
            json!({
                "textDocument": { "uri": &self.document.uri },
                "position": { "line": position.line, "character": wire }
            }),
        )?;
        let Some(contents) = response.get("contents") else {
            return Ok(None);
        };
        if contents.is_null() {
            return Ok(None);
        }
        if let Some(text) = contents.as_str() {
            return Ok(Some(text.into()));
        }
        if let Some(value) = contents.get("value").and_then(Value::as_str) {
            return Ok(Some(value.into()));
        }
        if let Some(values) = contents.as_array() {
            let parts: Vec<&str> = values
                .iter()
                .filter_map(|item| {
                    item.as_str()
                        .or_else(|| item.get("value").and_then(Value::as_str))
                })
                .collect();
            if parts.len() == values.len() {
                return Ok(Some(parts.join("\n")));
            }
        }
        Err(LspError::Protocol("invalid hover response".into()))
    }

    pub fn definition(&mut self, position: TextPosition) -> Result<Option<LspLocation>, LspError> {
        let wire = to_utf16(&self.document.text, position)?;
        let response = self.request(
            "textDocument/definition",
            json!({
                "textDocument": { "uri": &self.document.uri },
                "position": { "line": position.line, "character": wire }
            }),
        )?;
        if response.is_null() {
            return Ok(None);
        }
        let location = if let Some(values) = response.as_array() {
            let Some(first) = values.first() else {
                return Ok(None);
            };
            first
        } else {
            &response
        };
        let (uri, start) = if location.get("targetUri").is_some() {
            (
                location.get("targetUri"),
                location
                    .pointer("/targetSelectionRange/start")
                    .or_else(|| location.pointer("/targetRange/start")),
            )
        } else {
            (location.get("uri"), location.pointer("/range/start"))
        };
        let uri = uri
            .and_then(Value::as_str)
            .ok_or_else(|| LspError::Protocol("definition is missing URI".into()))?;
        let line = start
            .and_then(|position| position.get("line"))
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| LspError::Protocol("definition is missing line".into()))?;
        let utf16_column = start
            .and_then(|position| position.get("character"))
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| LspError::Protocol("definition is missing character".into()))?;
        Ok(Some(LspLocation {
            uri: uri.into(),
            line,
            utf16_column,
        }))
    }

    pub fn read_diagnostics(&mut self) -> Result<(), LspError> {
        let message = read_frame(&mut self.reader)?;
        self.process_incoming(&message)
    }

    pub fn replace_text(&mut self, text: String, new_snapshot_id: String) -> Result<(), LspError> {
        self.validate_snapshot_id(&new_snapshot_id)?;
        self.commit_text(text, new_snapshot_id)
    }

    pub fn format(&mut self, request: FormatRequest) -> Result<(), LspError> {
        if self.document.snapshot_id != request.inspected_snapshot_id {
            return Err(LspError::StaleSnapshot);
        }
        self.validate_snapshot_id(&request.new_snapshot_id)?;
        if let Some(text) = self.request_formatted_text()? {
            self.commit_text(text, request.new_snapshot_id)?;
        }
        Ok(())
    }

    pub fn format_proposal(
        &mut self,
        source: &TextSnapshot,
    ) -> Result<SnapshotBound<Option<String>>, LspError> {
        if !self
            .bound_snapshot
            .as_ref()
            .is_some_and(|bound| bound.same_identity(source))
        {
            return Err(LspError::StaleSnapshot);
        }
        Ok(SnapshotBound {
            source: source.clone(),
            value: self.request_formatted_text()?,
        })
    }

    fn request_formatted_text(&mut self) -> Result<Option<String>, LspError> {
        let options = self.formatting_options;
        let response = self.request(
            "textDocument/formatting",
            json!({
                "textDocument": { "uri": &self.document.uri },
                "options": { "tabSize": options.tab_size, "insertSpaces": options.insert_spaces }
            }),
        )?;
        if response.is_null() {
            return Ok(None);
        }
        let edits = response
            .as_array()
            .ok_or_else(|| LspError::Protocol("formatting response is not a list".into()))?;
        if edits.is_empty() {
            return Ok(None);
        }
        let mut replacements = Vec::with_capacity(edits.len());
        for edit in edits {
            let start = wire_offset(
                &self.document.text,
                edit.pointer("/range/start")
                    .ok_or(LspError::InvalidPosition)?,
            )?;
            let end = wire_offset(
                &self.document.text,
                edit.pointer("/range/end")
                    .ok_or(LspError::InvalidPosition)?,
            )?;
            let replacement = edit
                .get("newText")
                .and_then(Value::as_str)
                .ok_or_else(|| LspError::Protocol("format edit is missing newText".into()))?;
            if start > end {
                return Err(LspError::InvalidPosition);
            }
            replacements.push((start, end, replacement));
        }
        replacements.sort_unstable_by_key(|(start, _, _)| *start);
        if replacements
            .windows(2)
            .any(|pair| pair[0].1 > pair[1].0 || pair[0].0 == pair[1].0)
        {
            return Err(LspError::Protocol("overlapping format edits".into()));
        }
        let mut text = self.document.text.clone();
        for (start, end, replacement) in replacements.into_iter().rev() {
            text.replace_range(start..end, replacement);
        }
        if text == self.document.text {
            return Ok(None);
        }
        Ok(Some(text))
    }

    pub fn undo(&mut self, new_snapshot_id: String) -> Result<(), LspError> {
        self.validate_snapshot_id(&new_snapshot_id)?;
        let next_version = self
            .document
            .version
            .checked_add(1)
            .ok_or_else(|| LspError::Protocol("document version overflow".into()))?;
        let Some(previous) = self.history.pop() else {
            return Err(LspError::StaleSnapshot);
        };
        if let Err(error) = self.send_change(&previous, next_version) {
            self.history.push(previous);
            return Err(error);
        }
        self.document.text = previous;
        self.document.version = next_version;
        self.document.snapshot_id = new_snapshot_id.clone();
        self.used_snapshot_ids.insert(new_snapshot_id);
        self.bound_snapshot = None;
        self.diagnostics_snapshot = None;
        self.diagnostics.clear();
        Ok(())
    }

    pub fn shutdown(&mut self) -> Result<(), LspError> {
        self.request("shutdown", Value::Null)?;
        self.notify("exit", Value::Null)
    }

    fn validate_snapshot_id(&self, id: &str) -> Result<(), LspError> {
        if id.is_empty() || self.used_snapshot_ids.contains(id) {
            return Err(LspError::StaleSnapshot);
        }
        Ok(())
    }

    fn commit_text(&mut self, text: String, new_snapshot_id: String) -> Result<(), LspError> {
        let version = self
            .document
            .version
            .checked_add(1)
            .ok_or_else(|| LspError::Protocol("document version overflow".into()))?;
        self.send_change(&text, version)?;
        self.history
            .push(std::mem::replace(&mut self.document.text, text));
        self.document.version = version;
        self.document.snapshot_id = new_snapshot_id.clone();
        self.used_snapshot_ids.insert(new_snapshot_id);
        self.diagnostics.clear();
        self.bound_snapshot = None;
        self.diagnostics_snapshot = None;
        Ok(())
    }

    fn send_change(&mut self, text: &str, version: i32) -> Result<(), LspError> {
        if self.desynchronised {
            return Err(LspError::Protocol(
                "language server state is unsynchronised".into(),
            ));
        }
        let message = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": &self.document.uri, "version": version },
                "contentChanges": [{ "text": text }]
            }
        });
        if let Err(error) = write_frame(&mut self.writer, &message) {
            self.desynchronised = true;
            return Err(error);
        }
        Ok(())
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), LspError> {
        if self.desynchronised {
            return Err(LspError::Protocol(
                "language server state is unsynchronised".into(),
            ));
        }
        write_frame(
            &mut self.writer,
            &json!({ "jsonrpc": "2.0", "method": method, "params": params }),
        )
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, LspError> {
        if self.desynchronised {
            return Err(LspError::Protocol(
                "language server state is unsynchronised".into(),
            ));
        }
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| LspError::Protocol("request ID overflow".into()))?;
        let version = self.document.version;
        write_frame(
            &mut self.writer,
            &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
        )?;
        loop {
            let message = read_frame(&mut self.reader)?;
            if message.get("id").and_then(Value::as_i64) == Some(id)
                && message.get("method").is_none()
            {
                if version != self.document.version {
                    return Err(LspError::StaleSnapshot);
                }
                if let Some(error) = message.get("error") {
                    return Err(LspError::Protocol(error.to_string()));
                }
                return message
                    .get("result")
                    .cloned()
                    .ok_or_else(|| LspError::Protocol("response has no result".into()));
            }
            self.process_incoming(&message)?;
        }
    }

    fn process_incoming(&mut self, message: &Value) -> Result<(), LspError> {
        if message.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics")
        {
            let params = message
                .get("params")
                .ok_or_else(|| LspError::Protocol("diagnostics missing parameters".into()))?;
            if params.get("uri").and_then(Value::as_str) != Some(&self.document.uri) {
                return Ok(());
            }
            let Some(version) = params.get("version").and_then(Value::as_i64) else {
                return Ok(());
            };
            if version != i64::from(self.document.version) {
                return Ok(());
            }
            let items = params
                .get("diagnostics")
                .and_then(Value::as_array)
                .ok_or_else(|| LspError::Protocol("diagnostics missing list".into()))?;
            let mut diagnostics = Vec::with_capacity(items.len());
            for item in items {
                let start = wire_position(
                    &self.document.text,
                    item.pointer("/range/start")
                        .ok_or(LspError::InvalidPosition)?,
                )?;
                let end = wire_position(
                    &self.document.text,
                    item.pointer("/range/end")
                        .ok_or(LspError::InvalidPosition)?,
                )?;
                let message = item
                    .get("message")
                    .and_then(Value::as_str)
                    .ok_or_else(|| LspError::Protocol("diagnostic missing message".into()))?;
                diagnostics.push(Diagnostic {
                    message: message.into(),
                    start,
                    end,
                });
            }
            self.diagnostics = diagnostics;
            self.diagnostics_snapshot = self.bound_snapshot.clone();
            return Ok(());
        }
        if let (Some(method), Some(id)) = (
            message.get("method").and_then(Value::as_str),
            message.get("id"),
        ) {
            let response = if method == "window/workDoneProgress/create" {
                json!({ "jsonrpc": "2.0", "id": id, "result": null })
            } else {
                json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "method not supported" } })
            };
            write_frame(&mut self.writer, &response)?;
        }
        Ok(())
    }
}

fn read_frame<R: Read>(reader: &mut R) -> Result<Value, LspError> {
    let mut headers = Vec::with_capacity(64);
    while !headers.ends_with(b"\r\n\r\n") {
        if headers.len() >= MAX_HEADER_BYTES {
            return Err(LspError::Protocol("LSP header exceeds limit".into()));
        }
        let mut byte = [0_u8; 1];
        reader.read_exact(&mut byte)?;
        headers.push(byte[0]);
    }
    let headers = std::str::from_utf8(&headers)
        .map_err(|_| LspError::Protocol("non-UTF-8 LSP header".into()))?;
    let mut length = None;
    for line in headers.split("\r\n") {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                if length.is_some() {
                    return Err(LspError::Protocol("duplicate Content-Length".into()));
                }
                length = Some(
                    value
                        .trim()
                        .parse::<usize>()
                        .map_err(|_| LspError::Protocol("invalid Content-Length".into()))?,
                );
            }
        }
    }
    let length = length.ok_or_else(|| LspError::Protocol("missing Content-Length".into()))?;
    if length == 0 || length > MAX_MESSAGE_BYTES {
        return Err(LspError::Protocol("LSP body length outside bounds".into()));
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    let message: Value =
        serde_json::from_slice(&body).map_err(|_| LspError::Protocol("invalid LSP JSON".into()))?;
    if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") || !message.is_object() {
        return Err(LspError::Protocol("invalid JSON-RPC message".into()));
    }
    Ok(message)
}

fn write_frame<W: Write>(writer: &mut W, message: &Value) -> Result<(), LspError> {
    let body =
        serde_json::to_vec(message).map_err(|error| LspError::Protocol(error.to_string()))?;
    if body.len() > MAX_MESSAGE_BYTES {
        return Err(LspError::Protocol("LSP message exceeds limit".into()));
    }
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    writer.flush()?;
    Ok(())
}

fn text_line(text: &str, line: u32) -> Result<&str, LspError> {
    text.split('\n')
        .nth(line as usize)
        .map(|text| text.strip_suffix('\r').unwrap_or(text))
        .ok_or(LspError::InvalidPosition)
}

fn to_utf16(text: &str, position: TextPosition) -> Result<u32, LspError> {
    let line = text_line(text, position.line)?;
    let prefix = line
        .get(..position.byte_column)
        .ok_or(LspError::InvalidPosition)?;
    u32::try_from(prefix.encode_utf16().count()).map_err(|_| LspError::InvalidPosition)
}

fn wire_position(text: &str, position: &Value) -> Result<TextPosition, LspError> {
    let line = position
        .get("line")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or(LspError::InvalidPosition)?;
    let column = position
        .get("character")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or(LspError::InvalidPosition)?;
    let content = text_line(text, line)?;
    let mut units = 0_u32;
    for (byte, character) in content.char_indices() {
        if units == column {
            return Ok(TextPosition {
                line,
                byte_column: byte,
            });
        }
        units += character.len_utf16() as u32;
        if units > column {
            return Err(LspError::InvalidPosition);
        }
    }
    if units == column {
        return Ok(TextPosition {
            line,
            byte_column: content.len(),
        });
    }
    Err(LspError::InvalidPosition)
}

fn wire_offset(text: &str, position: &Value) -> Result<usize, LspError> {
    let converted = wire_position(text, position)?;
    let mut offset = 0;
    for line in text.split_inclusive('\n').take(converted.line as usize) {
        offset += line.len();
    }
    Ok(offset + converted.byte_column)
}

pub struct TimedReader {
    receiver: Receiver<std::io::Result<Vec<u8>>>,
    pending: Cursor<Vec<u8>>,
    timeout: Duration,
    eof: bool,
}

impl TimedReader {
    fn spawn(mut stdout: ChildStdout, timeout: Duration) -> Self {
        let (sender, receiver) = mpsc::sync_channel(16);
        std::thread::spawn(move || {
            let mut buffer = [0_u8; 8192];
            loop {
                match stdout.read(&mut buffer) {
                    Ok(0) => {
                        let _ = sender.send(Ok(Vec::new()));
                        break;
                    }
                    Ok(count) => {
                        if sender.send(Ok(buffer[..count].to_vec())).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = sender.send(Err(error));
                        break;
                    }
                }
            }
        });
        Self {
            receiver,
            pending: Cursor::new(Vec::new()),
            timeout,
            eof: false,
        }
    }
}

impl Read for TimedReader {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if buffer.is_empty() || self.eof {
            return Ok(0);
        }
        if self.pending.position() < self.pending.get_ref().len() as u64 {
            return self.pending.read(buffer);
        }
        match self.receiver.recv_timeout(self.timeout) {
            Ok(Ok(bytes)) if bytes.is_empty() => {
                self.eof = true;
                Ok(0)
            }
            Ok(Ok(bytes)) => {
                self.pending = Cursor::new(bytes);
                self.pending.read(buffer)
            }
            Ok(Err(error)) => Err(error),
            Err(RecvTimeoutError::Timeout) => Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "language server response timed out",
            )),
            Err(RecvTimeoutError::Disconnected) => Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "language server output ended",
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub struct LanguageServerConfig {
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub root_uri: Option<String>,
    pub language_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerCapabilities {
    pub hover: bool,
    pub definition: bool,
    pub formatting: bool,
}

pub struct LspProcess {
    child: Child,
    session: LspSession<TimedReader, ChildStdin>,
    capabilities: ServerCapabilities,
}

impl LspProcess {
    pub fn start(
        config: Option<&LanguageServerConfig>,
        document: Document,
    ) -> Result<Self, LspError> {
        let config = config.ok_or(LspError::Unavailable)?;
        let mut child = Command::new(&config.executable)
            .args(&config.arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| LspError::Protocol("server stdout is unavailable".into()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| LspError::Protocol("server stdin is unavailable".into()))?;
        let session = LspSession::new(
            TimedReader::spawn(stdout, Duration::from_secs(30)),
            stdin,
            document,
        );
        let mut process = Self {
            child,
            session,
            capabilities: ServerCapabilities {
                hover: false,
                definition: false,
                formatting: false,
            },
        };
        let result = process
            .session
            .initialise(config.root_uri.as_deref(), &config.language_id)?;
        let capabilities = result
            .get("capabilities")
            .ok_or_else(|| LspError::Protocol("initialise response missing capabilities".into()))?;
        process.capabilities = ServerCapabilities {
            hover: enabled(capabilities.get("hoverProvider")),
            definition: enabled(capabilities.get("definitionProvider")),
            formatting: enabled(capabilities.get("documentFormattingProvider")),
        };
        Ok(process)
    }

    pub fn capabilities(&self) -> ServerCapabilities {
        self.capabilities
    }

    pub fn session_mut(&mut self) -> &mut LspSession<TimedReader, ChildStdin> {
        &mut self.session
    }

    pub fn hover(&mut self, position: TextPosition) -> Result<Option<String>, LspError> {
        if !self.capabilities.hover {
            return Err(LspError::Unavailable);
        }
        self.session.hover(position)
    }

    pub fn definition(&mut self, position: TextPosition) -> Result<Option<LspLocation>, LspError> {
        if !self.capabilities.definition {
            return Err(LspError::Unavailable);
        }
        self.session.definition(position)
    }

    pub fn format(&mut self, request: FormatRequest) -> Result<(), LspError> {
        if !self.capabilities.formatting {
            return Err(LspError::Unavailable);
        }
        self.session.format(request)
    }

    pub fn shutdown(mut self) -> Result<(), LspError> {
        self.session.shutdown()?;
        let deadline = Instant::now() + Duration::from_secs(3);
        while self.child.try_wait()?.is_none() {
            if Instant::now() >= deadline {
                return Err(LspError::Io(
                    "language server did not exit after shutdown".into(),
                ));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(())
    }
}

impl Drop for LspProcess {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn enabled(value: Option<&Value>) -> bool {
    value.is_some_and(|value| value == true || value.is_object())
}
