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
    AddSeatArgs, CreateRoomArgs, ListRoomsArgs, RetireSeatArgs, SendMessageArgs, WaitOutputArgs,
    json_result,
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
    let service = ConferMcp::new()?.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

#[tool_router]
impl ConferMcp {
    fn new() -> Result<Self> {
        Ok(Self {
            store: StateStore::discover()?,
            runtime: DeliveryRuntime::new(),
            tool_router: Self::tool_router(),
        })
    }

    #[tool(
        description = "Create a multi-agent task room. Pass workspace as the actual current task's absolute directory. The returned workspace is normalized to its Git worktree root, or the canonical directory outside Git. Verify that root belongs to your task and use it for followup calls; never substitute another room's workspace to bypass a mismatch. The current host counts toward target_size, which defaults to three. This checks local readiness and seat configuration and creates logical seats; it never calls a model. Explicit unavailable agents may be replaced, and every replacement is reported.",
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
        description = "Add one private seat to a room. Pass the normalized workspace root already verified against your actual current task when creating or recovering the room. A different workspace is rejected; never substitute another room's workspace to bypass a mismatch. The seat starts a new native session on its first message. Explicit unavailable agents may be replaced, and every replacement is reported.",
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
        description = "List Confer rooms. scope defaults to current, which requires workspace as the actual current task's absolute directory. Verify the returned normalized Git worktree root, or canonical directory outside Git, belongs to your task before using that root for followup calls. Never substitute another room's workspace to bypass a mismatch. scope all requires no workspace and lists rooms across every recorded workspace. Returns room and participant metadata only, never messages or agent outputs.",
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
        description = "Queue one message for one or more external seats in a room. Pass the normalized workspace root already verified against your actual current task when creating or recovering the room. A different workspace is rejected; never substitute another room's workspace to bypass a mismatch. Use recipient '*' to broadcast. Idle seats start promptly and busy seats run messages FIFO. Every recipient gets a delivery ID for wait_output.",
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
        description = "Wait for final answers from live deliveries. Pass the normalized workspace root already verified against your actual current task when creating or recovering the room. A different workspace is rejected; never substitute another room's workspace to bypass a mismatch. Pass delivery IDs to wait for specific sends, or omit them to wait for every delivery from this room still known to the current MCP process. A timeout returns completed answers plus queued and running statuses without cancellation. Thinking, token deltas, and tool events are never returned.",
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
    use super::capabilities;

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
}
