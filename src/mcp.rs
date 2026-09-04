use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::{SecondsFormat, Utc};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Implementation, ServerCapabilities, ServerInfo, Tool};
use rmcp::{
    ServerHandler, ServiceExt, schemars, tool, tool_handler, tool_router, transport::stdio,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Mutex, mpsc};

use crate::adapters::{self, Invocation};
use crate::state::{StateStore, current_workspace};
use crate::types::{
    AgentKind, HostRecord, Readiness, Replacement, RoomRecord, SeatRecord, SeatStatus,
};

const DEFAULT_ROOM_SIZE: usize = 3;
const MAX_ROOM_SIZE: usize = 16;
const DEFAULT_WAIT_MS: u64 = 120_000;
const MAX_WAIT_MS: u64 = 600_000;

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub(crate) enum CapabilitiesFormat {
    Text,
    Json,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SeatSpecInput {
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    reasoning_effort: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    instructions: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CreateRoomArgs {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    #[schemars(range(min = 2, max = 16))]
    target_size: Option<usize>,
    #[serde(default)]
    host_agent: Option<String>,
    #[serde(default)]
    seats: Vec<SeatSpecInput>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum RoomScope {
    #[default]
    Current,
    All,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListRoomsArgs {
    #[serde(default)]
    scope: Option<RoomScope>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct AddSeatArgs {
    room_id: String,
    seat: SeatSpecInput,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RetireSeatArgs {
    room_id: String,
    seat: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SendMessageArgs {
    room_id: String,
    recipients: Vec<String>,
    message: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct WaitOutputArgs {
    room_id: String,
    #[serde(default)]
    delivery_ids: Vec<String>,
    #[serde(default)]
    #[schemars(range(min = 0, max = 600_000))]
    timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
struct SeatView {
    id: String,
    name: String,
    agent: AgentKind,
    model: Option<String>,
    reasoning_effort: Option<String>,
    native_session: bool,
    status: SeatStatus,
}

#[derive(Clone, Debug, Serialize)]
struct RoomView {
    id: String,
    name: String,
    workspace: String,
    host_agent: Option<String>,
    seats: Vec<SeatView>,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Serialize)]
struct CreateRoomOutput {
    room: RoomView,
    readiness: Vec<Readiness>,
    replacements: Vec<Replacement>,
}

#[derive(Debug, Serialize)]
struct ListRoomsOutput {
    scope: RoomScope,
    workspace: Option<String>,
    rooms: Vec<RoomView>,
}

#[derive(Debug, Serialize)]
struct AddSeatOutput {
    room: RoomView,
    readiness: Vec<Readiness>,
    replacements: Vec<Replacement>,
}

#[derive(Debug, Serialize)]
struct RetireSeatOutput {
    room: RoomView,
}

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
struct SendMessageOutput {
    room_id: String,
    deliveries: Vec<SendReceipt>,
}

#[derive(Debug, Serialize)]
struct WaitOutput {
    room_id: String,
    completed: bool,
    timed_out: bool,
    deliveries: Vec<DeliveryState>,
}

#[derive(Debug, Serialize)]
struct ErrorOutput {
    error: String,
}

#[derive(Debug, Serialize)]
struct CapabilitiesReport {
    server: ServerInfo,
    tools: Vec<Tool>,
}

#[derive(Clone)]
struct QueuedDelivery {
    delivery_id: String,
    room_id: String,
    seat_id: String,
    message: String,
    workspace: PathBuf,
}

#[derive(Clone)]
struct ConferMcp {
    store: StateStore,
    deliveries: Arc<Mutex<HashMap<String, DeliveryState>>>,
    workers: Arc<Mutex<HashMap<String, mpsc::UnboundedSender<QueuedDelivery>>>>,
    tool_router: ToolRouter<Self>,
}

pub(crate) fn run() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?;
    runtime.block_on(serve())
}

pub(crate) fn run_capabilities(format: CapabilitiesFormat) -> Result<()> {
    let report = capabilities()?;
    match format {
        CapabilitiesFormat::Text => print!("{}", render_capabilities(&report)),
        CapabilitiesFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
    }
    Ok(())
}

async fn serve() -> Result<()> {
    let service = ConferMcp::new()?.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

#[tool_router]
impl ConferMcp {
    fn new() -> Result<Self> {
        Ok(Self {
            store: StateStore::discover()?,
            deliveries: Arc::new(Mutex::new(HashMap::new())),
            workers: Arc::new(Mutex::new(HashMap::new())),
            tool_router: Self::tool_router(),
        })
    }

    #[tool(
        description = "Create a multi-agent task room for the current Git worktree. The current host counts toward target_size, which defaults to three. This only checks local readiness and creates logical seats; it never calls a model. Explicit unavailable agents may be replaced, and every replacement is reported.",
        annotations(
            title = "Create room",
            read_only_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn create_room(
        &self,
        Parameters(args): Parameters<CreateRoomArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(json_result(self.create_room_inner(args)))
    }

    #[tool(
        description = "Add one private seat to a room in the current Git worktree. The seat starts a new native session on its first message. Explicit unavailable agents may be replaced, and every replacement is reported.",
        annotations(
            title = "Add seat",
            read_only_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn add_seat(
        &self,
        Parameters(args): Parameters<AddSeatArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(json_result(self.add_seat_inner(args)))
    }

    #[tool(
        description = "Retire one seat in a room in the current Git worktree. A retired seat keeps its metadata and native session mapping but can no longer receive messages. A known running delivery must finish first.",
        annotations(
            title = "Retire seat",
            read_only_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn retire_seat(
        &self,
        Parameters(args): Parameters<RetireSeatArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(json_result(self.retire_seat_inner(args).await))
    }

    #[tool(
        description = "List Confer rooms. scope defaults to current for the current Git worktree; use all to inspect rooms across every recorded workspace. Returns room and participant metadata only, never messages or agent outputs.",
        annotations(
            title = "List rooms",
            read_only_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn list_rooms(
        &self,
        Parameters(args): Parameters<ListRoomsArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(json_result(
            self.list_rooms_inner(args.scope.unwrap_or_default()),
        ))
    }

    #[tool(
        description = "Queue one message for one or more external seats in a room. Use recipient '*' to broadcast. Idle seats start promptly and busy seats run messages FIFO. Every recipient gets a delivery ID for wait_output.",
        annotations(
            title = "Send message",
            read_only_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn send_message(
        &self,
        Parameters(args): Parameters<SendMessageArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(json_result(self.send_message_inner(args).await))
    }

    #[tool(
        description = "Wait for final answers from live deliveries. Pass delivery IDs to wait for specific sends, or omit them to wait for every delivery from this room still known to the current MCP process. A timeout returns completed answers plus queued and running statuses without cancellation. Thinking, token deltas, and tool events are never returned.",
        annotations(
            title = "Wait for output",
            read_only_hint = true,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn wait_output(
        &self,
        Parameters(args): Parameters<WaitOutputArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(json_result(self.wait_output_inner(args).await))
    }

    fn create_room_inner(&self, args: CreateRoomArgs) -> Result<CreateRoomOutput> {
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

    fn add_seat_inner(&self, args: AddSeatArgs) -> Result<AddSeatOutput> {
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

    async fn retire_seat_inner(&self, args: RetireSeatArgs) -> Result<RetireSeatOutput> {
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
        self.workers
            .lock()
            .await
            .remove(&seat_key(&room.id, &seat_id));
        Ok(RetireSeatOutput {
            room: room_view(&room),
        })
    }

    fn list_rooms_inner(&self, scope: RoomScope) -> Result<ListRoomsOutput> {
        let workspace = current_workspace()?;
        let workspace_text = workspace.to_string_lossy().into_owned();
        let rooms = rooms_for_scope(self.store.load()?.rooms, scope, &workspace_text);
        Ok(ListRoomsOutput {
            scope,
            workspace: matches!(scope, RoomScope::Current).then_some(workspace_text),
            rooms,
        })
    }

    async fn send_message_inner(&self, args: SendMessageArgs) -> Result<SendMessageOutput> {
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
        self.deliveries.lock().await.insert(
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
        let mut workers = self.workers.lock().await;
        if let Some(sender) = workers.get(&key)
            && !sender.is_closed()
        {
            return sender.clone();
        }
        let (sender, receiver) = mpsc::unbounded_channel();
        tokio::spawn(run_seat_worker(
            receiver,
            self.store.clone(),
            self.deliveries.clone(),
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
        self.deliveries.lock().await.insert(
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
        if let Some(delivery) = self.deliveries.lock().await.get_mut(delivery_id) {
            delivery.status = DeliveryStatus::Failed;
            delivery.final_answer = None;
            delivery.error = Some(error);
        }
    }

    async fn wait_output_inner(&self, args: WaitOutputArgs) -> Result<WaitOutput> {
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
        let map = self.deliveries.lock().await;
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
    let output = adapters::run(invocation, None).await;
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

#[tool_handler]
impl ServerHandler for ConferMcp {
    fn get_info(&self) -> ServerInfo {
        server_info()
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
        seats.push(SeatRecord {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            agent: selected,
            model: (!replaced).then_some(spec.model).flatten(),
            reasoning_effort: (!replaced).then_some(spec.reasoning_effort).flatten(),
            instructions: spec.instructions,
            native_session_id: None,
            status: SeatStatus::Active,
        });
    }
    Ok((seats, replacements))
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

fn room_view(room: &RoomRecord) -> RoomView {
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

fn rooms_for_scope(rooms: Vec<RoomRecord>, scope: RoomScope, workspace: &str) -> Vec<RoomView> {
    let mut rooms = rooms
        .into_iter()
        .filter(|room| matches!(scope, RoomScope::All) || room.workspace == workspace)
        .map(|room| room_view(&room))
        .collect::<Vec<_>>();
    rooms.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    rooms
}

fn timestamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn json_result<T: Serialize>(result: Result<T>) -> CallToolResult {
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

fn server_info() -> ServerInfo {
    ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
        .with_server_info(Implementation::new("confer", env!("CARGO_PKG_VERSION")))
        .with_instructions(
            "Create a room when the user asks to consult or coordinate other coding agents. Reuse a room only when its ID is already part of the current host session context; create a new room for a new host session or when the user asks for one. Use list_rooms only to recover a room the user explicitly wants to continue. The current host moderates every relay. Seats are private by default: do not reveal one seat's answer to another unless the user requests critique or collaboration. Messages use per-seat FIFO queues.",
        )
}

fn capabilities() -> Result<CapabilitiesReport> {
    let server = ConferMcp::new()?;
    Ok(CapabilitiesReport {
        server: server_info(),
        tools: server.tool_router.list_all(),
    })
}

fn render_capabilities(report: &CapabilitiesReport) -> String {
    let mut output = format!("Confer MCP {}\n", env!("CARGO_PKG_VERSION"));
    if let Some(instructions) = report.server.instructions.as_deref() {
        output.push_str(instructions);
        output.push('\n');
    }
    for tool in &report.tools {
        output.push('\n');
        output.push_str(tool.name.as_ref());
        output.push('\n');
        if let Some(description) = tool.description.as_deref() {
            output.push_str(description);
            output.push('\n');
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{
        ConferMcp, DeliveryState, DeliveryStatus, ListRoomsArgs, RetireSeatArgs, RoomScope,
        SeatSpecInput, capabilities, deliveries_completed, finish_delivery, native_session_outcome,
        resolve_recipients, rooms_for_scope, seat_key, select_seats, select_seats_with_names,
    };
    use crate::state::{StateStore, current_workspace};
    use crate::types::{AgentKind, HostRecord, Readiness, RoomRecord, SeatRecord, SeatStatus};
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::Mutex;

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
            deliveries: Arc::new(Mutex::new(HashMap::new())),
            workers: Arc::new(Mutex::new(HashMap::new())),
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
    fn capabilities_expose_the_six_room_tools() {
        let mut names = capabilities()
            .unwrap()
            .tools
            .into_iter()
            .map(|tool| tool.name.into_owned())
            .collect::<Vec<_>>();
        names.sort();

        assert_eq!(
            names,
            [
                "add_seat",
                "create_room",
                "list_rooms",
                "retire_seat",
                "send_message",
                "wait_output",
            ]
        );
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
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        service
            .workers
            .lock()
            .await
            .insert(seat_key("room-1", "seat-1"), sender);

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
        assert!(receiver.recv().await.is_none());
    }
}
