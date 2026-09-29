#![cfg(unix)]

use chvrn_core::{TextSnapshot, edit::TextBuffer};

use chvrn_integrations::lsp::{Document, FormatRequest, LspSession, TextPosition};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::thread;
use std::time::Duration;

fn document(text: &str) -> Document {
    Document {
        uri: "file:///tmp/project/main.rs".into(),
        text: text.into(),
        version: 4,
        snapshot_id: "snapshot-17".into(),
    }
}

fn session(text: &str) -> (LspSession<UnixStream, UnixStream>, UnixStream) {
    let (client, server) = UnixStream::pair().unwrap();
    for stream in [&client, &server] {
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(3)))
            .unwrap();
    }
    (
        LspSession::new(client.try_clone().unwrap(), client, document(text)),
        server,
    )
}

fn read_message(stream: &mut UnixStream) -> Value {
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        let mut next = [0];
        stream.read_exact(&mut next).unwrap();
        header.push(next[0]);
        assert!(header.len() < 1024);
    }
    let header = String::from_utf8(header).unwrap();
    let length: usize = header
        .split("\r\n")
        .find_map(|line| line.strip_prefix("Content-Length: "))
        .unwrap()
        .parse()
        .unwrap();
    assert!(length <= 1_048_576);
    let mut body = vec![0; length];
    stream.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap()
}

fn send_message(stream: &mut UnixStream, payload: Value) {
    let body = serde_json::to_vec(&payload).unwrap();
    write!(stream, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
    stream.write_all(&body).unwrap();
}

#[test]
fn hover_sends_utf16_coordinates_in_lsp_framing_and_ignores_an_old_response_id() {
    let (mut client, mut server) = session("a😀b\n");
    let peer = thread::spawn(move || {
        let request = read_message(&mut server);
        assert_eq!(request["jsonrpc"], "2.0");
        assert_eq!(request["method"], "textDocument/hover");
        assert_eq!(
            request["params"]["textDocument"]["uri"],
            "file:///tmp/project/main.rs"
        );
        assert_eq!(
            request["params"]["position"],
            json!({"line": 0, "character": 3})
        );
        send_message(
            &mut server,
            json!({"jsonrpc": "2.0", "id": request["id"].as_i64().unwrap() - 1, "result": {"contents": "stale"}}),
        );
        send_message(
            &mut server,
            json!({"jsonrpc": "2.0", "id": request["id"], "result": {"contents": {"kind": "plaintext", "value": "the current hover"}}}),
        );
    });
    assert_eq!(
        client
            .hover(TextPosition {
                line: 0,
                byte_column: 5
            })
            .unwrap()
            .as_deref(),
        Some("the current hover")
    );
    peer.join().unwrap();
}

#[test]
fn cross_file_definition_keeps_target_coordinates_in_utf16_until_the_target_is_loaded() {
    let (mut client, mut server) = session("a😀b\n");
    let peer = thread::spawn(move || {
        let request = read_message(&mut server);
        assert_eq!(request["method"], "textDocument/definition");
        assert_eq!(request["params"]["position"]["character"], 3);
        send_message(
            &mut server,
            json!({
                "jsonrpc": "2.0",
                "id": request["id"],
                "result": {"uri": "file:///tmp/project/other.rs", "range": {"start": {"line": 8, "character": 3}, "end": {"line": 8, "character": 4}}}
            }),
        );
    });
    let location = client
        .definition(TextPosition {
            line: 0,
            byte_column: 5,
        })
        .unwrap()
        .unwrap();
    assert_eq!(location.uri, "file:///tmp/project/other.rs");
    assert_eq!(location.line, 8);
    assert_eq!(location.utf16_column, 3);
    peer.join().unwrap();
}

#[test]
fn did_change_versions_the_document_and_discards_old_diagnostics() {
    let (mut client, mut server) = session("a😀b\n");
    let peer = thread::spawn(move || {
        let notification = read_message(&mut server);
        assert_eq!(notification["method"], "textDocument/didChange");
        assert_eq!(notification["params"]["textDocument"]["version"], 5);
        assert_eq!(
            notification["params"]["contentChanges"][0]["text"],
            "a😀c\n"
        );
        send_message(
            &mut server,
            json!({
                "jsonrpc": "2.0", "method": "textDocument/publishDiagnostics",
                "params": {"uri": "file:///tmp/project/main.rs", "version": 4, "diagnostics": [
                    {"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}}, "message": "obsolete"}
                ]}
            }),
        );
        send_message(
            &mut server,
            json!({
                "jsonrpc": "2.0", "method": "textDocument/publishDiagnostics",
                "params": {"uri": "file:///tmp/project/main.rs", "version": 5, "diagnostics": [
                    {"range": {"start": {"line": 0, "character": 3}, "end": {"line": 0, "character": 4}}, "message": "current problem"}
                ]}
            }),
        );
    });
    client
        .replace_text("a😀c\n".into(), "snapshot-18".into())
        .unwrap();
    assert_eq!(client.document().version, 5);
    client.read_diagnostics().unwrap();
    assert!(client.diagnostics().is_empty());
    client.read_diagnostics().unwrap();
    assert_eq!(client.diagnostics().len(), 1);
    assert_eq!(client.diagnostics()[0].message, "current problem");
    assert_eq!(client.diagnostics()[0].start.byte_column, 5);
    assert_eq!(client.diagnostics()[0].end.byte_column, 6);
    peer.join().unwrap();
}

#[test]
fn formatting_applies_utf16_edits_and_undo_restores_text_with_fresh_snapshot_identity() {
    let original = "let a =  1;\n😀tail\n";
    let (mut client, mut server) = session(original);
    let peer = thread::spawn(move || {
        let request = read_message(&mut server);
        assert_eq!(request["method"], "textDocument/formatting");
        assert_eq!(
            request["params"]["textDocument"]["uri"],
            "file:///tmp/project/main.rs"
        );
        send_message(
            &mut server,
            json!({
                "jsonrpc": "2.0", "id": request["id"], "result": [
                    {"range": {"start": {"line": 0, "character": 7}, "end": {"line": 0, "character": 8}}, "newText": ""},
                    {"range": {"start": {"line": 1, "character": 2}, "end": {"line": 1, "character": 6}}, "newText": "head"}
                ]
            }),
        );
        let formatted = read_message(&mut server);
        assert_eq!(formatted["method"], "textDocument/didChange");
        assert_eq!(
            formatted["params"]["textDocument"]["uri"],
            "file:///tmp/project/main.rs"
        );
        assert_eq!(formatted["params"]["textDocument"]["version"], 5);
        assert_eq!(
            formatted["params"]["contentChanges"][0]["text"],
            "let a = 1;\n😀head\n"
        );
        let undone = read_message(&mut server);
        assert_eq!(undone["method"], "textDocument/didChange");
        assert_eq!(
            undone["params"]["textDocument"]["uri"],
            "file:///tmp/project/main.rs"
        );
        assert_eq!(undone["params"]["textDocument"]["version"], 6);
        assert_eq!(
            undone["params"]["contentChanges"][0]["text"],
            "let a =  1;\n😀tail\n"
        );
    });
    assert!(
        client
            .format(FormatRequest {
                inspected_snapshot_id: "different-snapshot".into(),
                new_snapshot_id: "snapshot-18".into(),
            })
            .is_err()
    );
    assert_eq!(client.document().text, original);
    client
        .format(FormatRequest {
            inspected_snapshot_id: "snapshot-17".into(),
            new_snapshot_id: "snapshot-18".into(),
        })
        .unwrap();
    assert_eq!(client.document().text, "let a = 1;\n😀head\n");
    assert_eq!(client.document().snapshot_id, "snapshot-18");
    assert_eq!(client.document().version, 5);
    client.undo("snapshot-19".into()).unwrap();
    assert_eq!(client.document().text, original);
    assert_eq!(client.document().snapshot_id, "snapshot-19");
    assert_eq!(client.document().version, 6);
    peer.join().unwrap();
}

#[test]
fn invalid_utf8_byte_boundary_cannot_be_sent_as_a_different_lsp_position() {
    let (mut client, _server) = session("a😀b\n");
    assert!(
        client
            .hover(TextPosition {
                line: 0,
                byte_column: 2
            })
            .is_err()
    );
    assert_eq!(client.document().text, "a😀b\n");
}

#[test]
fn formatting_proposal_rejects_edit_then_undo_to_equal_text_before_touching_the_buffer() {
    let original = "let  a\n";
    let (mut client, mut server) = session(original);
    let mut buffer = TextBuffer::new(TextSnapshot::from_bytes(original.as_bytes()).unwrap());
    let inspected = buffer.snapshot();
    client.bind_snapshot(inspected.clone()).unwrap();
    let peer = thread::spawn(move || {
        let request = read_message(&mut server);
        assert_eq!(request["method"], "textDocument/formatting");
        send_message(
            &mut server,
            json!({
                "jsonrpc": "2.0", "id": request["id"], "result": [
                    {"range": {"start": {"line": 0, "character": 4}, "end": {"line": 0, "character": 5}}, "newText": ""}
                ]
            }),
        );
    });
    let proposal = client.format_proposal(&inspected).unwrap();
    assert_eq!(proposal.value.as_deref(), Some("let a\n"));
    buffer.insert("x").unwrap();
    assert!(buffer.undo());
    assert_eq!(buffer.text(), original);
    assert!(!inspected.same_identity(&buffer.snapshot()));
    assert!(matches!(
        proposal.apply_to_buffer(&mut buffer),
        Err(chvrn_integrations::lsp::LspError::StaleSnapshot)
    ));
    assert_eq!(buffer.text(), original);
    assert_eq!(client.document().version, 4);
    peer.join().unwrap();
}

#[test]
fn formatting_proposal_edits_core_buffer_then_versioned_change_and_undo_reach_the_server() {
    let original = "let  a\n";
    let (mut client, mut server) = session(original);
    let mut buffer = TextBuffer::new(TextSnapshot::from_bytes(original.as_bytes()).unwrap());
    let inspected = buffer.snapshot();
    client.bind_snapshot(inspected.clone()).unwrap();
    let peer = thread::spawn(move || {
        let request = read_message(&mut server);
        send_message(
            &mut server,
            json!({
                "jsonrpc": "2.0", "id": request["id"], "result": [
                    {"range": {"start": {"line": 0, "character": 4}, "end": {"line": 0, "character": 5}}, "newText": ""}
                ]
            }),
        );
        let changed = read_message(&mut server);
        assert_eq!(changed["method"], "textDocument/didChange");
        assert_eq!(changed["params"]["textDocument"]["version"], 5);
        assert_eq!(changed["params"]["contentChanges"][0]["text"], "let a\n");
        let undone = read_message(&mut server);
        assert_eq!(undone["params"]["textDocument"]["version"], 6);
        assert_eq!(undone["params"]["contentChanges"][0]["text"], original);
    });
    let proposal = client.format_proposal(&inspected).unwrap();
    let formatted = proposal.apply_to_buffer(&mut buffer).unwrap().unwrap();
    assert!(!formatted.same_identity(&inspected));
    client
        .replace_text(buffer.text(), "snapshot-18".into())
        .unwrap();
    client.bind_snapshot(formatted.clone()).unwrap();
    assert!(buffer.undo());
    let undone = buffer.snapshot();
    assert_eq!(undone.text(), original);
    assert!(!undone.same_identity(&inspected));
    assert!(!undone.same_identity(&formatted));
    client
        .replace_text(buffer.text(), "snapshot-19".into())
        .unwrap();
    client.bind_snapshot(undone).unwrap();
    peer.join().unwrap();
}

#[test]
fn diagnostics_for_an_undone_but_new_snapshot_identity_are_not_shown() {
    let original = "abc\n";
    let (mut client, mut server) = session(original);
    let mut buffer = TextBuffer::new(TextSnapshot::from_bytes(original.as_bytes()).unwrap());
    let inspected = buffer.snapshot();
    client.bind_snapshot(inspected.clone()).unwrap();
    send_message(
        &mut server,
        json!({
            "jsonrpc": "2.0", "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": "file:///tmp/project/main.rs", "version": 4,
                "diagnostics": [{"message": "missing semicolon", "range": {
                    "start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 3}
                }}]
            }
        }),
    );
    client.read_diagnostics().unwrap();
    assert_eq!(
        client.diagnostics_for_snapshot(&inspected).unwrap()[0].message,
        "missing semicolon"
    );
    buffer.insert("x").unwrap();
    assert!(buffer.undo());
    let undone = buffer.snapshot();
    assert_eq!(undone.text(), original);
    assert!(client.diagnostics_for_snapshot(&undone).is_none());
    client.bind_snapshot(undone).unwrap();
    assert!(client.diagnostics().is_empty());
}
