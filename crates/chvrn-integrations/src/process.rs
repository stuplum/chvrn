use std::io;
use std::process::ExitStatus;
use std::sync::{Arc, Mutex};
use tokio::process::{ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;

#[derive(Clone, Default)]
pub struct ProcessSupervisor {
    children: Arc<Mutex<Children>>,
}

#[derive(Default)]
struct Children {
    closed: bool,
    tasks: Vec<(watch::Sender<bool>, JoinHandle<()>)>,
}

impl ProcessSupervisor {
    pub fn spawn(&self, command: &mut Command) -> io::Result<OwnedChild> {
        let mut children = self
            .children
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if children.closed {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "process supervisor stopped",
            ));
        }
        let mut child = command.kill_on_drop(true).spawn()?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let (cancel, mut cancellation) = watch::channel(false);
        let (status, completion) = oneshot::channel();
        let task = tokio::spawn(async move {
            let result = tokio::select! {
                result = child.wait() => result,
                _ = cancellation.changed() => {
                    let _ = child.start_kill();
                    child.wait().await
                }
            };
            let _ = status.send(result);
        });
        children.tasks.retain(|(_, task)| !task.is_finished());
        children.tasks.push((cancel.clone(), task));
        Ok(OwnedChild {
            stdin,
            stdout,
            stderr,
            cancel: Some(cancel),
            completion: Some(completion),
        })
    }

    pub async fn shutdown(&self) {
        let tasks = {
            let mut children = self
                .children
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            children.closed = true;
            std::mem::take(&mut children.tasks)
        };
        for (cancel, _) in &tasks {
            let _ = cancel.send(true);
        }
        for (_, task) in tasks {
            let _ = task.await;
        }
    }
}

pub struct OwnedChild {
    pub stdin: Option<ChildStdin>,
    pub stdout: Option<ChildStdout>,
    pub stderr: Option<ChildStderr>,
    cancel: Option<watch::Sender<bool>>,
    completion: Option<oneshot::Receiver<io::Result<ExitStatus>>>,
}

impl OwnedChild {
    pub async fn wait(&mut self) -> io::Result<ExitStatus> {
        let completion = self
            .completion
            .as_mut()
            .ok_or_else(|| io::Error::other("child already reaped"))?;
        let result = completion
            .await
            .map_err(|_| io::Error::other("child owner stopped before reaping"))?;
        self.completion.take();
        result
    }

    pub async fn terminate(&mut self) -> io::Result<ExitStatus> {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(true);
        }
        self.wait().await
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(true);
        }
    }
}
