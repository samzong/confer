pub(crate) mod activity;
mod api;
mod delivery;
mod inline;
mod input;
mod rooms;

use anyhow::Result;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Implementation, ServerCapabilities, ServerConfig, Tool};
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router, transport::stdio};
use serde::Serialize;

use self::api::{
    AddSeatArgs, CancelDeliveryArgs, ClearUncertainDeliveryArgs, CreateRoomArgs, ListRoomsArgs,
    RetireSeatArgs, SendMessageArgs, WaitOutputArgs, WaitOutputParams,
};
use self::delivery::DeliveryRuntime;
use self::inline::{
    render_create_room, render_list_rooms, render_room_output, render_send_message,
    render_wait_output, structured_err, structured_ok,
};
use crate::state::StateStore;

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub(crate) enum CapabilitiesFormat {
    Text,
    Json,
}

#[derive(Debug, Serialize)]
struct CapabilitiesReport {
    server: ServerConfig,
    tools: Vec<Tool>,
}

#[derive(Clone)]
struct ConferMcp {
    store: StateStore,
    runtime: DeliveryRuntime,
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
    let mut server = ConferMcp::new()?;
    match activity::Instance::register(&server.store.runtime_path()) {
        Ok(instance) => server.runtime.activity = Some(instance),
        Err(error) => eprintln!("Confer running state unavailable: {error:#}"),
    }
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

#[tool_router]
impl ConferMcp {
    fn new() -> Result<Self> {
        Ok(Self::with_store(StateStore::discover()?))
    }

    fn with_store(store: StateStore) -> Self {
        Self {
            store,
            runtime: DeliveryRuntime::new(),
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "Create a multi-agent task room. Pass workspace as the actual current task's absolute directory. The returned workspace is normalized to its Git worktree root, or the canonical directory outside Git. Verify that root belongs to your task and use it for followup calls; never substitute another room's workspace to bypass a mismatch. target_size counts execution seats and excludes the host. Provide a positive target_size or at least one seat; there is no default count or fixed seat limit. Explicit seats are preserved and any remaining target positions are selected automatically, preferring agent types other than the host. This checks local readiness and seat configuration and creates logical seats; it never calls a model. A seat that sets model or reasoning_effort must also set agent; Confer never infers the agent from a model name. An explicitly requested unavailable agent fails the operation without changing room state; the caller decides whether to retry with another agent.",
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
        Ok(match self.create_room_inner(args) {
            Ok(output) => structured_ok(render_create_room(output, None)),
            Err(error) => structured_err(error),
        })
    }

    #[tool(
        description = "Add one private seat to a room. Pass the normalized workspace root already verified against your actual current task when creating or recovering the room. A different workspace is rejected; never substitute another room's workspace to bypass a mismatch. The seat starts a new native session on its first message. A seat that sets model or reasoning_effort must also set agent; Confer never infers the agent from a model name. An explicitly requested unavailable agent fails the operation without changing room state; the caller decides whether to retry with another agent.",
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
        Ok(match self.add_seat_inner(args) {
            Ok(output) => {
                let added_id = output.room.seats.last().map(|seat| seat.id.clone());
                structured_ok(render_create_room(output, added_id.as_deref()))
            }
            Err(error) => structured_err(error),
        })
    }

    #[tool(
        description = "Retire one seat in a room. Pass the normalized workspace root already verified against your actual current task when creating or recovering the room. A different workspace is rejected; never substitute another room's workspace to bypass a mismatch. A retired seat keeps its metadata and native session mapping but can no longer receive messages. A known running delivery must finish first. When the seat has a Codex thread, Confer then archives that thread on a best-effort basis.",
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
        Ok(match self.retire_seat_inner(args).await {
            Ok(output) => structured_ok(render_room_output(output)),
            Err(error) => structured_err(error),
        })
    }

    #[tool(
        description = "List Confer rooms. scope defaults to current, which requires workspace as the absolute directory of the current task. Make sure that the returned normalized workspace root belongs to your task before you use it for later calls. Never substitute the workspace of another room to bypass a mismatch. scope all requires no workspace and lists rooms across every recorded workspace.\n\nOutput contains room and participant metadata only, never messages or replies. previous_delivery_uncertain marks a seat whose last delivery ended unobserved. Inspect its native work before you send another message. By default, inline JSON output stays within 8,800 UTF-8 bytes and sets omitted_rooms or omitted_seats when items do not fit. Pass full_output set to true to return every matching room and seat.",
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
        let full_output = args.full_output.unwrap_or(false);
        Ok(match self.list_rooms_inner(args) {
            Ok(output) => structured_ok(render_list_rooms(output, full_output)),
            Err(error) => structured_err(error),
        })
    }

    #[tool(
        description = "Queue one message for one or more external seats in a room. Pass the normalized workspace root already verified against your actual current task when creating or recovering the room. A different workspace is rejected; never substitute another room's workspace to bypass a mismatch. Use recipient '*' to broadcast. Idle seats start promptly and busy seats run messages FIFO. Every recipient gets a delivery ID for wait_output. Uncertain seats refuse dispatch. Receipts include previous_delivery_uncertain, uncertain_delivery_id, and resume_command. Inspect native work, then call clear_uncertain_delivery with that ID. The worker also blocks queued messages if uncertainty arises before dispatch.",
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
        Ok(match self.send_message_inner(args).await {
            Ok(output) => structured_ok(render_send_message(output)),
            Err(error) => structured_err(error),
        })
    }

    #[tool(
        description = "Clear one uncertain delivery after the caller inspects native work. Pass the verified workspace, room_id, seat name or ID, and the uncertain_delivery_id as delivery_id. A running seat or changed marker is rejected. This action sends no message and never retries native work.",
        annotations(
            title = "Clear uncertain delivery",
            read_only_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn clear_uncertain_delivery(
        &self,
        Parameters(args): Parameters<ClearUncertainDeliveryArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(match self.clear_uncertain_delivery_inner(args) {
            Ok(output) => structured_ok(render_room_output(output)),
            Err(error) => structured_err(error),
        })
    }

    #[tool(
        description = "Cancel specific live deliveries in a room. Pass the normalized workspace root and a nonempty delivery_ids list. Requested queued deliveries never start. A cancelled status means only that the tracked process tree stopped. Detached or reparented processes, such as background daemons or development servers, can continue running. The host must inspect those processes separately.\n\nCleanup can time out after eight seconds and return timed_out set to true. When previous_delivery_uncertain is true, queued messages fail before dispatch and new sends are refused. Inspect native work, then call clear_uncertain_delivery with the uncertain delivery ID before sending again. Later deliveries resume an observed native session ID or start a new session when none exists. Finished deliveries keep their current status, and partial answers set answer_incomplete to true.\n\nCancellation does not undo file changes or other side effects. Delivery IDs belong to the current MCP process. By default, inline JSON output stays within 8,800 UTF-8 bytes, folds long text, and sets output_folded to true. Call wait_output with delivery_ids, timeout_ms set to 0, and full_output set to true to read unfolded text.",
        annotations(
            title = "Cancel delivery",
            read_only_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn cancel_delivery(
        &self,
        Parameters(args): Parameters<CancelDeliveryArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(match self.cancel_delivery_inner(args).await {
            Ok(output) => structured_ok(render_wait_output(output, false)),
            Err(error) => structured_err(error),
        })
    }

    #[tool(
        description = "Wait for final answers from live deliveries. Pass the normalized workspace root that matches your current task, because a mismatched workspace fails. Never substitute the workspace of another room to bypass a mismatch. Pass delivery_ids for specific sends, or omit delivery_ids to wait for all room deliveries in this MCP process. A timeout returns completed answers plus queued and running statuses without cancellation.\n\nCancelled and awaiting_answer deliveries are terminal. A Codex or Cursor question stops the native turn and returns question with awaiting_answer unless the caller cancelled. Send the answer to the same seat as a new turn. Failed cleanup keeps the question and error. An answer with answer_incomplete set to true is partial. Thinking, token deltas, and tool events are never returned.\n\nBy default, inline JSON output stays within 8,800 UTF-8 bytes, folds long text, and sets output_folded to true. Pass full_output set to true to return complete raw answers, errors, and questions without folding.",
        annotations(
            title = "Wait for output",
            read_only_hint = true,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn wait_output(
        &self,
        Parameters(args): Parameters<WaitOutputParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let full_output = args.full_output.unwrap_or(false);
        let inner_args = WaitOutputArgs {
            workspace: args.workspace,
            room_id: args.room_id,
            delivery_ids: args.delivery_ids,
            timeout_ms: args.timeout_ms,
        };
        Ok(match self.wait_output_inner(inner_args).await {
            Ok(output) => structured_ok(render_wait_output(output, full_output)),
            Err(error) => structured_err(error),
        })
    }
}

#[tool_handler]
impl ServerHandler for ConferMcp {
    fn get_info(&self) -> ServerConfig {
        server_config()
    }
}

fn server_config() -> ServerConfig {
    ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
        .with_server_info(Implementation::new("confer", env!("CARGO_PKG_VERSION")))
        .with_instructions(
            "Confer provides local coding-agent execution through MCP. The caller manages task decomposition, room reuse, relays, and result acceptance. The host is not an execution seat. Create rooms with target_size or explicit seats; automatic selection prefers agent types other than the host, and identical configurations may occupy multiple independent seats. Explicit unavailable agents fail without replacement. Seats receive only addressed messages and use per-seat FIFO queues.",
        )
}

fn capabilities() -> Result<CapabilitiesReport> {
    let server = ConferMcp::new()?;
    Ok(CapabilitiesReport {
        server: server_config(),
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
mod tests;
