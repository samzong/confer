use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use fs2::FileExt;

use crate::types::{RoomRecord, RoomsFile};

#[derive(Clone, Debug)]
pub(crate) struct StateStore {
    path: PathBuf,
    lock_path: PathBuf,
}

impl StateStore {
    pub(crate) fn discover() -> Result<Self> {
        let home = dirs::home_dir().context("cannot determine home directory")?;
        Ok(Self::new(home.join(".confer").join("rooms.json")))
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
        lock.lock_exclusive()
            .context("failed to lock Confer room cache")?;
        let mut state = self.read_unlocked()?;
        let result = change(&mut state)?;
        self.write_unlocked(&state)?;
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
        let mut file = match OpenOptions::new().read(true).open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(RoomsFile::default());
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to open {}", self.path.display()));
            }
        };
        let mut body = String::new();
        file.read_to_string(&mut body)
            .with_context(|| format!("failed to read {}", self.path.display()))?;
        if body.trim().is_empty() {
            return Ok(RoomsFile::default());
        }
        let state: RoomsFile = serde_json::from_str(&body)
            .with_context(|| format!("failed to parse {}", self.path.display()))?;
        if state.schema_version != 1 {
            bail!(
                "unsupported Confer room cache schema {}",
                state.schema_version
            );
        }
        Ok(state)
    }

    fn write_unlocked(&self, state: &RoomsFile) -> Result<()> {
        let parent = self
            .path
            .parent()
            .context("Confer room cache has no parent directory")?;
        let body = serde_json::to_vec_pretty(state)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)
            .with_context(|| format!("failed to create temporary file in {}", parent.display()))?;
        temp.write_all(&body)
            .context("failed to write Confer room cache")?;
        temp.write_all(b"\n")
            .context("failed to finish Confer room cache")?;
        temp.as_file()
            .sync_all()
            .context("failed to sync Confer room cache")?;
        temp.persist(&self.path)
            .map_err(|error| error.error)
            .with_context(|| format!("failed to replace {}", self.path.display()))?;
        Ok(())
    }
}

pub(crate) fn current_workspace() -> Result<PathBuf> {
    let cwd = std::env::current_dir().context("cannot determine current directory")?;
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(&cwd)
        .output();
    let candidate = match output {
        Ok(output) if output.status.success() => {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if path.is_empty() {
                cwd
            } else {
                PathBuf::from(path)
            }
        }
        _ => cwd,
    };
    candidate
        .canonicalize()
        .with_context(|| format!("failed to resolve workspace {}", candidate.display()))
}

#[cfg(test)]
mod tests {
    use super::StateStore;
    use crate::types::{HostRecord, RoomRecord, RoomStatus};

    #[test]
    fn cache_round_trip_preserves_rooms() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::new(dir.path().join("rooms.json"));
        store
            .mutate(|state| {
                state.rooms.push(RoomRecord {
                    id: "room-1".into(),
                    name: "Review".into(),
                    workspace: "/tmp/project".into(),
                    status: RoomStatus::Inactive,
                    host: HostRecord {
                        agent: Some("codex".into()),
                    },
                    seats: Vec::new(),
                    created_at: "2026-01-01T00:00:00Z".into(),
                    updated_at: "2026-01-01T00:00:00Z".into(),
                });
                Ok(())
            })
            .unwrap();

        let state = store.load().unwrap();
        assert_eq!(state.rooms.len(), 1);
        assert_eq!(state.rooms[0].id, "room-1");
    }

    #[test]
    fn cache_rejects_unknown_schema() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::new(dir.path().join("rooms.json"));
        std::fs::write(store.path(), r#"{"schema_version":2,"rooms":[]}"#).unwrap();
        assert!(store.load().is_err());
    }
}
