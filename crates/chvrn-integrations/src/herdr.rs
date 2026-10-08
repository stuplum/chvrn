use crate::process::ProcessSupervisor;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::env;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::process::Command;
use tokio::time::{Instant, timeout_at};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleReliability {
    Unverified,
    Verified,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentSessionIdentity {
    Unverified,
    Verified(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HerdrError {
    InvalidEnvelope,
    WrongPane,
    StaleReport,
    FeedbackNotPending,
}

impl std::fmt::Display for HerdrError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for HerdrError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HunkRange {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewedFile {
    pub path: String,
    pub snapshot_id: String,
    pub accepted: Vec<HunkRange>,
    pub rejected: Vec<HunkRange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewReport {
    pub id: String,
    pub pane_id: String,
    pub terminal_id: String,
    pub agent_session_id: String,
    pub snapshot_id: String,
    pub files: Vec<ReviewedFile>,
    pub comment: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeEffect {
    OfferReview { pane_id: String, revision: u64 },
    SendFeedback { report: ReviewReport },
    InvalidateReview { snapshot_id: String },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AgentStatus {
    Idle,
    Working,
    Blocked,
    Done,
    Unknown,
}

#[derive(Clone)]
struct Observation {
    terminal_id: String,
    status: AgentStatus,
    revision: u64,
    state_change_seq: u64,
}

struct PendingFeedback {
    report: ReviewReport,
    in_flight: bool,
}

pub struct HerdrBridge {
    pane_id: String,
    agent_session: AgentSessionIdentity,
    last: Option<Observation>,
    pending: Option<PendingFeedback>,
    invalidated: HashSet<String>,
    delivered: HashSet<String>,
    review_visible: bool,
}

pub struct VerifiedAgentTarget<'a> {
    pub pane_id: &'a str,
    pub terminal_id: &'a str,
    pub agent_session_id: &'a str,
}

impl HerdrBridge {
    pub fn new(pane_id: &str) -> Self {
        Self {
            pane_id: pane_id.into(),
            agent_session: AgentSessionIdentity::Unverified,
            last: None,
            pending: None,
            invalidated: HashSet::new(),
            delivered: HashSet::new(),
            review_visible: false,
        }
    }

    pub fn observe_agent_session(&mut self, identity: AgentSessionIdentity) -> Vec<BridgeEffect> {
        let changed = match &identity {
            AgentSessionIdentity::Verified(next) => {
                matches!(&self.agent_session, AgentSessionIdentity::Verified(previous) if previous != next)
                    || self
                        .pending
                        .as_ref()
                        .is_some_and(|pending| pending.report.agent_session_id != *next)
            }
            AgentSessionIdentity::Unverified => false,
        };
        let effects = if changed {
            self.invalidate_pending()
        } else {
            Vec::new()
        };
        if changed || matches!(identity, AgentSessionIdentity::Unverified) {
            self.last = None;
            self.review_visible = false;
        }
        self.agent_session = identity;
        effects
    }

    pub fn observe(
        &mut self,
        json_envelope: &str,
        reliability: LifecycleReliability,
    ) -> Result<Vec<BridgeEffect>, HerdrError> {
        let envelope: Value =
            serde_json::from_str(json_envelope).map_err(|_| HerdrError::InvalidEnvelope)?;
        if envelope.get("id").and_then(Value::as_str) != Some("cli:agent:get")
            || envelope.pointer("/result/type").and_then(Value::as_str) != Some("agent_info")
        {
            return Err(HerdrError::InvalidEnvelope);
        }
        let agent = envelope
            .pointer("/result/agent")
            .ok_or(HerdrError::InvalidEnvelope)?;
        let pane = agent
            .get("pane_id")
            .and_then(Value::as_str)
            .ok_or(HerdrError::InvalidEnvelope)?;
        if pane != self.pane_id {
            return Err(HerdrError::WrongPane);
        }
        let terminal = agent
            .get("terminal_id")
            .and_then(Value::as_str)
            .ok_or(HerdrError::InvalidEnvelope)?;
        let revision = agent
            .get("revision")
            .and_then(Value::as_u64)
            .ok_or(HerdrError::InvalidEnvelope)?;
        let seq = agent
            .get("state_change_seq")
            .and_then(Value::as_u64)
            .ok_or(HerdrError::InvalidEnvelope)?;
        let status = match agent
            .get("agent_status")
            .and_then(Value::as_str)
            .ok_or(HerdrError::InvalidEnvelope)?
        {
            "working" => AgentStatus::Working,
            "blocked" => AgentStatus::Blocked,
            "idle" => AgentStatus::Idle,
            "done" => AgentStatus::Done,
            _ => AgentStatus::Unknown,
        };
        let mut effects = Vec::new();
        if self
            .last
            .as_ref()
            .is_some_and(|last| last.terminal_id != terminal)
            || self
                .pending
                .as_ref()
                .is_some_and(|pending| pending.report.terminal_id != terminal)
        {
            effects.extend(self.invalidate_pending());
            self.agent_session = AgentSessionIdentity::Unverified;
            self.last = None;
        }
        if status == AgentStatus::Unknown {
            return Ok(effects);
        }
        let previous = self.last.as_ref();
        let transition = previous.is_some_and(|last| {
            last.status == AgentStatus::Working
                && matches!(
                    status,
                    AgentStatus::Blocked | AgentStatus::Done | AgentStatus::Idle
                )
                && seq > last.state_change_seq
        });
        let session_verified = matches!(self.agent_session, AgentSessionIdentity::Verified(_));
        if transition && session_verified && reliability == LifecycleReliability::Verified {
            self.review_visible = true;
            effects.push(BridgeEffect::OfferReview {
                pane_id: self.pane_id.clone(),
                revision,
            });
        }
        self.last = Some(Observation {
            terminal_id: terminal.into(),
            status,
            revision,
            state_change_seq: seq,
        });
        if matches!(status, AgentStatus::Idle | AgentStatus::Done)
            && session_verified
            && reliability == LifecycleReliability::Verified
        {
            if let Some(pending) = &mut self.pending {
                if !pending.in_flight {
                    pending.in_flight = true;
                    effects.push(BridgeEffect::SendFeedback {
                        report: pending.report.clone(),
                    });
                }
            }
        }
        Ok(effects)
    }

    pub fn invalidate_snapshot(&mut self, snapshot_id: &str) -> Vec<BridgeEffect> {
        self.invalidated.insert(snapshot_id.into());
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.report.snapshot_id == snapshot_id)
        {
            return self.invalidate_pending();
        }
        Vec::new()
    }

    pub fn submit(&mut self, report: ReviewReport) -> Result<(), HerdrError> {
        let AgentSessionIdentity::Verified(session_id) = &self.agent_session else {
            return Err(HerdrError::StaleReport);
        };
        if report.pane_id != self.pane_id {
            return Err(HerdrError::WrongPane);
        }
        if report.agent_session_id != *session_id
            || self
                .last
                .as_ref()
                .is_some_and(|last| last.terminal_id != report.terminal_id)
            || self.invalidated.contains(&report.snapshot_id)
            || self.pending.is_some()
            || self.delivered.contains(&report.id)
            || report.snapshot_id.is_empty()
            || report.id.is_empty()
        {
            return Err(HerdrError::StaleReport);
        }
        self.pending = Some(PendingFeedback {
            report,
            in_flight: false,
        });
        Ok(())
    }

    pub fn feedback_delivered(&mut self, report_id: &str) -> Result<(), HerdrError> {
        if !self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.in_flight && pending.report.id == report_id)
        {
            return Err(HerdrError::FeedbackNotPending);
        }
        self.delivered.insert(report_id.into());
        self.pending = None;
        Ok(())
    }

    pub fn feedback_failed(&mut self, report_id: &str) -> Result<(), HerdrError> {
        let Some(pending) = &mut self.pending else {
            return Err(HerdrError::FeedbackNotPending);
        };
        if pending.report.id != report_id || !pending.in_flight {
            return Err(HerdrError::FeedbackNotPending);
        }
        pending.in_flight = false;
        Ok(())
    }

    pub fn quit(&mut self) {
        self.review_visible = false;
    }

    pub fn review_visible(&self) -> bool {
        self.review_visible
    }

    pub fn explicit_gate(&self) -> BridgeEffect {
        BridgeEffect::OfferReview {
            pane_id: self.pane_id.clone(),
            revision: self.last.as_ref().map_or(0, |last| last.revision),
        }
    }

    pub fn pending_report(&self) -> Option<&ReviewReport> {
        self.pending.as_ref().map(|pending| &pending.report)
    }

    pub fn verified_identity(&self) -> Option<VerifiedAgentTarget<'_>> {
        let AgentSessionIdentity::Verified(session) = &self.agent_session else {
            return None;
        };
        let observed = self.last.as_ref()?;
        Some(VerifiedAgentTarget {
            pane_id: &self.pane_id,
            terminal_id: &observed.terminal_id,
            agent_session_id: session,
        })
    }

    fn invalidate_pending(&mut self) -> Vec<BridgeEffect> {
        self.review_visible = false;
        self.pending.take().map_or_else(Vec::new, |pending| {
            self.invalidated.insert(pending.report.snapshot_id.clone());
            vec![BridgeEffect::InvalidateReview {
                snapshot_id: pending.report.snapshot_id,
            }]
        })
    }
}

#[derive(Debug)]
pub enum HerdrProcessError {
    Unavailable,
    Io(std::io::Error),
    Rejected(String),
    InvalidResponse,
}

impl From<std::io::Error> for HerdrProcessError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl std::fmt::Display for HerdrProcessError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => write!(formatter, "Herdr is unavailable outside HERDR_ENV=1"),
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Rejected(message) => write!(formatter, "Herdr rejected the request: {message}"),
            Self::InvalidResponse => write!(formatter, "Herdr returned an invalid response"),
        }
    }
}

impl std::error::Error for HerdrProcessError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDirection {
    Right,
    Down,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedbackDelivery {
    Delivered,
    RejectedBlocked,
    Uncertain(String),
}

#[derive(Clone, Default)]
pub struct DeliveryState(Arc<AtomicBool>);

impl DeliveryState {
    pub fn unconfirmed(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

pub struct HerdrProcess {
    binary: PathBuf,
    supervisor: ProcessSupervisor,
    deadline: Option<Instant>,
    delivery: DeliveryState,
}

impl HerdrProcess {
    pub fn from_environment(supervisor: ProcessSupervisor) -> Result<Self, HerdrProcessError> {
        let binary = env::var_os("HERDR_BIN_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("herdr"));
        Self::new(binary, supervisor)
    }

    pub fn new(
        binary: impl Into<PathBuf>,
        supervisor: ProcessSupervisor,
    ) -> Result<Self, HerdrProcessError> {
        if env::var("HERDR_ENV").as_deref() != Ok("1") {
            return Err(HerdrProcessError::Unavailable);
        }
        Ok(Self {
            binary: binary.into(),
            supervisor,
            deadline: None,
            delivery: DeliveryState::default(),
        })
    }

    pub fn delivery_state(&self) -> DeliveryState {
        self.delivery.clone()
    }

    fn operation(&self) -> Self {
        Self {
            binary: self.binary.clone(),
            supervisor: self.supervisor.clone(),
            deadline: Some(
                self.deadline
                    .unwrap_or_else(|| Instant::now() + Duration::from_secs(30)),
            ),
            delivery: self.delivery.clone(),
        }
    }

    async fn output(&self, arguments: &[&str]) -> Result<Output, HerdrProcessError> {
        let deadline = self
            .deadline
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(30));
        if Instant::now() >= deadline {
            return Err(deadline_error());
        }
        let mut child = self.supervisor.spawn(
            Command::new(&self.binary)
                .args(arguments)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )?;
        let stdout = child
            .stdout
            .take()
            .ok_or(HerdrProcessError::InvalidResponse)?;
        let stderr = child
            .stderr
            .take()
            .ok_or(HerdrProcessError::InvalidResponse)?;
        let result = timeout_at(deadline, async {
            let (stdout, stderr) = tokio::try_join!(capture(stdout), capture(stderr))?;
            let status = child.wait().await?;
            Ok::<_, HerdrProcessError>(Output {
                status,
                stdout,
                stderr,
            })
        })
        .await;
        match result {
            Ok(Ok(output)) => Ok(output),
            result => {
                let _ = child.terminate().await;
                Err(match result {
                    Ok(Err(error)) => error,
                    _ => deadline_error(),
                })
            }
        }
    }

    async fn invoke(&self, arguments: &[&str]) -> Result<Value, HerdrProcessError> {
        let output = self.output(arguments).await?;
        if !output.status.success() {
            return Err(HerdrProcessError::Rejected(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ));
        }
        serde_json::from_slice(&output.stdout).map_err(|_| HerdrProcessError::InvalidResponse)
    }

    pub async fn get_agent(&self, pane_id: &str) -> Result<Value, HerdrProcessError> {
        let envelope = self.invoke(&["agent", "get", pane_id]).await?;
        if envelope
            .pointer("/result/agent/pane_id")
            .and_then(Value::as_str)
            != Some(pane_id)
        {
            return Err(HerdrProcessError::InvalidResponse);
        }
        Ok(envelope)
    }

    pub async fn resolve_target(&self, name_or_pane: &str) -> Result<String, HerdrProcessError> {
        let envelope = self.invoke(&["agent", "get", name_or_pane]).await?;
        envelope
            .pointer("/result/agent/pane_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .ok_or(HerdrProcessError::InvalidResponse)
    }

    pub async fn pane_get(&self, pane_id: &str) -> Result<Value, HerdrProcessError> {
        let envelope = self.invoke(&["pane", "get", pane_id]).await?;
        if envelope
            .pointer("/result/pane/pane_id")
            .and_then(Value::as_str)
            != Some(pane_id)
        {
            return Err(HerdrProcessError::InvalidResponse);
        }
        Ok(envelope)
    }

    pub async fn focus_agent(&self, name_or_pane: &str) -> Result<Value, HerdrProcessError> {
        let operation = self.operation();
        operation.resolve_target(name_or_pane).await?;
        operation.invoke(&["agent", "focus", name_or_pane]).await
    }

    pub async fn focus_pane(&self, pane_id: &str) -> Result<(), HerdrProcessError> {
        let operation = self.operation();
        operation.pane_get(pane_id).await?;
        let socket = env::var_os("HERDR_SOCKET_PATH").ok_or(HerdrProcessError::Unavailable)?;
        let deadline = operation
            .deadline
            .unwrap()
            .min(Instant::now() + Duration::from_secs(3));
        focus_exchange(&PathBuf::from(socket), pane_id, deadline).await?;
        if operation
            .pane_get(pane_id)
            .await?
            .pointer("/result/pane/focused")
            .and_then(Value::as_bool)
            != Some(true)
        {
            return Err(HerdrProcessError::InvalidResponse);
        }
        Ok(())
    }

    pub async fn split(
        &self,
        pane_id: &str,
        direction: SplitDirection,
        cwd: &Path,
    ) -> Result<String, HerdrProcessError> {
        let direction = match direction {
            SplitDirection::Right => "right",
            SplitDirection::Down => "down",
        };
        let cwd = cwd.to_str().ok_or(HerdrProcessError::InvalidResponse)?;
        let response = self
            .invoke(&[
                "pane",
                "split",
                "--pane",
                pane_id,
                "--direction",
                direction,
                "--cwd",
                cwd,
                "--no-focus",
            ])
            .await?;
        response
            .pointer("/result/pane/pane_id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or(HerdrProcessError::InvalidResponse)
    }

    pub async fn run_pane(&self, pane_id: &str, command: &str) -> Result<(), HerdrProcessError> {
        let output = self.output(&["pane", "run", pane_id, command]).await?;
        if !output.status.success() {
            return Err(HerdrProcessError::Rejected(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ));
        }
        Ok(())
    }

    pub async fn sample(
        &self,
        bridge: &mut HerdrBridge,
    ) -> Result<Vec<BridgeEffect>, HerdrProcessError> {
        let operation = self.operation();
        let (envelope, identity) = operation.authoritative_agent(&bridge.pane_id).await?;
        let reliability = if matches!(identity, AgentSessionIdentity::Verified(_)) {
            LifecycleReliability::Verified
        } else {
            LifecycleReliability::Unverified
        };
        let mut effects = bridge.observe_agent_session(identity);
        let raw =
            serde_json::to_string(&envelope).map_err(|_| HerdrProcessError::InvalidResponse)?;
        effects.extend(
            bridge
                .observe(&raw, reliability)
                .map_err(|_| HerdrProcessError::InvalidResponse)?,
        );
        Ok(effects)
    }

    async fn authoritative_agent(
        &self,
        pane_id: &str,
    ) -> Result<(Value, AgentSessionIdentity), HerdrProcessError> {
        let operation = self.operation();
        let first = operation.get_agent(pane_id).await?;
        let output = operation
            .output(&["agent", "explain", "--json", pane_id])
            .await?;
        let explanation = if output.status.success() {
            serde_json::from_slice(&output.stdout)
                .map_err(|_| HerdrProcessError::InvalidResponse)?
        } else {
            Value::Null
        };
        let latest = operation.get_agent(pane_id).await?;
        let before = first
            .pointer("/result/agent")
            .ok_or(HerdrProcessError::InvalidResponse)?;
        let agent = latest
            .pointer("/result/agent")
            .ok_or(HerdrProcessError::InvalidResponse)?;
        let identity = verified_agent_identity(before, &explanation, agent);
        Ok((latest, identity))
    }

    pub async fn deliver_feedback(
        &self,
        bridge: &mut HerdrBridge,
        report: &ReviewReport,
    ) -> Result<FeedbackDelivery, HerdrProcessError> {
        if bridge
            .pending
            .as_ref()
            .is_none_or(|pending| !pending.in_flight || pending.report.id != report.id)
        {
            return Err(HerdrProcessError::InvalidResponse);
        }
        let operation = self.operation();
        let text = serde_json::to_string(&json!({ "chvrn_review": report }))
            .map_err(|_| HerdrProcessError::InvalidResponse)?;
        let target = bridge
            .verified_identity()
            .ok_or(HerdrProcessError::Unavailable)?;
        if target.pane_id != report.pane_id
            || target.terminal_id != report.terminal_id
            || target.agent_session_id != report.agent_session_id
        {
            return Ok(FeedbackDelivery::Uncertain(
                "review target changed before prompt".into(),
            ));
        }
        let result = operation.send_verified_prompt(&target, &text).await?;
        match &result {
            FeedbackDelivery::Delivered => bridge
                .feedback_delivered(&report.id)
                .map_err(|_| HerdrProcessError::InvalidResponse)?,
            FeedbackDelivery::RejectedBlocked => bridge
                .feedback_failed(&report.id)
                .map_err(|_| HerdrProcessError::InvalidResponse)?,
            FeedbackDelivery::Uncertain(_) => {}
        }
        Ok(result)
    }

    pub async fn request_explanation(
        &self,
        bridge: &HerdrBridge,
        snapshot_id: &str,
        path: &str,
        range: HunkRange,
        question: &str,
    ) -> Result<FeedbackDelivery, HerdrProcessError> {
        if snapshot_id.is_empty()
            || path.is_empty()
            || question.is_empty()
            || bridge.invalidated.contains(snapshot_id)
        {
            return Err(HerdrProcessError::InvalidResponse);
        }
        let operation = self.operation();
        let target = bridge
            .verified_identity()
            .ok_or(HerdrProcessError::Unavailable)?;
        let text = serde_json::to_string(&json!({
            "chvrn_explain": { "snapshot_id": snapshot_id, "path": path, "range": range, "question": question }
        })).map_err(|_| HerdrProcessError::InvalidResponse)?;
        operation.send_verified_prompt(&target, &text).await
    }

    async fn send_verified_prompt(
        &self,
        target: &VerifiedAgentTarget<'_>,
        text: &str,
    ) -> Result<FeedbackDelivery, HerdrProcessError> {
        let (current, identity) = self.authoritative_agent(target.pane_id).await?;
        let agent = current
            .pointer("/result/agent")
            .ok_or(HerdrProcessError::InvalidResponse)?;
        if identity != AgentSessionIdentity::Verified(target.agent_session_id.into())
            || agent.get("terminal_id").and_then(Value::as_str) != Some(target.terminal_id)
            || !matches!(
                agent.get("agent_status").and_then(Value::as_str),
                Some("idle" | "done")
            )
        {
            return Ok(FeedbackDelivery::Uncertain(
                "agent identity or readiness changed before prompt".into(),
            ));
        }
        self.delivery.0.store(true, Ordering::Release);
        let output = match self
            .output(&["agent", "prompt", target.pane_id, text])
            .await
        {
            Ok(output) => output,
            Err(error) => return Ok(FeedbackDelivery::Uncertain(error.to_string())),
        };
        if output.status.success() {
            let result: Value = match serde_json::from_slice(&output.stdout) {
                Ok(result) => result,
                Err(error) => return Ok(FeedbackDelivery::Uncertain(error.to_string())),
            };
            if result.get("id").and_then(Value::as_str) != Some("cli:agent:prompt")
                || result.pointer("/result/type").and_then(Value::as_str) != Some("agent_prompted")
                || result
                    .pointer("/result/agent/pane_id")
                    .and_then(Value::as_str)
                    != Some(target.pane_id)
                || result
                    .pointer("/result/agent/terminal_id")
                    .and_then(Value::as_str)
                    != Some(target.terminal_id)
            {
                return Ok(FeedbackDelivery::Uncertain(
                    "prompt response did not confirm the target pane".into(),
                ));
            }
            if self
                .authoritative_agent(target.pane_id)
                .await
                .is_ok_and(|(_, identity)| {
                    identity == AgentSessionIdentity::Verified(target.agent_session_id.into())
                })
            {
                self.delivery.0.store(false, Ordering::Release);
                return Ok(FeedbackDelivery::Delivered);
            }
            return Ok(FeedbackDelivery::Uncertain(
                "prompt accepted without matching agent session confirmation".into(),
            ));
        }
        let error: Value = serde_json::from_slice(&output.stderr).unwrap_or(Value::Null);
        if error.pointer("/error/code").and_then(Value::as_str) == Some("agent_blocked") {
            self.delivery.0.store(false, Ordering::Release);
            return Ok(FeedbackDelivery::RejectedBlocked);
        }
        Ok(FeedbackDelivery::Uncertain(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ))
    }
}

fn deadline_error() -> HerdrProcessError {
    HerdrProcessError::Io(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "Herdr command deadline exceeded",
    ))
}

async fn capture(mut stream: impl AsyncRead + Unpin) -> Result<Vec<u8>, HerdrProcessError> {
    let mut output = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        let count = stream.read(&mut buffer).await?;
        if count == 0 {
            return Ok(output);
        }
        if output.len() + count > 1_048_576 {
            return Err(HerdrProcessError::Io(std::io::Error::other(
                "Herdr output exceeds 1 MiB limit",
            )));
        }
        output.extend_from_slice(&buffer[..count]);
    }
}

fn verified_agent_identity(
    before: &Value,
    explanation: &Value,
    agent: &Value,
) -> AgentSessionIdentity {
    let Some(name) = agent.get("agent").and_then(Value::as_str) else {
        return AgentSessionIdentity::Unverified;
    };
    let session = agent.get("agent_session");
    let expected_source = format!("herdr:{name}");
    let same_agent = before.get("agent") == agent.get("agent")
        && before.get("terminal_id") == agent.get("terminal_id")
        && before.get("agent_session") == session
        && before.get("state_change_seq") == agent.get("state_change_seq");
    let verified = same_agent
        && explanation
            .get("screen_detection_skipped")
            .and_then(Value::as_bool)
            == Some(true)
        && explanation
            .get("screen_detection_skip_reason")
            .and_then(Value::as_str)
            == Some("full_lifecycle_hook_authority")
        && explanation
            .get("skip_state_update")
            .and_then(Value::as_bool)
            == Some(false)
        && explanation.get("agent").and_then(Value::as_str) == Some(name)
        && session
            .and_then(|value| value.get("source"))
            .and_then(Value::as_str)
            == Some(expected_source.as_str())
        && session
            .and_then(|value| value.get("agent"))
            .and_then(Value::as_str)
            == Some(name)
        && matches!(
            session
                .and_then(|value| value.get("kind"))
                .and_then(Value::as_str),
            Some("id" | "path")
        );
    if !verified {
        return AgentSessionIdentity::Unverified;
    }
    session
        .and_then(|value| value.get("value"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(|value| AgentSessionIdentity::Verified(value.into()))
        .unwrap_or(AgentSessionIdentity::Unverified)
}

async fn focus_exchange(
    socket: &Path,
    pane_id: &str,
    deadline: Instant,
) -> Result<(), HerdrProcessError> {
    timeout_at(deadline, async {
        let mut stream = UnixStream::connect(socket).await?;
        let request = json!({ "id": "chvrn_focus", "method": "pane.focus", "params": { "pane_id": pane_id } });
        let body = serde_json::to_vec(&request).map_err(|_| HerdrProcessError::InvalidResponse)?;
        stream.write_all(&body).await?;
        stream.write_all(b"\n").await?;
        let mut response = Vec::new();
        BufReader::new(stream.take(65_536)).read_until(b'\n', &mut response).await?;
        if response.len() > 65_535 || !response.ends_with(b"\n") {
            return Err(HerdrProcessError::InvalidResponse);
        }
        let result: Value = serde_json::from_slice(&response).map_err(|_| HerdrProcessError::InvalidResponse)?;
        if result.get("id").and_then(Value::as_str) != Some("chvrn_focus") || result.get("error").is_some() {
            return Err(HerdrProcessError::Rejected(result.to_string()));
        }
        Ok(())
    }).await.map_err(|_| deadline_error())?
}

#[cfg(test)]
mod tests {
    use super::{AgentSessionIdentity, verified_agent_identity};
    #[cfg(unix)]
    use super::{
        BridgeEffect, HerdrBridge, HerdrProcess, HerdrProcessError, HunkRange,
        LifecycleReliability, ReviewReport, ReviewedFile,
    };
    use serde_json::json;
    #[cfg(unix)]
    use std::fs;
    #[cfg(unix)]
    use std::io::Write;

    #[cfg(unix)]
    fn write_executable(path: &std::path::Path, contents: impl AsRef<[u8]>) {
        let mut writer = std::process::Command::new("/bin/sh")
            .args(["-c", "umask 077; cat > \"$1\" && chmod 700 \"$1\"", "sh"])
            .arg(path)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let written = writer.stdin.take().unwrap().write_all(contents.as_ref());
        let status = writer.wait().unwrap();
        written.unwrap();
        assert!(status.success());
    }

    #[tokio::test]
    async fn cancellation_during_target_resolution_reaps_the_child() {
        use tokio::io::AsyncReadExt;
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let binary = dir.path().join("herdr");
        let path = dir.path().join("ready.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        write_executable(
            &binary,
            format!(
                "#!/bin/sh\nexec /usr/bin/python3 -c 'import socket,os,time;s=socket.socket(socket.AF_UNIX);s.connect(\"{}\");s.sendall(str(os.getpid()).encode());s.shutdown(socket.SHUT_WR);time.sleep(60)'\n",
                path.display()
            ),
        );
        let supervisor = crate::process::ProcessSupervisor::default();
        let process = HerdrProcess {
            binary,
            supervisor: supervisor.clone(),
            deadline: None,
            delivery: super::DeliveryState::default(),
        };
        let operation = tokio::spawn(async move { process.resolve_target("pane").await });
        let pid: libc::pid_t = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut pid = String::new();
            stream.read_to_string(&mut pid).await.unwrap();
            pid.parse().unwrap()
        })
        .await
        .unwrap();
        operation.abort();
        assert!(operation.await.unwrap_err().is_cancelled());
        tokio::time::timeout(std::time::Duration::from_secs(2), supervisor.shutdown())
            .await
            .unwrap();
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }

    #[tokio::test]
    async fn lost_prompt_confirmation_stays_uncertain_and_is_not_retried() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("herdr");
        let pidfile = dir.path().join("prompt-pid");
        let envelope = json!({"id":"cli:agent:get","result":{"type":"agent_info","agent":{
            "agent":"omp","agent_status":"done","pane_id":"pane","terminal_id":"terminal",
            "revision":1,"state_change_seq":1,
            "agent_session":{"agent":"omp","source":"herdr:omp","kind":"path","value":"/sessions/one.jsonl"}
        }}});
        let explanation = json!({"agent":"omp","screen_detection_skipped":true,"screen_detection_skip_reason":"full_lifecycle_hook_authority","skip_state_update":false});
        write_executable(
            &binary,
            format!(
                "#!/bin/sh\ncase \"$1 $2\" in\n'agent get') printf '%s' '{envelope}';;\n'agent explain') printf '%s' '{explanation}';;\n'agent prompt') printf '%s' $$ > '{}'; exec sleep 60;;\nesac\n",
                pidfile.display()
            ),
        );
        let supervisor = crate::process::ProcessSupervisor::default();
        let mut process = HerdrProcess {
            binary,
            supervisor: supervisor.clone(),
            deadline: None,
            delivery: super::DeliveryState::default(),
        };
        let mut bridge = HerdrBridge::new("pane");
        process.sample(&mut bridge).await.unwrap();
        let report = ReviewReport {
            id: "report".into(),
            pane_id: "pane".into(),
            terminal_id: "terminal".into(),
            agent_session_id: "/sessions/one.jsonl".into(),
            snapshot_id: "snapshot".into(),
            files: vec![],
            comment: "accept".into(),
        };
        bridge.submit(report.clone()).unwrap();
        assert!(matches!(
            process.sample(&mut bridge).await.unwrap().as_slice(),
            [BridgeEffect::SendFeedback { .. }]
        ));
        process.deadline = Some(tokio::time::Instant::now() + std::time::Duration::from_secs(1));
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            process.deliver_feedback(&mut bridge, &report),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(matches!(result, super::FeedbackDelivery::Uncertain(_)));
        assert!(process.delivery_state().unconfirmed());
        assert_eq!(bridge.pending_report(), Some(&report));
        assert!(
            bridge
                .observe(&envelope.to_string(), LifecycleReliability::Verified)
                .unwrap()
                .is_empty()
        );
        supervisor.shutdown().await;
        let pid: libc::pid_t = fs::read_to_string(pidfile).unwrap().parse().unwrap();
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    }

    #[tokio::test]
    async fn hung_child_times_out_and_is_reaped() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("herdr");
        let pidfile = dir.path().join("pid");
        write_executable(
            &binary,
            format!(
                "#!/bin/sh\nprintf '%s' $$ > '{}'\nexec sleep 60\n",
                pidfile.display()
            ),
        );
        let supervisor = crate::process::ProcessSupervisor::default();
        let process = HerdrProcess {
            binary,
            supervisor: supervisor.clone(),
            deadline: None,
            delivery: super::DeliveryState::default(),
        };
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(35),
            process.run_pane("pane", "ignored"),
        )
        .await
        .unwrap();
        assert!(
            matches!(result, Err(HerdrProcessError::Io(error)) if error.kind() == std::io::ErrorKind::TimedOut)
        );
        supervisor.shutdown().await;
        let pid: libc::pid_t = fs::read_to_string(pidfile).unwrap().parse().unwrap();
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }

    #[tokio::test]
    async fn simultaneous_child_streams_are_drained_and_stderr_overflow_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("herdr");
        write_executable(&binary, b"#!/bin/sh\nexec /usr/bin/python3 -c 'import os,threading; t=threading.Thread(target=lambda:os.write(1,b\"x\"*524288));t.start();os.write(2,b\"x\"*524288);t.join()'\n");
        let supervisor = crate::process::ProcessSupervisor::default();
        let process = HerdrProcess {
            binary: binary.clone(),
            supervisor: supervisor.clone(),
            deadline: None,
            delivery: super::DeliveryState::default(),
        };
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            process.run_pane("pane", "ignored"),
        )
        .await
        .unwrap()
        .unwrap();
        write_executable(
            &binary,
            b"#!/bin/sh\nexec /usr/bin/python3 -c 'import os;os.write(2,b\"x\"*2097152)'\n",
        );
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            process.run_pane("pane", "ignored"),
        )
        .await
        .unwrap();
        assert!(
            matches!(result, Err(HerdrProcessError::Io(error)) if error.to_string().contains("1 MiB"))
        );
        supervisor.shutdown().await;
    }

    #[tokio::test]
    async fn focus_socket_trickle_cannot_extend_the_exchange_deadline() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let path = dir.path().join("focus.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let (ready, started) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut byte = [0];
            loop {
                stream.read_exact(&mut byte).await.unwrap();
                if byte[0] == b'\n' {
                    break;
                }
            }
            let _ = ready.send(());
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(50));
            loop {
                interval.tick().await;
                if stream.write_all(b" ").await.is_err() {
                    break;
                }
            }
        });
        let focus = super::focus_exchange(
            &path,
            "pane",
            tokio::time::Instant::now() + std::time::Duration::from_millis(300),
        );
        let (result, handshake) = tokio::join!(
            tokio::time::timeout(std::time::Duration::from_secs(2), focus),
            tokio::time::timeout(std::time::Duration::from_secs(2), started),
        );
        handshake.unwrap().unwrap();
        assert!(
            matches!(result.unwrap(), Err(HerdrProcessError::Io(error)) if error.kind() == std::io::ErrorKind::TimedOut)
        );
        tokio::time::timeout(std::time::Duration::from_secs(1), server)
            .await
            .unwrap()
            .unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pane_run_rejects_oversized_successful_child_output() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("herdr");
        write_executable(
            &binary,
            b"#!/bin/sh\ndd if=/dev/zero bs=1048576 count=2 2>/dev/null\n",
        );
        let supervisor = crate::process::ProcessSupervisor::default();
        let process = HerdrProcess {
            binary,
            supervisor: supervisor.clone(),
            deadline: None,
            delivery: super::DeliveryState::default(),
        };
        assert!(process.run_pane("w9:p5", "ignored").await.is_err());
        drop(process);
        supervisor.shutdown().await;
    }
    #[test]
    fn official_omp_session_path_is_verified_and_replacement_is_not_the_same_session() {
        let explanation = json!({
            "agent": "omp", "screen_detection_skipped": true,
            "screen_detection_skip_reason": "full_lifecycle_hook_authority", "skip_state_update": false
        });
        let first = json!({
            "agent": "omp", "terminal_id": "term-1", "state_change_seq": 5,
            "agent_session": {
                "agent": "omp", "kind": "path", "source": "herdr:omp",
                "value": "/sessions/2026-09-28T09-53-09-731Z_01a0e76e.jsonl"
            }
        });
        assert_eq!(
            verified_agent_identity(&first, &explanation, &first),
            AgentSessionIdentity::Verified(
                "/sessions/2026-09-28T09-53-09-731Z_01a0e76e.jsonl".into()
            )
        );
        let mut replacement = first.clone();
        replacement["agent_session"]["value"] = json!("/sessions/replacement.jsonl");
        assert_eq!(
            verified_agent_identity(&first, &explanation, &replacement),
            AgentSessionIdentity::Unverified
        );
    }

    #[test]
    fn screen_detection_skip_without_official_lifecycle_authority_cannot_verify_identity() {
        let agent = json!({
            "agent": "omp", "terminal_id": "term-1", "state_change_seq": 5,
            "agent_session": {"agent": "omp", "kind": "path", "source": "herdr:omp", "value": "/sessions/one.jsonl"}
        });
        let explanation = json!({
            "agent": "omp", "screen_detection_skipped": true,
            "screen_detection_skip_reason": "unavailable", "skip_state_update": false
        });
        assert_eq!(
            verified_agent_identity(&agent, &explanation, &agent),
            AgentSessionIdentity::Unverified
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pane_run_accepts_success_with_empty_stdout_and_preserves_command_errors() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("herdr");
        write_executable(&binary, b"#!/bin/sh\nif [ \"$1\" = pane ] && [ \"$2\" = run ] && [ \"$3\" = w9:p5 ] && [ \"$4\" = 'echo hello' ]; then exit 0; fi\necho 'unexpected pane run' >&2\nexit 2\n");
        let supervisor = crate::process::ProcessSupervisor::default();
        let process = HerdrProcess {
            binary,
            supervisor: supervisor.clone(),
            deadline: None,
            delivery: super::DeliveryState::default(),
        };
        assert!(process.run_pane("w9:p5", "echo hello").await.is_ok());
        assert!(
            matches!(process.run_pane("w9:p5", "bad command").await, Err(HerdrProcessError::Rejected(message)) if message.contains("unexpected pane run"))
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn completed_official_turn_accepts_feedback_and_explanation_prompts_with_verified_session()
     {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("herdr");
        let payload = dir.path().join("prompt.json");
        let agent = json!({
            "id": "cli:agent:get", "result": {"type": "agent_info", "agent": {
                "agent": "omp", "agent_status": "done", "pane_id": "w9:p5",
                "terminal_id": "term-1", "revision": 31, "state_change_seq": 5,
                "agent_session": {"agent": "omp", "source": "herdr:omp", "kind": "path", "value": "/sessions/one.jsonl"}
            }}
        });
        let explain = json!({
            "agent": "omp", "screen_detection_skipped": true,
            "screen_detection_skip_reason": "full_lifecycle_hook_authority", "skip_state_update": false
        });
        let prompt = json!({
            "id": "cli:agent:prompt", "result": {"type": "agent_prompted", "agent": {
                "pane_id": "w9:p5", "terminal_id": "term-1", "agent_status": "done"
            }}
        });
        let script = format!(
            "#!/bin/sh\ncase \"$1 $2\" in\n 'agent get') printf '%s\\n' '{agent}';;\n 'agent explain') printf '%s\\n' '{explain}';;\n 'agent prompt') printf '%s\\n' \"$4\" > '{payload_path}'; printf '%s\\n' '{prompt}';;\n *) exit 2;;\nesac\n",
            payload_path = payload.display()
        );
        write_executable(&binary, script);
        let supervisor = crate::process::ProcessSupervisor::default();
        let process = HerdrProcess {
            binary,
            supervisor: supervisor.clone(),
            deadline: None,
            delivery: super::DeliveryState::default(),
        };
        let mut bridge = HerdrBridge::new("w9:p5");
        assert!(process.sample(&mut bridge).await.unwrap().is_empty());
        let report = ReviewReport {
            id: "review-one".into(),
            pane_id: "w9:p5".into(),
            terminal_id: "term-1".into(),
            agent_session_id: "/sessions/one.jsonl".into(),
            snapshot_id: "snapshot-17".into(),
            files: vec![
                ReviewedFile {
                    path: "src/main.rs".into(),
                    snapshot_id: "snapshot-17:0".into(),
                    accepted: vec![HunkRange { start: 2, end: 4 }],
                    rejected: vec![],
                },
                ReviewedFile {
                    path: "src/main.rs".into(),
                    snapshot_id: "snapshot-18:0".into(),
                    accepted: vec![],
                    rejected: vec![HunkRange { start: 8, end: 10 }],
                },
            ],
            comment: "accept".into(),
        };
        bridge.submit(report.clone()).unwrap();
        assert!(
            matches!(process.sample(&mut bridge).await.unwrap().as_slice(),
            [BridgeEffect::SendFeedback { report: queued }] if queued.id == "review-one")
        );
        assert_eq!(
            process
                .deliver_feedback(&mut bridge, &report)
                .await
                .unwrap(),
            super::FeedbackDelivery::Delivered
        );
        let sent: serde_json::Value = serde_json::from_slice(&fs::read(&payload).unwrap()).unwrap();
        assert_eq!(
            sent["chvrn_review"]["files"],
            json!([
                {"path": "src/main.rs", "snapshot_id": "snapshot-17:0", "accepted": [{"start": 2, "end": 4}], "rejected": []},
                {"path": "src/main.rs", "snapshot_id": "snapshot-18:0", "accepted": [], "rejected": [{"start": 8, "end": 10}]}
            ])
        );
        assert!(bridge.pending_report().is_none());
        assert!(
            bridge
                .observe(&agent.to_string(), LifecycleReliability::Verified)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            process
                .request_explanation(
                    &bridge,
                    "snapshot-17",
                    "src/main.rs",
                    super::HunkRange { start: 2, end: 4 },
                    "Explain this hunk",
                )
                .await
                .unwrap(),
            super::FeedbackDelivery::Delivered
        );
    }
}
