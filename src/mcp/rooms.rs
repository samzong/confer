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
    if std::env::var_os("COPILOT_AGENT_SESSION_ID").is_some() {
        return Some("copilot".into());
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
    use super::select_seats;
    use crate::mcp::ConferMcp;
    use crate::mcp::api::{RetireSeatArgs, SeatSpecInput};
    use crate::mcp::delivery::DeliveryRuntime;
    use crate::state::StateStore;
    use crate::types::{AgentKind, HostRecord, Readiness, RoomRecord, SeatRecord, SeatStatus};
    use std::collections::HashSet;

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
    fn automatic_seats_prefer_other_agents_and_repeat_with_unique_addresses() {
        let readiness = vec![
            ready(AgentKind::Claude),
            ready(AgentKind::Codex),
            ready(AgentKind::Grok),
        ];
        let seats =
            select_seats(Vec::new(), 20, Some("codex"), &readiness, HashSet::new()).unwrap();
        assert_eq!(seats.len(), 20);
        assert_eq!(seats[0].agent, AgentKind::Claude);
        assert_eq!(seats[1].agent, AgentKind::Grok);
        assert_eq!(seats[2].agent, AgentKind::Codex);
        assert_eq!(seats[3].agent, AgentKind::Claude);
        assert_eq!(
            seats
                .iter()
                .map(|seat| &seat.id)
                .collect::<HashSet<_>>()
                .len(),
            seats.len()
        );
        assert_eq!(
            seats
                .iter()
                .map(|seat| &seat.name)
                .collect::<HashSet<_>>()
                .len(),
            seats.len()
        );
    }

    #[test]
    fn unavailable_requested_agent_fails_even_when_an_alternative_is_ready() {
        for readiness in [vec![ready(AgentKind::Claude)], Vec::new()] {
            let request = SeatSpecInput {
                agent: Some("cursor".into()),
                model: Some("cursor-model".into()),
                reasoning_effort: Some("high".into()),
                name: Some("reviewer".into()),
                instructions: Some("Review only".into()),
            };
            assert!(
                select_seats(vec![request], 1, Some("codex"), &readiness, HashSet::new()).is_err()
            );
        }
        assert!(select_seats(Vec::new(), 1, None, &[], HashSet::new()).is_err());
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
        let seats = select_seats(
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
    fn identical_configurations_create_separate_seats() {
        let requests = (0..2)
            .map(|_| SeatSpecInput {
                agent: Some("claude".into()),
                model: Some("sonnet".into()),
                reasoning_effort: Some("high".into()),
                name: None,
                instructions: Some("Review only".into()),
            })
            .collect();
        let seats = select_seats(
            requests,
            1,
            Some("claude"),
            &[ready(AgentKind::Claude)],
            HashSet::new(),
        )
        .unwrap();
        assert_eq!(seats.len(), 2);
        assert_ne!(seats[0].id, seats[1].id);
        assert_ne!(seats[0].name, seats[1].name);
        for seat in seats {
            assert_eq!(seat.agent, AgentKind::Claude);
            assert_eq!(seat.model.as_deref(), Some("sonnet"));
            assert_eq!(seat.reasoning_effort.as_deref(), Some("high"));
            assert_eq!(seat.instructions.as_deref(), Some("Review only"));
            assert!(seat.native_session_id.is_none());
        }
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
            assert!(select_seats(vec![request], 1, None, &[ready(agent)], HashSet::new()).is_err());
        }
    }

    #[tokio::test]
    async fn retiring_seat_preserves_its_native_session() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::new(dir.path().join("rooms.json"));
        let workspace = dir.path().canonicalize().unwrap();
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

        let output = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .arg(&workspace)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .unwrap();
        assert!(output.status.success());
        let subdir = workspace.join("src");
        std::fs::create_dir(&subdir).unwrap();
        let other = tempfile::tempdir().unwrap();
        let before = std::fs::read(store.path()).unwrap();
        for caller in [other.path().canonicalize().unwrap(), subdir] {
            assert!(
                service
                    .add_seat_inner(
                        serde_json::from_value(serde_json::json!({
                            "workspace": caller,
                            "room_id": "room-1",
                            "seat": {"agent": "claude"},
                        }))
                        .unwrap()
                    )
                    .is_err()
            );
            assert!(
                service
                    .retire_seat_inner(
                        serde_json::from_value(serde_json::json!({
                            "workspace": caller,
                            "room_id": "room-1",
                            "seat": "reviewer",
                        }))
                        .unwrap()
                    )
                    .await
                    .is_err()
            );
            assert!(
                service
                    .send_message_inner(
                        serde_json::from_value(serde_json::json!({
                            "workspace": caller,
                            "room_id": "room-1",
                            "recipients": ["reviewer"],
                            "message": "Review this change",
                        }))
                        .unwrap()
                    )
                    .await
                    .is_err()
            );
            assert!(
                service
                    .wait_output_inner(
                        serde_json::from_value(serde_json::json!({
                            "workspace": caller,
                            "room_id": "room-1",
                            "timeout_ms": 0,
                        }))
                        .unwrap()
                    )
                    .await
                    .is_err()
            );
            assert_eq!(std::fs::read(store.path()).unwrap(), before);
            assert!(service.runtime.has_worker("room-1", "seat-1").await);
        }

        service
            .retire_seat_inner(RetireSeatArgs {
                workspace,
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
