use std::sync::{Arc, Mutex};

use crate::types::AgentKind;
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    CloseSessionRequest, ContentBlock, InitializeRequest, LoadSessionRequest, NewSessionRequest,
    NewSessionResponse, PermissionOptionKind, PromptRequest, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, ResumeSessionRequest,
    SelectedPermissionOutcome, SessionNotification, SessionUpdate, SetSessionConfigOptionRequest,
    StopReason, TextContent,
};
use agent_client_protocol::{Client, ConnectTo, Error, UntypedMessage};
use serde_json::{Value, json};

use super::{AdapterOutput, Invocation, prompt_text, redact_secrets, truncate};

#[derive(Default)]
struct Output {
    active_session: Option<String>,
    native_id: Option<String>,
    text: String,
}

pub(super) async fn run_connection(
    transport: impl ConnectTo<Client> + 'static,
    invocation: Invocation,
    native: bool,
) -> AdapterOutput {
    let output = Arc::new(Mutex::new(Output::default()));
    let notifications = output.clone();
    let state = output.clone();
    let permissions = output.clone();
    let result = Client.builder()
        .name("confer")
        .on_receive_notification(
            async move |notification: SessionNotification, _cx| {
                let mut output = notifications.lock().expect("ACP output lock");
                if output.active_session.as_deref() != Some(notification.session_id.0.as_ref()) {
                    return Ok(());
                }
                if let SessionUpdate::AgentMessageChunk(chunk) = notification.update
                    && let ContentBlock::Text(text) = chunk.content
                {
                    output.text.push_str(&text.text);
                }
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |request: RequestPermissionRequest, responder, _cx| {
                let active = permissions.lock().expect("ACP output lock").active_session.clone();
                if active.as_deref() != Some(request.session_id.0.as_ref()) {
                    return responder.respond_with_error(Error::invalid_params());
                }
                let outcome = request
                    .options
                    .iter()
                    .find(|option| option.kind == PermissionOptionKind::AllowOnce)
                    .map(|option| {
                        RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                            option.option_id.clone(),
                        ))
                    })
                    .unwrap_or(RequestPermissionOutcome::Cancelled);
                responder.respond(RequestPermissionResponse::new(outcome))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: UntypedMessage, responder, _cx| {
                match request.method().trim_start_matches('_') {
                    "cursor/ask_question" => responder.respond(json!({"outcome":{"outcome":"skipped","reason":"Confer has no interactive question UI"}})),
                    "cursor/create_plan" => responder.respond(json!({"outcome":{"outcome":"cancelled"}})),
                    _ => responder.respond_with_error(Error::new(-32601, "unsupported non-interactive request")),
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(transport, async move |cx| {
            let mut initialize = InitializeRequest::new(ProtocolVersion::V1);
            if invocation.agent == AgentKind::Cursor {
                initialize.client_capabilities.meta = json!({"parameterizedModelPicker":true}).as_object().cloned();
            }
            let initialized = cx
                .send_request(initialize)
                .block_task()
                .await?;
            if initialized.protocol_version != ProtocolVersion::V1 {
                return Err(Error::new(-32000, "agent did not negotiate ACP v1"));
            }
            let caps = initialized.agent_capabilities;
            let setup;
            let session = if invocation.first_message {
                let mut request = NewSessionRequest::new(invocation.workspace.clone());
                if invocation.agent == AgentKind::Grok {
                    request.meta = Some(
                        json!({
                            "sessionId": invocation.native_session_id,
                            "yoloMode": true,
                            "modelId": invocation.model,
                            "reasoningEffort": invocation.reasoning_effort,
                            "sessionKind": "headless",
                        })
                        .as_object()
                        .expect("session metadata")
                        .clone(),
                    );
                }
                if invocation.agent == AgentKind::Grok {
                    setup = cx.send_request(UntypedMessage::new("session/new", &request)?).block_task().await?;
                    serde_json::from_value::<NewSessionResponse>(setup.clone())?.session_id
                } else {
                    let response = cx.send_request(request).block_task().await?;
                    setup = serde_json::to_value(&response)?;
                    response.session_id
                }
            } else {
                let id = invocation
                    .native_session_id
                    .clone()
                    .ok_or_else(|| Error::new(-32000, "resume requires a native session ID"))?;
                if caps.session_capabilities.resume.is_some() {
                    let request = ResumeSessionRequest::new(id.clone(), invocation.workspace.clone());
                    setup = if invocation.agent == AgentKind::Grok {
                        cx.send_request(UntypedMessage::new("session/resume", request)?).block_task().await?
                    } else {
                        serde_json::to_value(cx.send_request(request).block_task().await?)?
                    };
                } else if caps.load_session {
                    let response = cx.send_request(LoadSessionRequest::new(id.clone(), invocation.workspace.clone()))
                        .block_task().await?;
                    setup = serde_json::to_value(response)?;
                } else {
                    return Err(Error::new(-32000, "agent does not support session recovery"));
                }
                id.into()
            };
            {
                let mut output = state.lock().expect("ACP output lock");
                if native {
                    output.native_id = Some(session.to_string());
                }
                output.active_session = Some(session.to_string());
                output.text.clear();
            }
            if invocation.agent == AgentKind::Grok && (invocation.model.is_some() || invocation.reasoning_effort.is_some()) {
                let model = invocation.model.as_deref().or_else(|| setup.pointer("/models/currentModelId").and_then(Value::as_str))
                    .ok_or_else(|| Error::new(-32000, "Grok did not report its current model"))?;
                let mut params = json!({"sessionId":session,"modelId":model});
                if let Some(effort) = &invocation.reasoning_effort {
                    params["_meta"] = json!({"reasoningEffort":effort});
                }
                cx.send_request(UntypedMessage::new("session/set_model", params)?).block_task().await?;
            }
            if invocation.agent == AgentKind::Cursor {
                for (id, value) in super::cursor_config(&invocation).map_err(|error| Error::new(-32602, error.to_string()))? {
                    cx.send_request(SetSessionConfigOptionRequest::new(session.clone(), id.to_owned(), value)).block_task().await?;
                }
            }

            let response = cx
                .send_request(PromptRequest::new(
                    session.clone(),
                    vec![ContentBlock::Text(TextContent::new(prompt_text(&invocation)))],
                ))
                .block_task()
                .await;
            if let Ok(response) = &response {
                let mut output = state.lock().expect("ACP output lock");
                if !native {
                    output.native_id = response.meta.as_ref()
                        .and_then(|meta| meta.get("confer.nativeSessionId"))
                        .and_then(Value::as_str).map(str::to_owned);
                }
            }
            state.lock().expect("ACP output lock").active_session = None;
            let close = if caps.session_capabilities.close.is_some() {
                match tokio::time::timeout(std::time::Duration::from_secs(3), cx.send_request(CloseSessionRequest::new(session)).block_task()).await {
                    Ok(result) => result.map(|_| ()),
                    Err(_) => Err(Error::new(-32000, "ACP session close timed out")),
                }
            } else { Ok(()) };
            let response = response?;
            close?;
            if response.stop_reason != StopReason::EndTurn {
                return Err(Error::new(-32000, format!("agent stopped: {:?}", response.stop_reason)));
            }
            Ok(())
        })
        .await;
    let mut output = output.lock().expect("ACP output lock");
    let error = result.err().map(|error| {
        if let Some(id) = error
            .data
            .as_ref()
            .and_then(|data| data.get("confer.nativeSessionId"))
            .and_then(Value::as_str)
        {
            output.native_id = Some(id.to_owned());
        }
        let detail = error.data.as_ref().and_then(|data| {
            data.get("message")
                .and_then(Value::as_str)
                .or_else(|| data.as_str())
        });
        let message = match detail {
            Some(detail) => format!("{}: {detail}", error.message),
            None => error.message.to_string(),
        };
        truncate(&redact_secrets(&message))
    });
    let answer = (!output.text.is_empty()).then(|| std::mem::take(&mut output.text));
    AdapterOutput {
        observed_session_id: output.native_id.clone(),
        error: error.or_else(|| {
            answer
                .is_none()
                .then(|| "agent returned no final answer".into())
        }),
        answer,
    }
}
