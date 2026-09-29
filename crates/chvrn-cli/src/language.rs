use crate::repository::snapshot_id;
use crate::{Options, Result};
use chvrn_core::TextSnapshot;
use chvrn_core::edit::CapturedText;
use chvrn_integrations::lsp::{
    Document, FormatRequest, LanguageServerConfig, LspLocation, LspProcess, TextPosition,
};
use chvrn_tui::{Pane, ReviewSession};
use crossterm::event::{Event, KeyCode, KeyEventKind};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use unicode_segmentation::UnicodeSegmentation;
use url::Url;

#[derive(Clone, Copy)]
enum Action {
    Hover,
    Definition,
    Format,
    Diagnostics,
}

struct Request {
    action: Action,
    path: PathBuf,
    pane: Pane,
    capture: CapturedText,
    line: usize,
    grapheme: usize,
}

enum ResponseValue {
    Message(String),
    Definition(Box<ReviewSession>),
    Formatted(String),
}

struct Response {
    path: PathBuf,
    pane: Pane,
    snapshot: TextSnapshot,
    value: std::result::Result<ResponseValue, String>,
}

struct ActiveServer {
    path: PathBuf,
    process: LspProcess,
    snapshot: Option<TextSnapshot>,
}

pub struct LanguageUi {
    requests: Option<SyncSender<Request>>,
    responses: Receiver<Response>,
    worker: Option<std::thread::JoinHandle<()>>,
    previous: Option<ReviewSession>,
}

impl LanguageUi {
    pub fn new(options: &Options, root: &Path) -> Result<Self> {
        let (requests, input) = mpsc::sync_channel::<Request>(1);
        let (output, responses) = mpsc::channel();
        let Some(executable) = options.lsp.clone() else {
            return Ok(Self {
                requests: None,
                responses,
                worker: None,
                previous: None,
            });
        };
        let arguments = options
            .lsp_args
            .iter()
            .map(|arg| {
                arg.clone()
                    .into_string()
                    .map_err(|_| "language-server arguments must be UTF-8")
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let root_uri = Url::from_directory_path(root)
            .map_err(|_| "language-server root is not an absolute directory")?
            .to_string();
        let worker = std::thread::spawn(move || {
            let mut active: Option<ActiveServer> = None;
            while let Ok(request) = input.recv() {
                let snapshot = request.capture.clone().snapshot();
                let value = perform(
                    &mut active,
                    &executable,
                    &arguments,
                    &root_uri,
                    &request,
                    &snapshot,
                )
                .map_err(|error| error.to_string());
                if output
                    .send(Response {
                        path: request.path,
                        pane: request.pane,
                        snapshot,
                        value,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Ok(Self {
            requests: Some(requests),
            responses,
            worker: Some(worker),
            previous: None,
        })
    }

    pub fn input(
        &mut self,
        session: &mut ReviewSession,
        path: &Path,
        event: &Event,
    ) -> Result<bool> {
        if self.previous.is_some() {
            if let Event::Key(key) = event {
                if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
                    *session = self
                        .previous
                        .take()
                        .ok_or("definition view lost its parent")?;
                    return Ok(true);
                }
                return Ok(!matches!(
                    key.code,
                    KeyCode::Up
                        | KeyCode::Down
                        | KeyCode::Left
                        | KeyCode::Right
                        | KeyCode::PageUp
                        | KeyCode::PageDown
                        | KeyCode::Home
                        | KeyCode::End
                        | KeyCode::Tab
                        | KeyCode::Char('j' | 'k' | 'h' | 'l' | '?' | '[' | ']')
                ));
            }
            return Ok(false);
        }
        let Event::Key(key) = event else {
            return Ok(false);
        };
        if key.kind != KeyEventKind::Press || session.is_editing() {
            return Ok(false);
        }
        let action = match key.code {
            KeyCode::Char('K') => Action::Hover,
            KeyCode::Char('g') => Action::Definition,
            KeyCode::Char('F') => Action::Format,
            KeyCode::Char('D') => Action::Diagnostics,
            _ => return Ok(false),
        };
        let sender = self.requests.as_ref().ok_or("language features unavailable: configure --lsp EXECUTABLE and optional --lsp-arg VALUE")?;
        let pane = session.focus();
        if matches!(action, Action::Format) && session.is_read_only(pane) {
            return Err("formatting is unavailable on a read-only pane".into());
        }
        let cursor = session.cursor();
        let request = Request {
            action,
            path: path.to_path_buf(),
            pane,
            capture: session.pane_capture(pane),
            line: cursor.line,
            grapheme: cursor.grapheme,
        };
        match sender.try_send(request) {
            Ok(()) => {
                session.set_message("Language-server request running; editing remains available")
            }
            Err(TrySendError::Full(_)) => {
                return Err("a language-server request is already queued".into());
            }
            Err(TrySendError::Disconnected(_)) => {
                return Err("language-server worker stopped".into());
            }
        }
        Ok(true)
    }

    pub fn tick(&mut self, session: &mut ReviewSession, path: &Path) -> Result<()> {
        for response in self.responses.try_iter() {
            if self.previous.is_some()
                || response.path != path
                || !session.pane_matches_snapshot(response.pane, &response.snapshot)
            {
                session
                    .set_message("Discarded a language-server result for an older text snapshot");
                continue;
            }
            match response.value {
                Err(error) => session.set_message(error),
                Ok(ResponseValue::Message(message)) => session.set_message(message),
                Ok(ResponseValue::Formatted(text)) => {
                    session
                        .replace_pane_text(response.pane, &text)
                        .map_err(|error| format!("formatting was not applied: {error:?}"))?;
                    session
                        .set_message("Formatting applied as one undoable edit; s saves, u undoes");
                }
                Ok(ResponseValue::Definition(view)) => {
                    self.previous = Some(std::mem::replace(session, *view));
                }
            }
        }
        Ok(())
    }

    pub fn viewing_definition(&self) -> bool {
        self.previous.is_some()
    }
}

impl Drop for LanguageUi {
    fn drop(&mut self) {
        self.requests.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn perform(
    active: &mut Option<ActiveServer>,
    executable: &Path,
    arguments: &[String],
    root_uri: &str,
    request: &Request,
    snapshot: &TextSnapshot,
) -> Result<ResponseValue> {
    let line = snapshot
        .text()
        .split('\n')
        .nth(request.line)
        .unwrap_or_default();
    let byte_column = line
        .grapheme_indices(true)
        .nth(request.grapheme)
        .map_or(line.trim_end_matches('\r').len(), |(offset, _)| offset);
    let position = TextPosition {
        line: request.line.try_into()?,
        byte_column,
    };
    if active
        .as_ref()
        .is_none_or(|active| active.path != request.path)
    {
        let language_id = match request.path.extension().and_then(|value| value.to_str()) {
            Some("rs") => "rust",
            Some("ts") => "typescript",
            Some("tsx") => "typescriptreact",
            Some("js") => "javascript",
            Some("jsx") => "javascriptreact",
            Some("py") => "python",
            Some("json") => "json",
            _ => "plaintext",
        };
        let config = LanguageServerConfig {
            executable: executable.to_path_buf(),
            arguments: arguments.to_vec(),
            root_uri: Some(root_uri.into()),
            language_id: language_id.into(),
        };
        let uri = Url::from_file_path(&request.path)
            .map_err(|_| "language-server document path is not absolute")?
            .to_string();
        let document = Document {
            uri,
            text: snapshot.text().into(),
            version: 1,
            snapshot_id: snapshot_id(),
        };
        let process = LspProcess::start(Some(&config), document)
            .map_err(|error| format!("language-server startup failed: {error:?}"))?;
        *active = Some(ActiveServer {
            path: request.path.clone(),
            process,
            snapshot: Some(snapshot.clone()),
        });
    }
    let active = active
        .as_mut()
        .ok_or("language-server startup returned no process")?;
    let process = &mut active.process;
    if active
        .snapshot
        .as_ref()
        .is_none_or(|previous| !previous.same_identity(snapshot))
    {
        process
            .session_mut()
            .replace_text(snapshot.text().into(), snapshot_id())
            .map_err(|error| format!("language-server sync failed: {error:?}"))?;
        active.snapshot = Some(snapshot.clone());
    }
    process
        .session_mut()
        .bind_snapshot(snapshot.clone())
        .map_err(|error| format!("language-server snapshot binding failed: {error:?}"))?;
    match request.action {
        Action::Hover => Ok(ResponseValue::Message(
            process
                .hover(position)
                .map_err(|error| format!("hover unavailable: {error:?}"))?
                .unwrap_or_else(|| "No hover information at the cursor".into()),
        )),
        Action::Definition => {
            let location = process
                .definition(position)
                .map_err(|error| format!("definition unavailable: {error:?}"))?
                .ok_or("No definition at the cursor")?;
            Ok(ResponseValue::Definition(Box::new(definition_view(
                &request.path,
                snapshot,
                location,
            )?)))
        }
        Action::Format => {
            active.snapshot = None;
            let inspected_snapshot_id = process.session_mut().document().snapshot_id.clone();
            process
                .format(FormatRequest {
                    inspected_snapshot_id,
                    new_snapshot_id: snapshot_id(),
                })
                .map_err(|error| format!("formatting unavailable: {error:?}"))?;
            Ok(ResponseValue::Formatted(
                process.session_mut().document().text.clone(),
            ))
        }
        Action::Diagnostics => Ok(ResponseValue::Message(diagnostic_message(
            process.session_mut(),
            snapshot,
        )?)),
    }
}

fn diagnostic_message<R: std::io::Read, W: std::io::Write>(
    session: &mut chvrn_integrations::lsp::LspSession<R, W>,
    snapshot: &TextSnapshot,
) -> Result<String> {
    while session.diagnostics_for_snapshot(snapshot).is_none() {
        session
            .read_diagnostics()
            .map_err(|error| format!("diagnostic update unavailable: {error:?}"))?;
    }
    let diagnostics = session
        .diagnostics_for_snapshot(snapshot)
        .ok_or("diagnostic snapshot changed")?;
    Ok(if diagnostics.is_empty() {
        "No current diagnostics reported by the server".into()
    } else {
        diagnostics
            .iter()
            .map(|diagnostic| {
                format!(
                    "{}:{} {}",
                    diagnostic.start.line + 1,
                    diagnostic.start.byte_column + 1,
                    diagnostic.message
                )
            })
            .collect::<Vec<_>>()
            .join(" | ")
    })
}

fn definition_view(
    path: &Path,
    snapshot: &TextSnapshot,
    location: LspLocation,
) -> Result<ReviewSession> {
    let target = Url::parse(&location.uri)?
        .to_file_path()
        .map_err(|_| "definition is not a local file URI")?;
    let file = if target == path {
        None
    } else {
        Some(crate::files::GuardedFile::read(&target)?)
    };
    let text = match &file {
        Some(file) => std::str::from_utf8(file.bytes())?,
        None => snapshot.text(),
    };
    let line = text
        .split('\n')
        .nth(location.line as usize)
        .ok_or("definition line is outside target document")?;
    let mut units = 0;
    let mut byte_column = 0;
    for (byte, character) in line.char_indices() {
        if units == location.utf16_column {
            byte_column = byte;
            break;
        }
        units += character.len_utf16() as u32;
        byte_column = byte + character.len_utf8();
    }
    if units != location.utf16_column {
        return Err("definition column is outside a Unicode boundary".into());
    }
    let grapheme = line[..byte_column].graphemes(true).count();
    let mut view = ReviewSession::two_way(text, text);
    view.set_paths(&target, &target);
    view.set_read_only(Pane::Left, true);
    view.set_read_only(Pane::Right, true);
    view.go_to(Pane::Right, location.line as usize, grapheme);
    view.set_message(format!(
        "Definition: {}:{} | Esc/q returns to the unchanged review",
        target.display(),
        location.line + 1
    ));
    Ok(view)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_diagnostics_reuse_the_current_snapshot_but_equal_new_text_invalidates_them() {
        let snapshot = TextSnapshot::from_bytes(b"unknown\n").unwrap();
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": "file:///diagnostics.cpp",
                "version": 1,
                "diagnostics": [{
                    "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 7}},
                    "message": "undeclared identifier"
                }]
            }
        }).to_string();
        let reader = std::io::Cursor::new(
            format!("Content-Length: {}\r\n\r\n{payload}", payload.len()).into_bytes(),
        );
        let document = Document {
            uri: "file:///diagnostics.cpp".into(),
            text: snapshot.text().into(),
            version: 1,
            snapshot_id: "first".into(),
        };
        let mut session = chvrn_integrations::lsp::LspSession::new(reader, Vec::new(), document);
        session.bind_snapshot(snapshot.clone()).unwrap();
        assert_eq!(
            diagnostic_message(&mut session, &snapshot).unwrap(),
            "1:1 undeclared identifier"
        );
        assert_eq!(
            diagnostic_message(&mut session, &snapshot).unwrap(),
            "1:1 undeclared identifier"
        );
        let fresh = TextSnapshot::from_bytes(snapshot.as_bytes()).unwrap();
        session
            .replace_text(fresh.text().into(), "second".into())
            .unwrap();
        session.bind_snapshot(fresh.clone()).unwrap();
        assert!(diagnostic_message(&mut session, &fresh).is_err());
    }

    #[test]
    fn same_document_definition_uses_unsaved_text_even_before_the_output_file_exists() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("result.ts");
        let snapshot = TextSnapshot::from_bytes("first\n😀value\n".as_bytes()).unwrap();
        let location = LspLocation {
            uri: Url::from_file_path(&path).unwrap().to_string(),
            line: 1,
            utf16_column: 2,
        };
        let view = definition_view(&path, &snapshot, location).unwrap();
        assert_eq!(view.pane_text(Pane::Right), snapshot.text());
        assert_eq!(view.cursor().line, 1);
        assert_eq!(view.cursor().grapheme, 1);
        assert!(view.is_read_only(Pane::Left));
        assert!(view.is_read_only(Pane::Right));
        assert!(!path.exists());
    }

    #[test]
    fn cross_file_definition_loads_the_target_instead_of_the_request_document() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.ts");
        let target = directory.path().join("target.ts");
        std::fs::write(&target, "target\nsymbol\n").unwrap();
        let snapshot = TextSnapshot::from_bytes(b"unsaved source\n").unwrap();
        let location = LspLocation {
            uri: Url::from_file_path(&target).unwrap().to_string(),
            line: 1,
            utf16_column: 0,
        };
        let view = definition_view(&path, &snapshot, location).unwrap();
        assert_eq!(view.pane_text(Pane::Right), "target\nsymbol\n");
        assert_eq!(view.cursor().line, 1);
        assert_eq!(snapshot.text(), "unsaved source\n");
    }
}
