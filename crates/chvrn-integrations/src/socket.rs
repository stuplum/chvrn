use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectedFile {
    pub relative_path: String,
    pub snapshot_id: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchCandidate {
    pub snapshot_id: String,
    pub path: String,
    pub patch: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewOutcome {
    Accepted,
    Declined,
}

pub struct SocketServer {
    listener: UnixListener,
    socket_path: PathBuf,
    socket_identity: (u64, u64),
    root: PathBuf,
    inspected: HashMap<String, InspectedFile>,
    candidates: Vec<PatchCandidate>,
    issued_candidates: HashSet<String>,
    outcomes: HashMap<String, ReviewOutcome>,
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
        let listener = UnixListener::bind(&socket_path)?;
        if let Err(error) = fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600)) {
            let _ = fs::remove_file(&socket_path);
            return Err(error);
        }
        let metadata = fs::symlink_metadata(&socket_path)?;
        Ok(Self {
            listener,
            socket_path,
            socket_identity: (metadata.dev(), metadata.ino()),
            root,
            inspected: files,
            candidates: Vec::new(),
            issued_candidates: HashSet::new(),
            outcomes: HashMap::new(),
        })
    }

    pub fn set_nonblocking(&self, enabled: bool) -> io::Result<()> {
        self.listener.set_nonblocking(enabled)
    }

    pub fn poll(&mut self) -> io::Result<bool> {
        match self.listener.accept() {
            Ok((stream, _)) => {
                self.serve_stream(stream)?;
                Ok(true)
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub fn serve_next(&mut self) -> io::Result<()> {
        let (stream, _) = self.listener.accept()?;
        self.serve_stream(stream)
    }

    fn serve_stream(&mut self, mut stream: UnixStream) -> io::Result<()> {
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(Duration::from_secs(3)))?;
        stream.set_write_timeout(Some(Duration::from_secs(3)))?;
        let reply = match read_request(&mut stream) {
            Ok(request) => self.handle_request(request),
            Err(reason) => json!({ "status": "rejected", "reason": reason }),
        };
        write_response(&mut stream, &reply)
    }

    pub fn pending_candidates(&self) -> &[PatchCandidate] {
        &self.candidates
    }

    pub fn take_pending_candidates(&mut self) -> Vec<PatchCandidate> {
        std::mem::take(&mut self.candidates)
    }

    pub fn refresh_inspected(&mut self, inspected: Vec<InspectedFile>) -> io::Result<usize> {
        let files = collect_inspected(inspected)?;
        let previous = self.candidates.len();
        let root = &self.root;
        self.candidates.retain(|candidate| {
            files.get(&candidate.path).is_some_and(|file| {
                file.snapshot_id == candidate.snapshot_id
                    && safe_target(root, &root.join(&candidate.path))
                    && file_matches_snapshot(&root.join(&candidate.path), &file.bytes)
            })
        });
        self.issued_candidates
            .retain(|snapshot| files.values().any(|file| &file.snapshot_id == snapshot));
        self.inspected = files;
        Ok(previous - self.candidates.len())
    }

    pub fn record_review(&mut self, snapshot_id: &str, outcome: ReviewOutcome) -> io::Result<()> {
        if !self.issued_candidates.contains(snapshot_id) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "candidate snapshot not found",
            ));
        }
        if self.outcomes.contains_key(snapshot_id) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "candidate review already completed",
            ));
        }
        self.outcomes.insert(snapshot_id.into(), outcome);
        Ok(())
    }

    fn handle_request(&mut self, request: Value) -> Value {
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
                return match outcome {
                    ReviewOutcome::Accepted => json!({ "status": "accepted" }),
                    ReviewOutcome::Declined => json!({ "status": "declined" }),
                };
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
        if !safe_relative(path) {
            return rejected("outside_root");
        }
        let target = self.root.join(path);
        if !safe_target(&self.root, &target) {
            return rejected("outside_root");
        }
        let Some(inspected) = self.inspected.get(path) else {
            return rejected("unknown_snapshot");
        };
        if inspected.snapshot_id != snapshot {
            return rejected("stale_snapshot");
        }
        if !file_matches_snapshot(&target, &inspected.bytes) {
            return rejected("stale_snapshot");
        }
        if self.issued_candidates.contains(snapshot) || self.outcomes.contains_key(snapshot) {
            return rejected("already_issued");
        }
        let candidate = PatchCandidate {
            snapshot_id: snapshot.into(),
            path: path.into(),
            patch: patch.into(),
        };
        self.issued_candidates.insert(snapshot.into());
        self.candidates.push(candidate);
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
        json!({
            "status": "inspected",
            "files": files.into_iter().map(|file| json!({
                "path": file.relative_path, "snapshot": file.snapshot_id
            })).collect::<Vec<_>>()
        })
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

fn file_matches_snapshot(target: &Path, expected: &[u8]) -> bool {
    let Ok(mut file) = OpenOptions::new().read(true).open(target) else {
        return false;
    };
    if file
        .metadata()
        .map_or(true, |metadata| metadata.len() != expected.len() as u64)
    {
        return false;
    }
    let mut offset = 0;
    let mut chunk = [0_u8; 8192];
    while offset < expected.len() {
        let count = chunk.len().min(expected.len() - offset);
        if file.read_exact(&mut chunk[..count]).is_err()
            || chunk[..count] != expected[offset..offset + count]
        {
            return false;
        }
        offset += count;
    }
    file.read(&mut chunk[..1]).is_ok_and(|count| count == 0)
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

fn safe_target(root: &Path, target: &Path) -> bool {
    let mut current = root.to_path_buf();
    let Ok(relative) = target.strip_prefix(root) else {
        return false;
    };
    for component in relative.components() {
        current.push(component);
        let Ok(metadata) = fs::symlink_metadata(&current) else {
            return false;
        };
        if metadata.file_type().is_symlink() {
            return false;
        }
    }
    target
        .canonicalize()
        .is_ok_and(|actual| actual.starts_with(root) && actual.is_file())
}

fn read_request(stream: &mut UnixStream) -> Result<Value, &'static str> {
    let mut length = [0_u8; 4];
    stream.read_exact(&mut length).map_err(|_| "malformed")?;
    let length = u32::from_be_bytes(length) as usize;
    if length > SocketServer::MAX_FRAME_BYTES {
        return Err("too_large");
    }
    if length == 0 {
        return Err("malformed");
    }
    let mut payload = vec![0; length];
    stream.read_exact(&mut payload).map_err(|_| "malformed")?;
    let request: Value = serde_json::from_slice(&payload).map_err(|_| "malformed")?;
    if !request.is_object() {
        return Err("malformed");
    }
    Ok(request)
}

fn write_response(stream: &mut UnixStream, response: &Value) -> io::Result<()> {
    let mut body = serde_json::to_vec(response).map_err(io::Error::other)?;
    if body.len() > SocketServer::MAX_FRAME_BYTES {
        body = serde_json::to_vec(&rejected("too_large")).map_err(io::Error::other)?;
    }
    stream.write_all(&(body.len() as u32).to_be_bytes())?;
    stream.write_all(&body)
}
