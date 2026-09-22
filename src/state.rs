use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::types::{ROOMS_SCHEMA_VERSION, RoomRecord, RoomsFile};

#[derive(Clone, Debug)]
pub(crate) struct StateStore {
    path: PathBuf,
    lock_path: PathBuf,
}

impl StateStore {
    pub(crate) fn discover() -> Result<Self> {
        let root = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| dirs::home_dir().map(|home| home.join(".local/state")))
            .context("cannot determine state directory")?;
        Ok(Self::new(root.join("confer").join("rooms.json")))
    }

    pub(crate) fn runtime_path(&self) -> PathBuf {
        self.path.parent().expect("state parent").join("runtime")
    }

    pub(crate) fn new(path: PathBuf) -> Self {
        let lock_path = path.with_extension("json.lock");
        Self { path, lock_path }
    }

    #[cfg(test)]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn load(&self) -> Result<RoomsFile> {
        self.ensure_parent()?;
        let lock = self.open_lock()?;
        lock.lock_shared()
            .context("failed to lock Confer room cache")?;
        let state = self.read_unlocked();
        let _ = lock.unlock();
        state
    }

    pub(crate) fn mutate<T>(&self, change: impl FnOnce(&mut RoomsFile) -> Result<T>) -> Result<T> {
        self.ensure_parent()?;
        let lock = self.open_lock()?;
        lock.lock().context("failed to lock Confer room cache")?;
        let mut state = self.read_unlocked()?;
        state.schema_version = ROOMS_SCHEMA_VERSION;
        let result = change(&mut state)?;
        write_json_atomic(&self.path, &state)?;
        lock.unlock()
            .context("failed to unlock Confer room cache")?;
        Ok(result)
    }

    pub(crate) fn room_for_workspace(&self, room_id: &str, workspace: &Path) -> Result<RoomRecord> {
        let workspace = workspace.to_string_lossy();
        self.load()?
            .rooms
            .into_iter()
            .find(|room| room.id == room_id && room.workspace == workspace)
            .ok_or_else(|| anyhow::anyhow!("room '{room_id}' was not found in this workspace"))
    }

    pub(crate) fn try_acquire_seat_lease(&self, room_id: &str, seat_id: &str) -> Result<File> {
        let path = self.seat_lease_path(room_id, seat_id)?;
        let lock_dir = path
            .parent()
            .context("Confer seat lease has no parent directory")?;
        fs::create_dir_all(lock_dir)
            .with_context(|| format!("failed to create {}", lock_dir.display()))?;
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        match file.try_lock() {
            Ok(()) => Ok(file),
            Err(TryLockError::WouldBlock) => {
                bail!("seat_busy: seat '{seat_id}' has a running delivery")
            }
            Err(TryLockError::Error(error)) => Err(error)
                .with_context(|| format!("failed to lock seat '{seat_id}' in room '{room_id}'")),
        }
    }

    fn seat_lease_path(&self, room_id: &str, seat_id: &str) -> Result<PathBuf> {
        let parent = self
            .path
            .parent()
            .context("Confer room cache has no parent directory")?;
        Ok(parent
            .join("seat-locks")
            .join(format!("{room_id}-{seat_id}.lock")))
    }

    fn ensure_parent(&self) -> Result<()> {
        let parent = self
            .path
            .parent()
            .context("Confer room cache has no parent directory")?;
        fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))
    }

    fn open_lock(&self) -> Result<File> {
        OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&self.lock_path)
            .with_context(|| format!("failed to open {}", self.lock_path.display()))
    }

    fn read_unlocked(&self) -> Result<RoomsFile> {
        let body = match fs::read_to_string(&self.path) {
            Ok(body) => body,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(RoomsFile::default());
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read {}", self.path.display()));
            }
        };
        if body.trim().is_empty() {
            return Ok(RoomsFile::default());
        }
        let state: RoomsFile = serde_json::from_str(&body)
            .with_context(|| format!("failed to parse {}", self.path.display()))?;
        if !matches!(state.schema_version, 1 | 2 | ROOMS_SCHEMA_VERSION) {
            bail!(
                "unsupported Confer room cache schema {}",
                state.schema_version
            );
        }
        Ok(state)
    }
}

pub(crate) fn write_json_atomic(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    let parent = path.parent().context("JSON file has no parent directory")?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("failed to create temporary file in {}", parent.display()))?;
    serde_json::to_writer_pretty(&mut temp, value)
        .with_context(|| format!("failed to write {}", path.display()))?;
    temp.write_all(b"\n")
        .with_context(|| format!("failed to finish {}", path.display()))?;
    temp.as_file()
        .sync_all()
        .with_context(|| format!("failed to sync {}", path.display()))?;
    temp.persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to replace {}", path.display()))?;
    Ok(())
}

pub(crate) fn canonical_workspace(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        bail!("workspace must be an absolute path");
    }
    let workspace = path
        .canonicalize()
        .with_context(|| format!("failed to resolve workspace {}", path.display()))?;
    if !workspace.is_dir() {
        bail!("workspace {} is not a directory", path.display());
    }
    Ok(workspace)
}

pub(crate) fn normalize_workspace(path: &Path) -> Result<PathBuf> {
    let workspace = canonical_workspace(path)?;
    let output = Command::new("git")
        .arg("-C")
        .arg(&workspace)
        .args(["rev-parse", "--show-toplevel"])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output();
    let candidate = match output {
        Ok(output) if output.status.success() => {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if path.is_empty() {
                workspace
            } else {
                PathBuf::from(path)
            }
        }
        _ => workspace,
    };
    canonical_workspace(&candidate)
}

#[cfg(test)]
mod tests;
