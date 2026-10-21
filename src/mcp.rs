pub(crate) mod activity;
mod api;
mod delivery;
mod rooms;

use anyhow::Result;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Implementation, ServerCapabilities, ServerInfo, Tool};
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router, transport::stdio};
use serde::Serialize;

use self::api::{
    AddSeatArgs, CancelDeliveryArgs, ClearUncertainDeliveryArgs, CreateRoomArgs, ListRoomsArgs,
    RetireSeatArgs, SendMessageArgs, WaitOutputArgs, json_result,
};
use self::delivery::DeliveryRuntime;
use crate::state::StateStore;

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub(crate) enum CapabilitiesFormat {
    Text,
    Json,
}

#[derive(Debug, Serialize)]
struct CapabilitiesReport {
    server: ServerInfo,
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
        Ok(json_result(self.create_room_inner(args)))
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
        Ok(json_result(self.add_seat_inner(args)))
    }

    #[tool(
        description = "Retire one seat in a room. Pass the normalized workspace root already verified against your actual current task when creating or recovering the room. A different workspace is rejected; never substitute another room's workspace to bypass a mismatch. A retired seat keeps its metadata and native session mapping but can no longer receive messages. A known running delivery must finish first.",
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
        description = "List Confer rooms. scope defaults to current, which requires workspace as the actual current task's absolute directory. Verify the returned normalized Git worktree root, or canonical directory outside Git, belongs to your task before using that root for followup calls. Never substitute another room's workspace to bypass a mismatch. scope all requires no workspace and lists rooms across every recorded workspace. Returns room and participant metadata only, never messages or agent outputs. previous_delivery_uncertain marks a seat whose last delivery ended unobserved; verify its native work before sending more.",
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
        Ok(json_result(self.list_rooms_inner(args)))
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
        Ok(json_result(self.send_message_inner(args).await))
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
        Ok(json_result(self.clear_uncertain_delivery_inner(args)))
    }

    #[tool(
        description = "Cancel specific live deliveries in a room. Pass the verified normalized workspace root and a nonempty delivery_ids list. Requested queued deliveries never start. A cancelled result only confirms that the process tree Confer can track stopped. Detached or reparented processes, such as background daemons or development servers, can continue running. The host must inspect these processes separately. A result with previous_delivery_uncertain set to true does not confirm that native work stopped. Cleanup can time out after eight seconds and return timed_out=true, leaving running deliveries marked previous_delivery_uncertain. If cleanup leaves an uncertain marker, queued messages fail before dispatch. Inspect native work, then call clear_uncertain_delivery with the uncertain delivery ID before sending again. Later deliveries resume an observed native session ID. If no ID was observed, cancellation reports that no native session was established. The next delivery can create a new native session. The next send refuses dispatch until uncertainty is cleared. Finished deliveries keep their current status. Partial answers have answer_incomplete set to true. Cancellation does not undo file changes or other side effects. Delivery IDs belong to the current MCP process, as with wait_output.",
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
        Ok(json_result(self.cancel_delivery_inner(args).await))
    }

    #[tool(
        description = "Wait for final answers from live deliveries. Pass the normalized workspace root already verified against your actual current task when creating or recovering the room. A different workspace is rejected; never substitute another room's workspace to bypass a mismatch. Pass delivery IDs to wait for specific sends, or omit them to wait for every delivery from this room still known to the current MCP process. A timeout returns completed answers plus queued and running statuses without cancellation. Cancelled and awaiting_answer deliveries are terminal. A Codex or Cursor question stops the native turn and returns question with awaiting_answer after a clean stop, unless the caller cancelled the delivery. Send the answer to the same seat as a new turn. Failed cleanup keeps the question and error. An answer with answer_incomplete set to true is partial. Thinking, token deltas, and tool events are never returned.",
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
}

#[tool_handler]
impl ServerHandler for ConferMcp {
    fn get_info(&self) -> ServerInfo {
        server_info()
    }
}

fn server_info() -> ServerInfo {
    ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
        .with_server_info(Implementation::new("confer", env!("CARGO_PKG_VERSION")))
        .with_instructions(
            "Confer provides local coding-agent execution through MCP. The caller manages task decomposition, room reuse, relays, and result acceptance. The host is not an execution seat. Create rooms with target_size or explicit seats; automatic selection prefers agent types other than the host, and identical configurations may occupy multiple independent seats. Explicit unavailable agents fail without replacement. Seats receive only addressed messages and use per-seat FIFO queues.",
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
    use super::capabilities;

    #[test]
    fn capabilities_expose_the_eight_room_tools() {
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
                "cancel_delivery",
                "clear_uncertain_delivery",
                "create_room",
                "list_rooms",
                "retire_seat",
                "send_message",
                "wait_output",
            ]
        );
    }
}
