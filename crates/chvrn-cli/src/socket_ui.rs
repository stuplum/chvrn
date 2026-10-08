use crate::Result;
use crate::integration_runtime::{Cancellation, IntegrationRuntime};
use chvrn_git::Review;
use chvrn_integrations::socket::{
    InspectedFile, PatchCandidate, ReviewOutcome, SocketCommand, SocketServer,
};
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};

pub struct DecisionPermit {
    command: mpsc::OwnedPermit<SocketCommand>,
    reply: OwnedSemaphorePermit,
}

pub struct PendingDecision {
    completion: oneshot::Receiver<std::result::Result<(), String>>,
    reply: Option<OwnedSemaphorePermit>,
}

impl DecisionPermit {
    pub fn record(self, snapshot: &str, accepted: bool) -> PendingDecision {
        let (reply, completion) = oneshot::channel();
        self.command.send(SocketCommand::Decision {
            snapshot: snapshot.into(),
            outcome: if accepted {
                ReviewOutcome::Accepted
            } else {
                ReviewOutcome::Declined
            },
            reply,
        });
        PendingDecision {
            completion,
            reply: Some(self.reply),
        }
    }
}

impl PendingDecision {
    pub fn try_complete(&mut self) -> Option<std::result::Result<(), String>> {
        self.reply.as_ref()?;
        let result = match self.completion.try_recv() {
            Ok(result) => result,
            Err(oneshot::error::TryRecvError::Empty) => return None,
            Err(oneshot::error::TryRecvError::Closed) => {
                Err("socket owner stopped before recording receipt".into())
            }
        };
        self.reply.take();
        Some(result)
    }
}

pub struct SocketUi {
    commands: mpsc::Sender<SocketCommand>,
    candidates: Mutex<mpsc::Receiver<PatchCandidate>>,
    errors: Mutex<mpsc::Receiver<String>>,
    replies: Arc<Semaphore>,
    _cancellation: Cancellation,
}

impl SocketUi {
    pub fn new(
        path: &Path,
        root: &Path,
        review: &Review,
        snapshot: &str,
        runtime: &IntegrationRuntime,
    ) -> Result<Self> {
        let parent = path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        if !parent.exists() {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)?;
        }
        let server = SocketServer::bind(path, root, inspected(review, snapshot)?)?;
        let (commands, input) = mpsc::channel(32);
        let (send_candidate, candidates) = mpsc::channel(32);
        let (send_error, errors) = mpsc::channel(32);
        let cancellation =
            runtime
                .handle()?
                .spawn(server.run(input, send_candidate, send_error))?;
        Ok(Self {
            commands,
            candidates: Mutex::new(candidates),
            errors: Mutex::new(errors),
            replies: Arc::new(Semaphore::new(32)),
            _cancellation: cancellation,
        })
    }

    pub fn candidates(&self) -> Vec<PatchCandidate> {
        let mut receiver = self
            .candidates
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut values = Vec::new();
        while let Ok(value) = receiver.try_recv() {
            values.push(value);
        }
        values
    }

    pub fn errors(&self) -> Vec<String> {
        let mut receiver = self
            .errors
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut values = Vec::new();
        while let Ok(value) = receiver.try_recv() {
            values.push(value);
        }
        values
    }

    pub fn refresh(&self, review: &Review, snapshot: &str) -> Result<()> {
        self.commands
            .try_send(SocketCommand::Refresh(inspected(review, snapshot)?))
            .map_err(|_| "socket command queue full or stopped")?;
        Ok(())
    }

    pub fn reserve_decision(&self) -> Result<DecisionPermit> {
        let reply = Arc::clone(&self.replies)
            .try_acquire_owned()
            .map_err(|_| "socket decision acknowledgements full")?;
        let command = self
            .commands
            .clone()
            .try_reserve_owned()
            .map_err(|_| "socket command queue full or stopped")?;
        Ok(DecisionPermit { command, reply })
    }
}

fn inspected(review: &Review, snapshot: &str) -> Result<Vec<InspectedFile>> {
    review.files().iter().enumerate().map(|(index, file)| {
        Ok(InspectedFile {
            relative_path: file.path.to_str().ok_or("socket review paths must be UTF-8; native-byte paths remain available for local review")?.into(),
            snapshot_id: format!("{snapshot}:{index}"),
            bytes: file.worktree.clone().unwrap_or_default(),
        })
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::SocketUi;
    use chvrn_git::{Base, Repository};
    use serde_json::{Value, json};
    use std::fs;
    use std::io::{Read, Write};
    use std::net::Shutdown;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use std::process::Command;
    use std::time::Duration;

    fn request(socket: &Path, message: &Value) -> Value {
        let mut stream = UnixStream::connect(socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let body = serde_json::to_vec(message).unwrap();
        stream
            .write_all(&(body.len() as u32).to_be_bytes())
            .unwrap();
        stream.write_all(&body).unwrap();
        stream.shutdown(Shutdown::Write).unwrap();
        let mut length = [0; 4];
        stream.read_exact(&mut length).unwrap();
        let mut response = vec![0; u32::from_be_bytes(length) as usize];
        stream.read_exact(&mut response).unwrap();
        serde_json::from_slice(&response).unwrap()
    }

    fn complete(mut pending: super::PendingDecision) -> std::result::Result<(), String> {
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(result) = pending.try_complete() {
                return result;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "decision receipt timed out"
            );
            std::thread::yield_now();
        }
    }

    #[test]
    fn decision_confirms_completed_receipt_and_propagates_failed_recording() {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(dir.path())
                .status()
                .unwrap()
                .success()
        );
        fs::write(dir.path().join("file.txt"), b"before\n").unwrap();
        let repo = Repository::open(dir.path()).unwrap();
        let review = repo
            .review(Base::Index, &[std::path::PathBuf::from("file.txt")])
            .unwrap();
        let socket = dir.path().join("chvrn.sock");
        let runtime = crate::integration_runtime::IntegrationRuntime::default();
        let ui = SocketUi::new(&socket, dir.path(), &review, "session-1", &runtime).unwrap();

        let not_issued = complete(ui.reserve_decision().unwrap().record("session-1:0", true));
        assert!(not_issued.is_err());

        let queued = request(
            &socket,
            &json!({
                "type": "patch_candidate",
                "snapshot": "session-1:0",
                "path": "file.txt",
                "patch": "--- a/file.txt\n+++ b/file.txt\n@@ -1 +1 @@\n-before\n+after\n"
            }),
        );
        assert_eq!(queued["status"], "queued");
        complete(ui.reserve_decision().unwrap().record("session-1:0", true)).unwrap();
        let status = request(
            &socket,
            &json!({"type": "review_status", "snapshot": "session-1:0"}),
        );
        assert_eq!(status["status"], "accepted");
        let permits: Vec<_> = (0..32).map(|_| ui.reserve_decision().unwrap()).collect();
        assert!(ui.reserve_decision().is_err());
        drop(permits);
        let mut pending = ui.reserve_decision().unwrap().record("missing", false);
        runtime.shutdown();
        assert!(pending.try_complete().unwrap().is_err());
    }
}
