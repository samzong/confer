use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    AgentCapabilities, ContentBlock, ContentChunk, InitializeRequest, InitializeResponse,
    LoadSessionRequest, LoadSessionResponse, NewSessionRequest, NewSessionResponse,
    PermissionOption, PermissionOptionKind, PromptRequest, PromptResponse,
    RequestPermissionOutcome, RequestPermissionRequest, SelectedPermissionOutcome,
    SessionNotification, SessionUpdate, StopReason, TextContent, ToolCall, ToolCallStatus,
    ToolCallUpdate, ToolCallUpdateFields,
};
use agent_client_protocol::{Agent, Channel, Error, UntypedMessage};
use serde_json::json;

use super::{AdapterOutput, Invocation, acp};
use crate::types::AgentKind;

struct Script {
    load_session: bool,
    replay: Vec<SessionNotification>,
    updates: Vec<SessionNotification>,
    permissions: Vec<(Vec<PermissionOption>, RequestPermissionOutcome)>,
    response: Result<PromptResponse, Error>,
}

impl Default for Script {
    fn default() -> Self {
        Self {
            load_session: false,
            replay: Vec::new(),
            updates: vec![message("answer")],
            permissions: Vec::new(),
            response: Ok(PromptResponse::new(StopReason::EndTurn)),
        }
    }
}

fn invocation(first_message: bool) -> Invocation {
    Invocation {
        agent: AgentKind::Cursor,
        executable: "unused-test-agent".into(),
        workspace: "/virtual/acp-test".into(),
        native_session_id: Some("native-session".into()),
        model: None,
        reasoning_effort: None,
        instructions: Some("Review without editing files".into()),
        message: "current request".into(),
        first_message,
    }
}

fn chunk(text: &str) -> ContentChunk {
    ContentChunk::new(ContentBlock::Text(TextContent::new(text)))
}

fn notification(update: SessionUpdate) -> SessionNotification {
    SessionNotification::new("native-session", update)
}

fn message(text: &str) -> SessionNotification {
    notification(SessionUpdate::AgentMessageChunk(chunk(text)))
}

async fn run_script(
    invocation: Invocation,
    native: bool,
    script: Script,
) -> (AdapterOutput, Vec<&'static str>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let init_calls = calls.clone();
    let new_calls = calls.clone();
    let load_calls = calls.clone();
    let prompt_calls = calls.clone();
    let expected_message = invocation.message.clone();
    let expected_instructions = invocation.instructions.clone();
    let (client, server) = Channel::duplex();
    let server = tokio::spawn(async move {
        Agent
            .builder()
            .on_receive_request(
                async move |_request: InitializeRequest, responder, _cx| {
                    init_calls.lock().unwrap().push("initialize");
                    responder.respond(
                        InitializeResponse::new(ProtocolVersion::V1).agent_capabilities(
                            AgentCapabilities::new().load_session(script.load_session),
                        ),
                    )
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |_request: NewSessionRequest, responder, _cx| {
                    new_calls.lock().unwrap().push("new");
                    responder.respond(NewSessionResponse::new("native-session"))
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: LoadSessionRequest, responder, cx| {
                    load_calls.lock().unwrap().push("load");
                    assert_eq!(request.session_id.to_string(), "native-session");
                    for update in &script.replay {
                        cx.send_notification(update.clone())?;
                    }
                    responder.respond(LoadSessionResponse::new())
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: PromptRequest, responder, cx| {
                    prompt_calls.lock().unwrap().push("prompt");
                    let text = request
                        .prompt
                        .iter()
                        .filter_map(|block| match block {
                            ContentBlock::Text(text) => Some(text.text.as_str()),
                            _ => None,
                        })
                        .collect::<String>();
                    assert!(text.contains(&expected_message));
                    if let Some(instructions) = &expected_instructions {
                        assert!(text.contains(instructions));
                    }
                    let permissions = script.permissions.clone();
                    let updates = script.updates.clone();
                    let response = script.response.clone();
                    cx.spawn({
                        let cx = cx.clone();
                        async move {
                            for (options, expected) in permissions {
                                let response = cx
                                    .send_request(RequestPermissionRequest::new(
                                        request.session_id.clone(),
                                        ToolCallUpdate::new("tool-1", ToolCallUpdateFields::new()),
                                        options,
                                    ))
                                    .block_task()
                                    .await?;
                                assert_eq!(response.outcome, expected);
                            }
                            for update in updates {
                                cx.send_notification(update)?;
                            }
                            match response {
                                Ok(response) => responder.respond(response),
                                Err(error) => responder.respond_with_error(error),
                            }
                        }
                    })
                },
                agent_client_protocol::on_receive_request!(),
            )
            .connect_to(server)
            .await
    });
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        acp::run_connection(client, invocation, native),
    )
    .await
    .expect("ACP request and callback exchange must terminate");
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("ACP server must stop after the client disconnects")
        .expect("ACP mock agent must not panic")
        .expect("ACP mock connection must close cleanly");
    let calls = calls.lock().unwrap().clone();
    (output, calls)
}

#[tokio::test]
async fn load_replay_is_excluded_from_the_current_answer() {
    let (output, calls) = run_script(
        invocation(false),
        true,
        Script {
            load_session: true,
            replay: vec![message("previous answer")],
            updates: vec![message("current "), message("answer")],
            ..Script::default()
        },
    )
    .await;

    assert_eq!(output.answer.as_deref(), Some("current answer"));
    assert_eq!(
        output.observed_session_id.as_deref(),
        Some("native-session")
    );
    assert!(output.error.is_none(), "{:?}", output.error);
    assert_eq!(calls, ["initialize", "load", "prompt"]);
}

#[tokio::test]
async fn only_current_session_assistant_text_forms_the_answer() {
    let (output, _) = run_script(
        invocation(true),
        true,
        Script {
            updates: vec![
                notification(SessionUpdate::AgentThoughtChunk(chunk("private thought"))),
                notification(SessionUpdate::UserMessageChunk(chunk("user replay"))),
                notification(SessionUpdate::ToolCall(ToolCall::new(
                    "tool-1",
                    "read file",
                ))),
                notification(SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                    "tool-1",
                    ToolCallUpdateFields::new()
                        .status(ToolCallStatus::Completed)
                        .content(vec![
                            ContentBlock::Text(TextContent::new("tool result")).into(),
                        ]),
                ))),
                SessionNotification::new(
                    "another-session",
                    SessionUpdate::AgentMessageChunk(chunk("another answer")),
                ),
                message("current "),
                message("answer"),
            ],
            ..Script::default()
        },
    )
    .await;

    assert_eq!(output.answer.as_deref(), Some("current answer"));
    assert!(output.error.is_none(), "{:?}", output.error);
}

#[tokio::test]
async fn native_prompt_failure_keeps_the_session_and_provider_detail() {
    let (output, _) = run_script(
        invocation(true),
        true,
        Script {
            updates: Vec::new(),
            response: Err(Error::new(-32603, "Internal error")
                .data(json!({"message":"Provider usage exhausted","http_status":402}))),
            ..Script::default()
        },
    )
    .await;

    assert_eq!(
        output.observed_session_id.as_deref(),
        Some("native-session")
    );
    assert!(output.answer.is_none());
    assert!(output.error.unwrap().contains("Provider usage exhausted"));
}

#[tokio::test]
async fn partial_text_does_not_hide_bridge_failure_or_lose_its_native_id() {
    let (output, _) = run_script(
        invocation(true),
        false,
        Script {
            updates: vec![message("incomplete answer")],
            response: Err(Error::new(-32000, "Agent failed").data(json!({
                "confer.nativeSessionId":"bridge-native-session",
                "message":"Connection lost during generation"
            }))),
            ..Script::default()
        },
    )
    .await;

    assert_eq!(
        output.observed_session_id.as_deref(),
        Some("bridge-native-session")
    );
    assert!(
        output
            .error
            .expect("partial text must not turn a failed prompt into success")
            .contains("Connection lost during generation")
    );
}

#[tokio::test]
async fn unsupported_recovery_never_creates_a_new_session() {
    let (output, calls) = run_script(invocation(false), true, Script::default()).await;

    assert!(output.error.is_some());
    assert!(output.answer.is_none());
    assert_eq!(calls, ["initialize"]);
}

#[tokio::test]
async fn permission_callbacks_complete_while_the_prompt_is_pending() {
    let (output, _) = run_script(
        invocation(true),
        true,
        Script {
            permissions: vec![
                (
                    vec![
                        PermissionOption::new("deny", "Reject", PermissionOptionKind::RejectOnce),
                        PermissionOption::new(
                            "allow",
                            "Allow once",
                            PermissionOptionKind::AllowOnce,
                        ),
                    ],
                    RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new("allow")),
                ),
                (
                    vec![PermissionOption::new(
                        "deny",
                        "Reject",
                        PermissionOptionKind::RejectOnce,
                    )],
                    RequestPermissionOutcome::Cancelled,
                ),
            ],
            ..Script::default()
        },
    )
    .await;

    assert_eq!(output.answer.as_deref(), Some("answer"));
    assert!(output.error.is_none(), "{:?}", output.error);
}

#[tokio::test]
async fn grok_effort_uses_the_model_from_legacy_new_and_resume_responses() {
    for first_message in [true, false] {
        let mut invocation = invocation(first_message);
        invocation.agent = AgentKind::Grok;
        invocation.reasoning_effort = Some("high".into());
        let (client, server) = Channel::duplex();
        let configured = Arc::new(Mutex::new(false));
        let server = tokio::spawn(async move {
            Agent
                .builder()
                .on_receive_request(
                    async move |request: UntypedMessage, responder, cx| match request.method() {
                        "initialize" => responder.respond(json!({
                            "protocolVersion":1,
                            "agentCapabilities":{"sessionCapabilities":{"resume":{}}}
                        })),
                        "session/new" | "session/resume" => {
                            assert_eq!(request.method() == "session/new", first_message);
                            responder.respond(json!({
                                "sessionId":"native-session",
                                "models":{
                                    "currentModelId":"configured-model",
                                    "availableModels":[{
                                        "modelId":"configured-model",
                                        "name":"Configured model"
                                    }]
                                }
                            }))
                        }
                        "session/set_model" => {
                            assert_eq!(
                                request.params(),
                                &json!({
                                    "sessionId":"native-session",
                                    "modelId":"configured-model",
                                    "_meta":{"reasoningEffort":"high"}
                                })
                            );
                            *configured.lock().unwrap() = true;
                            responder.respond(json!({}))
                        }
                        "session/prompt" => {
                            assert!(*configured.lock().unwrap());
                            cx.send_notification(message("configured answer"))?;
                            responder.respond(json!({"stopReason":"end_turn"}))
                        }
                        _ => responder.respond_with_error(Error::method_not_found()),
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .connect_to(server)
                .await
        });
        let output = tokio::time::timeout(
            Duration::from_secs(5),
            acp::run_connection(client, invocation, true),
        )
        .await
        .expect("Grok setup and model selection must terminate");
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();

        assert_eq!(output.answer.as_deref(), Some("configured answer"));
        assert_eq!(
            output.observed_session_id.as_deref(),
            Some("native-session")
        );
        assert!(output.error.is_none(), "{:?}", output.error);
    }
}

#[tokio::test]
async fn cursor_negotiates_parameterized_model_configuration() {
    for model in [None, Some("cursor-model[effort=high]")] {
        let mut invocation = invocation(true);
        invocation.model = model.map(str::to_owned);
        invocation.reasoning_effort = model.is_none().then(|| "high".into());
        super::validate_invocation(&invocation).unwrap();
        let expected = if model.is_some() {
            vec![
                (json!("model"), json!("cursor-model")),
                (json!("effort"), json!("high")),
            ]
        } else {
            vec![(json!("effort"), json!("high"))]
        };
        let (client, server) = Channel::duplex();
        let parameterized = Arc::new(Mutex::new(false));
        let configured = Arc::new(Mutex::new(Vec::new()));
        let server = tokio::spawn(async move {
            Agent
                .builder()
                .on_receive_request(
                    async move |request: UntypedMessage, responder, cx| {
                        let params = request.params();
                        match request.method() {
                            "initialize" => {
                                *parameterized.lock().unwrap() = params
                                    .pointer("/clientCapabilities/_meta/parameterizedModelPicker")
                                    .and_then(serde_json::Value::as_bool)
                                    == Some(true);
                                responder
                                    .respond(json!({"protocolVersion":1,"agentCapabilities":{}}))
                            }
                            "session/new" => {
                                responder.respond(json!({"sessionId":"native-session"}))
                            }
                            "session/set_config_option" => {
                                if !*parameterized.lock().unwrap() {
                                    return responder.respond_with_error(Error::invalid_params());
                                }
                                configured
                                    .lock()
                                    .unwrap()
                                    .push((params["configId"].clone(), params["value"].clone()));
                                responder.respond(json!({"configOptions":[]}))
                            }
                            "session/prompt" => {
                                assert_eq!(*configured.lock().unwrap(), expected);
                                cx.send_notification(message("configured answer"))?;
                                responder.respond(json!({"stopReason":"end_turn"}))
                            }
                            _ => responder.respond_with_error(Error::method_not_found()),
                        }
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .connect_to(server)
                .await
        });
        let output = acp::run_connection(client, invocation, true).await;
        assert!(output.error.is_none(), "{output:?}");
        assert_eq!(output.answer.as_deref(), Some("configured answer"));
        server.await.unwrap().unwrap();
    }
}
