use crate::Result;
use chvrn_tui::{DiffCompletion, DiffRequest};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::time::{Duration, Instant};

pub struct FileWatch {
    _watcher: RecommendedWatcher,
    events: Receiver<notify::Result<Event>>,
    changed_at: Option<Instant>,
}

impl FileWatch {
    pub fn new(paths: &[&Path], recursive: bool) -> Result<Self> {
        let (send, events) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |event| {
            let _ = send.send(event);
        })?;
        let mut roots = std::collections::BTreeSet::new();
        for path in paths {
            let root = if path.is_dir() {
                *path
            } else {
                path.parent().unwrap_or(Path::new("."))
            };
            if roots.insert(root.to_path_buf()) {
                watcher.watch(
                    root,
                    if recursive {
                        RecursiveMode::Recursive
                    } else {
                        RecursiveMode::NonRecursive
                    },
                )?;
            }
        }
        Ok(Self {
            _watcher: watcher,
            events,
            changed_at: None,
        })
    }

    pub fn changed(&mut self) -> Result<bool> {
        for event in self.events.try_iter() {
            let event = event?;
            if !matches!(event.kind, EventKind::Access(_)) {
                self.changed_at = Some(Instant::now());
            }
        }
        if self
            .changed_at
            .is_some_and(|time| time.elapsed() >= Duration::from_millis(120))
        {
            self.changed_at = None;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

pub struct BackgroundDiff {
    requests: SyncSender<DiffRequest>,
    results: Receiver<DiffCompletion>,
    pending: Option<DiffRequest>,
}

impl BackgroundDiff {
    pub fn new() -> Self {
        let (requests, input) = mpsc::sync_channel::<DiffRequest>(1);
        let (output, results) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(mut request) = input.recv() {
                while let Ok(newer) = input.try_recv() {
                    request = newer;
                }
                if output.send(request.compute()).is_err() {
                    break;
                }
            }
        });
        Self {
            requests,
            results,
            pending: None,
        }
    }

    pub fn request(&mut self, request: DiffRequest) {
        self.pending = Some(request);
        self.flush();
    }

    fn flush(&mut self) {
        if let Some(request) = self.pending.take() {
            if let Err(TrySendError::Full(request)) = self.requests.try_send(request) {
                self.pending = Some(request);
            }
        }
    }

    pub fn latest(&mut self) -> Option<DiffCompletion> {
        self.flush();
        self.results.try_iter().last()
    }
}
