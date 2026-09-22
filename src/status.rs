use std::collections::HashMap;
use std::fs::{self, File, TryLockError};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use unicode_width::UnicodeWidthStr;

use crate::state::{StateStore, write_json_atomic};
use crate::types::{RoomRecord, SeatRecord};

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RunningSeat {
    room: String,
    project: String,
    seat: String,
    agent: String,
    model: String,
    effort: String,
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
        write_json_atomic(
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
            && let Err(error) = write_json_atomic(
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

struct Process {
    pid: u32,
    parent: u32,
    elapsed: String,
    executable: String,
    mcp: bool,
}

fn output(program: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(program)
        .args(args)
        .env("LC_ALL", "C")
        .output()?;
    if !output.status.success() {
        bail!("{program} could not read process information");
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn processes() -> Result<Vec<Process>> {
    let uid = output("id", &["-u"])?;
    let listing = output("ps", &["-axo", "uid=,pid=,ppid=,etime=,comm="])?;
    let commands = output("ps", &["-axo", "pid=,args="])?;
    let mcp_pids = commands
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let (pid, command) = line.split_once(char::is_whitespace)?;
            let executable = command.trim().strip_suffix(" mcp")?;
            (Path::new(executable).file_name()?.to_str()? == "confer")
                .then(|| pid.parse::<u32>().ok())
                .flatten()
        })
        .collect::<Vec<_>>();
    listing
        .lines()
        .filter_map(|line| {
            let mut rest = line.trim();
            let mut fields = Vec::new();
            for _ in 0..4 {
                let (field, tail) = rest.split_once(char::is_whitespace)?;
                fields.push(field);
                rest = tail.trim_start();
            }
            if fields[0] != uid.trim() {
                return None;
            }
            Some((fields, rest))
        })
        .map(|(fields, executable)| {
            let pid = fields[1].parse()?;
            Ok(Process {
                pid,
                parent: fields[2].parse()?,
                elapsed: fields[3].into(),
                executable: Path::new(executable)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into(),
                mcp: mcp_pids.contains(&pid),
            })
        })
        .collect()
}

fn live_seats(root: &Path) -> HashMap<u32, Option<Vec<RunningSeat>>> {
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

fn clean(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect()
}

fn table(headers: &[&str], rows: Vec<Vec<String>>, right: &[usize]) -> String {
    let rows = std::iter::once(headers.iter().map(|s| s.to_string()).collect::<Vec<_>>())
        .chain(rows)
        .map(|row| row.iter().map(|cell| clean(cell)).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    let widths = (0..headers.len())
        .map(|column| {
            rows.iter()
                .map(|row| row[column].width())
                .max()
                .unwrap_or(0)
        })
        .collect::<Vec<_>>();
    rows.iter()
        .map(|row| {
            row.iter()
                .enumerate()
                .map(|(column, cell)| {
                    let padding = " ".repeat(widths[column] - cell.width());
                    if right.contains(&column) {
                        format!("{padding}{cell}")
                    } else {
                        format!("{cell}{padding}")
                    }
                })
                .collect::<Vec<_>>()
                .join("  ")
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn uptime(elapsed: &str) -> String {
    let (days, clock) = elapsed.split_once('-').unwrap_or(("0", elapsed));
    let parts = clock
        .split(':')
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>();
    let Ok(parts) = parts else {
        return "unknown".into();
    };
    let Ok(days) = days.parse::<u64>() else {
        return "unknown".into();
    };
    match parts.as_slice() {
        [hours, minutes, _] if days > 0 => format!("{days}d{hours}h{minutes}m"),
        [hours, minutes, _] if *hours > 0 => format!("{hours}h{minutes}m"),
        [_, minutes, seconds] | [minutes, seconds] => {
            if *minutes > 0 {
                format!("{minutes}m")
            } else {
                format!("{seconds}s")
            }
        }
        _ => "unknown".into(),
    }
}

fn project_path(project: &str) -> String {
    if let Some(home) = dirs::home_dir()
        && let Ok(relative) = Path::new(project).strip_prefix(home)
    {
        if relative.as_os_str().is_empty() {
            return "~".into();
        }
        return format!("~/{}", relative.display());
    }
    project.into()
}

pub(crate) fn run() -> Result<()> {
    let processes = processes()?;
    let mut live = live_seats(&StateStore::discover()?.runtime_path());
    let mut servers = processes
        .iter()
        .filter(|p| p.mcp || live.contains_key(&p.pid))
        .collect::<Vec<_>>();
    servers.sort_by_key(|p| p.pid);
    let mut seats = Vec::new();
    let mut unavailable = false;
    for server in &servers {
        match live.remove(&server.pid).flatten() {
            Some(running) => seats.extend(running),
            None => unavailable = true,
        }
    }
    println!("── MCP PROCESSES ──");
    if servers.is_empty() {
        println!("No MCP processes are running.");
    } else {
        println!(
            "{}",
            table(
                &["PID", "PARENT", "UPTIME"],
                servers
                    .iter()
                    .map(|server| vec![
                        server.pid.to_string(),
                        processes
                            .iter()
                            .find(|p| p.pid == server.parent)
                            .map(|p| p.executable.clone())
                            .unwrap_or_else(|| "unknown".into()),
                        uptime(&server.elapsed),
                    ])
                    .collect(),
                &[0, 2]
            )
        );
    }
    println!("\n── RUNNING SEATS ──");
    seats.sort_by(|a, b| (&a.project, &a.room, &a.seat).cmp(&(&b.project, &b.room, &b.seat)));
    if !seats.is_empty() {
        println!(
            "{}",
            table(
                &["ROOM", "PROJECT", "SEAT", "AGENT", "MODEL", "EFFORT"],
                seats
                    .into_iter()
                    .map(|seat| vec![
                        seat.room,
                        project_path(&seat.project),
                        seat.seat,
                        seat.agent,
                        seat.model,
                        seat.effort,
                    ])
                    .collect(),
                &[]
            )
        );
    } else if !unavailable {
        println!("No seats are running.");
    }
    if unavailable {
        println!("Seat activity unavailable.");
    }
    Ok(())
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
        instance.state.lock().unwrap().lease.take();
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

    #[test]
    fn columns_align_display_width_and_escape_terminal_controls() {
        let rendered = table(
            &["SEAT", "AGENT"],
            vec![
                vec!["\u{68c0}\u{67e5}\u{5458}".into(), "claude".into()],
                vec!["e\u{301}\t".into(), "codex".into()],
            ],
            &[],
        );
        let lines = rendered.lines().collect::<Vec<_>>();
        for (line, name) in lines.iter().zip(["AGENT", "claude", "codex"]) {
            let prefix = &line[..line.find(name).unwrap()];
            assert_eq!(prefix.width(), 8);
        }
        assert!(!rendered.contains('\t'));
        let numbers = table(
            &["PID", "UPTIME"],
            vec![vec!["12".into(), "2m".into()]],
            &[0, 1],
        );
        assert_eq!(numbers.lines().nth(1).unwrap(), " 12      2m");
    }
}
