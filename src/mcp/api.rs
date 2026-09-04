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
    pub(super) scope: Option<RoomScope>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct AddSeatArgs {
    pub(super) room_id: String,
    pub(super) seat: SeatSpecInput,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct RetireSeatArgs {
    pub(super) room_id: String,
    pub(super) seat: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct SendMessageArgs {
    pub(super) room_id: String,
    pub(super) recipients: Vec<String>,
    pub(super) message: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct WaitOutputArgs {
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
    workspace: &str,
) -> Vec<RoomView> {
    let mut rooms = rooms
        .into_iter()
        .filter(|room| matches!(scope, RoomScope::All) || room.workspace == workspace)
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
    use super::{ListRoomsArgs, RoomScope, rooms_for_scope};
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
        let rooms = vec![room("current", "/tmp/current"), room("other", "/tmp/other")];

        assert_eq!(
            rooms_for_scope(rooms.clone(), RoomScope::Current, "/tmp/current").len(),
            1
        );
        assert_eq!(
            rooms_for_scope(rooms, RoomScope::All, "/tmp/current").len(),
            2
        );
    }

    #[test]
    fn null_room_scope_defaults_to_current() {
        let args: ListRoomsArgs = serde_json::from_value(serde_json::json!({ "scope": null }))
            .expect("null scope should deserialize");

        assert!(matches!(args.scope.unwrap_or_default(), RoomScope::Current));
    }
}
