# Integrations public API

Production APIs follow the approved behavioural tests. The coordinator owns build, test, and runtime verification after integration. `chvrn-integrations` exposes four independent modules: `herdr`, `socket`, `lsp`, and `jev`. None sends an approval keystroke, stages a change, or silently writes a reviewed file.

## Dependencies for implementation and tests

Library: `chvrn-core`, `serde` with `derive`, `serde_json`, `url`, and pinned `ureq =3.4.2` with rustls and default features disabled. Tests: `tempfile` and `serde_json`; otherwise standard library. Unix socket tests use `std::os::unix`, and are Unix-targeted. Runtime uses the installed herdr 0.9 `agent get`, `agent explain --json`, `agent prompt`, `pane get`, `pane split`, `pane run` and Unix socket `pane.focus` APIs; no guessed flags.

## `chvrn_integrations::herdr`

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleReliability { Unverified, Verified }
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentSessionIdentity { Unverified, Verified(String) }
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HerdrError { InvalidEnvelope, WrongPane, StaleReport, FeedbackNotPending }
pub struct ReviewReport {
    pub id: String,
    pub pane_id: String,
    pub terminal_id: String,
    pub agent_session_id: String,
    pub snapshot_id: String,
    pub files: Vec<ReviewedFile>,
    pub comment: String,
}
pub struct ReviewedFile {
    pub path: String,
    pub snapshot_id: String,
    pub accepted: Vec<HunkRange>,
    pub rejected: Vec<HunkRange>,
}
pub struct HunkRange { pub start: usize, pub end: usize }
pub enum BridgeEffect {
    OfferReview { pane_id: String, revision: u64 },
    SendFeedback { report: ReviewReport },
    InvalidateReview { snapshot_id: String },
}
pub struct HerdrBridge;
impl HerdrBridge {
    pub fn new(pane_id: &str) -> Self;
    pub fn observe_agent_session(&mut self, identity: AgentSessionIdentity) -> Vec<BridgeEffect>;
    pub fn observe(&mut self, json_envelope: &str, reliability: LifecycleReliability) -> Result<Vec<BridgeEffect>, HerdrError>;
    pub fn invalidate_snapshot(&mut self, snapshot_id: &str) -> Vec<BridgeEffect>;
    pub fn submit(&mut self, report: ReviewReport) -> Result<(), HerdrError>;
    pub fn feedback_delivered(&mut self, report_id: &str) -> Result<(), HerdrError>;
    pub fn feedback_failed(&mut self, report_id: &str) -> Result<(), HerdrError>;
    pub fn quit(&mut self);
    pub fn explicit_gate(&self) -> BridgeEffect;
    pub fn review_visible(&self) -> bool;
    pub fn pending_report(&self) -> Option<&ReviewReport>;
    pub fn verified_identity(&self) -> Option<VerifiedAgentTarget<'_>>;
}
pub struct VerifiedAgentTarget<'a> {
    pub pane_id: &'a str,
    pub terminal_id: &'a str,
    pub agent_session_id: &'a str,
}
pub enum HerdrProcessError { Unavailable, Io(std::io::Error), Rejected(String), InvalidResponse }
pub enum SplitDirection { Right, Down }
pub enum FeedbackDelivery { Delivered, RejectedBlocked, Uncertain(String) }
pub struct HerdrProcess;
impl HerdrProcess {
    pub fn from_environment() -> Result<Self, HerdrProcessError>;
    pub fn new(binary: impl Into<std::path::PathBuf>) -> Result<Self, HerdrProcessError>;
    pub fn get_agent(&self, pane_id: &str) -> Result<serde_json::Value, HerdrProcessError>;
    pub fn resolve_target(&self, name_or_pane: &str) -> Result<String, HerdrProcessError>;
    pub fn pane_get(&self, pane_id: &str) -> Result<serde_json::Value, HerdrProcessError>;
    pub fn focus_agent(&self, name_or_pane: &str) -> Result<serde_json::Value, HerdrProcessError>;
    pub fn focus_pane(&self, pane_id: &str) -> Result<(), HerdrProcessError>;
    pub fn split(&self, pane_id: &str, direction: SplitDirection, cwd: &std::path::Path) -> Result<String, HerdrProcessError>;
    pub fn run_pane(&self, pane_id: &str, command: &str) -> Result<(), HerdrProcessError>;
    pub fn sample(&self, bridge: &mut HerdrBridge) -> Result<Vec<BridgeEffect>, HerdrProcessError>;
    pub fn deliver_feedback(&self, bridge: &mut HerdrBridge, report: &ReviewReport) -> Result<FeedbackDelivery, HerdrProcessError>;
    pub fn request_explanation(&self, bridge: &HerdrBridge, snapshot_id: &str, path: &str, range: HunkRange, question: &str) -> Result<FeedbackDelivery, HerdrProcessError>;
}
```

`observe` accepts a herdr 0.9 envelope with `id: "cli:agent:get"`, `result.type: "agent_info"`, and `result.agent` holding `agent_status`, `pane_id`, `revision`, `state_change_seq`, and `terminal_id`. The installed OMP lifecycle hook now supplies `agent_session` with `agent: "omp"`, `source: "herdr:omp"`, `kind: "path"`, and an opaque session-path `value`. `HerdrProcess::sample` compares two agent reads around `agent explain --json`; it marks the identity verified only if agent, terminal, session, and state sequence stay stable, and the explanation confirms `screen_detection_skipped: true`, `screen_detection_skip_reason: "full_lifecycle_hook_authority"`, and `skip_state_update: false`. Official opaque `kind: "id"` is also accepted. A missing or conflicting session fails closed; neither output revision, agent label, title, nor terminal ID can substitute for session identity.

Unknown statuses or malformed/error envelopes return an error or no effects and leave pending decisions intact. `revision` advances during normal output (observed 7 -> 327 -> 753 -> 1102 with unchanged `state_change_seq`) and is informational only. `terminal_id` identifies the pane PTY, not the agent: replacing Codex with omp can retain it. A changed verified agent-session identity, changed terminal identity, or explicit `invalidate_snapshot` call invalidates pending reviews. An initial idle observation, repeated state, unverified identity, or unverified lifecycle never offers review automatically. A verified observed `working` -> `blocked`/`done`/`idle` transition with advancing `state_change_seq` offers review once. Only authoritative lifecycle-hook evidence permits `Verified`; the older unhooked omp session reported `idle` while actively working. `explicit_gate` remains available without lifecycle evidence.

`submit` enqueues a reviewed result bound to explicit pane, terminal, verified agent session, and content snapshot identities. It rejects a report without a verified session. Feedback cannot dispatch at `blocked` (possibly a permission prompt), `working`, unknown state, unverified agent session, or unverified lifecycle. After explicit submission, authoritative `done` or `idle` with a verified session is input-ready and emits `SendFeedback` once. The observed official OMP turn transitioned `working` at `state_change_seq: 28` to `done` at sequence 29, and `herdr agent prompt` accepted input from that completed state. An initial `done` or `idle` never offers an automatic review, and neither state itself approves the result. Repeated observations do not emit an in-flight report. `feedback_failed` releases it for a later verified input-ready observation; `feedback_delivered` completes it. `quit` never submits, approves, or drops submitted feedback. Session replacement or content snapshot invalidation blocks stale delivery, while loss of verified identity suspends delivery without silently clearing it.

Each `ReviewedFile.snapshot_id` is required and identifies the inspected content snapshot whose accepted and rejected zero-based line ranges were reviewed. A report can contain multiple entries with the same `path` but different `snapshot_id` values; the report-wide `ReviewReport.snapshot_id` identifies the review, not every file revision. Deserialisation has no missing-field default. The CLI prepares the report without enqueuing it, persists the reviewed result, then submits the report; the feedback worker validates the whole review again before delivery.

The CLI checks `HERDR_ENV=1` before running herdr commands. The reducer only returns effects and never invokes a CLI or guesses pane targets; its consumer may call `herdr agent prompt <pane-id> <text>` only for `SendFeedback` after verified input readiness. A permission dialog or transport error leaves the review unapproved.

An accepted `agent prompt` response may omit `agent_session`; `send_verified_prompt` requires authoritative `done` or `idle` plus verified session identity before sending either review feedback or a requested explanation. It requires the response to name the expected pane and terminal, then re-reads authoritative session identity before marking the report delivered. A failed re-read after a successful prompt is `Uncertain`, not a reason to retry automatically.

`pane run` success can have empty stdout. `run_pane` therefore relies on the process exit status and reports errors from stderr without requiring or manufacturing JSON. An observed OMP `/restart` exec was followed by `agent_not_ready` despite a displayed idle hook state; do not treat `/restart` as a verified activation path until lifecycle authority and shell handoff are confirmed on the new session.

## `chvrn_integrations::socket`

```rust
pub struct InspectedFile { pub relative_path: String, pub snapshot_id: String, pub bytes: Vec<u8> }
pub struct PatchCandidate { pub snapshot_id: String, pub path: String, pub patch: String }
pub enum ReviewOutcome { Accepted, Declined }
pub struct SocketServer;
impl SocketServer {
    pub const MAX_FRAME_BYTES: usize = 65_536;
    pub fn bind(socket_path: &std::path::Path, root: &std::path::Path, inspected: Vec<InspectedFile>) -> std::io::Result<Self>;
    pub fn set_nonblocking(&self, enabled: bool) -> std::io::Result<()>;
    pub fn poll(&mut self) -> std::io::Result<bool>;
    pub fn serve_next(&mut self) -> std::io::Result<()>;
    pub fn pending_candidates(&self) -> &[PatchCandidate];
    pub fn take_pending_candidates(&mut self) -> Vec<PatchCandidate>;
    pub fn refresh_inspected(&mut self, inspected: Vec<InspectedFile>) -> std::io::Result<usize>;
    pub fn record_review(&mut self, snapshot_id: &str, outcome: ReviewOutcome) -> std::io::Result<()>;
}
```

Unix-domain protocol, distinct from LSP: four-byte unsigned big-endian payload length followed by one UTF-8 JSON object. One request and one response per connection; both are bounded by `MAX_FRAME_BYTES`. `{"type":"inspect"}` returns `{"status":"inspected","files":[{"path":"src/a.rs","snapshot":"s1"}]}` so clients can discover per-file snapshot IDs without guessing. An inventory larger than the response limit returns `{"status":"rejected","reason":"too_large"}`. Snapshot IDs must be unique across inspected paths.

Candidate request: `{"type":"patch_candidate","snapshot":"s1","path":"src/a.rs","patch":"..."}`. A candidate is queued only when its path resolves inside root, is not a symlink escape, matches an inspected relative path/snapshot, and current disk bytes match inspected bytes. Another candidate for an issued pending or completed snapshot is rejected with `already_issued`, because review status is keyed by snapshot ID. `take_pending_candidates` transfers suggestions to the host for explicit review. Neither receiving nor inspecting a candidate writes files, approves changes, or rewrites stale suggestions under refreshed IDs.

Status request: `{"type":"review_status","snapshot":"s1"}`. A completed `accepted` or `declined` receipt survives `refresh_inspected` even after the old snapshot leaves the active file map; the first completed outcome is immutable. Refresh removes queued unreviewed suggestions that no longer match current inspected files, and those old pending IDs return `unknown_snapshot`. Invalid framing and JSON are rejected without queuing or writing. Other rejection reasons include `malformed`, `too_large`, `outside_root`, and `stale_snapshot`. Binding requires a private (`0700`) socket parent to avoid a publication race, sets socket file mode `0600`, and Drop removes only the socket inode it bound. `poll` returns immediately in nonblocking mode. Framing and I/O have bounded lengths and per-connection timeouts.

`SocketUi::decision` waits at most three seconds for its worker to return the `record_review` result. It succeeds only after that receipt is recorded; a rejected snapshot, worker failure, or timeout propagates to its caller. Sending a command to the worker is not evidence of an accepted or declined receipt.

`SocketServer` is `Send` so an owner can run `serve_next` on a dedicated worker. Framing errors must not leave that worker blocked on unread payload bytes.

## `chvrn_integrations::lsp`

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LspError { Io(String), Protocol(String), InvalidPosition, StaleSnapshot, Unavailable }
pub struct Document { pub uri: String, pub text: String, pub version: i32, pub snapshot_id: String }
pub struct TextPosition { pub line: u32, pub byte_column: usize }
pub struct LspLocation { pub uri: String, pub line: u32, pub utf16_column: u32 }
pub struct Diagnostic { pub message: String, pub start: TextPosition, pub end: TextPosition }
pub struct FormatRequest { pub inspected_snapshot_id: String, pub new_snapshot_id: String }
pub struct FormattingOptions { pub tab_size: u32, pub insert_spaces: bool }
pub struct SnapshotBound<T> { pub value: T }
impl<T> SnapshotBound<T> {
    pub fn source(&self) -> &chvrn_core::TextSnapshot;
    pub fn is_current(&self, current: &chvrn_core::TextSnapshot) -> bool;
    pub fn into_current(self, current: &chvrn_core::TextSnapshot) -> Result<T, LspError>;
}
impl SnapshotBound<Option<String>> {
    pub fn apply_to_buffer(&self, buffer: &mut chvrn_core::edit::TextBuffer) -> Result<Option<chvrn_core::TextSnapshot>, LspError>;
}
pub struct LspSession<R: std::io::Read, W: std::io::Write>;
impl<R: std::io::Read, W: std::io::Write> LspSession<R, W> {
    pub fn new(reader: R, writer: W, document: Document) -> Self;
    pub fn initialise(&mut self, root_uri: Option<&str>, language_id: &str) -> Result<serde_json::Value, LspError>;
    pub fn hover(&mut self, position: TextPosition) -> Result<Option<String>, LspError>;
    pub fn definition(&mut self, position: TextPosition) -> Result<Option<LspLocation>, LspError>;
    pub fn read_diagnostics(&mut self) -> Result<(), LspError>;
    pub fn diagnostics(&self) -> &[Diagnostic];
    pub fn bind_snapshot(&mut self, snapshot: chvrn_core::TextSnapshot) -> Result<(), LspError>;
    pub fn diagnostics_for_snapshot(&self, current: &chvrn_core::TextSnapshot) -> Option<&[Diagnostic]>;
    pub fn set_formatting_options(&mut self, options: FormattingOptions) -> Result<(), LspError>;
    pub fn replace_text(&mut self, text: String, new_snapshot_id: String) -> Result<(), LspError>;
    pub fn format(&mut self, request: FormatRequest) -> Result<(), LspError>;
    pub fn format_proposal(&mut self, source: &chvrn_core::TextSnapshot) -> Result<SnapshotBound<Option<String>>, LspError>;
    pub fn undo(&mut self, new_snapshot_id: String) -> Result<(), LspError>;
    pub fn shutdown(&mut self) -> Result<(), LspError>;
    pub fn document(&self) -> &Document;
}
pub struct LanguageServerConfig {
    pub executable: std::path::PathBuf,
    pub arguments: Vec<String>,
    pub root_uri: Option<String>,
    pub language_id: String,
}
pub struct ServerCapabilities { pub hover: bool, pub definition: bool, pub formatting: bool }
pub struct LspProcess;
impl LspProcess {
    pub fn start(config: Option<&LanguageServerConfig>, document: Document) -> Result<Self, LspError>;
    pub fn capabilities(&self) -> ServerCapabilities;
    pub fn session_mut(&mut self) -> &mut LspSession<TimedReader, std::process::ChildStdin>;
    pub fn hover(&mut self, position: TextPosition) -> Result<Option<String>, LspError>;
    pub fn definition(&mut self, position: TextPosition) -> Result<Option<LspLocation>, LspError>;
    pub fn format(&mut self, request: FormatRequest) -> Result<(), LspError>;
    pub fn shutdown(self) -> Result<(), LspError>;
}
```

LSP uses `Content-Length: N\r\n\r\n` headers followed by exactly N UTF-8 JSON bytes, not the socket's four-byte prefix. Outgoing requests use `jsonrpc: "2.0"`, distinct IDs, `textDocument/hover`, `textDocument/definition`, and `textDocument/formatting`. Caller positions are UTF-8 byte offsets on Unicode scalar boundaries; wire positions are UTF-16 code-unit offsets. Incoming cross-file definition results retain the target URI and UTF-16 location until the target text can be loaded. Diagnostics for the loaded document convert to byte offsets; invalid byte boundaries are errors. `textDocument/didChange` carries the new full text and increasing version. Stale diagnostics versions and stale responses never replace current state. Server requests do not approve reviews. `LspProcess` starts a configured executable, performs initialise/initialized/didOpen, gates unsupported capabilities, bounds server reads, shuts down explicitly or kills its child on Drop. Missing configuration reports `Unavailable`.

`FormatRequest.new_snapshot_id` names the resulting content after successful low-level `LspSession::format`. Low-level `undo` restores the prior text with a fresh caller-supplied ID, and both operations send a versioned full-text `didChange`. For the shared editor buffer, bind its cloned `TextSnapshot`, request `format_proposal`, and apply it with `apply_to_buffer` only if `same_identity` still matches the current `TextBuffer` snapshot. Core `TextBuffer` then owns undo. After apply or undo, pass the resulting text and a distinct caller-supplied ID to `replace_text`, then bind the fresh core snapshot; this keeps the LSP document synchronised without allowing old equal text to restore stale authority. `diagnostics_for_snapshot` only exposes a diagnostic batch bound to the exact current core snapshot. Hunk ranges in review reports are zero-based, end-exclusive line ranges in their inspected file snapshots.

The preceding `format_proposal`/`apply_to_buffer` sequence is the library API pattern. The current CLI instead calls `LspProcess::format`, then applies the returned text with `ReviewSession::replace_pane_text` only after verifying the response path, pane and buffer identity. Stale responses are discarded. Later language requests synchronise changed or undone pane text with a fresh identity.

`replace_text` accepts unchanged bytes with a fresh snapshot ID: edit/undo can return to equal text without restoring the old snapshot's authority. It still advances the document version and invalidates previous diagnostics. The CLI caches diagnostics only for the currently bound core snapshot and consumes incoming messages until that snapshot has a diagnostic batch, instead of waiting for a new notification on every repeated diagnostics request.

Herdr lifecycle, socket framing, and LSP framing remain separate. The CLI and TUI compose them after coordinator verification; no automatic approval follows any protocol response.

## `chvrn_integrations::jev`

```rust
use std::time::Duration;
use chvrn_core::merge_advice::{MergeAdviceInput, MergeAdviceSuggestion};

pub struct JevConfig {
    pub api_key: String,
    pub endpoint: String,
    pub timeout: Duration,
}

pub enum JevError {
    InvalidConfiguration,
    InvalidInput,
    RequestTooLarge,
    HttpStatus(u16),
    Timeout,
    Transport,
    ResponseTooLarge,
    InvalidResponse,
}

pub struct JevClient;
impl JevClient {
    pub fn new(config: JevConfig) -> Result<Self, JevError>;
    pub fn suggest(&self, input: &MergeAdviceInput) -> Result<MergeAdviceSuggestion, JevError>;
}
```

Construction validates configuration without making a request. HTTPS is required except for literal loopback-IP HTTP endpoints used by local protocol fixtures. Embedded URL credentials, fragments, invalid bearer-key characters and zero timeouts are rejected. The CLI fixes the production endpoint and a 30-second deadline; it exposes no endpoint override.

`suggest` is synchronous and must run off the terminal thread. It validates zero-based half-open source line ranges with the core parser's LF/CRLF/bare-CR semantics, then borrows the complete conflict and up to 20 lines before and after each region. It rejects raw selected/context bytes over 24 KiB before serialisation and checks the complete JSON body against the same limit before transmitting. No conflict truncation or line-ending normalisation occurs.

The request uses bearer authentication, `jev-1.13.0` and one choice question named `resolution`. Redirects and retries are disabled; the configured timeout is global. At most 64 KiB of response data is accepted before decoding. The response must contain the expected choice answer and all three probabilities; confidence and probabilities must be finite numbers in `[0,1]`. Choices are exactly ours, theirs or leave unresolved. Returned model metadata must be non-empty and contain no control characters. Malformed responses never default to a side; errors omit remote bodies and credentials.

Protocol tests use bounded real loopback HTTP peers. The client returns only data; review authority, application, undo and write confirmation belong to the TUI and CLI contracts.
