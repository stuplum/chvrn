use crate::integration_runtime::Cancellation;
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
use std::sync::Arc;
use tokio::sync::{
    OwnedSemaphorePermit, Semaphore,
    mpsc::{self, Receiver, Sender, error::TrySendError},
    oneshot,
};
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
    permit: OwnedSemaphorePermit,
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
    _permit: OwnedSemaphorePermit,
}

struct ActiveServer {
    path: PathBuf,
    process: LspProcess,
    snapshot: Option<TextSnapshot>,
}

pub struct LanguageUi {
    requests: Option<Sender<Request>>,
    responses: Receiver<Response>,
    cancellation: Option<Cancellation>,
    admission: Arc<Semaphore>,
    previous: Option<ReviewSession>,
}

impl LanguageUi {
    pub fn new(options: &Options, root: &Path) -> Result<Self> {
        let (requests, mut input) = mpsc::channel::<Request>(1);
        let (output, responses) = mpsc::channel(1);
        let admission = Arc::new(Semaphore::new(2));
        let Some(executable) = options.lsp.clone() else {
            return Ok(Self {
                requests: None,
                responses,
                cancellation: None,
                admission,
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
        let handle = options.runtime.handle()?;
        let processes = handle.processes.clone();
        let preparation = Preparation::new()?;
        let config = LanguageServerConfig {
            executable,
            arguments,
            root_uri: Some(root_uri),
            language_id: "plaintext".into(),
        };
        let cancellation = handle.spawn_owned(move |mut shutdown| async move {
            let mut active: Option<ActiveServer> = None;
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => {},
                _ = async {
                    while let Some(request) = input.recv().await {
                        let snapshot = match preparation.capture(request.capture.clone()).await {
                            Ok(snapshot) => snapshot,
                            Err(_) => break,
                        };
                        let value = perform(&mut active, &config, &request, &snapshot, &processes, &preparation)
                            .await.map_err(|error| error.to_string());
                        if output.send(Response {
                            path: request.path, pane: request.pane, snapshot, value,
                            _permit: request.permit,
                        }).await.is_err() { break; }
                    }
                } => {},
            }
            if let Some(active) = active { let _ = active.process.shutdown().await; }
        })?;
        Ok(Self {
            requests: Some(requests),
            responses,
            cancellation: Some(cancellation),
            admission,
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
            permit: Arc::clone(&self.admission)
                .try_acquire_owned()
                .map_err(|_| "language-server results are awaiting consumption")?,
        };
        match sender.try_send(request) {
            Ok(()) => {
                session.set_message("Language-server request running; editing remains available")
            }
            Err(TrySendError::Full(_)) => {
                return Err("a language-server request is already queued".into());
            }
            Err(TrySendError::Closed(_)) => {
                return Err("language-server worker stopped".into());
            }
        }
        Ok(true)
    }

    pub fn tick(&mut self, session: &mut ReviewSession, path: &Path) -> Result<()> {
        if session.is_review_modal() {
            return Ok(());
        }
        while let Ok(response) = self.responses.try_recv() {
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
                Ok(ResponseValue::Definition(mut view)) => {
                    session.cancel_merge_advice();
                    view.set_theme(std::sync::Arc::clone(session.theme()));
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
        self.cancellation.take();
    }
}

enum PreparationJob {
    Capture(CapturedText, oneshot::Sender<TextSnapshot>),
    Definition(
        PathBuf,
        TextSnapshot,
        LspLocation,
        oneshot::Sender<Result<Box<ReviewSession>>>,
    ),
}

struct Preparation(std::sync::mpsc::SyncSender<PreparationJob>);

impl Preparation {
    fn new() -> Result<Self> {
        let (sender, jobs) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("chvrn-language-preparation".into())
            .spawn(move || {
                while let Ok(job) = jobs.recv() {
                    match job {
                        PreparationJob::Capture(capture, reply) => {
                            let _ = reply.send(capture.snapshot());
                        }
                        PreparationJob::Definition(path, snapshot, location, reply) => {
                            let _ = reply
                                .send(definition_view(&path, &snapshot, location).map(Box::new));
                        }
                    }
                }
            })?;
        Ok(Self(sender))
    }

    async fn capture(&self, capture: CapturedText) -> Result<TextSnapshot> {
        let (reply, response) = oneshot::channel();
        self.0
            .try_send(PreparationJob::Capture(capture, reply))
            .map_err(|_| "language preparation busy or stopped")?;
        response
            .await
            .map_err(|_| "language preparation stopped".into())
    }

    async fn definition(
        &self,
        path: PathBuf,
        snapshot: TextSnapshot,
        location: LspLocation,
    ) -> Result<Box<ReviewSession>> {
        let (reply, response) = oneshot::channel();
        self.0
            .try_send(PreparationJob::Definition(path, snapshot, location, reply))
            .map_err(|_| "language preparation busy or stopped")?;
        response.await.map_err(|_| "language preparation stopped")?
    }
}

async fn perform(
    active: &mut Option<ActiveServer>,
    config: &LanguageServerConfig,
    request: &Request,
    snapshot: &TextSnapshot,
    processes: &chvrn_integrations::process::ProcessSupervisor,
    preparation: &Preparation,
) -> Result<ResponseValue> {
    let result = perform_action(active, config, request, snapshot, processes, preparation).await;
    if active
        .as_ref()
        .is_some_and(|server| server.process.is_desynchronised())
    {
        if let Some(server) = active.take() {
            let _ = server.process.shutdown().await;
        }
    }
    result
}

async fn perform_action(
    active: &mut Option<ActiveServer>,
    config: &LanguageServerConfig,
    request: &Request,
    snapshot: &TextSnapshot,
    processes: &chvrn_integrations::process::ProcessSupervisor,
    preparation: &Preparation,
) -> Result<ResponseValue> {
    let range = chvrn_core::text::line_range(snapshot.text(), request.line)
        .ok_or("cursor line is outside document")?;
    let line = &snapshot.text()[range];
    let byte_column = line
        .grapheme_indices(true)
        .nth(request.grapheme)
        .map_or(line.len(), |(offset, _)| offset);
    let position = TextPosition {
        line: request.line.try_into()?,
        byte_column,
    };
    if active
        .as_ref()
        .is_none_or(|active| active.path != request.path)
    {
        if let Some(previous) = active.take() {
            let _ = previous.process.shutdown().await;
        }
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
        let mut config = config.clone();
        config.language_id = language_id.into();
        let uri = Url::from_file_path(&request.path)
            .map_err(|_| "language-server document path is not absolute")?
            .to_string();
        let document = Document {
            uri,
            text: snapshot.text().into(),
            version: 1,
            snapshot_id: snapshot_id(),
        };
        let process = LspProcess::start(Some(&config), document, processes)
            .await
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
            .await
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
                .await
                .map_err(|error| format!("hover unavailable: {error:?}"))?
                .unwrap_or_else(|| "No hover information at the cursor".into()),
        )),
        Action::Definition => {
            let location = process
                .definition(position)
                .await
                .map_err(|error| format!("definition unavailable: {error:?}"))?
                .ok_or("No definition at the cursor")?;
            Ok(ResponseValue::Definition(
                preparation
                    .definition(request.path.clone(), snapshot.clone(), location)
                    .await?,
            ))
        }
        Action::Format => {
            active.snapshot = None;
            let inspected_snapshot_id = process.session_mut().document().snapshot_id.clone();
            process
                .format(FormatRequest {
                    inspected_snapshot_id,
                    new_snapshot_id: snapshot_id(),
                })
                .await
                .map_err(|error| format!("formatting unavailable: {error:?}"))?;
            Ok(ResponseValue::Formatted(
                process.session_mut().document().text.clone(),
            ))
        }
        Action::Diagnostics => Ok(ResponseValue::Message(
            tokio::time::timeout(
                std::time::Duration::from_secs(30),
                diagnostic_message(process.session_mut(), snapshot),
            )
            .await
            .map_err(|_| "diagnostic operation deadline exceeded")??,
        )),
    }
}

async fn diagnostic_message<R: tokio::io::AsyncRead + Unpin, W: tokio::io::AsyncWrite + Unpin>(
    session: &mut chvrn_integrations::lsp::LspSession<R, W>,
    snapshot: &TextSnapshot,
) -> Result<String> {
    while session.diagnostics_for_snapshot(snapshot).is_none() {
        session
            .read_diagnostics()
            .await
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
    let range = chvrn_core::text::line_range(text, location.line as usize)
        .ok_or("definition line is outside target document")?;
    let line = &text[range];
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
    fn partial_failure_retires_child_and_later_request_gets_a_fresh_gracefully_closed_server() {
        use clap::Parser;
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("language-server");
        let launches = dir.path().join("launches");
        let exited = dir.path().join("exited");
        let script = format!(
            r#"#!/usr/bin/python3
import json, sys
launches = {launches:?}
exited = {exited:?}
try:
    with open(launches, 'rb') as source: first = not source.read()
except FileNotFoundError:
    first = True
with open(launches, 'ab') as target: target.write(b'x')
def read():
    size = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line: raise EOFError()
        if line == b'\r\n': break
        if line.lower().startswith(b'content-length:'): size = int(line.split(b':')[1])
    return json.loads(sys.stdin.buffer.read(size))
def send(value):
    body = json.dumps(value).encode()
    sys.stdout.buffer.write(('Content-Length: %d\r\n\r\n' % len(body)).encode() + body)
    sys.stdout.buffer.flush()
request = read()
send({{'jsonrpc':'2.0','id':request['id'],'result':{{'capabilities':{{'hoverProvider':True}}}}}})
read()
read()
request = read()
if first:
    sys.stdout.buffer.write(b'Content-Length: 100\r\n\r\n{{}}')
    sys.stdout.buffer.flush()
    sys.exit(0)
send({{'jsonrpc':'2.0','id':request['id'],'result':{{'contents':'recovered'}}}})
request = read()
assert request['method'] == 'shutdown'
send({{'jsonrpc':'2.0','id':request['id'],'result':None}})
assert read()['method'] == 'exit'
with open(exited, 'w') as target: target.write('graceful')
"#,
            launches = launches.to_str().unwrap(),
            exited = exited.to_str().unwrap()
        );
        std::fs::write(&binary, script).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        let cli = crate::Cli::try_parse_from(["chvrn", "--lsp", binary.to_str().unwrap()]).unwrap();
        let mut ui = LanguageUi::new(&cli.options, dir.path()).unwrap();
        let buffer = chvrn_core::edit::TextBuffer::new(TextSnapshot::from_bytes(b"value").unwrap());
        let enqueue = |ui: &LanguageUi| {
            ui.requests
                .as_ref()
                .unwrap()
                .try_send(Request {
                    action: Action::Hover,
                    path: dir.path().join("file.rs"),
                    pane: Pane::Right,
                    capture: buffer.capture(),
                    line: 0,
                    grapheme: 0,
                    permit: Arc::clone(&ui.admission).try_acquire_owned().unwrap(),
                })
                .unwrap_or_else(|_| panic!("language admission failed"));
        };
        let response = |ui: &mut LanguageUi| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                if let Ok(response) = ui.responses.try_recv() {
                    break response;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "language response deadline exceeded"
                );
                std::thread::yield_now();
            }
        };
        enqueue(&ui);
        let failed = response(&mut ui);
        assert!(matches!(&failed.value, Err(error) if error.contains("early eof")));
        drop(failed);
        enqueue(&ui);
        let recovered = response(&mut ui);
        assert!(
            matches!(&recovered.value, Ok(ResponseValue::Message(text)) if text == "recovered")
        );
        assert_eq!(std::fs::read(launches).unwrap(), b"xx");
        drop(recovered);
        drop(ui);
        cli.options.runtime.shutdown();
        assert_eq!(std::fs::read_to_string(exited).unwrap(), "graceful");
    }

    #[tokio::test]
    async fn repeated_diagnostics_reuse_the_current_snapshot_but_equal_new_text_invalidates_them() {
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
        let frame = format!("Content-Length: {}\r\n\r\n{payload}", payload.len()).into_bytes();
        let reader = frame.as_slice();
        let document = Document {
            uri: "file:///diagnostics.cpp".into(),
            text: snapshot.text().into(),
            version: 1,
            snapshot_id: "first".into(),
        };
        let mut session =
            chvrn_integrations::lsp::LspSession::new(reader, tokio::io::sink(), document);
        session.bind_snapshot(snapshot.clone()).unwrap();
        assert_eq!(
            diagnostic_message(&mut session, &snapshot).await.unwrap(),
            "1:1 undeclared identifier"
        );
        assert_eq!(
            diagnostic_message(&mut session, &snapshot).await.unwrap(),
            "1:1 undeclared identifier"
        );
        let fresh = TextSnapshot::from_bytes(snapshot.as_bytes()).unwrap();
        session
            .replace_text(fresh.text().into(), "second".into())
            .await
            .unwrap();
        session.bind_snapshot(fresh.clone()).unwrap();
        assert!(diagnostic_message(&mut session, &fresh).await.is_err());
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

    #[test]
    fn incoming_definition_positions_use_cr_and_terminal_empty_lines_but_reject_half_surrogates() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("result.ts");
        let snapshot = TextSnapshot::from_bytes("first\r😀value\r".as_bytes()).unwrap();
        let uri = Url::from_file_path(&path).unwrap().to_string();
        let view = definition_view(
            &path,
            &snapshot,
            LspLocation {
                uri: uri.clone(),
                line: 1,
                utf16_column: 2,
            },
        )
        .unwrap();
        assert_eq!((view.cursor().line, view.cursor().grapheme), (1, 1));
        let eof = definition_view(
            &path,
            &snapshot,
            LspLocation {
                uri: uri.clone(),
                line: 2,
                utf16_column: 0,
            },
        )
        .unwrap();
        assert_eq!((eof.cursor().line, eof.cursor().grapheme), (2, 0));
        assert!(
            definition_view(
                &path,
                &snapshot,
                LspLocation {
                    uri,
                    line: 1,
                    utf16_column: 1
                }
            )
            .is_err()
        );
    }
}
