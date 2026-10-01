use chvrn_core::{
    TextSnapshot,
    merge_advice::{MergeAdviceChoice, MergeAdviceInput, MergeAdviceSource},
};
use chvrn_integrations::jev::{JevClient, JevConfig, JevError};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::ops::Range;
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const DUMMY_KEY: &str = "jev-protocol-test-key-not-a-secret";
const FIXTURE_LIMIT: Duration = Duration::from_secs(5);

struct Request {
    method: String,
    target: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

impl Request {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }
}

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    chunked: bool,
    stall: bool,
    disconnect: bool,
}

impl Reply {
    fn json(body: Value) -> Self {
        Self {
            status: 200,
            headers: Vec::new(),
            body: serde_json::to_vec(&body).unwrap(),
            chunked: false,
            stall: false,
            disconnect: false,
        }
    }

    fn bytes(&self) -> Vec<u8> {
        let mut bytes = format!(
            "HTTP/1.1 {} Fixture\r\nContent-Type: application/json\r\nConnection: close\r\n",
            self.status
        )
        .into_bytes();
        for (name, value) in &self.headers {
            write!(bytes, "{name}: {value}\r\n").unwrap();
        }
        if self.chunked {
            bytes.extend_from_slice(b"Transfer-Encoding: chunked\r\n\r\n");
            for chunk in self.body.chunks(4096) {
                write!(bytes, "{:x}\r\n", chunk.len()).unwrap();
                bytes.extend_from_slice(chunk);
                bytes.extend_from_slice(b"\r\n");
            }
            bytes.extend_from_slice(b"0\r\n\r\n");
        } else {
            write!(bytes, "Content-Length: {}\r\n\r\n", self.body.len()).unwrap();
            bytes.extend_from_slice(&self.body);
        }
        bytes
    }
}

struct Fixture {
    endpoint: String,
    stop: Sender<()>,
    peer: Option<JoinHandle<Vec<Request>>>,
}

impl Fixture {
    fn new(reply: Reply) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/v1/systemone", listener.local_addr().unwrap());
        let (stop, receiver) = mpsc::channel();
        let peer = thread::spawn(move || {
            let deadline = Instant::now() + FIXTURE_LIMIT;
            let response = reply.bytes();
            let mut requests = Vec::new();
            let mut held_connections = Vec::new();
            loop {
                assert!(Instant::now() < deadline, "fixture accept deadline elapsed");
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        stream
                            .set_read_timeout(Some(Duration::from_millis(500)))
                            .unwrap();
                        stream
                            .set_write_timeout(Some(Duration::from_millis(500)))
                            .unwrap();
                        requests.push(read_request(&mut stream, deadline));
                        if reply.stall {
                            held_connections.push(stream);
                            continue;
                        }
                        if !reply.disconnect {
                            if let Err(error) = stream.write_all(&response) {
                                assert!(matches!(
                                    error.kind(),
                                    ErrorKind::BrokenPipe | ErrorKind::ConnectionReset
                                ));
                            }
                        }
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        if receiver.try_recv().is_ok() {
                            return requests;
                        }
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("fixture accept failed: {error}"),
                }
            }
        });
        Self {
            endpoint,
            stop,
            peer: Some(peer),
        }
    }

    fn client(&self) -> JevClient {
        client(&self.endpoint, Duration::from_secs(2))
    }

    fn finish(mut self) -> Vec<Request> {
        let _ = self.stop.send(());
        let deadline = Instant::now() + FIXTURE_LIMIT;
        while !self.peer.as_ref().unwrap().is_finished() {
            assert!(Instant::now() < deadline, "fixture join deadline elapsed");
            thread::sleep(Duration::from_millis(2));
        }
        self.peer.take().unwrap().join().unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(peer) = self.peer.take() {
            let deadline = Instant::now() + FIXTURE_LIMIT;
            while !peer.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(2));
            }
            if peer.is_finished() {
                let _ = peer.join();
            }
        }
    }
}

fn read_request(stream: &mut TcpStream, deadline: Instant) -> Request {
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        assert!(Instant::now() < deadline, "fixture header deadline elapsed");
        assert!(header.len() < 8192);
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        header.push(byte[0]);
    }
    let header = String::from_utf8(header).unwrap();
    let mut lines = header.split("\r\n");
    let mut start = lines.next().unwrap().split_whitespace();
    let method = start.next().unwrap().to_owned();
    let target = start.next().unwrap().to_owned();
    let headers: BTreeMap<String, String> = lines
        .filter(|line| !line.is_empty())
        .map(|line| {
            let (name, value) = line.split_once(':').unwrap();
            (name.to_ascii_lowercase(), value.trim().to_owned())
        })
        .collect();
    let body = if headers
        .get("transfer-encoding")
        .is_some_and(|value| value.eq_ignore_ascii_case("chunked"))
    {
        let mut body = Vec::new();
        loop {
            let line = read_line(stream, deadline);
            let length = usize::from_str_radix(line.split(';').next().unwrap(), 16).unwrap();
            if length == 0 {
                while !read_line(stream, deadline).is_empty() {}
                break;
            }
            assert!(length <= 128 * 1024 - body.len());
            body.extend(read_body(stream, length, deadline));
            assert_eq!(read_line(stream, deadline), "");
        }
        body
    } else {
        let length = headers["content-length"].parse().unwrap();
        read_body(stream, length, deadline)
    };
    Request {
        method,
        target,
        headers,
        body,
    }
}

fn read_line(stream: &mut TcpStream, deadline: Instant) -> String {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n") {
        assert!(
            Instant::now() < deadline,
            "fixture framing deadline elapsed"
        );
        assert!(bytes.len() < 8192);
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        bytes.push(byte[0]);
    }
    bytes.truncate(bytes.len() - 2);
    String::from_utf8(bytes).unwrap()
}

fn read_body(stream: &mut TcpStream, length: usize, deadline: Instant) -> Vec<u8> {
    assert!(length <= 128 * 1024);
    let mut body = vec![0; length];
    let mut read = 0;
    while read < length {
        assert!(Instant::now() < deadline, "fixture body deadline elapsed");
        let count = stream.read(&mut body[read..]).unwrap();
        assert_ne!(count, 0);
        read += count;
    }
    body
}

fn client(endpoint: &str, timeout: Duration) -> JevClient {
    JevClient::new(JevConfig {
        api_key: DUMMY_KEY.into(),
        endpoint: endpoint.into(),
        timeout,
    })
    .unwrap()
}

fn source(text: &str, lines: Range<usize>) -> MergeAdviceSource {
    MergeAdviceSource {
        snapshot: TextSnapshot::from_bytes(text.as_bytes()).unwrap(),
        lines,
    }
}

fn input() -> MergeAdviceInput {
    MergeAdviceInput {
        base: source("base\n", 0..1),
        ours: source("ours\n", 0..1),
        theirs: source("theirs\n", 0..1),
    }
}

fn answer() -> Value {
    json!({
        "model": "jev-1.13.0",
        "answers": {
            "resolution": {
                "type": "choice",
                "choice": "theirs",
                "probabilities": {"ours": 0.1, "theirs": 0.8, "leave_unresolved": 0.1},
                "confidence": 0.64
            }
        },
        "usage": {"input_tokens": 318, "output_tokens": 34}
    })
}

fn exchange(reply: Reply) -> Result<chvrn_core::merge_advice::MergeAdviceSuggestion, JevError> {
    let fixture = Fixture::new(reply);
    let result = fixture.client().suggest(&input());
    let requests = fixture.finish();
    assert_eq!(requests.len(), 1);
    result
}

fn assert_no_connection(listener: &TcpListener) {
    match listener.accept() {
        Err(error) => assert_eq!(error.kind(), ErrorKind::WouldBlock),
        Ok(_) => panic!("rejected input opened a connection"),
    }
}

fn unused_listener() -> (TcpListener, JevClient) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}/v1/systemone", listener.local_addr().unwrap());
    let client = client(&endpoint, Duration::from_millis(100));
    (listener, client)
}

#[test]
fn posts_authenticated_choice_question_and_retains_returned_model_and_confidence() {
    let mut response = answer();
    response["model"] = json!("jev-returned-version");
    let fixture = Fixture::new(Reply::json(response));
    let result = fixture.client().suggest(&input()).unwrap();
    let requests = fixture.finish();
    assert_eq!(result.choice, MergeAdviceChoice::Theirs);
    assert_eq!(result.confidence, 0.64);
    assert_eq!(result.model, "jev-returned-version");
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.method, "POST");
    assert_eq!(request.target, "/v1/systemone");
    assert_eq!(
        request.headers["authorization"],
        format!("Bearer {DUMMY_KEY}")
    );
    assert_eq!(
        request.headers["content-type"].split(';').next().unwrap(),
        "application/json"
    );
    let body = request.json();
    assert_eq!(body["model"], "jev-1.13.0");
    let questions = body["questions"].as_object().unwrap();
    assert_eq!(
        questions.keys().map(String::as_str).collect::<Vec<_>>(),
        ["resolution"]
    );
    let question = &questions["resolution"];
    assert_eq!(question["type"], "choice");
    assert!(question["instructions"].is_string());
    let mut options: Vec<_> = question["criteria"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    options.sort_unstable();
    assert_eq!(options, ["leave_unresolved", "ours", "theirs"]);
    assert_eq!(
        body["state"],
        json!({
            "base": {"before": "", "conflict": "base\n", "after": ""},
            "ours": {"before": "", "conflict": "ours\n", "after": ""},
            "theirs": {"before": "", "conflict": "theirs\n", "after": ""}
        })
    );
    assert!(!String::from_utf8_lossy(&request.body).contains(DUMMY_KEY));
}

#[test]
fn only_twenty_surrounding_lines_are_disclosed_without_truncating_long_conflicts() {
    let mut sources = Vec::new();
    let mut expected = serde_json::Map::new();
    for (side, conflict_lines) in [("base", 25), ("ours", 31), ("theirs", 27)] {
        let before = (0..20)
            .map(|n| format!("{side}-before-{n}\n"))
            .collect::<String>();
        let conflict = (0..conflict_lines)
            .map(|n| format!("{side}-conflict-{n}\n"))
            .collect::<String>();
        let after = (0..20)
            .map(|n| format!("{side}-after-{n}\n"))
            .collect::<String>();
        let text =
            format!("{side}-PRIVATE-BEFORE\n{before}{conflict}{after}{side}-PRIVATE-AFTER\n");
        sources.push(source(&text, 21..21 + conflict_lines));
        expected.insert(
            side.into(),
            json!({"before": before, "conflict": conflict, "after": after}),
        );
    }
    let mut sources = sources.into_iter();
    let input = MergeAdviceInput {
        base: sources.next().unwrap(),
        ours: sources.next().unwrap(),
        theirs: sources.next().unwrap(),
    };
    let fixture = Fixture::new(Reply::json(answer()));
    fixture.client().suggest(&input).unwrap();
    let requests = fixture.finish();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].json()["state"], Value::Object(expected));
    let raw = String::from_utf8_lossy(&requests[0].body);
    assert!(!raw.contains("PRIVATE-BEFORE"));
    assert!(!raw.contains("PRIVATE-AFTER"));
}

#[test]
fn source_slices_preserve_crlf_unicode_and_a_missing_final_newline() {
    let input = MergeAdviceInput {
        base: source("pré\r\nbase𐍈\r\ntail", 1..2),
        ours: source("ours-before\r\nours-final", 1..2),
        theirs: source("theirs\r\nafter\r\n", 0..1),
    };
    let fixture = Fixture::new(Reply::json(answer()));
    fixture.client().suggest(&input).unwrap();
    let requests = fixture.finish();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].json()["state"],
        json!({
            "base": {"before": "pré\r\n", "conflict": "base𐍈\r\n", "after": "tail"},
            "ours": {"before": "ours-before\r\n", "conflict": "ours-final", "after": ""},
            "theirs": {"before": "", "conflict": "theirs\r\n", "after": "after\r\n"}
        })
    );
}

#[test]
fn bare_carriage_returns_obey_the_twenty_line_disclosure_boundary() {
    let context = "context\r".repeat(20);
    let text = format!("base\r{context}PRIVATE-AFTER\r");
    let mut input = input();
    input.base = source(&text, 0..1);
    let fixture = Fixture::new(Reply::json(answer()));
    fixture.client().suggest(&input).unwrap();
    let requests = fixture.finish();
    assert_eq!(
        requests[0].json()["state"]["base"],
        json!({"before": "", "conflict": "base\r", "after": context})
    );
    assert!(!String::from_utf8_lossy(&requests[0].body).contains("PRIVATE-AFTER"));
}

#[test]
fn mixed_line_endings_preserve_the_merge_engines_selected_regions() {
    let input = MergeAdviceInput {
        base: source("before\r\nbase\rafter\n", 1..2),
        ours: source("before\rours\nafter\r\n", 1..2),
        theirs: source("before\ntheirs\r\nafter\r", 1..2),
    };
    let fixture = Fixture::new(Reply::json(answer()));
    fixture.client().suggest(&input).unwrap();
    let requests = fixture.finish();
    assert_eq!(
        requests[0].json()["state"],
        json!({
            "base": {"before": "before\r\n", "conflict": "base\r", "after": "after\n"},
            "ours": {"before": "before\r", "conflict": "ours\n", "after": "after\r\n"},
            "theirs": {"before": "before\n", "conflict": "theirs\r\n", "after": "after\r"}
        })
    );
}

#[test]
fn empty_conflict_regions_keep_context_on_the_correct_side_of_the_insertion_point() {
    let input = MergeAdviceInput {
        base: source("", 0..0),
        ours: source("before\nremaining", 1..1),
        theirs: source("last\r\n", 1..1),
    };
    let fixture = Fixture::new(Reply::json(answer()));
    fixture.client().suggest(&input).unwrap();
    let requests = fixture.finish();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].json()["state"],
        json!({
            "base": {"before": "", "conflict": "", "after": ""},
            "ours": {"before": "before\n", "conflict": "", "after": "remaining"},
            "theirs": {"before": "last\r\n", "conflict": "", "after": ""}
        })
    );
}

#[test]
fn reversed_and_out_of_bounds_ranges_are_rejected_before_connecting() {
    let (listener, client) = unused_listener();
    let mut reversed = input();
    reversed.base = source("first\nsecond\n", Range { start: 2, end: 1 });
    let mut past_end = input();
    past_end.ours = source("one\n", 0..2);
    let mut empty_past_end = input();
    empty_past_end.theirs = source("last", 2..2);
    let mut overflow = input();
    overflow.base = source("", usize::MAX..usize::MAX);
    for invalid in [reversed, past_end, empty_past_end, overflow] {
        assert_eq!(client.suggest(&invalid), Err(JevError::InvalidInput));
        assert_no_connection(&listener);
    }
}

#[test]
fn serialized_escape_expansion_is_limited_before_any_connection() {
    let (listener, client) = unused_listener();
    let mut input = input();
    input.ours = source(&"\t".repeat(13 * 1024), 0..1);
    assert_eq!(client.suggest(&input), Err(JevError::RequestTooLarge));
    assert_no_connection(&listener);
}

#[test]
fn exactly_twenty_four_kib_is_sent_but_one_more_byte_is_rejected() {
    let mut input = MergeAdviceInput {
        base: source("b", 0..1),
        ours: source("o", 0..1),
        theirs: source("t", 0..1),
    };
    let seed = Fixture::new(Reply::json(answer()));
    seed.client().suggest(&input).unwrap();
    let seed_requests = seed.finish();
    assert_eq!(seed_requests.len(), 1);
    let padding = 24 * 1024 - seed_requests[0].body.len() + 1;
    input.base = source(&"x".repeat(padding), 0..1);
    let boundary = Fixture::new(Reply::json(answer()));
    boundary.client().suggest(&input).unwrap();
    let requests = boundary.finish();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].body.len(), 24 * 1024);
    assert_eq!(
        requests[0].json()["state"]["base"]["conflict"],
        "x".repeat(padding)
    );
    input.base = source(&"x".repeat(padding + 1), 0..1);
    let (listener, client) = unused_listener();
    assert_eq!(client.suggest(&input), Err(JevError::RequestTooLarge));
    assert_no_connection(&listener);
}

#[test]
fn zero_confidence_does_not_turn_an_ours_choice_into_a_different_outcome() {
    let mut response = answer();
    response["answers"]["resolution"]["choice"] = json!("ours");
    response["answers"]["resolution"]["probabilities"] = json!({
        "ours": 0.34, "theirs": 0.33, "leave_unresolved": 0.33
    });
    response["answers"]["resolution"]["confidence"] = json!(0.0);
    let result = exchange(Reply::json(response)).unwrap();
    assert_eq!(result.choice, MergeAdviceChoice::Ours);
    assert_eq!(result.confidence, 0.0);
}

#[test]
fn leave_unresolved_is_preserved_even_at_full_confidence() {
    let mut response = answer();
    response["answers"]["resolution"]["choice"] = json!("leave_unresolved");
    response["answers"]["resolution"]["probabilities"] = json!({
        "ours": 0, "theirs": 0, "leave_unresolved": 1
    });
    response["answers"]["resolution"]["confidence"] = json!(1);
    let result = exchange(Reply::json(response)).unwrap();
    assert_eq!(result.choice, MergeAdviceChoice::LeaveUnresolved);
    assert_eq!(result.confidence, 1.0);
}

#[test]
fn incorrect_answer_identity_type_and_choice_are_not_defaulted() {
    let mut wrong_id = answer();
    let resolution = wrong_id["answers"]
        .as_object_mut()
        .unwrap()
        .remove("resolution")
        .unwrap();
    wrong_id["answers"]["another_conflict"] = resolution;
    let mut extra_answer = answer();
    extra_answer["answers"]["another_conflict"] = extra_answer["answers"]["resolution"].clone();
    let mut wrong_type = answer();
    wrong_type["answers"]["resolution"]["type"] = json!("score");
    let mut missing_type = answer();
    missing_type["answers"]["resolution"]
        .as_object_mut()
        .unwrap()
        .remove("type");
    let mut unsupported_choice = answer();
    unsupported_choice["answers"]["resolution"]["choice"] = json!("both");
    let mut case_changed = answer();
    case_changed["answers"]["resolution"]["choice"] = json!("Theirs");
    let mut missing_choice = answer();
    missing_choice["answers"]["resolution"]
        .as_object_mut()
        .unwrap()
        .remove("choice");
    let mut wrong_model = answer();
    wrong_model["model"] = json!(17);
    let mut missing_model = answer();
    missing_model.as_object_mut().unwrap().remove("model");
    let mut empty_model = answer();
    empty_model["model"] = json!(" ");
    let mut terminal_control_model = answer();
    terminal_control_model["model"] = json!("jev-\u{1b}[2J");
    for (name, response) in [
        ("answer identity", wrong_id),
        ("additional answer identity", extra_answer),
        ("answer type", wrong_type),
        ("missing type", missing_type),
        ("unsupported choice", unsupported_choice),
        ("exact choice identity", case_changed),
        ("missing choice", missing_choice),
        ("model type", wrong_model),
        ("missing model", missing_model),
        ("empty model", empty_model),
        ("terminal control in model", terminal_control_model),
    ] {
        assert_eq!(
            exchange(Reply::json(response)),
            Err(JevError::InvalidResponse),
            "{name}"
        );
    }
}

#[test]
fn confidence_must_be_present_numeric_and_between_zero_and_one() {
    for (name, value) in [
        ("negative", json!(-0.01)),
        ("above one", json!(1.01)),
        ("numeric string", json!("0.64")),
        ("null", Value::Null),
    ] {
        let mut response = answer();
        response["answers"]["resolution"]["confidence"] = value;
        assert_eq!(
            exchange(Reply::json(response)),
            Err(JevError::InvalidResponse),
            "{name}"
        );
    }
    let mut response = answer();
    response["answers"]["resolution"]
        .as_object_mut()
        .unwrap()
        .remove("confidence");
    assert_eq!(
        exchange(Reply::json(response)),
        Err(JevError::InvalidResponse)
    );
}

#[test]
fn every_expected_probability_is_required_and_validated_including_unchosen_options() {
    for (name, probabilities) in [
        (
            "missing ours",
            json!({"theirs": 0.9, "leave_unresolved": 0.1}),
        ),
        (
            "missing theirs",
            json!({"ours": 0.1, "leave_unresolved": 0.9}),
        ),
        (
            "missing leave_unresolved",
            json!({"ours": 0.1, "theirs": 0.9}),
        ),
        (
            "negative unchosen",
            json!({"ours": -0.1, "theirs": 0.8, "leave_unresolved": 0.3}),
        ),
        (
            "oversized chosen",
            json!({"ours": 0, "theirs": 1.1, "leave_unresolved": 0}),
        ),
        (
            "string unchosen",
            json!({"ours": 0.1, "theirs": 0.8, "leave_unresolved": "0.1"}),
        ),
        ("null probabilities", Value::Null),
    ] {
        let mut response = answer();
        response["answers"]["resolution"]["probabilities"] = probabilities;
        assert_eq!(
            exchange(Reply::json(response)),
            Err(JevError::InvalidResponse),
            "{name}"
        );
    }
    let mut response = answer();
    response["answers"]["resolution"]
        .as_object_mut()
        .unwrap()
        .remove("probabilities");
    assert_eq!(
        exchange(Reply::json(response)),
        Err(JevError::InvalidResponse)
    );
}

#[test]
fn malformed_truncated_and_nonfinite_numeric_responses_are_rejected() {
    let overflow_confidence = serde_json::to_string(&answer())
        .unwrap()
        .replace("0.64", "1e999");
    let overflow_probability = serde_json::to_string(&answer())
        .unwrap()
        .replace("0.8", "1e999");
    let nonfinite_probability = serde_json::to_string(&answer())
        .unwrap()
        .replace("0.8", "NaN");
    for body in [
        b"not json".to_vec(),
        b"{\"answers\":".to_vec(),
        overflow_confidence.into_bytes(),
        overflow_probability.into_bytes(),
        nonfinite_probability.into_bytes(),
    ] {
        let mut reply = Reply::json(answer());
        reply.body = body;
        assert_eq!(exchange(reply), Err(JevError::InvalidResponse));
    }
}

#[test]
fn chunked_response_at_sixty_four_kib_is_accepted_without_using_content_length() {
    let mut reply = Reply::json(answer());
    reply.body.resize(64 * 1024, b' ');
    reply.chunked = true;
    let result = exchange(reply).unwrap();
    assert_eq!(result.choice, MergeAdviceChoice::Theirs);
    assert_eq!(result.confidence, 0.64);
}

#[test]
fn oversized_response_is_rejected_for_both_content_length_and_chunked_framing() {
    for chunked in [false, true] {
        let mut reply = Reply::json(answer());
        reply.body.resize(64 * 1024 + 1, b' ');
        reply.chunked = chunked;
        assert_eq!(
            exchange(reply),
            Err(JevError::ResponseTooLarge),
            "chunked={chunked}"
        );
    }
}

#[test]
fn stalled_response_times_out_without_retrying() {
    let mut reply = Reply::json(answer());
    reply.stall = true;
    let fixture = Fixture::new(reply);
    let client = client(&fixture.endpoint, Duration::from_millis(100));
    let start = Instant::now();
    let result = client.suggest(&input());
    let elapsed = start.elapsed();
    let requests = fixture.finish();
    assert_eq!(result, Err(JevError::Timeout));
    assert!(elapsed < Duration::from_secs(2));
    assert_eq!(requests.len(), 1);
}

#[test]
fn authentication_rate_limit_and_server_failures_preserve_status_without_retrying() {
    for status in [401, 429, 503] {
        let mut reply = Reply::json(json!({"error": "PRIVATE-RESPONSE-BODY"}));
        reply.status = status;
        reply.headers.push(("Retry-After".into(), "0".into()));
        let error = exchange(reply).unwrap_err();
        assert_eq!(error, JevError::HttpStatus(status));
        let displayed = error.to_string();
        assert!(!displayed.contains("PRIVATE-RESPONSE-BODY"));
        assert!(!displayed.contains(DUMMY_KEY));
    }
}

#[test]
fn dropped_connection_is_a_transport_error_without_retrying() {
    let mut reply = Reply::json(answer());
    reply.disconnect = true;
    assert_eq!(exchange(reply), Err(JevError::Transport));
}

#[test]
fn redirect_never_discloses_authorization_or_source_to_another_listener() {
    for status in [302, 307, 308] {
        let destination = Fixture::new(Reply::json(answer()));
        let mut reply = Reply::json(answer());
        reply.status = status;
        reply
            .headers
            .push(("Location".into(), destination.endpoint.clone()));
        let origin = Fixture::new(reply);
        let result = origin.client().suggest(&input());
        let origin_requests = origin.finish();
        let destination_requests = destination.finish();
        assert_eq!(result, Err(JevError::HttpStatus(status)));
        assert_eq!(origin_requests.len(), 1);
        assert!(destination_requests.is_empty());
    }
}

#[test]
fn unsafe_configuration_is_rejected_without_opening_a_connection() {
    let (listener, _) = unused_listener();
    let address = listener.local_addr().unwrap();
    for (key, endpoint) in [
        ("", format!("http://{address}/v1/systemone")),
        ("   \t", format!("http://{address}/v1/systemone")),
        (
            "bad\r\nInjected: value",
            format!("http://{address}/v1/systemone"),
        ),
        (
            DUMMY_KEY,
            format!("http://user:password@{address}/v1/systemone"),
        ),
        (DUMMY_KEY, "http://api.typesafe.ai/v1/systemone".into()),
        (DUMMY_KEY, "http://localhost/v1/systemone".into()),
    ] {
        let result = JevClient::new(JevConfig {
            api_key: key.into(),
            endpoint,
            timeout: Duration::from_millis(100),
        });
        assert!(matches!(result, Err(JevError::InvalidConfiguration)));
        assert_no_connection(&listener);
    }
}
