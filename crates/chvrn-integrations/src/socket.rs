use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use tokio::task::JoinSet;
use tokio::time::{Instant, timeout_at};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectedFile {
    pub relative_path: String,
    pub snapshot_id: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct PatchCandidate {
    pub snapshot_id: String,
    pub path: String,
    pub patch: String,
    _capacity: Arc<OwnedSemaphorePermit>,
}

impl PartialEq for PatchCandidate {
    fn eq(&self, other: &Self) -> bool {
        self.snapshot_id == other.snapshot_id
            && self.path == other.path
            && self.patch == other.patch
    }
}
impl Eq for PatchCandidate {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewOutcome {
    Accepted,
    Declined,
}

pub enum SocketCommand {
    Refresh(Vec<InspectedFile>),
    Decision {
        snapshot: String,
        outcome: ReviewOutcome,
        reply: oneshot::Sender<Result<(), String>>,
    },
}

pub struct SocketServer {
    listener: Option<std::os::unix::net::UnixListener>,
    socket_path: PathBuf,
    socket_identity: (u64, u64),
    root: PathBuf,
    inspected: HashMap<String, InspectedFile>,
    candidates: VecDeque<PatchCandidate>,
    issued_candidates: HashMap<String, Arc<OwnedSemaphorePermit>>,
    outcomes: HashMap<String, ReviewOutcome>,
    capacity: Arc<Semaphore>,
    validation: ValidationLane,
}

struct ValidationJob {
    root: PathBuf,
    file: InspectedFile,
    reply: oneshot::Sender<Result<bool, &'static str>>,
}
struct ValidationLane(std::sync::mpsc::SyncSender<ValidationJob>);

type PendingValidation = (InspectedFile, oneshot::Receiver<Result<bool, &'static str>>);

impl ValidationLane {
    fn new() -> io::Result<Self> {
        let (sender, receiver) = std::sync::mpsc::sync_channel::<ValidationJob>(1);
        std::thread::Builder::new()
            .name("chvrn-socket-preparation".into())
            .spawn(move || {
                while let Ok(job) = receiver.recv() {
                    let valid =
                        file_matches_snapshot(&job.root, &job.file.relative_path, &job.file.bytes);
                    let _ = job.reply.send(valid);
                }
            })?;
        Ok(Self(sender))
    }
    fn start(
        &self,
        root: &Path,
        file: InspectedFile,
    ) -> Result<oneshot::Receiver<Result<bool, &'static str>>, &'static str> {
        let (reply, response) = oneshot::channel();
        self.0
            .try_send(ValidationJob {
                root: root.into(),
                file,
                reply,
            })
            .map_err(|_| "busy")?;
        Ok(response)
    }
}

struct Exchange {
    request: Value,
    reply: oneshot::Sender<Value>,
}
struct Prepared {
    exchange: Exchange,
    inspected: InspectedFile,
    valid: Result<bool, &'static str>,
}

impl SocketServer {
    pub const MAX_FRAME_BYTES: usize = 65_536;

    pub fn bind(
        socket_path: &Path,
        root: &Path,
        inspected: Vec<InspectedFile>,
    ) -> io::Result<Self> {
        let root = root.canonicalize()?;
        if !root.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid inspected repository",
            ));
        }
        let files = collect_inspected(inspected)?;
        let socket_parent = socket_path
            .parent()
            .unwrap_or(Path::new("."))
            .canonicalize()?;
        if fs::metadata(&socket_parent)?.permissions().mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "socket parent must be private to its owner",
            ));
        }
        let socket_name = socket_path.file_name().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "socket path has no name")
        })?;
        let socket_path = socket_parent.join(socket_name);
        let validation = ValidationLane::new()?;
        let listener = std::os::unix::net::UnixListener::bind(&socket_path)?;
        let setup = (|| {
            fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))?;
            listener.set_nonblocking(true)?;
            fs::symlink_metadata(&socket_path)
        })();
        let metadata = match setup {
            Ok(metadata) => metadata,
            Err(error) => {
                let _ = fs::remove_file(&socket_path);
                return Err(error);
            }
        };
        Ok(Self {
            listener: Some(listener),
            socket_path,
            socket_identity: (metadata.dev(), metadata.ino()),
            root,
            inspected: files,
            candidates: VecDeque::new(),
            issued_candidates: HashMap::new(),
            outcomes: HashMap::new(),
            capacity: Arc::new(Semaphore::new(32)),
            validation,
        })
    }

    pub async fn serve_next(&mut self) -> io::Result<()> {
        let listener = UnixListener::from_std(
            self.listener
                .as_ref()
                .ok_or_else(|| io::Error::other("socket owner already running"))?
                .try_clone()?,
        )?;
        let (mut stream, _) = listener.accept().await?;
        timeout_at(Instant::now() + Duration::from_secs(3), async {
            let reply = match read_request(&mut stream).await {
                Ok(request) => {
                    let validation = self.prepare(&request);
                    match validation {
                        Ok(Some((inspected, response))) => {
                            let valid = response.await.unwrap_or(Err("closed"));
                            self.handle_request(request, Some((&inspected, valid)))
                        }
                        Ok(None) => self.handle_request(request, None),
                        Err(reason) => rejected(reason),
                    }
                }
                Err(reason) => rejected(reason),
            };
            write_response(&mut stream, &reply).await
        })
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "socket exchange deadline exceeded"))?
    }

    pub async fn run(
        mut self,
        mut commands: mpsc::Receiver<SocketCommand>,
        candidates: mpsc::Sender<PatchCandidate>,
        errors: mpsc::Sender<String>,
    ) {
        let listener = match self
            .listener
            .take()
            .and_then(|listener| UnixListener::from_std(listener).ok())
        {
            Some(listener) => listener,
            None => {
                let _ = errors.try_send("socket listener unavailable".into());
                return;
            }
        };
        let (requests, mut input) = mpsc::channel::<Exchange>(32);
        let (prepared, mut preparation) = mpsc::channel::<Prepared>(2);
        let mut connections: JoinSet<io::Result<()>> = JoinSet::new();
        let mut validations: JoinSet<()> = JoinSet::new();
        loop {
            tokio::select! {
                biased;
                command = commands.recv() => match command {
                    Some(SocketCommand::Refresh(files)) => {
                        if let Err(error) = self.refresh_inspected(files) { let _ = errors.try_send(error.to_string()); }
                    }
                    Some(SocketCommand::Decision { snapshot, outcome, reply }) => {
                        let _ = reply.send(self.record_review(&snapshot, outcome).map_err(|error| error.to_string()));
                    }
                    None => break,
                },
                Some(result) = preparation.recv() => {
                    if !result.exchange.reply.is_closed() {
                        let value = self.handle_request(result.exchange.request, Some((&result.inspected, result.valid)));
                        let _ = result.exchange.reply.send(value);
                    }
                },
                Some(exchange) = input.recv() => {
                    if exchange.reply.is_closed() { continue; }
                    match self.prepare(&exchange.request) {
                        Ok(Some((inspected, response))) => {
                            let prepared = prepared.clone();
                            validations.spawn(async move {
                                let valid = response.await.unwrap_or(Err("closed"));
                                let _ = prepared.send(Prepared { exchange, inspected, valid }).await;
                            });
                        }
                        Ok(None) => { let value = self.handle_request(exchange.request, None); let _ = exchange.reply.send(value); }
                        Err(reason) => { let _ = exchange.reply.send(rejected(reason)); }
                    }
                },
                permit = candidates.reserve(), if !self.candidates.is_empty() => {
                    match permit {
                        Ok(permit) => { if let Some(candidate) = self.candidates.pop_front() { permit.send(candidate); } }
                        Err(_) => break,
                    }
                },
                Some(result) = connections.join_next(), if !connections.is_empty() => {
                    if let Ok(Err(error)) = result { let _ = errors.try_send(error.to_string()); }
                },
                _ = validations.join_next(), if !validations.is_empty() => {},
                accepted = listener.accept() => match accepted {
                    Ok((stream, _)) if connections.len() < 16 => {
                        let requests = requests.clone();
                        connections.spawn(async move { exchange(stream, requests).await });
                    }
                    Ok((stream, _)) => drop(stream),
                    Err(error) => { let _ = errors.try_send(error.to_string()); }
                },
            }
        }
    }

    pub fn pending_candidates(&self) -> &VecDeque<PatchCandidate> {
        &self.candidates
    }
    pub fn take_pending_candidates(&mut self) -> Vec<PatchCandidate> {
        self.candidates.drain(..).collect()
    }

    pub fn refresh_inspected(&mut self, inspected: Vec<InspectedFile>) -> io::Result<usize> {
        let files = collect_inspected(inspected)?;
        let previous = self.candidates.len();
        self.candidates.retain(|candidate| {
            files
                .get(&candidate.path)
                .is_some_and(|file| file.snapshot_id == candidate.snapshot_id)
        });
        self.issued_candidates
            .retain(|snapshot, _| files.values().any(|file| &file.snapshot_id == snapshot));
        self.inspected = files;
        Ok(previous - self.candidates.len())
    }

    pub fn record_review(&mut self, snapshot: &str, outcome: ReviewOutcome) -> io::Result<()> {
        if self.outcomes.contains_key(snapshot) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "candidate review already completed",
            ));
        }
        if self.issued_candidates.remove(snapshot).is_none() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "candidate snapshot not found",
            ));
        }
        self.candidates
            .retain(|candidate| candidate.snapshot_id != snapshot);
        self.outcomes.insert(snapshot.into(), outcome);
        Ok(())
    }

    fn prepare(&self, request: &Value) -> Result<Option<PendingValidation>, &'static str> {
        if request.get("type").and_then(Value::as_str) != Some("patch_candidate") {
            return Ok(None);
        }
        let path = request
            .get("path")
            .and_then(Value::as_str)
            .ok_or("malformed")?;
        if !safe_relative(path) {
            return Err("outside_root");
        }
        let inspected = self
            .inspected
            .get(path)
            .cloned()
            .unwrap_or_else(|| InspectedFile {
                relative_path: path.into(),
                snapshot_id: String::new(),
                bytes: Vec::new(),
            });
        let response = self.validation.start(&self.root, inspected.clone())?;
        Ok(Some((inspected, response)))
    }

    fn handle_request(
        &mut self,
        request: Value,
        validation: Option<(&InspectedFile, Result<bool, &'static str>)>,
    ) -> Value {
        let Some(kind) = request.get("type").and_then(Value::as_str) else {
            return rejected("malformed");
        };
        if kind == "inspect" {
            return self.inspect_response();
        }
        let Some(snapshot) = request
            .get("snapshot")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        else {
            return rejected("malformed");
        };
        if kind == "review_status" {
            if let Some(outcome) = self.outcomes.get(snapshot) {
                return json!({ "status": match outcome { ReviewOutcome::Accepted => "accepted", ReviewOutcome::Declined => "declined" } });
            }
            if !self
                .inspected
                .values()
                .any(|file| file.snapshot_id == snapshot)
            {
                return rejected("unknown_snapshot");
            }
            return json!({ "status": "pending" });
        }
        if kind != "patch_candidate" {
            return rejected("malformed");
        }
        let (Some(path), Some(patch)) = (
            request.get("path").and_then(Value::as_str),
            request
                .get("patch")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty()),
        ) else {
            return rejected("malformed");
        };
        let Some((expected, valid)) = validation else {
            return rejected("stale_snapshot");
        };
        let valid = match valid {
            Ok(valid) => valid,
            Err(reason) => return rejected(reason),
        };
        if !self.inspected.contains_key(path) {
            return rejected("unknown_snapshot");
        }
        if !valid {
            return rejected("stale_snapshot");
        }
        if self.inspected.get(path) != Some(expected) || expected.snapshot_id != snapshot {
            return rejected("stale_snapshot");
        }
        if self.issued_candidates.contains_key(snapshot) || self.outcomes.contains_key(snapshot) {
            return rejected("already_issued");
        }
        let Ok(capacity) = Arc::clone(&self.capacity).try_acquire_owned() else {
            return rejected("busy");
        };
        let capacity = Arc::new(capacity);
        self.issued_candidates
            .insert(snapshot.into(), Arc::clone(&capacity));
        self.candidates.push_back(PatchCandidate {
            snapshot_id: snapshot.into(),
            path: path.into(),
            patch: patch.into(),
            _capacity: capacity,
        });
        json!({ "status": "queued" })
    }

    fn inspect_response(&self) -> Value {
        let mut estimated = 36_usize;
        for file in self.inspected.values() {
            estimated = estimated.saturating_add(
                file.relative_path
                    .len()
                    .saturating_add(file.snapshot_id.len())
                    .saturating_add(32),
            );
            if estimated > Self::MAX_FRAME_BYTES {
                return rejected("too_large");
            }
        }
        let mut files: Vec<_> = self.inspected.values().collect();
        files.sort_unstable_by(|left, right| left.relative_path.cmp(&right.relative_path));
        json!({ "status": "inspected", "files": files.into_iter().map(|file| json!({ "path": file.relative_path, "snapshot": file.snapshot_id })).collect::<Vec<_>>() })
    }
}

impl Drop for SocketServer {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.socket_path).is_ok_and(|metadata| {
            metadata.file_type().is_socket()
                && (metadata.dev(), metadata.ino()) == self.socket_identity
        }) {
            let _ = fs::remove_file(&self.socket_path);
        }
    }
}

async fn exchange(mut stream: UnixStream, requests: mpsc::Sender<Exchange>) -> io::Result<()> {
    timeout_at(Instant::now() + Duration::from_secs(3), async {
        let reply = match read_request(&mut stream).await {
            Ok(request) => {
                let (reply, response) = oneshot::channel();
                match requests.try_send(Exchange { request, reply }) {
                    Ok(()) => response.await.unwrap_or_else(|_| rejected("closed")),
                    Err(_) => rejected("busy"),
                }
            }
            Err(reason) => rejected(reason),
        };
        write_response(&mut stream, &reply).await
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "socket exchange deadline exceeded"))?
}

fn collect_inspected(inspected: Vec<InspectedFile>) -> io::Result<HashMap<String, InspectedFile>> {
    let mut files = HashMap::with_capacity(inspected.len());
    let mut snapshots = HashSet::with_capacity(inspected.len());
    for file in inspected {
        if file.snapshot_id.is_empty()
            || !safe_relative(&file.relative_path)
            || !snapshots.insert(file.snapshot_id.clone())
            || files.insert(file.relative_path.clone(), file).is_some()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid inspected path",
            ));
        }
    }
    Ok(files)
}

fn file_matches_snapshot(
    root: &Path,
    relative: &str,
    expected: &[u8],
) -> Result<bool, &'static str> {
    let mut file = open_regular(root, relative).map_err(|_| "outside_root")?;
    let metadata = file.metadata().map_err(|_| "outside_root")?;
    if !metadata.is_file() {
        return Err("outside_root");
    }
    if metadata.len() != expected.len() as u64 {
        return Ok(false);
    }
    let mut offset = 0;
    let mut chunk = [0_u8; 8192];
    while offset < expected.len() {
        let count = chunk.len().min(expected.len() - offset);
        if file.read_exact(&mut chunk[..count]).is_err()
            || chunk[..count] != expected[offset..offset + count]
        {
            return Ok(false);
        }
        offset += count;
    }
    Ok(file.read(&mut chunk[..1]).is_ok_and(|count| count == 0))
}

fn open_regular(root: &Path, relative: &str) -> io::Result<std::fs::File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    let mut directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(root)?;
    let mut components = Path::new(relative).components().peekable();
    while let Some(Component::Normal(component)) = components.next() {
        let name = std::ffi::CString::new(component.as_bytes()).map_err(io::Error::other)?;
        let flags = libc::O_RDONLY
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | libc::O_NONBLOCK
            | if components.peek().is_some() {
                libc::O_DIRECTORY
            } else {
                0
            };
        let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        directory = unsafe { std::fs::File::from_raw_fd(fd) };
    }
    Ok(directory)
}

fn rejected(reason: &str) -> Value {
    json!({ "status": "rejected", "reason": reason })
}
fn safe_relative(path: &str) -> bool {
    let candidate = Path::new(path);
    !path.is_empty()
        && !candidate.is_absolute()
        && candidate
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

async fn read_request(stream: &mut UnixStream) -> Result<Value, &'static str> {
    let mut length = [0_u8; 4];
    stream
        .read_exact(&mut length)
        .await
        .map_err(|_| "malformed")?;
    let length = u32::from_be_bytes(length) as usize;
    if length > SocketServer::MAX_FRAME_BYTES {
        return Err("too_large");
    }
    if length == 0 {
        return Err("malformed");
    }
    let mut payload = vec![0; length];
    stream
        .read_exact(&mut payload)
        .await
        .map_err(|_| "malformed")?;
    let request: Value = serde_json::from_slice(&payload).map_err(|_| "malformed")?;
    if !request.is_object() {
        return Err("malformed");
    }
    Ok(request)
}
async fn write_response(stream: &mut UnixStream, response: &Value) -> io::Result<()> {
    let mut body = serde_json::to_vec(response).map_err(io::Error::other)?;
    if body.len() > SocketServer::MAX_FRAME_BYTES {
        body = serde_json::to_vec(&rejected("too_large")).map_err(io::Error::other)?;
    }
    stream.write_all(&(body.len() as u32).to_be_bytes()).await?;
    stream.write_all(&body).await
}
