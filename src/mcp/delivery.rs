mod worker;
#[cfg(test)]
use worker::native_session_outcome;
use worker::run_seat_worker;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use tokio::sync::{Mutex, mpsc, watch};

use super::ConferMcp;
use super::api::{SendMessageArgs, WaitOutputArgs, timestamp};
use crate::adapters::{self, Invocation};
use crate::state::{StateStore, canonical_workspace};
use crate::types::{AgentKind, RoomRecord, SeatRecord, SeatStatus};

const DEFAULT_WAIT_MS: u64 = 120_000;
const MAX_WAIT_MS: u64 = 600_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum DeliveryStatus {
    Queued,
    Running,
    Completed,
    Failed,
}

#[derive(Clone, Debug, Serialize)]
struct DeliveryState {
    delivery_id: String,
    room_id: String,
    seat_id: String,
    seat_name: String,
    agent: AgentKind,
    status: DeliveryStatus,
    final_answer: Option<String>,
    error: Option<String>,
}

impl DeliveryState {
    fn terminal(&self) -> bool {
        matches!(
            self.status,
            DeliveryStatus::Completed | DeliveryStatus::Failed
        )
    }
}

fn deliveries_completed(deliveries: &[DeliveryState]) -> bool {
    !deliveries.is_empty() && deliveries.iter().all(DeliveryState::terminal)
}

#[derive(Debug, Serialize)]
struct SendReceipt {
    delivery_id: String,
    seat_id: String,
    seat_name: String,
    agent: AgentKind,
    accepted: bool,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct SendMessageOutput {
    room_id: String,
    deliveries: Vec<SendReceipt>,
}

#[derive(Debug, Serialize)]
pub(super) struct WaitOutput {
    room_id: String,
    completed: bool,
    timed_out: bool,
    deliveries: Vec<DeliveryState>,
}

#[derive(Clone)]
struct QueuedDelivery {
    delivery_id: String,
    room_id: String,
    seat_id: String,
    message: String,
    workspace: PathBuf,
}

#[cfg(test)]
pub(super) struct WorkerProbe(mpsc::UnboundedReceiver<QueuedDelivery>);

#[cfg(test)]
impl WorkerProbe {
    pub(super) async fn closed(&mut self) -> bool {
        self.0.recv().await.is_none()
    }
}

#[derive(Clone)]
struct DeliveryTracker {
    states: watch::Sender<HashMap<String, DeliveryState>>,
}

impl DeliveryTracker {
    fn new() -> Self {
        Self {
            states: watch::channel(HashMap::new()).0,
        }
    }

    fn subscribe(&self) -> watch::Receiver<HashMap<String, DeliveryState>> {
        self.states.subscribe()
    }

    #[cfg(test)]
    fn subscriber_count(&self) -> usize {
        self.states.receiver_count()
    }

    fn insert(&self, delivery: DeliveryState) {
        self.states.send_modify(|states| {
            states.insert(delivery.delivery_id.clone(), delivery);
        });
    }

    fn update(&self, delivery_id: &str, update: impl FnOnce(&mut DeliveryState)) {
        self.states.send_if_modified(|states| {
            let Some(delivery) = states.get_mut(delivery_id) else {
                return false;
            };
            update(delivery);
            true
        });
    }

    fn set_running(&self, delivery_id: &str) {
        self.update(delivery_id, |delivery| {
            delivery.status = DeliveryStatus::Running;
        });
    }

    fn set_failed(&self, delivery_id: &str, error: String) {
        self.update(delivery_id, |delivery| {
            delivery.status = DeliveryStatus::Failed;
            delivery.final_answer = None;
            delivery.error = Some(error);
        });
    }

    fn finish(
        &self,
        delivery_id: &str,
        mismatch: Option<String>,
        persistence_error: Option<String>,
        output_error: Option<String>,
        answer: Option<String>,
    ) {
        self.update(delivery_id, |delivery| {
            finish_delivery(delivery, mismatch, persistence_error, output_error, answer);
        });
    }

    fn snapshots(&self, room_id: &str, requested: &[String]) -> Result<Vec<DeliveryState>> {
        let map = self.states.borrow();
        if requested.is_empty() {
            let mut deliveries = map
                .values()
                .filter(|delivery| delivery.room_id == room_id)
                .cloned()
                .collect::<Vec<_>>();
            deliveries.sort_by(|left, right| left.delivery_id.cmp(&right.delivery_id));
            return Ok(deliveries);
        }
        let mut deliveries = Vec::new();
        for id in requested {
            let delivery = map
                .get(id)
                .filter(|delivery| delivery.room_id == room_id)
                .cloned()
                .with_context(|| format!("delivery '{id}' is unknown in room '{room_id}'"))?;
            deliveries.push(delivery);
        }
        Ok(deliveries)
    }
}

#[derive(Clone)]
pub(super) struct DeliveryRuntime {
    pub(super) activity: Option<Arc<crate::mcp::activity::Instance>>,
    deliveries: DeliveryTracker,
    workers: Arc<Mutex<HashMap<String, mpsc::UnboundedSender<QueuedDelivery>>>>,
}

impl DeliveryRuntime {
    pub(super) fn new() -> Self {
        Self {
            deliveries: DeliveryTracker::new(),
            activity: None,
            workers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(super) async fn stop_seat(&self, room_id: &str, seat_id: &str) {
        self.workers
            .lock()
            .await
            .remove(&seat_key(room_id, seat_id));
    }

    #[cfg(test)]
    pub(super) async fn register_worker(&self, room_id: &str, seat_id: &str) -> WorkerProbe {
        let (sender, receiver) = mpsc::unbounded_channel();
        self.workers
            .lock()
            .await
            .insert(seat_key(room_id, seat_id), sender);
        WorkerProbe(receiver)
    }

    #[cfg(test)]
    pub(super) async fn has_worker(&self, room_id: &str, seat_id: &str) -> bool {
        self.workers
            .lock()
            .await
            .contains_key(&seat_key(room_id, seat_id))
    }
}

impl ConferMcp {
    pub(super) async fn send_message_inner(
        &self,
        args: SendMessageArgs,
    ) -> Result<SendMessageOutput> {
        if args.message.trim().is_empty() {
            bail!("message must not be empty");
        }
        let workspace = canonical_workspace(&args.workspace)?;
        let room = self.store.room_for_workspace(&args.room_id, &workspace)?;
        let seats = resolve_recipients(&room, &args.recipients)?;
        let mut receipts = Vec::new();
        for seat in seats {
            receipts.push(self.enqueue_delivery(&room, seat, &args.message).await);
        }
        Ok(SendMessageOutput {
            room_id: room.id,
            deliveries: receipts,
        })
    }

    async fn enqueue_delivery(
        &self,
        room: &RoomRecord,
        seat: &SeatRecord,
        message: &str,
    ) -> SendReceipt {
        let readiness = adapters::check_readiness(seat.agent);
        let mut error = (!readiness.locally_ready).then(|| {
            readiness
                .reason
                .unwrap_or_else(|| "agent is not locally ready".into())
        });
        let delivery_id = uuid::Uuid::new_v4().to_string();
        self.runtime.deliveries.insert(DeliveryState {
            delivery_id: delivery_id.clone(),
            room_id: room.id.clone(),
            seat_id: seat.id.clone(),
            seat_name: seat.name.clone(),
            agent: seat.agent,
            status: if error.is_some() {
                DeliveryStatus::Failed
            } else {
                DeliveryStatus::Queued
            },
            final_answer: None,
            error: error.clone(),
        });
        if error.is_none() {
            let queued = QueuedDelivery {
                delivery_id: delivery_id.clone(),
                room_id: room.id.clone(),
                seat_id: seat.id.clone(),
                message: message.to_string(),
                workspace: PathBuf::from(&room.workspace),
            };
            let sender = self.worker_sender(seat_key(&room.id, &seat.id)).await;
            if sender.send(queued).is_err() {
                let failure = format!("queue worker for seat '{}' stopped", seat.name);
                self.runtime
                    .deliveries
                    .set_failed(&delivery_id, failure.clone());
                error = Some(failure);
            }
        }
        SendReceipt {
            delivery_id,
            seat_id: seat.id.clone(),
            seat_name: seat.name.clone(),
            agent: seat.agent,
            accepted: error.is_none(),
            error,
        }
    }

    async fn worker_sender(&self, key: String) -> mpsc::UnboundedSender<QueuedDelivery> {
        let mut workers = self.runtime.workers.lock().await;
        if let Some(sender) = workers.get(&key)
            && !sender.is_closed()
        {
            return sender.clone();
        }
        let (sender, receiver) = mpsc::unbounded_channel();
        tokio::spawn(run_seat_worker(
            receiver,
            self.store.clone(),
            self.runtime.deliveries.clone(),
            self.runtime.activity.clone(),
        ));
        workers.insert(key, sender.clone());
        sender
    }

    pub(super) async fn wait_output_inner(&self, args: WaitOutputArgs) -> Result<WaitOutput> {
        let workspace = canonical_workspace(&args.workspace)?;
        self.store.room_for_workspace(&args.room_id, &workspace)?;
        let timeout_ms = args.timeout_ms.unwrap_or(DEFAULT_WAIT_MS).min(MAX_WAIT_MS);
        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
        let mut updates = self.runtime.deliveries.subscribe();
        loop {
            let deliveries = self
                .runtime
                .deliveries
                .snapshots(&args.room_id, &args.delivery_ids)?;
            let empty = args.delivery_ids.is_empty() && deliveries.is_empty();
            let completed = deliveries_completed(&deliveries);
            let timed_out = !empty && !completed && tokio::time::Instant::now() >= deadline;
            if empty || completed || timed_out || timeout_ms == 0 {
                return Ok(WaitOutput {
                    room_id: args.room_id,
                    completed,
                    timed_out,
                    deliveries,
                });
            }
            tokio::select! {
                _ = updates.changed() => {}
                _ = tokio::time::sleep_until(deadline) => {}
            }
        }
    }
}

fn seat_key(room_id: &str, seat_id: &str) -> String {
    format!("{room_id}:{seat_id}")
}

fn resolve_recipients<'a>(
    room: &'a RoomRecord,
    recipients: &[String],
) -> Result<Vec<&'a SeatRecord>> {
    if recipients.is_empty() {
        bail!("recipients must not be empty; use '*' to broadcast");
    }
    if recipients.iter().any(|recipient| recipient == "*") {
        if recipients.len() != 1 {
            bail!("recipient '*' must be used alone");
        }
        let seats = room
            .seats
            .iter()
            .filter(|seat| seat.status == SeatStatus::Active)
            .collect::<Vec<_>>();
        if seats.is_empty() {
            bail!("room '{}' has no active seats", room.id);
        }
        return Ok(seats);
    }
    let mut seen = HashSet::new();
    let mut seats = Vec::new();
    for recipient in recipients {
        let seat = room
            .seats
            .iter()
            .find(|seat| seat.id == *recipient || seat.name == *recipient)
            .with_context(|| format!("unknown seat '{recipient}' in room '{}'", room.id))?;
        if seat.status == SeatStatus::Retired {
            bail!("seat '{}' is retired", seat.name);
        }
        if seen.insert(&seat.id) {
            seats.push(seat);
        }
    }
    Ok(seats)
}

fn finish_delivery(
    delivery: &mut DeliveryState,
    mismatch: Option<String>,
    persistence_error: Option<String>,
    output_error: Option<String>,
    answer: Option<String>,
) {
    if output_error.is_none() {
        delivery.final_answer = answer;
    }
    if delivery.status != DeliveryStatus::Running {
        return;
    }
    if let Some(error) = mismatch.or(persistence_error).or(output_error) {
        delivery.status = DeliveryStatus::Failed;
        delivery.error = Some(error);
    } else {
        delivery.status = DeliveryStatus::Completed;
    }
}

#[cfg(test)]
mod tests;
