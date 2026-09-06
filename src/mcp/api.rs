use std::path::PathBuf;

use anyhow::Result;
use chrono::{SecondsFormat, Utc};
use rmcp::model::CallToolResult;
use rmcp::schemars;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::{AgentKind, Readiness, Replacement, RoomRecord, SeatStatus};

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct SeatSpecInput {
    #[serde(default)]
    pub(super) agent: Option<String>,
    #[serde(default)]
    pub(super) model: Option<String>,
    #[serde(default)]
    pub(super) reasoning_effort: Option<String>,
    #[serde(default)]
    pub(super) name: Option<String>,
    #[serde(default)]
    pub(super) instructions: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct CreateRoomArgs {
    pub(super) workspace: PathBuf,
    #[serde(default)]
    pub(super) name: Option<String>,
    #[serde(default)]
    #[schemars(range(min = 2, max = 16))]
    pub(super) target_size: Option<usize>,
    #[serde(default)]
    pub(super) host_agent: Option<String>,
    #[serde(default)]
    pub(super) seats: Vec<SeatSpecInput>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum RoomScope {
    #[default]
    Current,
    All,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct ListRoomsArgs {
    #[serde(default)]
    pub(super) workspace: Option<PathBuf>,
    #[serde(default)]
    pub(super) scope: Option<RoomScope>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct AddSeatArgs {
    pub(super) workspace: PathBuf,
    pub(super) room_id: String,
    pub(super) seat: SeatSpecInput,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct RetireSeatArgs {
    pub(super) workspace: PathBuf,
    pub(super) room_id: String,
    pub(super) seat: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct SendMessageArgs {
    pub(super) workspace: PathBuf,
    pub(super) room_id: String,
    pub(super) recipients: Vec<String>,
    pub(super) message: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct WaitOutputArgs {
    pub(super) workspace: PathBuf,
    pub(super) room_id: String,
    #[serde(default)]
    pub(super) delivery_ids: Vec<String>,
    #[serde(default)]
    #[schemars(range(min = 0, max = 600_000))]
    pub(super) timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct SeatView {
    id: String,
    name: String,
    agent: AgentKind,
    model: Option<String>,
    reasoning_effort: Option<String>,
    native_session: bool,
    status: SeatStatus,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct RoomView {
    id: String,
    name: String,
    workspace: String,
    host_agent: Option<String>,
    seats: Vec<SeatView>,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Serialize)]
pub(super) struct CreateRoomOutput {
    pub(super) room: RoomView,
    pub(super) readiness: Vec<Readiness>,
    pub(super) replacements: Vec<Replacement>,
}

#[derive(Debug, Serialize)]
pub(super) struct ListRoomsOutput {
    pub(super) scope: RoomScope,
    pub(super) workspace: Option<String>,
    pub(super) rooms: Vec<RoomView>,
}

#[derive(Debug, Serialize)]
pub(super) struct AddSeatOutput {
    pub(super) room: RoomView,
    pub(super) readiness: Vec<Readiness>,
    pub(super) replacements: Vec<Replacement>,
}

#[derive(Debug, Serialize)]
pub(super) struct RetireSeatOutput {
    pub(super) room: RoomView,
}

#[derive(Debug, Serialize)]
struct ErrorOutput {
    error: String,
}

pub(super) fn room_view(room: &RoomRecord) -> RoomView {
    RoomView {
        id: room.id.clone(),
        name: room.name.clone(),
        workspace: room.workspace.clone(),
        host_agent: room.host.agent.clone(),
        seats: room
            .seats
            .iter()
            .map(|seat| SeatView {
                id: seat.id.clone(),
                name: seat.name.clone(),
                agent: seat.agent,
                model: seat.model.clone(),
                reasoning_effort: seat.reasoning_effort.clone(),
                native_session: seat.native_session_id.is_some(),
                status: seat.status,
            })
            .collect(),
        created_at: room.created_at.clone(),
        updated_at: room.updated_at.clone(),
    }
}

pub(super) fn rooms_for_scope(
    rooms: Vec<RoomRecord>,
    scope: RoomScope,
    workspace: Option<&str>,
) -> Vec<RoomView> {
    let mut rooms = rooms
        .into_iter()
        .filter(|room| {
            matches!(scope, RoomScope::All) || Some(room.workspace.as_str()) == workspace
        })
        .map(|room| room_view(&room))
        .collect::<Vec<_>>();
    rooms.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    rooms
}

pub(super) fn timestamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub(super) fn json_result<T: Serialize>(result: Result<T>) -> CallToolResult {
    match result {
        Ok(value) => CallToolResult::structured(to_json(value)),
        Err(error) => CallToolResult::structured_error(to_json(ErrorOutput {
            error: error.to_string(),
        })),
    }
}

fn to_json(value: impl Serialize) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::{ListRoomsArgs, RoomScope};
    use crate::mcp::ConferMcp;
    use crate::mcp::delivery::DeliveryRuntime;
    use crate::state::StateStore;
    use crate::types::{HostRecord, RoomRecord};

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

    #[test]
    fn all_scope_lists_rooms_across_workspaces() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().canonicalize().unwrap();
        let other = tempfile::tempdir().unwrap();
        let store = StateStore::new(dir.path().join("rooms.json"));
        store
            .mutate(|state| {
                state.rooms = vec![
                    room("current", &workspace.to_string_lossy()),
                    room(
                        "other",
                        &other.path().canonicalize().unwrap().to_string_lossy(),
                    ),
                ];
                Ok(())
            })
            .unwrap();
        let server = ConferMcp {
            store,
            runtime: DeliveryRuntime::new(),
            tool_router: ConferMcp::tool_router(),
        };
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

        for scope in [serde_json::Value::Null, serde_json::json!("current")] {
            let args = serde_json::from_value(serde_json::json!({
                "scope": scope,
                "workspace": subdir,
            }))
            .unwrap();
            let output = server.list_rooms_inner(args).unwrap();
            assert_eq!(output.workspace.as_deref(), workspace.to_str());
            assert_eq!(output.rooms.len(), 1);
            assert_eq!(output.rooms[0].id, "current");
        }
        for value in [
            serde_json::json!({}),
            serde_json::json!({"scope": "current"}),
            serde_json::json!({"scope": null, "workspace": null}),
        ] {
            assert!(
                server
                    .list_rooms_inner(serde_json::from_value(value).unwrap())
                    .is_err()
            );
        }
        for value in [
            serde_json::json!({"scope": "all"}),
            serde_json::json!({"scope": "all", "workspace": "relative/missing"}),
        ] {
            let output = server
                .list_rooms_inner(serde_json::from_value(value).unwrap())
                .unwrap();
            assert!(output.workspace.is_none());
            assert_eq!(output.rooms.len(), 2);
        }
    }

    #[test]
    fn null_room_scope_defaults_to_current() {
        let args: ListRoomsArgs = serde_json::from_value(serde_json::json!({ "scope": null }))
            .expect("null scope should deserialize");

        assert!(matches!(args.scope.unwrap_or_default(), RoomScope::Current));
    }
}
