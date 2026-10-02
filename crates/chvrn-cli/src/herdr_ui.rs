use crate::{HerdrMode, Options, Result, ReviewArgs};
use chvrn_git::{GitError, Repository, Review};
use chvrn_integrations::herdr::{
    BridgeEffect, FeedbackDelivery, HerdrBridge, HerdrProcess, HunkRange, ReviewReport,
    ReviewedFile, SplitDirection,
};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    mpsc::{self, Receiver, RecvTimeoutError, Sender},
};
use std::time::Duration;

#[derive(Clone, PartialEq, Eq)]
pub struct Identity {
    pane: String,
    terminal: String,
    session: String,
}

pub enum Notice {
    Identity(Option<Identity>),
    Offer,
    Invalidated,
    Delivered,
    Message(String),
    Failed(String),
}

enum Command {
    Submit {
        report: ReviewReport,
        review: Arc<Review>,
        root: PathBuf,
    },
    Invalidate(String),
    Explain {
        snapshot: String,
        path: String,
        range: HunkRange,
    },
}

pub struct HerdrUi {
    sender: Option<Sender<Command>>,
    notices: Receiver<Notice>,
    worker: Option<std::thread::JoinHandle<()>>,
    identity: Option<Identity>,
    pub pending: bool,
    pub mode: HerdrMode,
}

impl HerdrUi {
    pub fn new(options: &Options) -> Result<Option<Self>> {
        let in_herdr = std::env::var("HERDR_ENV").as_deref() == Ok("1");
        if !in_herdr && options.herdr.is_none() {
            return Ok(None);
        }
        let mode = options.herdr.unwrap_or(HerdrMode::Auto);
        let target = options
            .agent
            .clone()
            .or_else(|| std::env::var("HERDR_PANE_ID").ok())
            .ok_or("herdr mode requires --agent NAME_OR_PANE")?;
        let process = HerdrProcess::from_environment()?;
        let pane = process.resolve_target(&target)?;
        let own_pane = std::env::var("HERDR_PANE_ID").ok();
        let (sender, commands) = mpsc::channel();
        let (output, notices) = mpsc::channel();
        let worker =
            std::thread::spawn(move || run_worker(process, pane, own_pane, mode, commands, output));
        Ok(Some(Self {
            sender: Some(sender),
            notices,
            worker: Some(worker),
            identity: None,
            pending: false,
            mode,
        }))
    }

    pub fn notices(&mut self) -> Vec<Notice> {
        let notices: Vec<_> = self.notices.try_iter().collect();
        for notice in &notices {
            match notice {
                Notice::Identity(identity) => self.identity = identity.clone(),
                Notice::Delivered | Notice::Invalidated | Notice::Failed(_) => self.pending = false,
                _ => {}
            }
        }
        notices
    }

    pub fn prepare_report(
        &self,
        snapshot: String,
        files: Vec<ReviewedFile>,
        comment: String,
    ) -> Result<ReviewReport> {
        if self.pending {
            return Err("a submitted review is already awaiting feedback delivery".into());
        }
        let identity = self.identity.as_ref().ok_or("cannot deliver feedback: official lifecycle/session identity is not verified for the selected agent")?;
        Ok(ReviewReport {
            id: crate::repository::snapshot_id(),
            pane_id: identity.pane.clone(),
            terminal_id: identity.terminal.clone(),
            agent_session_id: identity.session.clone(),
            snapshot_id: snapshot,
            files,
            comment,
        })
    }

    pub fn submit(
        &mut self,
        review: Arc<Review>,
        root: PathBuf,
        report: ReviewReport,
    ) -> Result<()> {
        if self.pending {
            return Err("a submitted review is already awaiting feedback delivery".into());
        }
        self.sender
            .as_ref()
            .ok_or("herdr worker stopped")?
            .send(Command::Submit {
                report,
                review,
                root,
            })?;
        self.pending = true;
        Ok(())
    }

    pub fn invalidate(&self, snapshot: &str) {
        if let Some(sender) = &self.sender {
            let _ = sender.send(Command::Invalidate(snapshot.into()));
        }
    }

    pub fn explain(
        &self,
        snapshot: &str,
        path: &Path,
        range: std::ops::Range<usize>,
    ) -> Result<()> {
        let path = path.to_str().ok_or("agent explanations require a UTF-8 path; native-byte paths remain available for local review")?;
        self.sender
            .as_ref()
            .ok_or("herdr worker stopped")?
            .send(Command::Explain {
                snapshot: snapshot.into(),
                path: path.into(),
                range: HunkRange {
                    start: range.start,
                    end: range.end,
                },
            })?;
        Ok(())
    }
}

impl Drop for HerdrUi {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn current_identity(bridge: &HerdrBridge) -> Option<Identity> {
    bridge.verified_identity().map(|identity| Identity {
        pane: identity.pane_id.into(),
        terminal: identity.terminal_id.into(),
        session: identity.agent_session_id.into(),
    })
}

fn validate_inspected(root: &Path, review: &Review) -> std::result::Result<(), GitError> {
    Repository::open(root)?.validate_review(review)
}

fn accept_submission(
    bridge: &mut HerdrBridge,
    inspected: &mut Option<(Arc<Review>, PathBuf)>,
    report: ReviewReport,
    review: Arc<Review>,
    root: PathBuf,
) -> Notice {
    if validate_inspected(&root, &review).is_err() {
        bridge.invalidate_snapshot(&report.snapshot_id);
        Notice::Invalidated
    } else if let Err(error) = bridge.submit(report) {
        Notice::Failed(format!(
            "Review target changed before submission: {error:?}"
        ))
    } else {
        *inspected = Some((review, root));
        Notice::Message("Review submitted. Feedback waits for verified input readiness; no permission dialog is approved".into())
    }
}

fn feedback_review_is_current(
    bridge: &mut HerdrBridge,
    inspected: &Option<(Arc<Review>, PathBuf)>,
    report: &ReviewReport,
) -> bool {
    if inspected
        .as_ref()
        .is_some_and(|(review, root)| validate_inspected(root, review).is_ok())
    {
        true
    } else {
        bridge.invalidate_snapshot(&report.snapshot_id);
        false
    }
}

fn run_worker(
    process: HerdrProcess,
    pane: String,
    own_pane: Option<String>,
    mode: HerdrMode,
    commands: Receiver<Command>,
    output: Sender<Notice>,
) {
    let mut bridge = HerdrBridge::new(&pane);
    let mut identity = None;
    let mut inspected: Option<(Arc<Review>, PathBuf)> = None;
    let mut previous_error = String::new();
    if mode == HerdrMode::Gate {
        let _ = output.send(Notice::Offer);
    }
    loop {
        let mut effects = match process.sample(&mut bridge) {
            Ok(effects) => {
                previous_error.clear();
                effects
            }
            Err(error) => {
                let message = format!("Herdr lifecycle unavailable: {error}");
                if previous_error != message {
                    let _ = output.send(Notice::Message(message.clone()));
                    previous_error = message;
                }
                Vec::new()
            }
        };
        let current = current_identity(&bridge);
        if current != identity {
            if identity.is_some() {
                let _ = output.send(Notice::Invalidated);
            }
            identity = current.clone();
            let _ = output.send(Notice::Identity(current));
        }
        for effect in effects.drain(..) {
            match effect {
                BridgeEffect::OfferReview { .. } if mode != HerdrMode::Companion => {
                    let _ = output.send(Notice::Offer);
                    if let Some(own) = &own_pane {
                        if own != &pane {
                            if let Err(error) = process.focus_pane(own) {
                                let _ = output.send(Notice::Message(format!(
                                    "Review ready; focus unchanged because: {error}"
                                )));
                            }
                        }
                    }
                }
                BridgeEffect::OfferReview { .. } => {}
                BridgeEffect::InvalidateReview { .. } => {
                    let _ = output.send(Notice::Invalidated);
                }
                BridgeEffect::SendFeedback { report } => {
                    if !feedback_review_is_current(&mut bridge, &inspected, &report) {
                        let _ = output.send(Notice::Invalidated);
                        continue;
                    }
                    match process.deliver_feedback(&mut bridge, &report) {
                        Ok(FeedbackDelivery::Delivered) => {
                            let _ = output.send(Notice::Delivered);
                        }
                        Ok(FeedbackDelivery::RejectedBlocked) => {
                            let _ = output.send(Notice::Message("Feedback remains pending; the agent is blocked. No approval keys were sent".into()));
                        }
                        Ok(FeedbackDelivery::Uncertain(message)) => {
                            let _ = output.send(Notice::Failed(format!(
                                "Feedback delivery uncertain; not retrying automatically: {message}"
                            )));
                        }
                        Err(error) => {
                            let _ = output.send(Notice::Failed(format!(
                                "Feedback transport failed; not retrying automatically: {error}"
                            )));
                        }
                    }
                }
            }
        }
        match commands.recv_timeout(Duration::from_millis(350)) {
            Ok(Command::Submit { report, review, root }) => {
                let notice = accept_submission(&mut bridge, &mut inspected, report, review, root);
                let _ = output.send(notice);
            }
            Ok(Command::Invalidate(snapshot)) => {
                bridge.invalidate_snapshot(&snapshot);
                let _ = output.send(Notice::Invalidated);
            }
            Ok(Command::Explain { snapshot, path, range }) => {
                match process.request_explanation(&bridge, &snapshot, &path, range, "Explain this hunk's intent and risks in your pane. Any proposed patch remains a suggestion, not permission to edit or approve.") {
                    Ok(FeedbackDelivery::Delivered) => { let _ = output.send(Notice::Message("Hunk explanation requested from the selected agent; any suggestion still requires explicit review".into())); }
                    Ok(result) => { let _ = output.send(Notice::Message(format!("Explanation not confirmed; no automatic retry: {result:?}"))); }
                    Err(error) => { let _ = output.send(Notice::Message(format!("Explanation unavailable: {error}"))); }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => { bridge.quit(); break; }
        }
    }
}

pub fn open_companion(args: &ReviewArgs, options: &Options, root: &Path, base: &str) -> Result<u8> {
    let process = HerdrProcess::from_environment()?;
    let target = options
        .agent
        .clone()
        .or_else(|| std::env::var("HERDR_PANE_ID").ok())
        .ok_or("--open-companion needs an explicit agent target")?;
    let target = process.resolve_target(&target)?;
    let caller = std::env::var("HERDR_PANE_ID").map_err(|_| "caller pane is unavailable")?;
    let pane = process.split(&caller, SplitDirection::Right, root)?;
    let binary = std::env::current_exe()?;
    let mut words = vec![
        binary.into_os_string(),
        "review".into(),
        "--base".into(),
        base.into(),
        "--herdr".into(),
        match options.herdr.unwrap_or(HerdrMode::Auto) {
            HerdrMode::Auto => "auto",
            HerdrMode::Companion => "companion",
            HerdrMode::Gate => "gate",
        }
        .into(),
        "--agent".into(),
        target.into(),
    ];
    if options.jev {
        words.push("--jev".into());
    }
    if let Some(theme) = &options.theme {
        words.push("--theme".into());
        words.push(theme.into());
    }
    for (flag, path) in [
        ("--patch", &args.patch),
        ("--socket", &args.socket),
        ("--report", &args.report),
        ("--export-patch", &args.export_patch),
    ] {
        if let Some(path) = path {
            if path == Path::new("-") {
                return Err("a companion cannot inherit piped stdin; supply a patch file".into());
            }
            words.push(flag.into());
            words.push(path.as_os_str().to_owned());
        }
    }
    if let Some(server) = &options.lsp {
        words.push("--lsp".into());
        words.push(server.as_os_str().to_owned());
        for arg in &options.lsp_args {
            words.push("--lsp-arg".into());
            words.push(arg.clone());
        }
    }
    words.push("--".into());
    words.extend(args.paths.iter().map(|path| path.as_os_str().to_owned()));
    let command = words
        .iter()
        .map(|word| {
            word.to_str()
                .map(shell_quote)
                .ok_or("companion shell arguments must be UTF-8")
        })
        .collect::<std::result::Result<Vec<_>, _>>()?
        .join(" ");
    process.run_pane(&pane, &command)?;
    println!(
        "Opened chvrn companion in {pane}; caller focus and working directory were not changed"
    );
    Ok(0)
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::{Notice, accept_submission, feedback_review_is_current};
    use chvrn_git::{Base, Repository};
    use chvrn_integrations::herdr::{AgentSessionIdentity, HerdrBridge, ReviewReport};
    use std::fs;
    use std::path::Path;
    use std::process::Command;
    use std::sync::Arc;

    fn git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn review_delivery_rejects_index_only_changes_after_submission_when_worktree_bytes_match() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        git(root, &["init", "--quiet"]);
        fs::write(root.join("file.txt"), b"original\n").unwrap();
        git(root, &["add", "--", "file.txt"]);

        let repo = Repository::open(root).unwrap();
        let review = Arc::new(
            repo.review(Base::Index, &[std::path::PathBuf::from("file.txt")])
                .unwrap(),
        );
        let report = ReviewReport {
            id: "review-one".into(),
            pane_id: "w9:p5".into(),
            terminal_id: "term-one".into(),
            agent_session_id: "session-one".into(),
            snapshot_id: "snapshot-one".into(),
            files: Vec::new(),
            comment: "accept".into(),
        };
        let mut bridge = HerdrBridge::new("w9:p5");
        bridge.observe_agent_session(AgentSessionIdentity::Verified("session-one".into()));
        let mut inspected = None;
        let notice = accept_submission(
            &mut bridge,
            &mut inspected,
            report.clone(),
            review.clone(),
            root.to_path_buf(),
        );
        assert!(matches!(notice, Notice::Message(_)));
        assert!(bridge.pending_report().is_some());

        fs::write(root.join("file.txt"), b"new index\n").unwrap();
        git(root, &["add", "--", "file.txt"]);
        fs::write(root.join("file.txt"), b"original\n").unwrap();

        assert_eq!(fs::read(root.join("file.txt")).unwrap(), b"original\n");
        assert_eq!(
            review.files()[0].worktree.as_deref(),
            Some(b"original\n".as_slice())
        );
        assert!(!feedback_review_is_current(
            &mut bridge,
            &inspected,
            &report
        ));
        assert!(bridge.pending_report().is_none());
    }
}
