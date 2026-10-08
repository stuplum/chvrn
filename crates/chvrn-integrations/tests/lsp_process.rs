#![cfg(unix)]

use chvrn_integrations::lsp::{Document, LanguageServerConfig, LspProcess, TextPosition};
use chvrn_integrations::process::ProcessSupervisor;
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixListener;
use tokio::time::timeout;

const SERVER: &str = r#"
import json, os, socket, sys, time
mode, path = sys.argv[1:]
control = socket.socket(socket.AF_UNIX)
control.connect(path)
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
control.sendall(('%d\n' % os.getpid()).encode())
if mode == 'invalid':
    send({'jsonrpc':'2.0','id':request['id'],'result':{}})
    time.sleep(60)
else:
    send({'jsonrpc':'2.0','id':request['id'],'result':{'capabilities':{'hoverProvider':True}}})
if mode == 'startup_block': time.sleep(60)
read()
read()
while True:
    request = read()
    if mode == 'notifications':
        while True:
            send({'jsonrpc':'2.0','method':'window/logMessage','params':{'message':'unrelated'}})
            time.sleep(0.02)
    elif mode == 'partial':
        sys.stdout.buffer.write(b'Content-Length: 100\r\n\r\n{')
        sys.stdout.buffer.flush()
        control.sendall(b'partial\n')
        time.sleep(60)
    elif mode == 'graceful' and request.get('method') == 'shutdown':
        send({'jsonrpc':'2.0','id':request['id'],'result':None})
        assert read()['method'] == 'exit'
        control.sendall(b'exit\n')
        assert control.recv(1) == b'y'
        with open(path + '.exit', 'w') as output: output.write('graceful')
        sys.exit(0)
    elif request.get('method') == 'shutdown':
        time.sleep(60)
    else:
        send({'jsonrpc':'2.0','id':request['id'],'result':{'contents':'ready'}})
"#;

fn config(mode: &str, path: &Path) -> LanguageServerConfig {
    LanguageServerConfig {
        executable: "/usr/bin/python3".into(),
        arguments: vec![
            "-u".into(),
            "-c".into(),
            SERVER.into(),
            mode.into(),
            path.to_str().unwrap().into(),
        ],
        root_uri: None,
        language_id: "rust".into(),
    }
}
fn document(text: String) -> Document {
    Document {
        uri: "file:///tmp/process.rs".into(),
        text,
        version: 1,
        snapshot_id: "process-one".into(),
    }
}
async fn handshake(listener: &UnixListener) -> (libc::pid_t, BufReader<tokio::net::UnixStream>) {
    timeout(Duration::from_secs(5), async {
        let (stream, _) = listener.accept().await.unwrap();
        let mut reader = BufReader::new(stream);
        let mut pid = String::new();
        reader.read_line(&mut pid).await.unwrap();
        (pid.trim().parse().unwrap(), reader)
    })
    .await
    .unwrap()
}
fn assert_reaped(pid: libc::pid_t) {
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}

#[tokio::test]
async fn cancellation_during_blocked_startup_reaps_the_registered_child() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let path = dir.path().join("control.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let supervisor = ProcessSupervisor::default();
    let owner = supervisor.clone();
    let config = config("startup_block", &path);
    let startup = tokio::spawn(async move {
        LspProcess::start(Some(&config), document("x".repeat(4 * 1024 * 1024)), &owner).await
    });
    let (pid, _control) = handshake(&listener).await;
    startup.abort();
    assert!(matches!(startup.await, Err(error) if error.is_cancelled()));
    timeout(Duration::from_secs(2), supervisor.shutdown())
        .await
        .unwrap();
    assert_reaped(pid);
}

#[tokio::test]
async fn blocked_startup_write_has_one_deadline_and_reaps_on_failure() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let path = dir.path().join("control.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let supervisor = ProcessSupervisor::default();
    let owner = supervisor.clone();
    let config = config("startup_block", &path);
    let startup = tokio::spawn(async move {
        LspProcess::start(Some(&config), document("x".repeat(4 * 1024 * 1024)), &owner).await
    });
    let (pid, _control) = handshake(&listener).await;
    let error = timeout(Duration::from_secs(35), startup)
        .await
        .unwrap()
        .unwrap()
        .err()
        .unwrap();
    assert!(error.to_string().contains("deadline"));
    timeout(Duration::from_secs(2), supervisor.shutdown())
        .await
        .unwrap();
    assert_reaped(pid);
}

#[tokio::test]
async fn unrelated_notifications_do_not_extend_request_deadline() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let path = dir.path().join("control.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let supervisor = ProcessSupervisor::default();
    let config = config("notifications", &path);
    let mut process = timeout(
        Duration::from_secs(5),
        LspProcess::start(Some(&config), document("x".into()), &supervisor),
    )
    .await
    .unwrap()
    .unwrap();
    let (pid, _control) = handshake(&listener).await;
    let error = timeout(
        Duration::from_secs(35),
        process.hover(TextPosition {
            line: 0,
            byte_column: 0,
        }),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(error.to_string().contains("deadline"));
    let _ = timeout(Duration::from_secs(2), process.shutdown())
        .await
        .unwrap();
    supervisor.shutdown().await;
    assert_reaped(pid);
}

#[tokio::test]
async fn cancelled_partial_response_retires_transport_and_shutdown_reaps() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let path = dir.path().join("control.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let supervisor = ProcessSupervisor::default();
    let config = config("partial", &path);
    let mut process = timeout(
        Duration::from_secs(5),
        LspProcess::start(Some(&config), document("x".into()), &supervisor),
    )
    .await
    .unwrap()
    .unwrap();
    let (pid, mut control) = handshake(&listener).await;
    let mut operation = Box::pin(process.hover(TextPosition {
        line: 0,
        byte_column: 0,
    }));
    let mut marker = String::new();
    tokio::select! {
        result = &mut operation => panic!("partial response unexpectedly completed: {result:?}"),
        result = timeout(Duration::from_secs(5), control.read_line(&mut marker)) => { result.unwrap().unwrap(); }
    }
    assert_eq!(marker, "partial\n");
    drop(operation);
    assert!(
        process
            .hover(TextPosition {
                line: 0,
                byte_column: 0
            })
            .await
            .unwrap_err()
            .to_string()
            .contains("unsynchronised")
    );
    timeout(Duration::from_secs(2), process.shutdown())
        .await
        .unwrap()
        .unwrap();
    supervisor.shutdown().await;
    assert_reaped(pid);
}

#[tokio::test]
async fn constructor_failure_and_unresponsive_graceful_shutdown_reap_children() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let path = dir.path().join("control.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let supervisor = ProcessSupervisor::default();
    let invalid = config("invalid", &path);
    assert!(
        timeout(
            Duration::from_secs(5),
            LspProcess::start(Some(&invalid), document("x".into()), &supervisor)
        )
        .await
        .unwrap()
        .is_err()
    );
    let (pid, _control) = handshake(&listener).await;
    supervisor.shutdown().await;
    assert_reaped(pid);
    let supervisor = ProcessSupervisor::default();
    let ready = config("ready", &path);
    let process = timeout(
        Duration::from_secs(5),
        LspProcess::start(Some(&ready), document("x".into()), &supervisor),
    )
    .await
    .unwrap()
    .unwrap();
    let (pid, _control) = handshake(&listener).await;
    assert!(
        timeout(Duration::from_secs(2), process.shutdown())
            .await
            .unwrap()
            .is_err()
    );
    supervisor.shutdown().await;
    assert_reaped(pid);
}

#[tokio::test]
async fn cooperative_shutdown_can_finish_after_receiving_exit_without_being_killed() {
    use tokio::io::AsyncWriteExt;
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let path = dir.path().join("control.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let supervisor = ProcessSupervisor::default();
    let config = config("graceful", &path);
    let process = timeout(
        Duration::from_secs(5),
        LspProcess::start(Some(&config), document("x".into()), &supervisor),
    )
    .await
    .unwrap()
    .unwrap();
    let (pid, mut control) = handshake(&listener).await;
    let peer = async {
        let mut marker = String::new();
        control.read_line(&mut marker).await.unwrap();
        assert_eq!(marker, "exit\n");
        control.get_mut().write_all(b"y").await.unwrap();
    };
    let (result, ()) = timeout(Duration::from_secs(2), async {
        tokio::join!(process.shutdown(), peer)
    })
    .await
    .unwrap();
    result.unwrap();
    assert_eq!(
        std::fs::read_to_string(path.with_extension("sock.exit")).unwrap(),
        "graceful"
    );
    supervisor.shutdown().await;
    assert_reaped(pid);
}
