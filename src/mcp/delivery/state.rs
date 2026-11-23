use serde::Serialize;

use crate::adapters;
use crate::types::AgentKind;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::mcp) enum DeliveryStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
    AwaitingAnswer,
}

#[derive(Clone, Debug, Serialize)]
pub(in crate::mcp) struct DeliveryState {
    pub(in crate::mcp) delivery_id: String,
    pub(in crate::mcp) room_id: String,
    pub(in crate::mcp) seat_id: String,
    pub(in crate::mcp) seat_name: String,
    pub(in crate::mcp) agent: AgentKind,
    pub(in crate::mcp) status: DeliveryStatus,
    pub(in crate::mcp) final_answer: Option<String>,
    pub(in crate::mcp) error: Option<String>,
    pub(in crate::mcp) resume_command: Option<String>,
    pub(in crate::mcp) answer_incomplete: bool,
    pub(in crate::mcp) previous_delivery_uncertain: bool,
    pub(in crate::mcp) question: Option<serde_json::Value>,
    pub(in crate::mcp) uncertain_delivery_id: Option<String>,
    #[serde(skip)]
    pub(in crate::mcp) cancel_requested: bool,
    #[serde(skip)]
    pub(in crate::mcp) cancellation: adapters::Cancellation,
}

impl DeliveryState {
    pub(in crate::mcp) fn terminal(&self) -> bool {
        matches!(
            self.status,
            DeliveryStatus::Completed
                | DeliveryStatus::Failed
                | DeliveryStatus::Cancelled
                | DeliveryStatus::AwaitingAnswer
        )
    }
}

pub(in crate::mcp) fn deliveries_completed(deliveries: &[DeliveryState]) -> bool {
    !deliveries.is_empty() && deliveries.iter().all(DeliveryState::terminal)
}

#[derive(Clone, Debug, Serialize)]
pub(in crate::mcp) struct SendReceipt {
    pub(in crate::mcp) delivery_id: String,
    pub(in crate::mcp) seat_id: String,
    pub(in crate::mcp) seat_name: String,
    pub(in crate::mcp) agent: AgentKind,
    pub(in crate::mcp) accepted: bool,
    pub(in crate::mcp) previous_delivery_uncertain: bool,
    pub(in crate::mcp) uncertain_delivery_id: Option<String>,
    pub(in crate::mcp) resume_command: Option<String>,
    pub(in crate::mcp) error: Option<String>,
}

#[derive(Debug, Serialize)]
pub(in crate::mcp) struct SendMessageOutput {
    pub(in crate::mcp) room_id: String,
    pub(in crate::mcp) deliveries: Vec<SendReceipt>,
}

#[derive(Debug, Serialize)]
pub(in crate::mcp) struct WaitOutput {
    pub(in crate::mcp) room_id: String,
    pub(in crate::mcp) completed: bool,
    pub(in crate::mcp) timed_out: bool,
    pub(in crate::mcp) deliveries: Vec<DeliveryState>,
}
