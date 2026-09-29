use crate::Result;
use chvrn_git::Review;
use chvrn_integrations::socket::{InspectedFile, PatchCandidate, ReviewOutcome, SocketServer};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

enum Command {
    Refresh(Vec<InspectedFile>),
    Decision {
        snapshot: String,
        outcome: ReviewOutcome,
        reply: Sender<std::io::Result<()>>,
    },
}

pub struct SocketUi {
    commands: Option<Sender<Command>>,
    candidates: Receiver<PatchCandidate>,
    errors: Receiver<String>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl SocketUi {
    pub fn new(path: &Path, root: &Path, review: &Review, snapshot: &str) -> Result<Self> {
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
        let mut server = SocketServer::bind(path, root, inspected(review, snapshot)?)?;
        server.set_nonblocking(true)?;
        let (commands, input) = mpsc::channel();
        let (send_candidate, candidates) = mpsc::channel();
        let (send_error, errors) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            loop {
                match server.poll() {
                    Ok(true) => {
                        for candidate in server.take_pending_candidates() {
                            if send_candidate.send(candidate).is_err() {
                                return;
                            }
                        }
                    }
                    Ok(false) => {}
                    Err(error) => {
                        let _ = send_error.send(error.to_string());
                    }
                }
                match input.recv_timeout(Duration::from_millis(30)) {
                    Ok(Command::Refresh(files)) => {
                        if let Err(error) = server.refresh_inspected(files) {
                            let _ = send_error.send(error.to_string());
                        }
                    }
                    Ok(Command::Decision {
                        snapshot,
                        outcome,
                        reply,
                    }) => {
                        let _ = reply.send(server.record_review(&snapshot, outcome));
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
        });
        Ok(Self {
            commands: Some(commands),
            candidates,
            errors,
            worker: Some(worker),
        })
    }

    pub fn candidates(&self) -> Vec<PatchCandidate> {
        self.candidates.try_iter().collect()
    }
    pub fn errors(&self) -> Vec<String> {
        self.errors.try_iter().collect()
    }

    pub fn refresh(&self, review: &Review, snapshot: &str) -> Result<()> {
        self.commands
            .as_ref()
            .ok_or("socket worker stopped")?
            .send(Command::Refresh(inspected(review, snapshot)?))?;
        Ok(())
    }

    pub fn decision(&self, snapshot: &str, accepted: bool) -> Result<()> {
        let (reply, acknowledgement) = mpsc::channel();
        self.commands
            .as_ref()
            .ok_or("socket worker stopped")?
            .send(Command::Decision {
                snapshot: snapshot.into(),
                outcome: if accepted {
                    ReviewOutcome::Accepted
                } else {
                    ReviewOutcome::Declined
                },
                reply,
            })?;
        acknowledgement.recv_timeout(Duration::from_secs(3))??;
        Ok(())
    }
}

impl Drop for SocketUi {
    fn drop(&mut self) {
        self.commands.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
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
        let ui = SocketUi::new(&socket, dir.path(), &review, "session-1").unwrap();

        let not_issued = ui.decision("session-1:0", true).unwrap_err();
        assert_eq!(
            not_issued.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::NotFound
        );

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
        ui.decision("session-1:0", true).unwrap();
        let status = request(
            &socket,
            &json!({"type": "review_status", "snapshot": "session-1:0"}),
        );
        assert_eq!(status["status"], "accepted");
    }
}
