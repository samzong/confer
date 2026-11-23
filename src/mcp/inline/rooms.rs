use serde::Serialize;

use super::{
    INLINE_MAX_BYTES, fold_text_to_json_budget, is_zero, json_escaped_str_len,
    sanitize_terminal_text, serialized_len,
};
use crate::mcp::api::{CreateRoomOutput, ListRoomsOutput, RoomOutput, RoomView, SeatView};
use crate::mcp::delivery::{SendMessageOutput, SendReceipt};
use crate::types::{Readiness, SeatStatus};

#[derive(Clone, Debug, Serialize)]
pub(in crate::mcp) struct SendMessageOutputView {
    pub(in crate::mcp) room_id: String,
    pub(in crate::mcp) deliveries: Vec<SendReceipt>,
    #[serde(skip_serializing_if = "is_zero")]
    pub(in crate::mcp) omitted_deliveries: usize,
}

fn seat_priority_rank(seat: &SeatView, target_seat_id: Option<&str>) -> u8 {
    if let Some(target) = target_seat_id
        && (seat.id == target || seat.name == target)
    {
        return 0;
    }
    if seat.previous_delivery_uncertain {
        return 1;
    }
    match seat.status {
        SeatStatus::Active => 2,
        SeatStatus::Retired => 3,
    }
}

fn select_seats_for_count(
    seats: &[SeatView],
    keep: usize,
    target_seat_id: Option<&str>,
) -> Vec<SeatView> {
    if keep >= seats.len() {
        return seats.to_vec();
    }
    let mut ranked: Vec<(u8, usize)> = seats
        .iter()
        .enumerate()
        .map(|(idx, seat)| (seat_priority_rank(seat, target_seat_id), idx))
        .collect();
    ranked.sort_by_key(|&(rank, idx)| (rank, idx));
    let mut chosen: Vec<usize> = ranked.into_iter().take(keep).map(|(_, idx)| idx).collect();
    chosen.sort_unstable();
    chosen.into_iter().map(|idx| seats[idx].clone()).collect()
}

fn trim_room_seats_with_measure<F>(
    room: &RoomView,
    target_seat_id: Option<&str>,
    mut fits: F,
) -> RoomView
where
    F: FnMut(&RoomView) -> bool,
{
    if fits(room) {
        return room.clone();
    }
    let total = room.seats.len();
    let mut low = 0usize;
    let mut high = total;
    let mut best: Option<RoomView> = None;
    while low <= high {
        let mid = low + (high - low) / 2;
        let mut candidate = room.clone();
        candidate.seats = select_seats_for_count(&room.seats, mid, target_seat_id);
        candidate.omitted_seats = total - mid;
        if fits(&candidate) {
            best = Some(candidate);
            if mid == total {
                break;
            }
            low = mid + 1;
        } else if mid == 0 {
            break;
        } else {
            high = mid - 1;
        }
    }
    if let Some(fitted) = best {
        return fitted;
    }
    let mut minimal = room.clone();
    minimal.seats = Vec::new();
    minimal.omitted_seats = total;
    if !fits(&minimal) {
        let original_name = minimal.name.clone();
        let mut low_budget = 0usize;
        let mut high_budget = json_escaped_str_len(&original_name);
        let mut best_name = String::new();
        while low_budget <= high_budget {
            let mid_budget = low_budget + (high_budget - low_budget) / 2;
            let (candidate_name, _) = fold_text_to_json_budget(&original_name, mid_budget, None);
            minimal.name = candidate_name.clone();
            if fits(&minimal) {
                best_name = candidate_name;
                low_budget = mid_budget + 1;
            } else if mid_budget == 0 {
                break;
            } else {
                high_budget = mid_budget - 1;
            }
        }
        minimal.name = best_name;
    }
    minimal
}

pub(in crate::mcp) fn render_list_rooms(
    output: ListRoomsOutput,
    full_output: bool,
) -> ListRoomsOutput {
    if full_output || serialized_len(&output) <= INLINE_MAX_BYTES {
        return output;
    }
    let total_rooms = output.rooms.len();
    if total_rooms == 0 {
        return output;
    }

    let scope = output.scope;
    let workspace = output.workspace.clone();
    let mut kept_rooms = Vec::new();
    for (idx, room) in output.rooms.into_iter().enumerate() {
        let remaining_after_this = total_rooms - (idx + 1);
        let probe = ListRoomsOutput {
            scope,
            workspace: workspace.clone(),
            rooms: {
                let mut next = kept_rooms.clone();
                next.push(room.clone());
                next
            },
            omitted_rooms: remaining_after_this,
        };
        if serialized_len(&probe) <= INLINE_MAX_BYTES {
            kept_rooms.push(room);
            continue;
        }
        if kept_rooms.is_empty() {
            let fitted_first = trim_room_seats_with_measure(&room, None, |candidate_room| {
                let single_probe = ListRoomsOutput {
                    scope,
                    workspace: workspace.clone(),
                    rooms: vec![candidate_room.clone()],
                    omitted_rooms: remaining_after_this,
                };
                serialized_len(&single_probe) <= INLINE_MAX_BYTES
            });
            kept_rooms.push(fitted_first);
        }
        break;
    }

    let omitted_rooms = total_rooms - kept_rooms.len();
    ListRoomsOutput {
        scope,
        workspace,
        rooms: kept_rooms,
        omitted_rooms,
    }
}

fn sanitize_readiness(items: Vec<Readiness>, fold_reasons: bool) -> Vec<Readiness> {
    items
        .into_iter()
        .map(|mut item| {
            if let Some(reason) = item.reason.take() {
                let cleaned = sanitize_terminal_text(&reason);
                item.reason = Some(if fold_reasons {
                    fold_text_to_json_budget(&cleaned, 240, None).0
                } else {
                    cleaned
                });
            }
            item
        })
        .collect()
}

pub(in crate::mcp) fn render_create_room(
    mut output: CreateRoomOutput,
    target_seat_id: Option<&str>,
) -> CreateRoomOutput {
    output.readiness = sanitize_readiness(output.readiness, false);
    if serialized_len(&output) <= INLINE_MAX_BYTES {
        return output;
    }
    let readiness = sanitize_readiness(output.readiness, true);
    let fitted_room = trim_room_seats_with_measure(&output.room, target_seat_id, |candidate| {
        let probe = CreateRoomOutput {
            room: candidate.clone(),
            readiness: readiness.clone(),
        };
        serialized_len(&probe) <= INLINE_MAX_BYTES
    });
    CreateRoomOutput {
        room: fitted_room,
        readiness,
    }
}

pub(in crate::mcp) fn render_room_output(output: RoomOutput) -> RoomOutput {
    if serialized_len(&output) <= INLINE_MAX_BYTES {
        return output;
    }
    let target = output.priority_seat_id.clone();
    let fitted = trim_room_seats_with_measure(&output.room, target.as_deref(), |candidate| {
        let probe = RoomOutput {
            room: candidate.clone(),
            priority_seat_id: None,
        };
        serialized_len(&probe) <= INLINE_MAX_BYTES
    });
    RoomOutput {
        room: fitted,
        priority_seat_id: None,
    }
}

pub(in crate::mcp) fn render_send_message(output: SendMessageOutput) -> SendMessageOutputView {
    let mut receipts: Vec<SendReceipt> = output
        .deliveries
        .into_iter()
        .map(|mut receipt| {
            if let Some(err) = receipt.error.take() {
                receipt.error = Some(sanitize_terminal_text(&err));
            }
            receipt
        })
        .collect();
    let full_view = SendMessageOutputView {
        room_id: output.room_id.clone(),
        deliveries: receipts.clone(),
        omitted_deliveries: 0,
    };
    if serialized_len(&full_view) <= INLINE_MAX_BYTES {
        return full_view;
    }
    for receipt in &mut receipts {
        if let Some(err) = receipt.error.take() {
            receipt.error = Some(fold_text_to_json_budget(&err, 480, None).0);
        }
    }
    let folded_view = SendMessageOutputView {
        room_id: output.room_id.clone(),
        deliveries: receipts.clone(),
        omitted_deliveries: 0,
    };
    if serialized_len(&folded_view) <= INLINE_MAX_BYTES {
        return folded_view;
    }
    let total = receipts.len();
    let mut low = 0usize;
    let mut high = total;
    let mut best_keep = 0usize;
    while low <= high {
        let mid = low + (high - low) / 2;
        let probe = SendMessageOutputView {
            room_id: output.room_id.clone(),
            deliveries: receipts[..mid].to_vec(),
            omitted_deliveries: total - mid,
        };
        if serialized_len(&probe) <= INLINE_MAX_BYTES {
            best_keep = mid;
            if mid == total {
                break;
            }
            low = mid + 1;
        } else if mid == 0 {
            break;
        } else {
            high = mid - 1;
        }
    }
    SendMessageOutputView {
        room_id: output.room_id,
        deliveries: receipts.into_iter().take(best_keep).collect(),
        omitted_deliveries: total - best_keep,
    }
}
