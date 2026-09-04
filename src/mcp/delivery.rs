use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use tokio::sync::{Mutex, mpsc};

use super::ConferMcp;
use super::api::{SendMessageArgs, WaitOutputArgs, timestamp};
use crate::adapters::{self, Invocation};
use crate::state::{StateStore, current_workspace};
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
pub(super) struct DeliveryRuntime {
    deliveries: Arc<Mutex<HashMap<String, DeliveryState>>>,
    workers: Arc<Mutex<HashMap<String, mpsc::UnboundedSender<QueuedDelivery>>>>,
}

impl DeliveryRuntime {
    pub(super) fn new() -> Self {
        Self {
            deliveries: Arc::new(Mutex::new(HashMap::new())),
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
        let workspace = current_workspace()?;
        let room = self.store.room_for_workspace(&args.room_id, &workspace)?;
        let seats = resolve_recipients(&room, &args.recipients)?;
        let mut receipts = Vec::new();
        for seat in seats {
            receipts.push(
                self.enqueue_delivery(&room, seat, &args.message, workspace.clone())
                    .await,
            );
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
        workspace: PathBuf,
    ) -> SendReceipt {
        let readiness = adapters::check_readiness(seat.agent);
        if !readiness.locally_ready {
            let error = readiness
                .reason
                .unwrap_or_else(|| "agent is not locally ready".into());
            return self.failed_delivery(room, seat, error).await;
        }
        let delivery_id = uuid::Uuid::new_v4().to_string();
        self.runtime.deliveries.lock().await.insert(
            delivery_id.clone(),
            DeliveryState {
                delivery_id: delivery_id.clone(),
                room_id: room.id.clone(),
                seat_id: seat.id.clone(),
                seat_name: seat.name.clone(),
                agent: seat.agent,
                status: DeliveryStatus::Queued,
                final_answer: None,
                error: None,
            },
        );
        let queued = QueuedDelivery {
            delivery_id: delivery_id.clone(),
            room_id: room.id.clone(),
            seat_id: seat.id.clone(),
            message: message.to_string(),
            workspace,
        };
        let key = seat_key(&room.id, &seat.id);
        let sender = self.worker_sender(key).await;
        if sender.send(queued).is_err() {
            let error = format!("queue worker for seat '{}' stopped", seat.name);
            self.mark_delivery_failed(&delivery_id, error.clone()).await;
            return SendReceipt {
                delivery_id,
                seat_id: seat.id.clone(),
                seat_name: seat.name.clone(),
                agent: seat.agent,
                accepted: false,
                error: Some(error),
            };
        }
        SendReceipt {
            delivery_id,
            seat_id: seat.id.clone(),
            seat_name: seat.name.clone(),
            agent: seat.agent,
            accepted: true,
            error: None,
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
        ));
        workers.insert(key, sender.clone());
        sender
    }

    async fn failed_delivery(
        &self,
        room: &RoomRecord,
        seat: &SeatRecord,
        error: String,
    ) -> SendReceipt {
        let delivery_id = uuid::Uuid::new_v4().to_string();
        self.runtime.deliveries.lock().await.insert(
            delivery_id.clone(),
            DeliveryState {
                delivery_id: delivery_id.clone(),
                room_id: room.id.clone(),
                seat_id: seat.id.clone(),
                seat_name: seat.name.clone(),
                agent: seat.agent,
                status: DeliveryStatus::Failed,
                final_answer: None,
                error: Some(error.clone()),
            },
        );
        SendReceipt {
            delivery_id,
            seat_id: seat.id.clone(),
            seat_name: seat.name.clone(),
            agent: seat.agent,
            accepted: false,
            error: Some(error),
        }
    }

    async fn mark_delivery_failed(&self, delivery_id: &str, error: String) {
        if let Some(delivery) = self.runtime.deliveries.lock().await.get_mut(delivery_id) {
            delivery.status = DeliveryStatus::Failed;
            delivery.final_answer = None;
            delivery.error = Some(error);
        }
    }

    pub(super) async fn wait_output_inner(&self, args: WaitOutputArgs) -> Result<WaitOutput> {
        let workspace = current_workspace()?;
        self.store.room_for_workspace(&args.room_id, &workspace)?;
        let timeout_ms = args.timeout_ms.unwrap_or(DEFAULT_WAIT_MS).min(MAX_WAIT_MS);
        let started = tokio::time::Instant::now();
        loop {
            let deliveries = self
                .delivery_snapshots(&args.room_id, &args.delivery_ids)
                .await?;
            if args.delivery_ids.is_empty() && deliveries.is_empty() {
                return Ok(WaitOutput {
                    room_id: args.room_id,
                    completed: false,
                    timed_out: false,
                    deliveries,
                });
            }
            let completed = deliveries_completed(&deliveries);
            let timed_out = !completed && started.elapsed() >= Duration::from_millis(timeout_ms);
            if completed || timed_out || timeout_ms == 0 {
                return Ok(WaitOutput {
                    room_id: args.room_id,
                    completed,
                    timed_out,
                    deliveries,
                });
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn delivery_snapshots(
        &self,
        room_id: &str,
        requested: &[String],
    ) -> Result<Vec<DeliveryState>> {
        let map = self.runtime.deliveries.lock().await;
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

async fn run_seat_worker(
    mut receiver: mpsc::UnboundedReceiver<QueuedDelivery>,
    store: StateStore,
    deliveries: Arc<Mutex<HashMap<String, DeliveryState>>>,
) {
    while let Some(queued) = receiver.recv().await {
        let session_guard = loop {
            match store.try_acquire_seat_lease(&queued.room_id, &queued.seat_id) {
                Ok(guard) => break Some(guard),
                Err(error) if error.to_string().starts_with("seat_busy:") => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(error) => {
                    set_delivery_failed(&deliveries, &queued.delivery_id, error.to_string()).await;
                    break None;
                }
            }
        };
        let Some(_session_guard) = session_guard else {
            continue;
        };
        process_queued_delivery(&queued, &store, &deliveries).await;
    }
}

async fn process_queued_delivery(
    queued: &QueuedDelivery,
    store: &StateStore,
    deliveries: &Arc<Mutex<HashMap<String, DeliveryState>>>,
) {
    let room = match store.room_for_workspace(&queued.room_id, &queued.workspace) {
        Ok(room) => room,
        Err(error) => {
            set_delivery_failed(deliveries, &queued.delivery_id, error.to_string()).await;
            return;
        }
    };
    let Some(seat) = room
        .seats
        .iter()
        .find(|seat| seat.id == queued.seat_id)
        .cloned()
    else {
        set_delivery_failed(
            deliveries,
            &queued.delivery_id,
            format!("seat '{}' disappeared before delivery", queued.seat_id),
        )
        .await;
        return;
    };
    if seat.status == SeatStatus::Retired {
        set_delivery_failed(
            deliveries,
            &queued.delivery_id,
            format!("seat '{}' is retired", seat.name),
        )
        .await;
        return;
    }
    let readiness = adapters::check_readiness(seat.agent);
    if !readiness.locally_ready {
        set_delivery_failed(
            deliveries,
            &queued.delivery_id,
            readiness
                .reason
                .unwrap_or_else(|| "agent is not locally ready".into()),
        )
        .await;
        return;
    }
    if let Some(delivery) = deliveries.lock().await.get_mut(&queued.delivery_id) {
        delivery.status = DeliveryStatus::Running;
    }
    let first_message = seat.native_session_id.is_none();
    let executable = match readiness.executable {
        Some(executable) => PathBuf::from(executable),
        None => {
            set_delivery_failed(
                deliveries,
                &queued.delivery_id,
                "agent readiness returned no executable".into(),
            )
            .await;
            return;
        }
    };
    let reserved = if first_message {
        match adapters::reserve_session(seat.agent, &executable).await {
            Ok(session) => session,
            Err(error) => {
                set_delivery_failed(deliveries, &queued.delivery_id, error.to_string()).await;
                return;
            }
        }
    } else {
        seat.native_session_id.clone()
    };
    if first_message
        && seat.agent == AgentKind::Cursor
        && let Err(error) =
            persist_native_session(store, &queued.room_id, &queued.seat_id, reserved.as_deref())
    {
        set_delivery_failed(deliveries, &queued.delivery_id, error.to_string()).await;
        return;
    }
    let invocation = Invocation {
        agent: seat.agent,
        executable,
        workspace: queued.workspace.clone(),
        native_session_id: reserved.clone(),
        model: seat.model.clone(),
        reasoning_effort: seat.reasoning_effort.clone(),
        instructions: seat.instructions.clone(),
        message: queued.message.clone(),
        first_message,
    };
    let output = adapters::run(invocation).await;
    let expected_session = reserved;
    let (mismatch, observed_session) = native_session_outcome(
        expected_session.as_deref(),
        output.observed_session_id.as_deref(),
    );
    let persistence_error =
        persist_native_session(store, &queued.room_id, &queued.seat_id, observed_session)
            .err()
            .map(|error| format!("native session could not be persisted: {error}"));
    let mut map = deliveries.lock().await;
    if let Some(delivery) = map.get_mut(&queued.delivery_id) {
        finish_delivery(
            delivery,
            mismatch,
            persistence_error,
            output.error,
            output.answer,
        );
    }
}

async fn set_delivery_failed(
    deliveries: &Arc<Mutex<HashMap<String, DeliveryState>>>,
    delivery_id: &str,
    error: String,
) {
    if let Some(delivery) = deliveries.lock().await.get_mut(delivery_id) {
        delivery.status = DeliveryStatus::Failed;
        delivery.final_answer = None;
        delivery.error = Some(error);
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

fn persist_native_session(
    store: &StateStore,
    room_id: &str,
    seat_id: &str,
    native_session_id: Option<&str>,
) -> Result<()> {
    let Some(native_session_id) = native_session_id else {
        return Ok(());
    };
    store.mutate(|state| {
        let room = state
            .rooms
            .iter_mut()
            .find(|room| room.id == room_id)
            .with_context(|| format!("room '{room_id}' disappeared while starting a session"))?;
        let seat = room
            .seats
            .iter_mut()
            .find(|seat| seat.id == seat_id)
            .with_context(|| format!("seat '{seat_id}' disappeared while starting a session"))?;
        match seat.native_session_id.as_deref() {
            Some(existing) if existing != native_session_id => {
                bail!(
                    "seat '{}' already targets native session '{existing}'",
                    seat.name
                )
            }
            _ => seat.native_session_id = Some(native_session_id.into()),
        }
        room.updated_at = timestamp();
        Ok(())
    })
}

fn native_session_outcome<'a>(
    expected: Option<&str>,
    observed: Option<&'a str>,
) -> (Option<String>, Option<&'a str>) {
    let mismatch = expected.zip(observed).and_then(|(expected, observed)| {
        (expected != observed)
            .then(|| format!("native session changed from '{expected}' to '{observed}'"))
    });
    let persistable = mismatch.is_none().then_some(observed).flatten();
    (mismatch, persistable)
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
mod tests {
    use super::{
        DeliveryState, DeliveryStatus, deliveries_completed, finish_delivery,
        native_session_outcome, resolve_recipients,
    };
    use crate::types::{AgentKind, HostRecord, RoomRecord, SeatRecord, SeatStatus};

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
    fn empty_delivery_set_is_not_completed_work() {
        assert!(!deliveries_completed(&[]));
    }

    #[test]
    fn queued_delivery_is_not_terminal() {
        let delivery = DeliveryState {
            delivery_id: "delivery-1".into(),
            room_id: "room-1".into(),
            seat_id: "seat-1".into(),
            seat_name: "reviewer".into(),
            agent: AgentKind::Codex,
            status: DeliveryStatus::Queued,
            final_answer: None,
            error: None,
        };

        assert!(!delivery.terminal());
        assert!(!deliveries_completed(&[delivery]));
    }

    #[test]
    fn retired_seat_is_not_addressable_or_broadcast() {
        let mut room = room("room-1", "/tmp/project");
        room.seats = vec![
            SeatRecord {
                id: "active".into(),
                name: "active".into(),
                agent: AgentKind::Claude,
                model: None,
                reasoning_effort: None,
                instructions: None,
                native_session_id: None,
                status: SeatStatus::Active,
            },
            SeatRecord {
                id: "retired".into(),
                name: "retired".into(),
                agent: AgentKind::Grok,
                model: None,
                reasoning_effort: None,
                instructions: None,
                native_session_id: Some("session-1".into()),
                status: SeatStatus::Retired,
            },
        ];

        assert_eq!(resolve_recipients(&room, &["*".into()]).unwrap().len(), 1);
        assert!(
            resolve_recipients(&room, &["retired".into()])
                .unwrap_err()
                .to_string()
                .contains("retired")
        );
        room.seats[0].status = SeatStatus::Retired;
        assert!(resolve_recipients(&room, &["*".into()]).is_err());
    }

    #[test]
    fn observed_native_session_survives_a_failed_delivery() {
        let (mismatch, persistable) = native_session_outcome(None, Some("session-1"));
        assert!(mismatch.is_none());
        assert_eq!(persistable, Some("session-1"));

        let (mismatch, persistable) = native_session_outcome(Some("session-1"), Some("session-2"));
        assert!(mismatch.is_some());
        assert!(persistable.is_none());

        let (mismatch, persistable) = native_session_outcome(Some("session-1"), None);
        assert!(mismatch.is_none());
        assert!(persistable.is_none());
    }

    #[test]
    fn completed_answer_survives_session_persistence_failure() {
        let mut delivery = DeliveryState {
            delivery_id: "delivery-1".into(),
            room_id: "room-1".into(),
            seat_id: "seat-1".into(),
            seat_name: "reviewer".into(),
            agent: AgentKind::Claude,
            status: DeliveryStatus::Running,
            final_answer: None,
            error: None,
        };

        finish_delivery(
            &mut delivery,
            None,
            Some("session persistence failed".into()),
            None,
            Some("completed review".into()),
        );

        assert!(matches!(delivery.status, DeliveryStatus::Failed));
        assert_eq!(delivery.final_answer.as_deref(), Some("completed review"));
        assert_eq!(
            delivery.error.as_deref(),
            Some("session persistence failed")
        );
    }
}
