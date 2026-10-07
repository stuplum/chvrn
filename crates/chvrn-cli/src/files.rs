use crate::Result;
use std::fs::{self, Metadata, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone)]
pub struct GuardedFile {
    pub path: PathBuf,
    bytes: Option<Arc<Vec<u8>>>,
    metadata: Option<Metadata>,
}

impl GuardedFile {
    pub fn capture(path: &Path) -> Result<Self> {
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()?.join(path)
        };
        let name = absolute.file_name().ok_or("expected a file path")?;
        let parent = absolute
            .parent()
            .ok_or("file has no parent")?
            .canonicalize()?;
        let path = parent.join(name);
        let metadata = regular_metadata(&path)?;
        let bytes = metadata
            .as_ref()
            .map(|_| read_regular(&path).map(|(bytes, _)| Arc::new(bytes)))
            .transpose()?;
        let captured = Self {
            path,
            bytes,
            metadata,
        };
        captured.validate()?;
        Ok(captured)
    }

    pub fn read(path: &Path) -> Result<Self> {
        let file = Self::capture(path)?;
        if file.bytes.is_none() {
            return Err(format!("input does not exist: {}", file.path.display()).into());
        }
        Ok(file)
    }

    pub fn bytes(&self) -> &[u8] {
        self.bytes.as_deref().map(Vec::as_slice).unwrap_or_default()
    }

    pub fn same_state(&self, other: &Self) -> bool {
        self.bytes == other.bytes
            && match (&self.metadata, &other.metadata) {
                (None, None) => true,
                (Some(left), Some(right)) => same_file(left, right),
                _ => false,
            }
    }

    pub fn validate(&self) -> Result<()> {
        let metadata = regular_metadata(&self.path)?;
        let same_metadata = match (&self.metadata, &metadata) {
            (None, None) => true,
            (Some(expected), Some(current)) => same_file(expected, current),
            _ => false,
        };
        let same_bytes = match (&self.bytes, &metadata) {
            (None, None) => true,
            (Some(expected), Some(_)) => read_regular(&self.path)?.0 == **expected,
            _ => false,
        };
        if !same_metadata || !same_bytes {
            return Err(format!(
                "stale review: {} changed after inspection; nothing written",
                self.path.display()
            )
            .into());
        }
        Ok(())
    }

    pub fn write(&mut self, bytes: &[u8]) -> Result<()> {
        let parent = self.path.parent().ok_or("output has no parent directory")?;
        let mut lock_name = self
            .path
            .file_name()
            .ok_or("output has no file name")?
            .to_os_string();
        lock_name.push(".chvrn-lock");
        let lock_path = parent.join(lock_name);
        let lock = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)?;
        let _guard = WriteLock {
            path: lock_path,
            _file: lock,
        };
        self.validate()?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        if let Some(metadata) = &self.metadata {
            temporary
                .as_file()
                .set_permissions(metadata.permissions())?;
        }
        temporary.write_all(bytes)?;
        temporary.as_file().sync_all()?;
        self.validate()?;
        if self.metadata.is_some() {
            temporary.persist(&self.path).map_err(|error| error.error)?;
        } else {
            temporary
                .persist_noclobber(&self.path)
                .map_err(|error| error.error)?;
        }
        self.bytes = Some(Arc::new(bytes.to_vec()));
        self.metadata = regular_metadata(&self.path)?;
        Ok(())
    }
}

fn read_regular(path: &Path) -> Result<(Vec<u8>, Metadata)> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(format!("refusing non-regular file: {}", path.display()).into());
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let current = fs::symlink_metadata(path)?;
    if !current.is_file() || !same_file(&metadata, &current) {
        return Err(format!("file changed while reading: {}", path.display()).into());
    }
    Ok((bytes, metadata))
}

fn regular_metadata(path: &Path) -> Result<Option<Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(Some(metadata)),
        Ok(_) => Err(format!("refusing non-regular or symlink file: {}", path.display()).into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn same_file(expected: &Metadata, current: &Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if expected.dev() != current.dev()
            || expected.ino() != current.ino()
            || expected.mode() != current.mode()
        {
            return false;
        }
    }
    expected.len() == current.len() && expected.modified().ok() == current.modified().ok()
}

struct WriteLock {
    path: PathBuf,
    _file: fs::File,
}

impl Drop for WriteLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub fn read_bytes(path: &Path) -> Result<Vec<u8>> {
    if path == Path::new("-") {
        let mut bytes = Vec::new();
        io::stdin().read_to_end(&mut bytes)?;
        Ok(bytes)
    } else {
        Ok(fs::read(path)?)
    }
}
