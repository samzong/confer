use std::path::PathBuf;

use chrono::{SecondsFormat, Utc};
use rmcp::schemars;
use serde::{Deserialize, Serialize};

use crate::adapters;
use crate::state::StateStore;
use crate::types::{AgentKind, Readiness, RoomRecord, SeatStatus};

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
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

#[derive(Debug, schemars::JsonSchema)]
pub(super) struct CreateRoomArgs {
    pub(super) workspace: PathBuf,
    #[serde(default)]
    pub(super) name: Option<String>,
    #[serde(default)]
    #[schemars(range(min = 1))]
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
    #[serde(default)]
    pub(super) full_output: Option<bool>,
}

#[derive(Debug, schemars::JsonSchema)]
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

#[derive(Debug, schemars::JsonSchema)]
pub(super) struct SendMessageArgs {
    pub(super) workspace: PathBuf,
    pub(super) room_id: String,
    pub(super) recipients: Vec<String>,
    pub(super) message: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct WaitOutputArgs {
    pub(super) workspace: PathBuf,
    pub(super) room_id: String,
    #[serde(default)]
    pub(super) delivery_ids: Vec<String>,
    #[serde(default)]
    pub(super) timeout_ms: Option<u64>,
}

#[derive(Debug, schemars::JsonSchema)]
pub(super) struct WaitOutputParams {
    pub(super) workspace: PathBuf,
    pub(super) room_id: String,
    #[serde(default)]
    pub(super) delivery_ids: Vec<String>,
    #[serde(default)]
    #[schemars(range(min = 0, max = 600_000))]
    pub(super) timeout_ms: Option<u64>,
    #[serde(default)]
    pub(super) full_output: Option<bool>,
}

#[derive(Debug, schemars::JsonSchema)]
pub(super) struct CancelDeliveryArgs {
    pub(super) workspace: PathBuf,
    pub(super) room_id: String,
    #[schemars(length(min = 1))]
    pub(super) delivery_ids: Vec<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct ClearUncertainDeliveryArgs {
    pub(super) workspace: PathBuf,
    pub(super) room_id: String,
    pub(super) seat: String,
    pub(super) delivery_id: String,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct SeatView {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) agent: AgentKind,
    pub(super) model: Option<String>,
    pub(super) reasoning_effort: Option<String>,
    pub(super) native_session: bool,
    pub(super) resume_command: Option<String>,
    pub(super) previous_delivery_uncertain: bool,
    pub(super) uncertain_delivery_id: Option<String>,
    pub(super) status: SeatStatus,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct RoomView {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) workspace: String,
    pub(super) host_agent: Option<String>,
    pub(super) seats: Vec<SeatView>,
    #[serde(skip_serializing_if = "super::inline::is_zero")]
    pub(super) omitted_seats: usize,
    pub(super) created_at: String,
    pub(super) updated_at: String,
}

#[derive(Debug, Serialize)]
pub(super) struct CreateRoomOutput {
    pub(super) room: RoomView,
    pub(super) readiness: Vec<Readiness>,
}

#[derive(Debug, Serialize)]
pub(super) struct ListRoomsOutput {
    pub(super) scope: RoomScope,
    pub(super) workspace: Option<String>,
    pub(super) rooms: Vec<RoomView>,
    #[serde(skip_serializing_if = "super::inline::is_zero")]
    pub(super) omitted_rooms: usize,
}

pub(super) type AddSeatOutput = CreateRoomOutput;

#[derive(Debug, Serialize)]
pub(super) struct RoomOutput {
    pub(super) room: RoomView,
    #[serde(skip)]
    pub(super) priority_seat_id: Option<String>,
}

pub(super) fn room_view(room: &RoomRecord, store: &StateStore) -> RoomView {
    RoomView {
        id: room.id.clone(),
        name: room.name.clone(),
        workspace: room.workspace.clone(),
        host_agent: room.host.agent.clone(),
        seats: room
            .seats
            .iter()
            .map(|seat| {
                let uncertain_delivery_id = store.uncertain_delivery_id(&room.id, &seat.id);
                SeatView {
                    id: seat.id.clone(),
                    name: seat.name.clone(),
                    agent: seat.agent,
                    model: seat.model.clone(),
                    reasoning_effort: seat.reasoning_effort.clone(),
                    native_session: seat.native_session_id.is_some(),
                    resume_command: seat.native_session_id.as_deref().and_then(|session| {
                        adapters::resume_command(seat.agent, &room.workspace, session)
                    }),
                    previous_delivery_uncertain: uncertain_delivery_id.is_some(),
                    uncertain_delivery_id,
                    status: seat.status,
                }
            })
            .collect(),
        omitted_seats: 0,
        created_at: room.created_at.clone(),
        updated_at: room.updated_at.clone(),
    }
}

pub(super) fn rooms_for_scope(
    rooms: Vec<RoomRecord>,
    scope: RoomScope,
    workspace: Option<&str>,
    store: &StateStore,
) -> Vec<RoomView> {
    let mut rooms = rooms
        .into_iter()
        .filter(|room| {
            matches!(scope, RoomScope::All) || Some(room.workspace.as_str()) == workspace
        })
        .map(|room| room_view(&room, store))
        .collect::<Vec<_>>();
    rooms.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    rooms
}

pub(super) fn timestamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests;
