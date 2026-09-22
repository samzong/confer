use std::collections::HashMap;
use std::fs::{self, File, TryLockError};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::types::{RoomRecord, SeatRecord};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct RunningSeat {
    pub(crate) room: String,
    pub(crate) project: String,
    pub(crate) seat: String,
    pub(crate) agent: String,
    pub(crate) model: String,
    pub(crate) effort: String,
}

pub(crate) struct Instance {
    directory: tempfile::TempDir,
    state: Mutex<PublishedState>,
}

struct PublishedState {
    lease: Option<File>,
    seats: HashMap<String, RunningSeat>,
}

pub(crate) struct Activity {
    instance: Arc<Instance>,
    delivery: String,
}

impl Instance {
    pub(crate) fn register(root: &Path) -> Result<Arc<Self>> {
        fs::create_dir_all(root)?;
        let directory = tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o700))
            .prefix(&format!("{}-", std::process::id()))
            .tempdir_in(root)?;
        let lease = File::create(directory.path().join("lease"))?;
        lease.lock()?;
        write_snapshot(
            &directory.path().join("seats.json"),
            &Vec::<RunningSeat>::new(),
        )?;
        Ok(Arc::new(Self {
            directory,
            state: Mutex::new(PublishedState {
                lease: Some(lease),
                seats: HashMap::new(),
            }),
        }))
    }

    pub(crate) fn start(
        self: &Arc<Self>,
        delivery: &str,
        room: &RoomRecord,
        seat: &SeatRecord,
    ) -> Activity {
        let mut state = self.state.lock().expect("activity state");
        state.seats.insert(
            delivery.to_owned(),
            RunningSeat {
                room: room.name.clone(),
                project: room.workspace.clone(),
                seat: seat.name.clone(),
                agent: seat.agent.id().to_owned(),
                model: seat.model.clone().unwrap_or_else(|| "default".into()),
                effort: seat.reasoning_effort.clone().unwrap_or_else(|| "—".into()),
            },
        );
        self.publish(&mut state);
        Activity {
            instance: self.clone(),
            delivery: delivery.into(),
        }
    }

    fn publish(&self, state: &mut PublishedState) {
        if state.lease.is_some()
            && let Err(error) = write_snapshot(
                &self.directory.path().join("seats.json"),
                &state.seats.values().collect::<Vec<_>>(),
            )
        {
            state.lease.take();
            eprintln!("Confer running state unavailable: {error:#}");
        }
    }
}

impl Drop for Activity {
    fn drop(&mut self) {
        let mut state = self.instance.state.lock().expect("activity state");
        state.seats.remove(&self.delivery);
        self.instance.publish(&mut state);
    }
}

fn write_snapshot(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut temp = tempfile::NamedTempFile::new_in(path.parent().expect("snapshot parent"))?;
    serde_json::to_writer(&mut temp, value)?;
    temp.persist(path).map_err(|error| error.error)?;
    Ok(())
}

pub(crate) fn live_seats(root: &Path) -> HashMap<u32, Option<Vec<RunningSeat>>> {
    let mut live = HashMap::new();
    let Ok(entries) = fs::read_dir(root) else {
        return live;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.split_once('-'))
            .and_then(|(pid, _)| pid.parse().ok())
        else {
            continue;
        };
        let Ok(lease) = File::open(entry.path().join("lease")) else {
            continue;
        };
        let held = matches!(lease.try_lock_shared(), Err(TryLockError::WouldBlock));
        if !held {
            continue;
        }
        let seats = fs::read(entry.path().join("seats.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        if live.insert(pid, seats).is_some() {
            live.insert(pid, None);
        }
    }
    live
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{room, seat};
    use crate::types::AgentKind;

    #[test]
    fn active_seats_follow_guards_and_ignore_unlocked_residue() {
        let root = tempfile::tempdir().unwrap();
        let instance = Instance::register(root.path()).unwrap();
        let room = room("review", "/project");
        let seat = seat("seat", "reviewer", AgentKind::Claude);
        let first = instance.start("first", &room, &seat);
        let second = instance.start("second", &room, &seat);
        let pid = std::process::id();
        let running = || {
            live_seats(root.path())
                .remove(&pid)
                .map(|seats| seats.map(|seats| seats.len()))
        };
        assert_eq!(running(), Some(Some(2)));
        drop(first);
        assert_eq!(running(), Some(Some(1)));
        drop(second);
        assert_eq!(running(), Some(Some(0)));
        instance
            .state
            .lock()
            .unwrap()
            .lease
            .take()
            .unwrap()
            .unlock()
            .unwrap();
        assert_eq!(running(), None);
    }

    #[test]
    fn failed_publication_invalidates_previous_running_state() {
        let root = tempfile::tempdir().unwrap();
        let instance = Instance::register(root.path()).unwrap();
        let room = room("review", "/project");
        let seat = seat("seat", "reviewer", AgentKind::Claude);
        let activity = instance.start("first", &room, &seat);
        let snapshot = instance.directory.path().join("seats.json");
        fs::remove_file(&snapshot).unwrap();
        fs::create_dir(&snapshot).unwrap();
        drop(activity);
        fs::remove_dir(&snapshot).unwrap();
        fs::write(&snapshot, "[]").unwrap();
        assert!(!live_seats(root.path()).contains_key(&std::process::id()));
    }
}
