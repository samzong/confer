use std::collections::HashSet;

use anyhow::{Context, Result, bail};

use super::ConferMcp;
use super::api::{
    AddSeatArgs, AddSeatOutput, CreateRoomArgs, CreateRoomOutput, ListRoomsOutput, RetireSeatArgs,
    RetireSeatOutput, RoomScope, SeatSpecInput, room_view, rooms_for_scope, timestamp,
};
use crate::adapters;
use crate::state::current_workspace;
use crate::types::{
    AgentKind, HostRecord, Readiness, Replacement, RoomRecord, SeatRecord, SeatStatus,
};

const DEFAULT_ROOM_SIZE: usize = 3;
const MAX_ROOM_SIZE: usize = 16;

impl ConferMcp {
    pub(super) fn create_room_inner(&self, args: CreateRoomArgs) -> Result<CreateRoomOutput> {
        let workspace = current_workspace()?;
        let readiness = adapters::readiness();
        let host_agent = detect_host_agent(args.host_agent.as_deref());
        let requested_size = args.target_size.unwrap_or(DEFAULT_ROOM_SIZE);
        let target_size = requested_size.max(args.seats.len() + 1);
        if !(2..=MAX_ROOM_SIZE).contains(&target_size) {
            bail!("target_size must be between 2 and {MAX_ROOM_SIZE}");
        }
        let (seats, replacements) = select_seats(
            args.seats,
            target_size - 1,
            host_agent.as_deref(),
            &readiness,
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
            replacements,
        })
    }

    pub(super) fn add_seat_inner(&self, args: AddSeatArgs) -> Result<AddSeatOutput> {
        let workspace = current_workspace()?;
        let workspace_text = workspace.to_string_lossy().into_owned();
        let readiness = adapters::readiness();
        let room = self.store.room_for_workspace(&args.room_id, &workspace)?;
        let names = room
            .seats
            .iter()
            .map(|seat| seat.name.clone())
            .collect::<HashSet<_>>();
        let (mut seats, replacements) = select_seats_with_names(
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
            replacements,
        })
    }

    pub(super) async fn retire_seat_inner(&self, args: RetireSeatArgs) -> Result<RetireSeatOutput> {
        let workspace = current_workspace()?;
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

    pub(super) fn list_rooms_inner(&self, scope: RoomScope) -> Result<ListRoomsOutput> {
        let workspace = current_workspace()?;
        let workspace_text = workspace.to_string_lossy().into_owned();
        let rooms = rooms_for_scope(self.store.load()?.rooms, scope, &workspace_text);
        Ok(ListRoomsOutput {
            scope,
            workspace: matches!(scope, RoomScope::Current).then_some(workspace_text),
            rooms,
        })
    }
}

fn select_seats(
    requested: Vec<SeatSpecInput>,
    count: usize,
    host_agent: Option<&str>,
    readiness: &[Readiness],
) -> Result<(Vec<SeatRecord>, Vec<Replacement>)> {
    select_seats_with_names(requested, count, host_agent, readiness, HashSet::new())
}

fn select_seats_with_names(
    requested: Vec<SeatSpecInput>,
    count: usize,
    host_agent: Option<&str>,
    readiness: &[Readiness],
    mut names: HashSet<String>,
) -> Result<(Vec<SeatRecord>, Vec<Replacement>)> {
    let ready = readiness
        .iter()
        .filter(|item| item.locally_ready)
        .map(|item| item.agent)
        .collect::<Vec<_>>();
    if ready.is_empty() {
        bail!("no locally ready supported agents were found");
    }
    let host_kind = host_agent.and_then(AgentKind::parse);
    let mut preferred = ready
        .iter()
        .copied()
        .filter(|agent| Some(*agent) != host_kind)
        .collect::<Vec<_>>();
    preferred.extend(
        ready
            .iter()
            .copied()
            .filter(|agent| Some(*agent) == host_kind),
    );
    let mut specs = requested;
    while specs.len() < count {
        specs.push(SeatSpecInput {
            agent: None,
            model: None,
            reasoning_effort: None,
            name: None,
            instructions: None,
        });
    }
    let mut seats = Vec::with_capacity(specs.len());
    let mut replacements = Vec::new();
    for (index, spec) in specs.into_iter().enumerate() {
        let requested_agent = spec
            .agent
            .as_deref()
            .map(|value| {
                AgentKind::parse(value).with_context(|| format!("unsupported agent '{value}'"))
            })
            .transpose()?;
        let selected = match requested_agent {
            Some(agent) if ready.contains(&agent) => agent,
            Some(_) => preferred[index % preferred.len()],
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
        let replaced = requested_agent.is_some_and(|agent| agent != selected);
        if let Some(requested_agent) = requested_agent
            && replaced
        {
            replacements.push(Replacement {
                seat_name: name.clone(),
                requested_agent: requested_agent.id().into(),
                replacement_agent: selected.id().into(),
                reason: "requested agent is not locally ready".into(),
            });
        }
        let model = (!replaced).then_some(spec.model).flatten();
        let reasoning_effort = (!replaced).then_some(spec.reasoning_effort).flatten();
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
    Ok((seats, replacements))
}

fn detect_host_agent(explicit: Option<&str>) -> Option<String> {
    if let Some(explicit) = explicit.map(str::trim).filter(|value| !value.is_empty()) {
        return Some(
            AgentKind::parse(explicit).map_or_else(|| explicit.to_string(), |a| a.id().into()),
        );
    }
    if std::env::var_os("CLAUDE_CODE_SESSION_ID").is_some() {
        return Some("claude".into());
    }
    if std::env::var_os("CODEX_THREAD_ID").is_some()
        || std::env::var_os("CODEX_SESSION_ID").is_some()
    {
        return Some("codex".into());
    }
    if std::env::var_os("CURSOR_SESSION_ID").is_some() {
        return Some("cursor".into());
    }
    if std::env::var_os("GROK_SESSION_ID").is_some() {
        return Some("grok".into());
    }
    None
}

fn normalized_name(name: Option<&str>, id: &str) -> String {
    name.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| format!("room-{}", &id[..8]))
}

#[cfg(test)]
mod tests {
    use super::{select_seats, select_seats_with_names};
    use crate::mcp::ConferMcp;
    use crate::mcp::api::{RetireSeatArgs, SeatSpecInput};
    use crate::mcp::delivery::DeliveryRuntime;
    use crate::state::{StateStore, current_workspace};
    use crate::types::{AgentKind, HostRecord, Readiness, RoomRecord, SeatRecord, SeatStatus};

    fn ready(agent: AgentKind) -> Readiness {
        Readiness {
            agent,
            locally_ready: true,
            executable: Some(agent.id().into()),
            reason: None,
        }
    }

    fn room(id: &str, workspace: &str) -> RoomRecord {
        RoomRecord {
            id: id.into(),
            name: id.into(),
            workspace: workspace.into(),
            host: HostRecord {
                agent: Some("codex".into()),
            },
            seats: Vec::new(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    fn service(store: StateStore) -> ConferMcp {
        ConferMcp {
            store,
            runtime: DeliveryRuntime::new(),
            tool_router: ConferMcp::tool_router(),
        }
    }

    #[test]
    fn default_room_prefers_agents_other_than_host() {
        let readiness = vec![
            ready(AgentKind::Claude),
            ready(AgentKind::Codex),
            ready(AgentKind::Grok),
        ];
        let (seats, replacements) = select_seats(Vec::new(), 2, Some("codex"), &readiness).unwrap();
        assert_eq!(seats[0].agent, AgentKind::Claude);
        assert_eq!(seats[1].agent, AgentKind::Grok);
        assert!(replacements.is_empty());
    }

    #[test]
    fn unavailable_requested_agent_is_reported_and_replaced() {
        let request = SeatSpecInput {
            agent: Some("cursor".into()),
            model: Some("cursor-model".into()),
            reasoning_effort: Some("high".into()),
            name: Some("reviewer".into()),
            instructions: Some("Review only".into()),
        };
        let (seats, replacements) =
            select_seats(vec![request], 1, Some("codex"), &[ready(AgentKind::Claude)]).unwrap();
        assert_eq!(seats[0].agent, AgentKind::Claude);
        assert!(seats[0].model.is_none());
        assert_eq!(replacements.len(), 1);
        assert_eq!(replacements[0].seat_name, "reviewer");
    }

    #[test]
    fn added_seat_uses_a_unique_room_address() {
        let names = ["claude".to_string()].into_iter().collect();
        let request = SeatSpecInput {
            agent: Some("claude".into()),
            model: None,
            reasoning_effort: None,
            name: None,
            instructions: None,
        };
        let (seats, _) = select_seats_with_names(
            vec![request],
            1,
            Some("codex"),
            &[ready(AgentKind::Claude)],
            names,
        )
        .unwrap();

        assert_eq!(seats[0].name, "claude-2");
        assert_eq!(seats[0].status, SeatStatus::Active);
    }

    #[test]
    fn replacement_names_the_added_seat() {
        let names = ["claude".to_string()].into_iter().collect();
        let request = SeatSpecInput {
            agent: Some("cursor".into()),
            model: None,
            reasoning_effort: None,
            name: None,
            instructions: None,
        };
        let (seats, replacements) = select_seats_with_names(
            vec![request],
            1,
            Some("codex"),
            &[ready(AgentKind::Claude)],
            names,
        )
        .unwrap();

        assert_eq!(replacements[0].seat_name, seats[0].name);
    }

    #[test]
    fn seat_selection_validates_the_final_agent_configuration() {
        for (agent, model, effort) in [
            (AgentKind::Cursor, Some("model[effort=high]"), Some("high")),
            (AgentKind::Cursor, Some("model[effort"), None),
            (AgentKind::Claude, None, Some("invalid")),
            (AgentKind::Agy, None, Some("xhigh")),
        ] {
            let request = SeatSpecInput {
                agent: Some(agent.id().into()),
                model: model.map(str::to_owned),
                reasoning_effort: effort.map(str::to_owned),
                name: None,
                instructions: None,
            };
            assert!(select_seats(vec![request], 1, None, &[ready(agent)]).is_err());
        }
        let request = SeatSpecInput {
            agent: Some("cursor".into()),
            model: Some("model[effort".into()),
            reasoning_effort: Some("invalid".into()),
            name: None,
            instructions: Some("Review only".into()),
        };
        let (seats, replacements) =
            select_seats(vec![request], 1, None, &[ready(AgentKind::Claude)]).unwrap();
        assert_eq!(replacements.len(), 1);
        assert_eq!(seats[0].agent, AgentKind::Claude);
        assert!(seats[0].model.is_none());
        assert!(seats[0].reasoning_effort.is_none());
        assert_eq!(seats[0].instructions.as_deref(), Some("Review only"));
    }

    #[tokio::test]
    async fn retiring_seat_preserves_its_native_session() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::new(dir.path().join("rooms.json"));
        let workspace = current_workspace().unwrap();
        let mut record = room("room-1", &workspace.to_string_lossy());
        record.seats.push(SeatRecord {
            id: "seat-1".into(),
            name: "reviewer".into(),
            agent: AgentKind::Claude,
            model: None,
            reasoning_effort: None,
            instructions: None,
            native_session_id: Some("session-1".into()),
            status: SeatStatus::Active,
        });
        store
            .mutate(|state| {
                state.rooms.push(record);
                Ok(())
            })
            .unwrap();
        let service = service(store.clone());
        let mut worker = service.runtime.register_worker("room-1", "seat-1").await;
        assert!(service.runtime.has_worker("room-1", "seat-1").await);

        service
            .retire_seat_inner(RetireSeatArgs {
                room_id: "room-1".into(),
                seat: "reviewer".into(),
            })
            .await
            .unwrap();

        let seat = &store.load().unwrap().rooms[0].seats[0];
        assert_eq!(seat.status, SeatStatus::Retired);
        assert_eq!(seat.native_session_id.as_deref(), Some("session-1"));
        assert!(!service.runtime.has_worker("room-1", "seat-1").await);
        assert!(worker.closed().await);
    }
}
