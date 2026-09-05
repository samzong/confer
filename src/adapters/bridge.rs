use std::sync::Arc;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    AgentCapabilities, ContentBlock, ContentChunk, InitializeRequest, InitializeResponse,
    NewSessionRequest, NewSessionResponse, PromptRequest, PromptResponse, ResumeSessionRequest,
    ResumeSessionResponse, SessionCapabilities, SessionNotification, SessionResumeCapabilities,
    SessionUpdate, StopReason, TextContent,
};
use agent_client_protocol::{Agent, Channel, Error};
use serde_json::json;
use tokio::sync::Mutex;

use super::{AdapterOutput, Invocation, acp, codex, run_cli};
use crate::types::AgentKind;

pub(super) async fn run(invocation: Invocation) -> AdapterOutput {
    let (client, server) = Channel::duplex();
    let source = invocation.clone();
    let task = tokio::spawn(async move { serve(server, source).await });
    let output = acp::run_connection(client, invocation, false).await;
    match task.await {
        Ok(Ok(())) => output,
        result if output.error.is_none() => AdapterOutput {
            error: Some(format!("ACP bridge failed: {result:?}")),
            ..output
        },
        _ => output,
    }
}

async fn serve(channel: Channel, invocation: Invocation) -> Result<(), Error> {
    let session_id = invocation
        .native_session_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let initialized = Arc::new(Mutex::new(false));
    let attached = Arc::new(Mutex::new(false));
    let pending = Arc::new(Mutex::new(Some(invocation.clone())));
    let init_state = initialized.clone();
    let new_init = initialized.clone();
    let new_attached = attached.clone();
    let new_id = session_id.clone();
    let new_workspace = invocation.workspace.clone();
    let is_first = invocation.first_message;
    let resume_attached = attached.clone();
    let resume_id = session_id.clone();
    let resume_workspace = invocation.workspace.clone();
    Agent
        .builder()
        .on_receive_request(
            async move |request: InitializeRequest, responder, _cx| {
                if request.protocol_version != ProtocolVersion::V1 {
                    return responder.respond_with_error(Error::invalid_params());
                }
                *init_state.lock().await = true;
                responder.respond(
                    InitializeResponse::new(ProtocolVersion::V1).agent_capabilities(
                        AgentCapabilities::new().session_capabilities(
                            SessionCapabilities::new().resume(SessionResumeCapabilities::new()),
                        ),
                    ),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: NewSessionRequest, responder, _cx| {
                if !*new_init.lock().await
                    || !is_first
                    || request.cwd != new_workspace
                    || !request.mcp_servers.is_empty()
                {
                    return responder.respond_with_error(Error::invalid_params());
                }
                let mut attached = new_attached.lock().await;
                if *attached {
                    return responder.respond_with_error(Error::invalid_request());
                }
                *attached = true;
                responder.respond(NewSessionResponse::new(new_id.clone()))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: ResumeSessionRequest, responder, _cx| {
                if !*initialized.lock().await
                    || is_first
                    || request.cwd != resume_workspace
                    || request.session_id.to_string() != resume_id
                    || !request.mcp_servers.is_empty()
                {
                    return responder.respond_with_error(Error::invalid_params());
                }
                let mut attached = resume_attached.lock().await;
                if *attached {
                    return responder.respond_with_error(Error::invalid_request());
                }
                *attached = true;
                responder.respond(ResumeSessionResponse::new())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: PromptRequest, responder, cx| {
                if !*attached.lock().await || request.session_id.to_string() != session_id {
                    return responder.respond_with_error(Error::invalid_params());
                }
                let prompt = request
                    .prompt
                    .iter()
                    .try_fold(String::new(), |mut text, block| {
                        if let ContentBlock::Text(content) = block {
                            text.push_str(&content.text);
                            Ok(text)
                        } else {
                            Err(Error::invalid_params())
                        }
                    });
                let prompt = match prompt {
                    Ok(prompt) if !prompt.trim().is_empty() => prompt,
                    _ => return responder.respond_with_error(Error::invalid_params()),
                };
                let Some(invocation) = pending.lock().await.take() else {
                    return responder.respond_with_error(Error::invalid_request());
                };
                let output = match invocation.agent {
                    AgentKind::Codex => codex::run(invocation, &prompt).await,
                    _ => run_cli(invocation, &prompt).await,
                };
                let meta = json!({"confer.nativeSessionId": output.observed_session_id});
                if let Some(error) = output.error {
                    return responder.respond_with_error(Error::new(-32000, error).data(meta));
                }
                if let Some(answer) = output.answer {
                    cx.send_notification(SessionNotification::new(
                        request.session_id,
                        SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
                            TextContent::new(answer),
                        ))),
                    ))?;
                }
                responder.respond(
                    PromptResponse::new(StopReason::EndTurn)
                        .meta(meta.as_object().expect("metadata object").clone()),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_to(channel)
        .await
}
