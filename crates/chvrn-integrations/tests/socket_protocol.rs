#![cfg(unix)]

use chvrn_integrations::socket::{InspectedFile, ReviewOutcome, SocketServer};
use serde_json::{Value, json};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const ORIGINAL: &str = "before\n";
const PATCH: &str = "--- a/file.txt\n+++ b/file.txt\n@@ -1 +1 @@\n-before\n+after\n";

fn private_socket_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

fn bind(root: &Path, socket: &Path) -> SocketServer {
    SocketServer::bind(
        socket,
        root,
        vec![InspectedFile {
            relative_path: "file.txt".into(),
            snapshot_id: "snapshot-17".into(),
            bytes: ORIGINAL.as_bytes().to_vec(),
        }],
    )
    .unwrap()
}

fn exchange(server: SocketServer, socket: &Path, request: &[u8]) -> (SocketServer, Value) {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut server = server;
        let result = server.serve_next();
        let _ = sender.send((server, result));
    });
    let mut stream = UnixStream::connect(socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream.write_all(request).unwrap();
    stream.shutdown(std::net::Shutdown::Write).unwrap();
    let mut length = [0; 4];
    stream.read_exact(&mut length).unwrap();
    let response_length = u32::from_be_bytes(length) as usize;
    assert!(response_length <= SocketServer::MAX_FRAME_BYTES);
    let mut response = vec![0; response_length];
    stream.read_exact(&mut response).unwrap();
    let (server, result) = receiver.recv_timeout(Duration::from_secs(3)).unwrap();
    result.unwrap();
    (server, serde_json::from_slice(&response).unwrap())
}

fn framed(value: &Value) -> Vec<u8> {
    let payload = serde_json::to_vec(value).unwrap();
    let mut frame = (payload.len() as u32).to_be_bytes().to_vec();
    frame.extend_from_slice(&payload);
    frame
}

#[test]
fn framed_candidate_is_queued_for_review_without_modifying_or_approving_the_file() {
    let dir = private_socket_dir();
    let file = dir.path().join("file.txt");
    let socket = dir.path().join("chvrn.sock");
    fs::write(&file, ORIGINAL).unwrap();
    let server = bind(dir.path(), &socket);
    assert_eq!(
        fs::metadata(&socket).unwrap().permissions().mode() & 0o777,
        0o600
    );

    let candidate = json!({
        "type": "patch_candidate",
        "snapshot": "snapshot-17",
        "path": "file.txt",
        "patch": PATCH
    });
    let (server, response) = exchange(server, &socket, &framed(&candidate));
    assert_eq!(response["status"], "queued");
    assert_eq!(server.pending_candidates().len(), 1);
    assert_eq!(server.pending_candidates()[0].path, "file.txt");
    assert_eq!(server.pending_candidates()[0].patch, PATCH);
    assert_eq!(fs::read(&file).unwrap(), ORIGINAL.as_bytes());

    let (server, status) = exchange(
        server,
        &socket,
        &framed(&json!({"type": "review_status", "snapshot": "snapshot-17"})),
    );
    assert_eq!(status["status"], "pending");
    assert_eq!(server.pending_candidates().len(), 1);
    assert_eq!(fs::read(&file).unwrap(), ORIGINAL.as_bytes());
}

#[test]
fn oversized_or_malformed_frame_is_rejected_without_queuing_a_candidate() {
    let dir = private_socket_dir();
    let socket = dir.path().join("chvrn.sock");
    fs::write(dir.path().join("file.txt"), ORIGINAL).unwrap();
    let server = bind(dir.path(), &socket);

    let length_only = ((SocketServer::MAX_FRAME_BYTES + 1) as u32).to_be_bytes();
    let (server, response) = exchange(server, &socket, &length_only);
    assert_eq!(response["status"], "rejected");
    assert_eq!(response["reason"], "too_large");

    let mut malformed = (4_u32).to_be_bytes().to_vec();
    malformed.extend_from_slice(b"nope");
    let (server, response) = exchange(server, &socket, &malformed);
    assert_eq!(response["status"], "rejected");
    assert_eq!(response["reason"], "malformed");
    assert!(server.pending_candidates().is_empty());
    assert_eq!(
        fs::read(dir.path().join("file.txt")).unwrap(),
        ORIGINAL.as_bytes()
    );
}

#[test]
fn traversal_and_symlink_escape_are_rejected_before_queuing_or_writing() {
    let dir = private_socket_dir();
    let outside = tempfile::tempdir().unwrap();
    let socket = dir.path().join("chvrn.sock");
    fs::write(dir.path().join("file.txt"), ORIGINAL).unwrap();
    fs::write(outside.path().join("private.txt"), "do not touch\n").unwrap();
    symlink(
        outside.path().join("private.txt"),
        dir.path().join("link.txt"),
    )
    .unwrap();
    let server = bind(dir.path(), &socket);

    let escape = json!({"type": "patch_candidate", "snapshot": "snapshot-17", "path": "../private.txt", "patch": PATCH});
    let (server, traversal) = exchange(server, &socket, &framed(&escape));
    assert_eq!(traversal["reason"], "outside_root");

    let linked = json!({"type": "patch_candidate", "snapshot": "snapshot-17", "path": "link.txt", "patch": PATCH});
    let (server, response) = exchange(server, &socket, &framed(&linked));
    assert_eq!(response["reason"], "outside_root");
    assert!(server.pending_candidates().is_empty());
    assert_eq!(
        fs::read(outside.path().join("private.txt")).unwrap(),
        b"do not touch\n"
    );
}

#[test]
fn changed_disk_content_invalidates_an_inspected_snapshot_without_partial_mutation() {
    let dir = private_socket_dir();
    let file = dir.path().join("file.txt");
    let socket = dir.path().join("chvrn.sock");
    fs::write(&file, ORIGINAL).unwrap();
    let server = bind(dir.path(), &socket);
    fs::write(&file, "someone else changed this\n").unwrap();
    let request = json!({"type": "patch_candidate", "snapshot": "snapshot-17", "path": "file.txt", "patch": PATCH});
    let (server, response) = exchange(server, &socket, &framed(&request));
    assert_eq!(response["status"], "rejected");
    assert_eq!(response["reason"], "stale_snapshot");
    assert!(server.pending_candidates().is_empty());
    assert_eq!(fs::read(&file).unwrap(), b"someone else changed this\n");
}

#[test]
fn socket_bind_rejects_a_public_parent_before_publishing_a_listener() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let shared = dir.path().join("shared");
    fs::create_dir(&shared).unwrap();
    fs::set_permissions(&shared, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(dir.path().join("file.txt"), ORIGINAL).unwrap();
    let socket = shared.join("chvrn.sock");
    let error = SocketServer::bind(
        &socket,
        dir.path(),
        vec![InspectedFile {
            relative_path: "file.txt".into(),
            snapshot_id: "snapshot-17".into(),
            bytes: ORIGINAL.as_bytes().to_vec(),
        }],
    )
    .err()
    .unwrap();
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(!socket.exists());
}

#[test]
fn accepted_receipt_survives_refresh_while_old_snapshot_loses_candidate_authority() {
    let dir = private_socket_dir();
    let socket = dir.path().join("chvrn.sock");
    let file = dir.path().join("file.txt");
    fs::write(&file, ORIGINAL).unwrap();
    let server = bind(dir.path(), &socket);
    let candidate = json!({"type": "patch_candidate", "snapshot": "snapshot-17", "path": "file.txt", "patch": PATCH});
    let (mut server, queued) = exchange(server, &socket, &framed(&candidate));
    assert_eq!(queued["status"], "queued");
    let pending = server.take_pending_candidates();
    assert_eq!(pending[0].path, "file.txt");
    assert!(server.pending_candidates().is_empty());
    server
        .record_review("snapshot-17", ReviewOutcome::Accepted)
        .unwrap();
    assert_eq!(
        server
            .record_review("snapshot-17", ReviewOutcome::Declined)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::AlreadyExists
    );
    fs::write(&file, "after\n").unwrap();
    assert_eq!(
        server
            .refresh_inspected(vec![InspectedFile {
                relative_path: "file.txt".into(),
                snapshot_id: "snapshot-18".into(),
                bytes: b"after\n".to_vec(),
            }])
            .unwrap(),
        0
    );
    let (server, old_status) = exchange(
        server,
        &socket,
        &framed(&json!({"type": "review_status", "snapshot": "snapshot-17"})),
    );
    assert_eq!(old_status["status"], "accepted");
    let (server, new_status) = exchange(
        server,
        &socket,
        &framed(&json!({"type": "review_status", "snapshot": "snapshot-18"})),
    );
    assert_eq!(new_status["status"], "pending");
    let (server, rejected) = exchange(server, &socket, &framed(&candidate));
    assert_eq!(rejected["status"], "rejected");
    assert!(server.pending_candidates().is_empty());
    assert_eq!(fs::read(file).unwrap(), b"after\n");
}

#[test]
fn refreshing_an_unreviewed_candidate_drops_it_without_creating_a_completed_receipt() {
    let dir = private_socket_dir();
    let socket = dir.path().join("chvrn.sock");
    let file = dir.path().join("file.txt");
    fs::write(&file, ORIGINAL).unwrap();
    let server = bind(dir.path(), &socket);
    let candidate = json!({"type": "patch_candidate", "snapshot": "snapshot-17", "path": "file.txt", "patch": PATCH});
    let (mut server, queued) = exchange(server, &socket, &framed(&candidate));
    assert_eq!(queued["status"], "queued");
    fs::write(&file, "after\n").unwrap();
    assert_eq!(
        server
            .refresh_inspected(vec![InspectedFile {
                relative_path: "file.txt".into(),
                snapshot_id: "snapshot-18".into(),
                bytes: b"after\n".to_vec(),
            }])
            .unwrap(),
        1
    );
    assert!(server.take_pending_candidates().is_empty());
    let (_server, old_status) = exchange(
        server,
        &socket,
        &framed(&json!({"type": "review_status", "snapshot": "snapshot-17"})),
    );
    assert_eq!(old_status["reason"], "unknown_snapshot");
}

#[test]
fn inspect_discovers_snapshot_id_and_rejects_a_second_distinct_candidate_for_it() {
    let dir = private_socket_dir();
    let socket = dir.path().join("chvrn.sock");
    fs::write(dir.path().join("file.txt"), ORIGINAL).unwrap();
    let server = bind(dir.path(), &socket);
    let (server, inventory) = exchange(server, &socket, &framed(&json!({"type": "inspect"})));
    assert_eq!(inventory["status"], "inspected");
    assert_eq!(
        inventory["files"],
        json!([{"path": "file.txt", "snapshot": "snapshot-17"}])
    );
    let snapshot = inventory["files"][0]["snapshot"].as_str().unwrap();
    let candidate = json!({"type": "patch_candidate", "snapshot": snapshot, "path": "file.txt", "patch": PATCH});
    let (server, first) = exchange(server, &socket, &framed(&candidate));
    assert_eq!(first["status"], "queued");
    let different = json!({"type": "patch_candidate", "snapshot": snapshot, "path": "file.txt", "patch": "--- a/file.txt\n+++ b/file.txt\n@@ -1 +1 @@\n-before\n+other\n"});
    let (server, second) = exchange(server, &socket, &framed(&different));
    assert_eq!(second["reason"], "already_issued");
    assert_eq!(server.pending_candidates().len(), 1);
    assert_eq!(server.pending_candidates()[0].patch, PATCH);
    assert_eq!(
        fs::read(dir.path().join("file.txt")).unwrap(),
        ORIGINAL.as_bytes()
    );
}

#[test]
fn inspect_returns_a_bounded_rejection_for_an_inventory_larger_than_one_frame() {
    let dir = private_socket_dir();
    let socket = dir.path().join("chvrn.sock");
    let inspected = (0..2_000)
        .map(|index| InspectedFile {
            relative_path: format!("src/file_{index:04}.rs"),
            snapshot_id: format!("snapshot_{index:04}"),
            bytes: Vec::new(),
        })
        .collect();
    let server = SocketServer::bind(&socket, dir.path(), inspected).unwrap();
    let (_server, inventory) = exchange(server, &socket, &framed(&json!({"type": "inspect"})));
    assert_eq!(
        inventory,
        json!({"status": "rejected", "reason": "too_large"})
    );
}

#[test]
fn duplicate_snapshot_ids_for_distinct_files_are_rejected_at_bind() {
    let dir = private_socket_dir();
    let socket = dir.path().join("chvrn.sock");
    let files = ["first.rs", "second.rs"]
        .into_iter()
        .map(|path| InspectedFile {
            relative_path: path.into(),
            snapshot_id: "same-snapshot".into(),
            bytes: Vec::new(),
        })
        .collect();
    let error = SocketServer::bind(&socket, dir.path(), files)
        .err()
        .unwrap();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert!(!socket.exists());
}

#[test]
fn nonblocking_poll_processes_a_candidate_without_a_dedicated_accept_thread() {
    let dir = private_socket_dir();
    let socket = dir.path().join("chvrn.sock");
    fs::write(dir.path().join("file.txt"), ORIGINAL).unwrap();
    let mut server = bind(dir.path(), &socket);
    server.set_nonblocking(true).unwrap();
    assert!(!server.poll().unwrap());
    let mut client = UnixStream::connect(&socket).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    client
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    client
        .write_all(&framed(&json!({
            "type": "patch_candidate", "snapshot": "snapshot-17", "path": "file.txt", "patch": PATCH
        })))
        .unwrap();
    client.shutdown(std::net::Shutdown::Write).unwrap();
    assert!(server.poll().unwrap());
    let mut length = [0; 4];
    client.read_exact(&mut length).unwrap();
    let mut response = vec![0; u32::from_be_bytes(length) as usize];
    client.read_exact(&mut response).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&response).unwrap()["status"],
        "queued"
    );
    assert_eq!(server.take_pending_candidates()[0].patch, PATCH);
    assert!(server.pending_candidates().is_empty());
}
