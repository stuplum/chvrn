#[path = "../src/jev_ui.rs"]
mod jev_ui;

use chvrn_integrations::jev::{JevClient, JevConfig};
use chvrn_tui::{Pane, ReviewInput, ReviewOutcome, ReviewSession};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use jev_ui::JevUi;
use serde_json::json;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

fn fixture() -> (JevUi, ReviewSession, TcpListener) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let client = JevClient::new(JevConfig {
        api_key: "fixture-secret".into(),
        endpoint: format!("http://{}/v1/systemone", listener.local_addr().unwrap()),
        timeout: Duration::from_secs(5),
    })
    .unwrap();
    let mut session = ReviewSession::three_way("base\n", "ours\n", "theirs\n");
    session.set_merge_advice_enabled(true);
    (JevUi::new(client), session, listener)
}

fn request_event() -> Event {
    Event::Key(KeyEvent::new(KeyCode::Char('J'), KeyModifiers::NONE))
}

fn key(session: &mut ReviewSession, code: KeyCode) -> ReviewOutcome {
    session.handle(ReviewInput::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}

fn accept_request(listener: &TcpListener) -> TcpStream {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "worker did not send the request");
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("accept failed: {error}"),
        }
    };
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut headers = Vec::new();
    while !headers.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        headers.push(byte[0]);
        assert!(headers.len() < 8192);
    }
    let headers = String::from_utf8(headers).unwrap();
    let length: usize = headers
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .unwrap()
        .1
        .trim()
        .parse()
        .unwrap();
    assert!(length <= 24 * 1024);
    let mut body = vec![0; length];
    stream.read_exact(&mut body).unwrap();
    stream
}

fn assert_no_queued_connection(listener: &TcpListener) {
    let deadline = Instant::now() + Duration::from_millis(100);
    while Instant::now() < deadline {
        assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
        thread::sleep(Duration::from_millis(5));
    }
}

fn respond(stream: &mut TcpStream) {
    let body = serde_json::to_vec(&json!({
        "model": "jev-1.13.0",
        "answers": {
            "resolution": {
                "type": "choice",
                "choice": "theirs",
                "probabilities": {"ours": 0.05, "theirs": 0.9, "leave_unresolved": 0.05},
                "confidence": 0.8
            }
        },
        "usage": {"input_tokens": 100, "output_tokens": 30}
    }))
    .unwrap();
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(&body).unwrap();
}

fn consume_reply(ui: &mut JevUi, session: &mut ReviewSession) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !ui.tick(session) {
        assert!(Instant::now() < deadline, "worker reply was not consumed");
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn source_is_sent_only_after_request_and_a_reply_still_requires_application() {
    let (mut ui, mut session, listener) = fixture();
    for _ in 0..3 {
        assert!(!ui.tick(&mut session));
    }
    assert_no_queued_connection(&listener);

    assert!(ui.input(&mut session, &request_event()));
    let mut stream = accept_request(&listener);
    assert_eq!(session.pane_text(Pane::Result), "ours\n");
    assert_eq!(session.unresolved_conflicts(), 1);
    respond(&mut stream);
    drop(stream);
    consume_reply(&mut ui, &mut session);

    assert_eq!(session.pane_text(Pane::Result), "ours\n");
    assert_eq!(session.unresolved_conflicts(), 1);
    assert_eq!(key(&mut session, KeyCode::Enter), ReviewOutcome::Continue);
    assert_eq!(session.pane_text(Pane::Result), "theirs\n");
    assert_eq!(session.unresolved_conflicts(), 0);
    assert!(session.is_confirming_merge());
}

#[test]
fn cancelling_and_repeating_request_does_not_queue_work_or_revive_the_cancelled_reply() {
    let (mut ui, mut session, listener) = fixture();
    assert!(ui.input(&mut session, &request_event()));
    let mut stream = accept_request(&listener);
    session.cancel_merge_advice();
    assert!(ui.input(&mut session, &request_event()));
    assert_no_queued_connection(&listener);

    respond(&mut stream);
    drop(stream);
    consume_reply(&mut ui, &mut session);
    assert_eq!(key(&mut session, KeyCode::Enter), ReviewOutcome::Continue);
    assert_eq!(session.pane_text(Pane::Result), "ours\n");
    assert_eq!(session.unresolved_conflicts(), 1);
    assert_no_queued_connection(&listener);

    assert!(ui.input(&mut session, &request_event()));
    let mut next_stream = accept_request(&listener);
    respond(&mut next_stream);
    drop(next_stream);
    consume_reply(&mut ui, &mut session);
    key(&mut session, KeyCode::Enter);
    assert_eq!(session.pane_text(Pane::Result), "theirs\n");
    assert_eq!(session.unresolved_conflicts(), 0);
}

#[test]
fn quitting_does_not_wait_for_an_unanswered_network_request() {
    let (mut ui, mut session, listener) = fixture();
    assert!(ui.input(&mut session, &request_event()));
    let stream = accept_request(&listener);

    assert_eq!(key(&mut session, KeyCode::Char('q')), ReviewOutcome::Quit);
    let started = Instant::now();
    drop(ui);
    drop(session);
    assert!(started.elapsed() < Duration::from_secs(1));
    drop(stream);
}
