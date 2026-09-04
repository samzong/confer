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
use tokio::sync::{Mutex, oneshot};

use crate::adapters::{self, Invocation};
use crate::state::{StateStore, current_workspace};
use crate::types::{
    AgentKind, HostRecord, Readiness, Replacement, RoomRecord, RoomStatus, SeatRecord,
};

const DEFAULT_ROOM_SIZE: usize = 3;
const MAX_ROOM_SIZE: usize = 16;
const DEFAULT_WAIT_MS: u64 = 120_000;
const MAX_WAIT_MS: u64 = 600_000;
const SESSION_READY_TIMEOUT: Duration = Duration::from_secs(30);

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

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct EmptyArgs {}

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

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RoomIdArgs {
    room_id: String,
}

#[derive(Clone, Debug, Serialize)]
struct SeatView {
    id: String,
    name: String,
    agent: AgentKind,
    model: Option<String>,
    reasoning_effort: Option<String>,
    native_session: bool,
}

#[derive(Clone, Debug, Serialize)]
struct RoomView {
    id: String,
    name: String,
    workspace: String,
    status: RoomStatus,
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
    workspace: String,
    rooms: Vec<RoomView>,
}

#[derive(Debug, Serialize)]
struct ResumeRoomOutput {
    room: RoomView,
    readiness: Vec<Readiness>,
    replacements: Vec<Replacement>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum DeliveryStatus {
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
    session_pending: bool,
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
struct CloseRoomOutput {
    room: RoomView,
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
struct ConferMcp {
    store: StateStore,
    deliveries: Arc<Mutex<HashMap<String, DeliveryState>>>,
    session_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
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
            session_locks: Arc::new(Mutex::new(HashMap::new())),
            tool_router: Self::tool_router(),
        })
    }

    #[tool(
        description = "Create a temporary multi-agent room for the current Git worktree. The current host counts toward target_size, which defaults to three. This only checks local readiness and creates logical seats; it never calls a model. Explicit unavailable agents may be replaced, and every replacement is reported.",
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
        description = "List active and inactive Confer rooms for the current Git worktree. Different worktrees are isolated. Returns room and participant metadata only, never messages or agent outputs.",
        annotations(
            title = "List rooms",
            read_only_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn list_rooms(
        &self,
        Parameters(_args): Parameters<EmptyArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(json_result(self.list_rooms_inner()))
    }

    #[tool(
        description = "Send one message directly to one or more external seats in a room. Use recipient '*' to broadcast. Every recipient gets a separate delivery ID. session_pending means dispatch succeeded but first-session addressing exceeded the readiness window; use wait_output for its live delivery. Confer does not wait for earlier messages, queue, retry, or expose one seat's messages to another seat.",
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
        description = "Wait for final answers from live deliveries. Pass delivery IDs to wait for specific sends, or omit them to wait for every delivery from this room still known to the current MCP process. A timeout returns completed answers and running statuses without cancellation. Thinking, token deltas, and tool events are never returned.",
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

    #[tool(
        description = "Reactivate a persisted room by ID for the current Git worktree. Rechecks local agent readiness and restores native session addressing without calling a model. Unavailable participants may be replaced and are reported.",
        annotations(
            title = "Resume room",
            read_only_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn resume_room(
        &self,
        Parameters(args): Parameters<RoomIdArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(json_result(self.resume_room_inner(&args.room_id)))
    }

    #[tool(
        description = "Mark a room inactive while keeping its lightweight room and native session mapping for later resume. Does not kill running agents, delete native sessions, remove cached metadata, or revert code changes.",
        annotations(
            title = "Close room",
            read_only_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn close_room(
        &self,
        Parameters(args): Parameters<RoomIdArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(json_result(self.close_room_inner(&args.room_id).await))
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
            status: RoomStatus::Active,
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

    fn list_rooms_inner(&self) -> Result<ListRoomsOutput> {
        let workspace = current_workspace()?;
        let workspace_text = workspace.to_string_lossy().into_owned();
        let mut rooms = self
            .store
            .load()?
            .rooms
            .into_iter()
            .filter(|room| room.workspace == workspace_text)
            .map(|room| room_view(&room))
            .collect::<Vec<_>>();
        rooms.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        Ok(ListRoomsOutput {
            workspace: workspace_text,
            rooms,
        })
    }

    async fn send_message_inner(&self, args: SendMessageArgs) -> Result<SendMessageOutput> {
        if args.message.trim().is_empty() {
            bail!("message must not be empty");
        }
        let workspace = current_workspace()?;
        let room = self.store.room_for_workspace(&args.room_id, &workspace)?;
        if room.status != RoomStatus::Active {
            bail!(
                "room '{}' is inactive; resume it before sending messages",
                room.id
            );
        }
        let seats = resolve_recipients(&room, &args.recipients)?;
        let mut receipts = Vec::new();
        for seat in seats {
            let readiness = adapters::check_readiness(seat.agent);
            if !readiness.locally_ready {
                let error = readiness
                    .reason
                    .unwrap_or_else(|| "agent is not locally ready".into());
                receipts.push(self.failed_delivery(&room, seat, error).await);
                continue;
            }
            let session_lock = self.session_lock(&room.id, &seat.id).await;
            let mut session_guard = Some(session_lock.lock_owned().await);
            let current_room = match self.store.room_for_workspace(&room.id, &workspace) {
                Ok(current_room) => current_room,
                Err(error) => {
                    let error = format!(
                        "failed to refresh seat '{}' in room '{}': {error}",
                        seat.name, room.id
                    );
                    receipts.push(self.failed_delivery(&room, seat, error).await);
                    continue;
                }
            };
            let native_session_id = current_room
                .seats
                .iter()
                .find(|current| current.id == seat.id)
                .with_context(|| {
                    format!(
                        "seat '{}' disappeared while preparing a delivery",
                        seat.name
                    )
                })?
                .native_session_id
                .clone();
            let executable = PathBuf::from(readiness.executable.context("missing executable")?);
            let first_message = native_session_id.is_none();
            let reserved = if first_message {
                match adapters::reserve_session(seat.agent, &executable).await {
                    Ok(session) => session,
                    Err(error) => {
                        let error = format!(
                            "failed to reserve a native {} session for seat '{}' in room '{}': {error}",
                            seat.agent.id(),
                            seat.name,
                            room.id
                        );
                        receipts.push(self.failed_delivery(&room, seat, error).await);
                        continue;
                    }
                }
            } else {
                native_session_id
            };
            if first_message
                && reserved.is_some()
                && seat.agent == AgentKind::Cursor
                && let Err(error) =
                    persist_native_session(&self.store, &room.id, &seat.id, reserved.as_deref())
            {
                let error = format!(
                    "failed to persist the native {} session for seat '{}' in room '{}': {error}",
                    seat.agent.id(),
                    seat.name,
                    room.id
                );
                receipts.push(self.failed_delivery(&room, seat, error).await);
                continue;
            }
            let delivery_id = uuid::Uuid::new_v4().to_string();
            let delivery = DeliveryState {
                delivery_id: delivery_id.clone(),
                room_id: room.id.clone(),
                seat_id: seat.id.clone(),
                seat_name: seat.name.clone(),
                agent: seat.agent,
                status: DeliveryStatus::Running,
                final_answer: None,
                error: None,
            };
            self.deliveries
                .lock()
                .await
                .insert(delivery_id.clone(), delivery);
            let invocation = Invocation {
                agent: seat.agent,
                executable,
                workspace: workspace.clone(),
                native_session_id: reserved.clone(),
                model: seat.model.clone(),
                reasoning_effort: seat.reasoning_effort.clone(),
                instructions: seat.instructions.clone(),
                message: args.message.clone(),
                first_message,
            };
            let needs_handshake = first_message
                && matches!(
                    seat.agent,
                    AgentKind::Claude | AgentKind::Codex | AgentKind::Grok | AgentKind::Agy
                );
            let (sender, receiver) = if needs_handshake {
                let (sender, receiver) = oneshot::channel();
                (Some(sender), Some(receiver))
            } else {
                (None, None)
            };
            let deliveries = self.deliveries.clone();
            let task_delivery_id = delivery_id.clone();
            let expected_session = reserved.clone();
            tokio::spawn(async move {
                let output = adapters::run(invocation, sender).await;
                let mismatch = expected_session
                    .as_ref()
                    .zip(output.observed_session_id.as_ref())
                    .and_then(|(expected, actual)| {
                        (expected != actual).then(|| {
                            format!("native session changed from '{expected}' to '{actual}'")
                        })
                    });
                let mut map = deliveries.lock().await;
                if let Some(delivery) = map.get_mut(&task_delivery_id) {
                    if delivery.status != DeliveryStatus::Running {
                        return;
                    }
                    if let Some(error) = mismatch.or(output.error) {
                        delivery.status = DeliveryStatus::Failed;
                        delivery.error = Some(error);
                    } else {
                        delivery.status = DeliveryStatus::Completed;
                        delivery.final_answer = output.answer;
                    }
                }
            });
            let mut receipt = SendReceipt {
                delivery_id,
                seat_id: seat.id.clone(),
                seat_name: seat.name.clone(),
                agent: seat.agent,
                accepted: true,
                session_pending: false,
                error: None,
            };
            if let Some(mut receiver) = receiver {
                let timeout = tokio::time::sleep(SESSION_READY_TIMEOUT);
                tokio::pin!(timeout);
                tokio::select! {
                    outcome = &mut receiver => match outcome {
                    Ok(Ok(id)) => {
                        if let Err(error) =
                            persist_native_session(&self.store, &room.id, &seat.id, Some(&id))
                        {
                            let error = format!(
                                "failed to persist the native {} session for seat '{}' in room '{}': {error}",
                                seat.agent.id(),
                                seat.name,
                                room.id
                            );
                            receipt.accepted = false;
                            receipt.error = Some(error.clone());
                            self.mark_delivery_failed(&receipt.delivery_id, error).await;
                        }
                    }
                    Ok(Err(error)) => {
                        receipt.accepted = false;
                        receipt.error = Some(error);
                    }
                    Err(_) => {
                        receipt.accepted = false;
                        receipt.error =
                            Some("agent ended before reporting a native session ID".into());
                    }
                    },
                    _ = &mut timeout => {
                        receipt.session_pending = true;
                        let store = self.store.clone();
                        let room_id = room.id.clone();
                        let seat_id = seat.id.clone();
                        let delivery_id = receipt.delivery_id.clone();
                        let deliveries = self.deliveries.clone();
                        let guard = session_guard.take().expect("session guard");
                        tokio::spawn(async move {
                            let _guard = guard;
                            if let Ok(Ok(id)) = receiver.await
                                && let Err(error) = persist_native_session(
                                    &store,
                                    &room_id,
                                    &seat_id,
                                    Some(&id),
                                )
                                && let Some(delivery) =
                                    deliveries.lock().await.get_mut(&delivery_id)
                            {
                                delivery.status = DeliveryStatus::Failed;
                                delivery.final_answer = None;
                                delivery.error = Some(format!(
                                    "native session became ready but could not be persisted: {error}"
                                ));
                            }
                        });
                    }
                }
            }
            drop(session_guard);
            receipts.push(receipt);
        }
        Ok(SendMessageOutput {
            room_id: room.id,
            deliveries: receipts,
        })
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
            session_pending: false,
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

    async fn session_lock(&self, room_id: &str, seat_id: &str) -> Arc<Mutex<()>> {
        let key = format!("{room_id}:{seat_id}");
        let mut locks = self.session_locks.lock().await;
        locks
            .entry(key)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
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

    fn resume_room_inner(&self, room_id: &str) -> Result<ResumeRoomOutput> {
        let workspace = current_workspace()?;
        let workspace_text = workspace.to_string_lossy().into_owned();
        let readiness = adapters::readiness();
        let ready_agents = readiness
            .iter()
            .filter(|item| item.locally_ready)
            .map(|item| item.agent)
            .collect::<Vec<_>>();
        if ready_agents.is_empty() {
            bail!("no locally ready supported agents were found");
        }
        let mut replacements = Vec::new();
        let room = self.store.mutate(|state| {
            let room = state
                .rooms
                .iter_mut()
                .find(|room| room.id == room_id && room.workspace == workspace_text)
                .with_context(|| format!("room '{room_id}' was not found in this workspace"))?;
            replacements = replace_unstarted_unready_seats(room, &readiness, &ready_agents);
            room.status = RoomStatus::Active;
            if let Some(agent) = detect_host_agent(None) {
                room.host.agent = Some(agent);
            }
            room.updated_at = timestamp();
            Ok(room.clone())
        })?;
        Ok(ResumeRoomOutput {
            room: room_view(&room),
            readiness,
            replacements,
        })
    }

    async fn close_room_inner(&self, room_id: &str) -> Result<CloseRoomOutput> {
        let workspace = current_workspace()?;
        let workspace_text = workspace.to_string_lossy().into_owned();
        let room = self.store.mutate(|state| {
            let room = state
                .rooms
                .iter_mut()
                .find(|room| room.id == room_id && room.workspace == workspace_text)
                .with_context(|| format!("room '{room_id}' was not found in this workspace"))?;
            room.status = RoomStatus::Inactive;
            room.updated_at = timestamp();
            Ok(room.clone())
        })?;
        self.deliveries
            .lock()
            .await
            .retain(|_, delivery| delivery.room_id != room.id || !delivery.terminal());
        let lock_prefix = format!("{}:", room.id);
        self.session_locks
            .lock()
            .await
            .retain(|key, lock| !key.starts_with(&lock_prefix) || Arc::strong_count(lock) > 1);
        Ok(CloseRoomOutput {
            room: room_view(&room),
        })
    }
}

fn replace_unstarted_unready_seats(
    room: &mut RoomRecord,
    readiness: &[Readiness],
    ready_agents: &[AgentKind],
) -> Vec<Replacement> {
    let mut replacements = Vec::new();
    for seat in &mut room.seats {
        let ready = readiness
            .iter()
            .any(|item| item.agent == seat.agent && item.locally_ready);
        if ready || seat.native_session_id.is_some() {
            continue;
        }
        let replacement = ready_agents[0];
        replacements.push(Replacement {
            seat_name: seat.name.clone(),
            requested_agent: seat.agent.id().into(),
            replacement_agent: replacement.id().into(),
            reason: "original agent is not locally ready".into(),
        });
        seat.agent = replacement;
        seat.model = None;
        seat.reasoning_effort = None;
    }
    replacements
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
    let mut names = HashSet::new();
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
            Some(agent) => {
                let replacement = preferred[index % preferred.len()];
                replacements.push(Replacement {
                    seat_name: spec
                        .name
                        .clone()
                        .unwrap_or_else(|| format!("seat-{}", index + 1)),
                    requested_agent: agent.id().into(),
                    replacement_agent: replacement.id().into(),
                    reason: "requested agent is not locally ready".into(),
                });
                replacement
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
        let replaced = requested_agent.is_some_and(|agent| agent != selected);
        seats.push(SeatRecord {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            agent: selected,
            model: (!replaced).then_some(spec.model).flatten(),
            reasoning_effort: (!replaced).then_some(spec.reasoning_effort).flatten(),
            instructions: spec.instructions,
            native_session_id: None,
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
        return Ok(room.seats.iter().collect());
    }
    let mut seen = HashSet::new();
    let mut seats = Vec::new();
    for recipient in recipients {
        let seat = room
            .seats
            .iter()
            .find(|seat| seat.id == *recipient || seat.name == *recipient)
            .with_context(|| format!("unknown seat '{recipient}' in room '{}'", room.id))?;
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
        status: room.status,
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
            })
            .collect(),
        created_at: room.created_at.clone(),
        updated_at: room.updated_at.clone(),
    }
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
            "Create a room when the user asks to consult or coordinate other coding agents. The current host moderates every relay. Seats are private by default: do not reveal one seat's answer to another unless the user requests critique or collaboration. Use list_rooms and resume_room to continue a room in this Git worktree. Close completed rooms; they remain resumable.",
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
        SeatSpecInput, deliveries_completed, replace_unstarted_unready_seats, select_seats,
    };
    use crate::types::{AgentKind, HostRecord, Readiness, RoomRecord, RoomStatus, SeatRecord};

    fn ready(agent: AgentKind) -> Readiness {
        Readiness {
            agent,
            locally_ready: true,
            executable: Some(agent.id().into()),
            reason: None,
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
    fn resume_preserves_started_session_when_agent_is_temporarily_unready() {
        let mut room = RoomRecord {
            id: "room-1".into(),
            name: "room".into(),
            workspace: "/tmp/project".into(),
            status: RoomStatus::Inactive,
            host: HostRecord {
                agent: Some("codex".into()),
            },
            seats: vec![SeatRecord {
                id: "seat-1".into(),
                name: "planner".into(),
                agent: AgentKind::Codex,
                model: Some("model".into()),
                reasoning_effort: Some("high".into()),
                instructions: None,
                native_session_id: Some("thread-1".into()),
            }],
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
        };
        let readiness = vec![ready(AgentKind::Claude)];
        let replacements =
            replace_unstarted_unready_seats(&mut room, &readiness, &[AgentKind::Claude]);

        assert!(replacements.is_empty());
        assert_eq!(room.seats[0].agent, AgentKind::Codex);
        assert_eq!(room.seats[0].native_session_id.as_deref(), Some("thread-1"));
    }
}
