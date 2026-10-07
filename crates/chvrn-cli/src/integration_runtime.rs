use crate::Result;
use chvrn_integrations::process::ProcessSupervisor;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::{oneshot, watch};

#[derive(Default)]
pub struct IntegrationRuntime {
    state: Mutex<Option<State>>,
    closed: AtomicBool,
}

struct State {
    handle: IntegrationHandle,
    stop: oneshot::Sender<()>,
    thread: std::thread::JoinHandle<()>,
}

#[derive(Clone)]
pub struct IntegrationHandle {
    runtime: tokio::runtime::Handle,
    shutdown: watch::Receiver<bool>,
    tasks: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    pub processes: ProcessSupervisor,
}

pub struct Cancellation(watch::Sender<bool>);

impl Drop for Cancellation {
    fn drop(&mut self) {
        let _ = self.0.send(true);
    }
}

pub struct Shutdown {
    runtime: watch::Receiver<bool>,
    adapter: watch::Receiver<bool>,
}

impl Shutdown {
    pub async fn cancelled(&mut self) {
        if *self.runtime.borrow() || *self.adapter.borrow() {
            return;
        }
        tokio::select! {
            _ = self.runtime.changed() => {},
            _ = self.adapter.changed() => {},
        }
    }
}

impl IntegrationRuntime {
    pub fn handle(&self) -> Result<IntegrationHandle> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "integration supervisor lock failed")?;
        if self.closed.load(Ordering::Acquire) {
            return Err("integration supervisor stopped".into());
        }
        if let Some(state) = state.as_ref() {
            return Ok(state.handle.clone());
        }
        let (ready, startup) = std::sync::mpsc::sync_channel(1);
        let (stop, stopped) = oneshot::channel();
        let thread = std::thread::Builder::new()
            .name("chvrn-integrations".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                let (shutdown, cancelled) = watch::channel(false);
                let tasks = Arc::new(Mutex::new(Vec::<tokio::task::JoinHandle<()>>::new()));
                let processes = ProcessSupervisor::default();
                let handle = IntegrationHandle {
                    runtime: runtime.handle().clone(),
                    shutdown: cancelled,
                    tasks: Arc::clone(&tasks),
                    processes: processes.clone(),
                };
                if ready.send(Ok(handle)).is_err() {
                    return;
                }
                runtime.block_on(async move {
                    let _ = stopped.await;
                    let _ = shutdown.send(true);
                    let pending = std::mem::take(
                        &mut *tasks.lock().unwrap_or_else(|error| error.into_inner()),
                    );
                    for task in pending {
                        let _ = task.await;
                    }
                    processes.shutdown().await;
                });
            })?;
        let handle = match startup.recv() {
            Ok(Ok(handle)) => handle,
            result => {
                let _ = thread.join();
                return Err(match result {
                    Ok(Err(error)) => error.into(),
                    _ => "integration supervisor failed during startup".into(),
                });
            }
        };
        *state = Some(State {
            handle: handle.clone(),
            stop,
            thread,
        });
        Ok(handle)
    }

    pub fn run<F: Future>(&self, future: F) -> Result<F::Output> {
        Ok(self.handle()?.runtime.block_on(future))
    }

    pub fn shutdown(&self) {
        self.closed.store(true, Ordering::Release);
        let state = self
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(state) = state {
            let _ = state.stop.send(());
            let _ = state.thread.join();
        }
    }
}

impl Drop for IntegrationRuntime {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl IntegrationHandle {
    pub fn spawn(&self, work: impl Future<Output = ()> + Send + 'static) -> Result<Cancellation> {
        self.spawn_owned(move |mut shutdown| async move {
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => {},
                _ = work => {},
            }
        })
    }

    pub fn spawn_owned<F, Fut>(&self, work: F) -> Result<Cancellation>
    where
        F: FnOnce(Shutdown) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let mut tasks = self
            .tasks
            .lock()
            .map_err(|_| "integration task registry failed")?;
        if *self.shutdown.borrow() {
            return Err("integration supervisor stopped".into());
        }
        let (cancel, cancelled) = watch::channel(false);
        let shutdown = Shutdown {
            runtime: self.shutdown.clone(),
            adapter: cancelled,
        };
        let task = self.runtime.spawn(work(shutdown));
        tasks.retain(|task| !task.is_finished());
        tasks.push(task);
        Ok(Cancellation(cancel))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn saturated_results_do_not_block_cancellation_or_child_reaping() {
        let (ready, observed) = std::sync::mpsc::sync_channel(1);
        let (stop, until_stop) = std::sync::mpsc::sync_channel(1);
        let (finished, completion) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let runtime = IntegrationRuntime::default();
            let handle = runtime.handle().unwrap();
            let supervisor = handle.processes.clone();
            let (output, _undrained) = tokio::sync::mpsc::channel(1);
            let cancellation = handle
                .spawn(async move {
                    use tokio::io::AsyncBufReadExt;
                    let mut child = supervisor
                        .spawn(
                            tokio::process::Command::new("/bin/sh")
                                .args(["-c", "printf '%s\\n' $$; exec sleep 60"])
                                .stdout(std::process::Stdio::piped()),
                        )
                        .unwrap();
                    let mut reader = tokio::io::BufReader::new(child.stdout.take().unwrap());
                    let mut pid = String::new();
                    reader.read_line(&mut pid).await.unwrap();
                    output.send(1).await.unwrap();
                    ready
                        .send(pid.trim().parse::<libc::pid_t>().unwrap())
                        .unwrap();
                    let _ = output.send(2).await;
                    let _ = child.terminate().await;
                })
                .unwrap();
            until_stop.recv_timeout(Duration::from_secs(5)).unwrap();
            runtime.shutdown();
            drop(cancellation);
            assert!(runtime.handle().is_err());
            finished.send(()).unwrap();
        });
        let pid = observed.recv_timeout(Duration::from_secs(5)).unwrap();
        stop.send(()).unwrap();
        completion.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }
}
