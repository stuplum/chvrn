#![cfg(unix)]

use chvrn_core::{TextSnapshot, edit::TextBuffer};

use chvrn_integrations::lsp::{Document, FormatRequest, LspSession, TextPosition};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

fn document(text: &str) -> Document {
    Document {
        uri: "file:///tmp/project/main.rs".into(),
        text: text.into(),
        version: 4,
        snapshot_id: "snapshot-17".into(),
    }
}

fn session(
    text: &str,
) -> (
    LspSession<tokio::net::unix::OwnedReadHalf, tokio::net::unix::OwnedWriteHalf>,
    UnixStream,
) {
    let (client, server) = UnixStream::pair().unwrap();
    let (reader, writer) = client.into_split();
    (LspSession::new(reader, writer, document(text)), server)
}

async fn read_message(stream: &mut UnixStream) -> Value {
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        let mut next = [0];
        stream.read_exact(&mut next).await.unwrap();
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
    stream.read_exact(&mut body).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

async fn send_message(stream: &mut UnixStream, payload: Value) {
    let body = serde_json::to_vec(&payload).unwrap();
    stream
        .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
        .await
        .unwrap();
    stream.write_all(&body).await.unwrap();
}

#[tokio::test]
async fn mirror_synchronisation_does_not_create_user_undo_history() {
    let (mut client, mut server) = session("before");
    client
        .replace_text("first".into(), "mirror-one".into())
        .await
        .unwrap();
    assert_eq!(
        read_message(&mut server).await["params"]["contentChanges"][0]["text"],
        "first"
    );
    client
        .replace_text("second".into(), "mirror-two".into())
        .await
        .unwrap();
    assert_eq!(
        read_message(&mut server).await["params"]["contentChanges"][0]["text"],
        "second"
    );
    assert!(client.undo("mirror-undo".into()).await.is_err());
    assert_eq!(client.document().text, "second");
}

#[tokio::test]
async fn hover_uses_cr_lines_without_splitting_unicode_line_separators() {
    let (mut client, mut server) = session("a\u{2028}b\r😀tail\r\n");
    let peer = tokio::spawn(async move {
        let request = read_message(&mut server).await;
        assert_eq!(
            request["params"]["position"],
            json!({"line": 1, "character": 2})
        );
        send_message(
            &mut server,
            json!({"jsonrpc":"2.0","id":request["id"],"result":{"contents":"current CR line"}}),
        )
        .await;
    });
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        client.hover(TextPosition {
            line: 1,
            byte_column: 4,
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result.as_deref(), Some("current CR line"));
    peer.await.unwrap();
}

#[tokio::test]
async fn incoming_diagnostics_preserve_cr_and_eof_coordinates_and_reject_half_surrogates() {
    let (mut client, mut server) = session("first\r😀value\r");
    send_message(
        &mut server,
        json!({
            "jsonrpc":"2.0", "method":"textDocument/publishDiagnostics",
            "params":{"uri":"file:///tmp/project/main.rs","version":4,"diagnostics":[{
                "range":{"start":{"line":1,"character":2},"end":{"line":2,"character":0}},
                "message":"CR through EOF"
            }]}
        }),
    )
    .await;
    client.read_diagnostics().await.unwrap();
    let diagnostic = &client.diagnostics()[0];
    assert_eq!(
        diagnostic.start,
        TextPosition {
            line: 1,
            byte_column: 4
        }
    );
    assert_eq!(
        diagnostic.end,
        TextPosition {
            line: 2,
            byte_column: 0
        }
    );
    send_message(
        &mut server,
        json!({
            "jsonrpc":"2.0", "method":"textDocument/publishDiagnostics",
            "params":{"uri":"file:///tmp/project/main.rs","version":4,"diagnostics":[{
                "range":{"start":{"line":1,"character":1},"end":{"line":1,"character":2}},
                "message":"invalid half surrogate"
            }]}
        }),
    )
    .await;
    assert!(matches!(
        client.read_diagnostics().await,
        Err(chvrn_integrations::lsp::LspError::InvalidPosition)
    ));
    assert_eq!(client.diagnostics()[0].message, "CR through EOF");
}

#[tokio::test]
async fn incoming_format_ranges_resolve_bare_cr_and_terminal_eof_byte_offsets() {
    let (mut client, mut server) = session("first\r😀tail\r");
    let peer = tokio::spawn(async move {
        let request = read_message(&mut server).await;
        send_message(&mut server, json!({"jsonrpc":"2.0","id":request["id"],"result":[
            {"range":{"start":{"line":1,"character":2},"end":{"line":1,"character":6}},"newText":"body"},
            {"range":{"start":{"line":2,"character":0},"end":{"line":2,"character":0}},"newText":"end"}
        ]})).await;
        let changed = read_message(&mut server).await;
        assert_eq!(
            changed["params"]["contentChanges"][0]["text"],
            "first\r😀body\rend"
        );
    });
    client
        .format(FormatRequest {
            inspected_snapshot_id: "snapshot-17".into(),
            new_snapshot_id: "formatted".into(),
        })
        .await
        .unwrap();
    assert_eq!(client.document().text, "first\r😀body\rend");
    peer.await.unwrap();
}
#[tokio::test]
async fn hover_sends_utf16_coordinates_in_lsp_framing_and_ignores_an_old_response_id() {
    let (mut client, mut server) = session("a😀b\n");
    let peer = tokio::spawn(async move {
        let request = read_message(&mut server).await;
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
        send_message(&mut server,
    json!({"jsonrpc": "2.0", "id": request["id"].as_i64().unwrap() - 1, "result": {"contents": "stale"}}),).await;
        send_message(&mut server,
    json!({"jsonrpc": "2.0", "id": request["id"], "result": {"contents": {"kind": "plaintext", "value": "the current hover"}}}),).await;
    });
    assert_eq!(
        client
            .hover(TextPosition {
                line: 0,
                byte_column: 5
            })
            .await
            .unwrap()
            .as_deref(),
        Some("the current hover")
    );
    peer.await.unwrap();
}

#[tokio::test]
async fn cross_file_definition_keeps_target_coordinates_in_utf16_until_the_target_is_loaded() {
    let (mut client, mut server) = session("a😀b\n");
    let peer = tokio::spawn(async move {
        let request = read_message(&mut server).await;
        assert_eq!(request["method"], "textDocument/definition");
        assert_eq!(request["params"]["position"]["character"], 3);
        send_message(&mut server,
    json!({
        "jsonrpc": "2.0",
        "id": request["id"],
        "result": {"uri": "file:///tmp/project/other.rs", "range": {"start": {"line": 8, "character": 3}, "end": {"line": 8, "character": 4}}}
    }),).await;
    });
    let location = client
        .definition(TextPosition {
            line: 0,
            byte_column: 5,
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(location.uri, "file:///tmp/project/other.rs");
    assert_eq!(location.line, 8);
    assert_eq!(location.utf16_column, 3);
    peer.await.unwrap();
}

#[tokio::test]
async fn did_change_versions_the_document_and_discards_old_diagnostics() {
    let (mut client, mut server) = session("a😀b\n");
    let peer = tokio::spawn(async move {
        let notification = read_message(&mut server).await;
        assert_eq!(notification["method"], "textDocument/didChange");
        assert_eq!(notification["params"]["textDocument"]["version"], 5);
        assert_eq!(
            notification["params"]["contentChanges"][0]["text"],
            "a😀c\n"
        );
        send_message(&mut server,
    json!({
        "jsonrpc": "2.0", "method": "textDocument/publishDiagnostics",
        "params": {"uri": "file:///tmp/project/main.rs", "version": 4, "diagnostics": [
            {"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}}, "message": "obsolete"}
        ]}
    }),).await;
        send_message(&mut server,
    json!({
        "jsonrpc": "2.0", "method": "textDocument/publishDiagnostics",
        "params": {"uri": "file:///tmp/project/main.rs", "version": 5, "diagnostics": [
            {"range": {"start": {"line": 0, "character": 3}, "end": {"line": 0, "character": 4}}, "message": "current problem"}
        ]}
    }),).await;
    });
    client
        .replace_text("a😀c\n".into(), "snapshot-18".into())
        .await
        .unwrap();
    assert_eq!(client.document().version, 5);
    client.read_diagnostics().await.unwrap();
    assert!(client.diagnostics().is_empty());
    client.read_diagnostics().await.unwrap();
    assert_eq!(client.diagnostics().len(), 1);
    assert_eq!(client.diagnostics()[0].message, "current problem");
    assert_eq!(client.diagnostics()[0].start.byte_column, 5);
    assert_eq!(client.diagnostics()[0].end.byte_column, 6);
    peer.await.unwrap();
}

#[tokio::test]
async fn formatting_applies_utf16_edits_and_undo_restores_text_with_fresh_snapshot_identity() {
    let original = "let a =  1;\n😀tail\n";
    let (mut client, mut server) = session(original);
    let peer = tokio::spawn(async move {
        let request = read_message(&mut server).await;
        assert_eq!(request["method"], "textDocument/formatting");
        assert_eq!(
            request["params"]["textDocument"]["uri"],
            "file:///tmp/project/main.rs"
        );
        send_message(&mut server,
    json!({
        "jsonrpc": "2.0", "id": request["id"], "result": [
            {"range": {"start": {"line": 0, "character": 7}, "end": {"line": 0, "character": 8}}, "newText": ""},
            {"range": {"start": {"line": 1, "character": 2}, "end": {"line": 1, "character": 6}}, "newText": "head"}
        ]
    }),).await;
        let formatted = read_message(&mut server).await;
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
        let undone = read_message(&mut server).await;
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
            .await
            .is_err()
    );
    assert_eq!(client.document().text, original);
    client
        .format(FormatRequest {
            inspected_snapshot_id: "snapshot-17".into(),
            new_snapshot_id: "snapshot-18".into(),
        })
        .await
        .unwrap();
    assert_eq!(client.document().text, "let a = 1;\n😀head\n");
    assert_eq!(client.document().snapshot_id, "snapshot-18");
    assert_eq!(client.document().version, 5);
    client.undo("snapshot-19".into()).await.unwrap();
    assert_eq!(client.document().text, original);
    assert_eq!(client.document().snapshot_id, "snapshot-19");
    assert_eq!(client.document().version, 6);
    peer.await.unwrap();
}

#[tokio::test]
async fn invalid_utf8_byte_boundary_cannot_be_sent_as_a_different_lsp_position() {
    let (mut client, _server) = session("a😀b\n");
    assert!(
        client
            .hover(TextPosition {
                line: 0,
                byte_column: 2
            })
            .await
            .is_err()
    );
    assert_eq!(client.document().text, "a😀b\n");
}

#[tokio::test]
async fn formatting_proposal_rejects_edit_then_undo_to_equal_text_before_touching_the_buffer() {
    let original = "let  a\n";
    let (mut client, mut server) = session(original);
    let mut buffer = TextBuffer::new(TextSnapshot::from_bytes(original.as_bytes()).unwrap());
    let inspected = buffer.snapshot();
    client.bind_snapshot(inspected.clone()).unwrap();
    let peer = tokio::spawn(async move {
        let request = read_message(&mut server).await;
        assert_eq!(request["method"], "textDocument/formatting");
        send_message(&mut server,
    json!({
        "jsonrpc": "2.0", "id": request["id"], "result": [
            {"range": {"start": {"line": 0, "character": 4}, "end": {"line": 0, "character": 5}}, "newText": ""}
        ]
    }),).await;
    });
    let proposal = client.format_proposal(&inspected).await.unwrap();
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
    peer.await.unwrap();
}

#[tokio::test]
async fn formatting_proposal_edits_core_buffer_then_versioned_change_and_undo_reach_the_server() {
    let original = "let  a\n";
    let (mut client, mut server) = session(original);
    let mut buffer = TextBuffer::new(TextSnapshot::from_bytes(original.as_bytes()).unwrap());
    let inspected = buffer.snapshot();
    client.bind_snapshot(inspected.clone()).unwrap();
    let peer = tokio::spawn(async move {
        let request = read_message(&mut server).await;
        send_message(&mut server,
    json!({
        "jsonrpc": "2.0", "id": request["id"], "result": [
            {"range": {"start": {"line": 0, "character": 4}, "end": {"line": 0, "character": 5}}, "newText": ""}
        ]
    }),).await;
        let changed = read_message(&mut server).await;
        assert_eq!(changed["method"], "textDocument/didChange");
        assert_eq!(changed["params"]["textDocument"]["version"], 5);
        assert_eq!(changed["params"]["contentChanges"][0]["text"], "let a\n");
        let undone = read_message(&mut server).await;
        assert_eq!(undone["params"]["textDocument"]["version"], 6);
        assert_eq!(undone["params"]["contentChanges"][0]["text"], original);
    });
    let proposal = client.format_proposal(&inspected).await.unwrap();
    let formatted = proposal.apply_to_buffer(&mut buffer).unwrap().unwrap();
    assert!(!formatted.same_identity(&inspected));
    client
        .replace_text(buffer.text(), "snapshot-18".into())
        .await
        .unwrap();
    client.bind_snapshot(formatted.clone()).unwrap();
    assert!(buffer.undo());
    let undone = buffer.snapshot();
    assert_eq!(undone.text(), original);
    assert!(!undone.same_identity(&inspected));
    assert!(!undone.same_identity(&formatted));
    client
        .replace_text(buffer.text(), "snapshot-19".into())
        .await
        .unwrap();
    client.bind_snapshot(undone).unwrap();
    peer.await.unwrap();
}

#[tokio::test]
async fn diagnostics_for_an_undone_but_new_snapshot_identity_are_not_shown() {
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
    )
    .await;
    client.read_diagnostics().await.unwrap();
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
