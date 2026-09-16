use std::collections::HashSet;

use anyhow::{Context, Result, bail};

use super::ConferMcp;
use super::api::{
    AddSeatArgs, AddSeatOutput, CreateRoomArgs, CreateRoomOutput, ListRoomsArgs, ListRoomsOutput,
    RetireSeatArgs, RetireSeatOutput, RoomScope, SeatSpecInput, room_view, rooms_for_scope,
    timestamp,
};
use crate::adapters;
use crate::state::{canonical_workspace, normalize_workspace};
use crate::types::{AgentKind, HostRecord, Readiness, RoomRecord, SeatRecord, SeatStatus};

impl ConferMcp {
    pub(super) fn create_room_inner(&self, args: CreateRoomArgs) -> Result<CreateRoomOutput> {
        let workspace = normalize_workspace(&args.workspace)?;
        if args.target_size == Some(0) {
            bail!("target_size must be positive");
        }
        let target_size = args.target_size.unwrap_or(0).max(args.seats.len());
        if target_size == 0 {
            bail!("provide target_size or at least one seat");
        }
        let readiness = adapters::readiness();
        let host_agent = detect_host_agent(args.host_agent.as_deref());
        let seats = select_seats(
            args.seats,
            target_size,
            host_agent.as_deref(),
            &readiness,
            HashSet::new(),
        )?;
        let now = timestamp();
        let id = uuid::Uuid::new_v4().to_string();
        let room = RoomRecord {
            name: normalized_name(args.name.as_deref(), &id),
            id,
            workspace: workspace.to_string_lossy().into_owned(),
            host: HostRecord { agent: host_agent },
            seats,
            created_at: now.clone(),
            updated_at: now,
        };
        self.store.mutate(|state| {
            state.rooms.push(room.clone());
            Ok(())
        })?;
        Ok(CreateRoomOutput {
            room: room_view(&room),
            readiness,
        })
    }

    pub(super) fn add_seat_inner(&self, args: AddSeatArgs) -> Result<AddSeatOutput> {
        let workspace = canonical_workspace(&args.workspace)?;
        let workspace_text = workspace.to_string_lossy().into_owned();
        let readiness = adapters::readiness();
        let room = self.store.room_for_workspace(&args.room_id, &workspace)?;
        let names = room
            .seats
            .iter()
            .map(|seat| seat.name.clone())
            .collect::<HashSet<_>>();
        let mut seats = select_seats(
            vec![args.seat],
            1,
            room.host.agent.as_deref(),
            &readiness,
            names,
        )?;
        let seat = seats.pop().context("seat selection returned no seat")?;
        let room = self.store.mutate(|state| {
            let room = state
                .rooms
                .iter_mut()
                .find(|room| room.id == args.room_id && room.workspace == workspace_text)
                .with_context(|| {
                    format!("room '{}' was not found in this workspace", args.room_id)
                })?;
            if room.seats.iter().any(|existing| existing.name == seat.name) {
                bail!("duplicate seat name '{}'", seat.name);
            }
            room.seats.push(seat);
            room.updated_at = timestamp();
            Ok(room.clone())
        })?;
        Ok(AddSeatOutput {
            room: room_view(&room),
            readiness,
        })
    }

    pub(super) async fn retire_seat_inner(&self, args: RetireSeatArgs) -> Result<RetireSeatOutput> {
        let workspace = canonical_workspace(&args.workspace)?;
        let workspace_text = workspace.to_string_lossy().into_owned();
        let room = self.store.room_for_workspace(&args.room_id, &workspace)?;
        let seat = room
            .seats
            .iter()
            .find(|seat| seat.id == args.seat || seat.name == args.seat)
            .with_context(|| format!("unknown seat '{}' in room '{}'", args.seat, room.id))?;
        if seat.status == SeatStatus::Retired {
            bail!("seat '{}' is already retired", seat.name);
        }
        let _session_guard = self.store.try_acquire_seat_lease(&room.id, &seat.id)?;
        let seat_id = seat.id.clone();
        let room = self.store.mutate(|state| {
            let room = state
                .rooms
                .iter_mut()
                .find(|room| room.id == args.room_id && room.workspace == workspace_text)
                .with_context(|| {
                    format!("room '{}' was not found in this workspace", args.room_id)
                })?;
            let seat = room
                .seats
                .iter_mut()
                .find(|seat| seat.id == seat_id)
                .with_context(|| format!("seat '{seat_id}' disappeared while retiring"))?;
            if seat.status == SeatStatus::Retired {
                bail!("seat '{}' is already retired", seat.name);
            }
            seat.status = SeatStatus::Retired;
            room.updated_at = timestamp();
            Ok(room.clone())
        })?;
        self.runtime.stop_seat(&room.id, &seat_id).await;
        Ok(RetireSeatOutput {
            room: room_view(&room),
        })
    }

    pub(super) fn list_rooms_inner(&self, args: ListRoomsArgs) -> Result<ListRoomsOutput> {
        let scope = args.scope.unwrap_or_default();
        let workspace = match scope {
            RoomScope::Current => Some(
                normalize_workspace(
                    args.workspace
                        .as_deref()
                        .context("workspace is required for current scope")?,
                )?
                .to_string_lossy()
                .into_owned(),
            ),
            RoomScope::All => None,
        };
        let rooms = rooms_for_scope(self.store.load()?.rooms, scope, workspace.as_deref());
        Ok(ListRoomsOutput {
            scope,
            workspace,
            rooms,
        })
    }
}

fn select_seats(
    requested: Vec<SeatSpecInput>,
    count: usize,
    host_agent: Option<&str>,
    readiness: &[Readiness],
    mut names: HashSet<String>,
) -> Result<Vec<SeatRecord>> {
    let mut preferred = readiness
        .iter()
        .filter(|item| item.locally_ready)
        .map(|item| item.agent)
        .collect::<Vec<_>>();
    let host_kind = host_agent.and_then(AgentKind::parse);
    preferred.sort_by_key(|agent| Some(*agent) == host_kind);
    let mut specs = requested;
    specs.resize_with(count.max(specs.len()), SeatSpecInput::default);
    let mut seats = Vec::with_capacity(specs.len());
    for (index, spec) in specs.into_iter().enumerate() {
        let requested_agent = spec
            .agent
            .as_deref()
            .map(|value| {
                AgentKind::parse(value).with_context(|| format!("unsupported agent '{value}'"))
            })
            .transpose()?;
        let selected = match requested_agent {
            Some(agent) if preferred.contains(&agent) => agent,
            Some(agent) => {
                let reason = readiness
                    .iter()
                    .find(|item| item.agent == agent)
                    .and_then(|item| item.reason.as_deref())
                    .unwrap_or("agent is not locally ready");
                bail!(
                    "agent '{}' for seat '{}' is not locally ready: {reason}",
                    agent.id(),
                    spec.name.as_deref().unwrap_or(agent.id())
                );
            }
            None if preferred.is_empty() => {
                bail!("no locally ready supported agents were found");
            }
            None => preferred[index % preferred.len()],
        };
        let mut name = spec
            .name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .unwrap_or_else(|| selected.id().to_string());
        if names.contains(&name) && spec.name.is_some() {
            bail!("duplicate seat name '{name}'");
        }
        if names.contains(&name) {
            let base = name.clone();
            let mut suffix = 2usize;
            while names.contains(&name) {
                name = format!("{base}-{suffix}");
                suffix += 1;
            }
        }
        names.insert(name.clone());
        let model = spec.model;
        let reasoning_effort = spec.reasoning_effort;
        adapters::validate_seat_config(selected, model.as_deref(), reasoning_effort.as_deref())?;
        seats.push(SeatRecord {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            agent: selected,
            model,
            reasoning_effort,
            instructions: spec.instructions,
            native_session_id: None,
            status: SeatStatus::Active,
        });
    }
    Ok(seats)
}

fn detect_host_agent(explicit: Option<&str>) -> Option<String> {
    if let Some(explicit) = explicit.map(str::trim).filter(|value| !value.is_empty()) {
        return Some(
            AgentKind::parse(explicit).map_or_else(|| explicit.to_string(), |a| a.id().into()),
        );
    }
    [
        ("CLAUDE_CODE_SESSION_ID", "claude"),
        ("CODEX_THREAD_ID", "codex"),
        ("CODEX_SESSION_ID", "codex"),
        ("CURSOR_SESSION_ID", "cursor"),
        ("GROK_SESSION_ID", "grok"),
        ("COPILOT_AGENT_SESSION_ID", "copilot"),
    ]
    .into_iter()
    .find(|(name, _)| std::env::var_os(name).is_some())
    .map(|(_, agent)| agent.into())
}

fn normalized_name(name: Option<&str>, id: &str) -> String {
    name.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| format!("room-{}", &id[..8]))
}

#[cfg(test)]
mod tests;
