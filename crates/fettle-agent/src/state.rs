use std::borrow::Cow;
use std::fmt::Write as _;
use std::fs::{self, DirBuilder, File, TryLockError};
use std::io::{self, ErrorKind, Write};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

use fettle_protocol::ResourceKey;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use crate::Result;

pub const PATH: &str = "/var/lib/fettle/state.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Created,
    Preexisting,
}

/// A managed resource as last written by fettle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    #[serde(flatten)]
    pub key: ResourceKey,
    pub origin: Origin,
    pub hash: String,
    pub state: Value,
}

#[derive(Serialize, Deserialize)]
struct Document<'a> {
    resources: Cow<'a, [Entry]>,
}

/// The host's record of what fettle manages on it, locked for as long as it lives.
#[derive(Debug)]
#[must_use = "dropping HostState releases the run lock"]
pub struct HostState {
    path: PathBuf,
    _lock: File,
    entries: Vec<Entry>,
}

impl HostState {
    /// Creates the state's directory, takes the run lock, and loads the state at `path`.
    ///
    /// # Errors
    ///
    /// Fails naming the file when the directory cannot be created, another run holds the lock,
    /// or the file cannot be read, does not parse, or holds a key twice.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let dir = parent(&path);
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|e| at(dir, &e))?;
        let lock = lock(&path.with_extension("lock"))?;
        let entries = load(&path)?;
        Ok(Self {
            path,
            _lock: lock,
            entries,
        })
    }

    #[must_use]
    pub fn get(&self, key: &ResourceKey) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.key == *key)
    }

    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Records `state` as written; a new entry is `preexisting` when the plan-time read found the resource.
    pub fn record(&mut self, key: ResourceKey, state: Value, found_at_plan: bool) {
        let hash = hash(&state);
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.key == key) {
            entry.hash = hash;
            entry.state = state;
        } else {
            let origin = if found_at_plan {
                Origin::Preexisting
            } else {
                Origin::Created
            };
            self.entries.push(Entry {
                key,
                origin,
                hash,
                state,
            });
        }
    }

    pub fn discard(&mut self, key: &ResourceKey) {
        self.entries.retain(|entry| entry.key != *key);
    }

    /// Replaces the file atomically, so a crash leaves the previous or the new state in full.
    ///
    /// # Errors
    ///
    /// Fails naming the path when the new file cannot be written or moved into place.
    pub fn save(&self) -> Result<()> {
        let path = &self.path;
        let dir = parent(path);
        let mut temp = NamedTempFile::new_in(dir).map_err(|e| at(dir, &e))?;
        let document = Document {
            resources: Cow::Borrowed(&self.entries),
        };
        serde_json::to_writer_pretty(&mut temp, &document).map_err(|e| at(path, &e.into()))?;
        temp.write_all(b"\n").map_err(|e| at(path, &e))?;
        temp.as_file().sync_all().map_err(|e| at(path, &e))?;
        temp.persist(path).map_err(|e| at(path, &e.error))?;
        File::open(dir)
            .and_then(|dir| dir.sync_all())
            .map_err(|e| at(dir, &e))
    }
}

fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

fn lock(path: &Path) -> Result<File> {
    let file = File::options()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)
        .map_err(|e| at(path, &e))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => {
            Err(format!("{}: another fettle run holds this lock", path.display()).into())
        }
        Err(TryLockError::Error(error)) => Err(at(path, &error)),
    }
}

fn load(path: &Path) -> Result<Vec<Entry>> {
    let bytes = match fs::read(path) {
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(at(path, &error)),
        Ok(bytes) => bytes,
    };
    let document: Document =
        serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    let entries = document.resources.into_owned();
    for (i, entry) in entries.iter().enumerate() {
        if entries[..i].iter().any(|seen| seen.key == entry.key) {
            return Err(
                format!("{}: {} appears twice", path.display(), describe(&entry.key)).into(),
            );
        }
    }
    Ok(entries)
}

fn hash(state: &Value) -> String {
    let digest = Sha256::digest(state.to_string());
    digest.iter().fold(String::from("sha256:"), |mut hex, b| {
        let _ = write!(hex, "{b:02x}");
        hex
    })
}

fn describe(key: &ResourceKey) -> String {
    let kind = serde_json::to_value(&key.kind).unwrap_or_default();
    format!("{} {}", kind.as_str().unwrap_or_default(), key.id)
}

fn at(path: &Path, error: &io::Error) -> Box<dyn std::error::Error + Send + Sync> {
    format!("{}: {error}", path.display()).into()
}
