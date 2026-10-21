use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_client_protocol::schema::v1::{
    ContentBlock, ContentChunk, PermissionOption, PermissionOptionKind, PromptRequest,
    PromptResponse, RequestPermissionOutcome, RequestPermissionRequest, SelectedPermissionOutcome,
    SessionNotification, SessionUpdate, StopReason, TextContent, ToolCall, ToolCallStatus,
    ToolCallUpdate, ToolCallUpdateFields,
};
use agent_client_protocol::{Agent, Channel, Error, UntypedMessage};
use serde_json::json;

use super::{AdapterOutput, Invocation, acp};
use crate::types::AgentKind;

#[derive(Clone)]
struct Script {
    load_session: bool,
    configure: bool,
    options: Vec<(serde_json::Value, serde_json::Value)>,
    rejected: Option<(String, String)>,
    replay: Vec<SessionNotification>,
    updates: Vec<SessionNotification>,
    permissions: Vec<(Vec<PermissionOption>, RequestPermissionOutcome)>,
    response: Result<PromptResponse, Error>,
}

impl Default for Script {
    fn default() -> Self {
        Self {
            load_session: false,
            configure: false,
            options: Vec::new(),
            rejected: None,
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
        cancellation: Default::default(),
        session_observer: Default::default(),
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
    let seen = calls.clone();
    let agent = invocation.agent;
    let first = invocation.first_message;
    let configured = Arc::new(Mutex::new(Vec::new()));
    let expected_message = invocation.message.clone();
    let expected_instructions = invocation.instructions.clone();
    let (client, server) = Channel::duplex();
    let server = tokio::spawn(async move {
        Agent.builder().on_receive_request(
            async move |request: UntypedMessage, responder, cx| {
                match request.method() {
                    "initialize" => {
                        calls.lock().unwrap().push("initialize");
                        if agent == AgentKind::Cursor {
                            assert_eq!(request.params().pointer("/clientCapabilities/_meta/parameterizedModelPicker"), Some(&json!(true)));
                        }
                        let mut caps = json!({"loadSession":script.load_session});
                        if script.configure {
                            caps["sessionCapabilities"] = json!({"close":{}});
                            if matches!(agent, AgentKind::Kimi | AgentKind::Grok | AgentKind::Opencode) {
                                caps["sessionCapabilities"]["resume"] = json!({});
                            }
                        }
                        responder.respond(json!({"protocolVersion":1,"agentCapabilities":caps}))
                    }
                    "session/new" | "session/load" | "session/resume" => {
                        let method = request.method().strip_prefix("session/").unwrap();
                        assert_eq!(method == "new", first);
                        if !first {
                            assert_eq!(request.params()["sessionId"], "native-session");
                        }
                        calls.lock().unwrap().push(match method { "new" => "new", "load" => "load", _ => "resume" });
                        for update in &script.replay {
                            cx.send_notification(update.clone())?;
                        }
                        let response = match agent {
                            AgentKind::Grok => json!({"sessionId":"native-session","models":{"currentModelId":"configured-model","availableModels":[{"modelId":"configured-model","name":"Configured model"}]}}),
                            AgentKind::Kimi => json!({"sessionId":"native-session","configOptions":[{"type":"select","id":"thinking","currentValue":"on","options":[{"value":"on"}]},{"type":"select","id":"mode","currentValue":"default"}]}),
                            AgentKind::Copilot => json!({"sessionId":"native-session","configOptions":[{"type":"select","id":"allow_all","currentValue":"on","options":[]}]}),
                            _ => json!({"sessionId":"native-session"}),
                        };
                        responder.respond(response)
                    }
                    "session/set_model" => {
                        assert_eq!(request.params(), &json!({"sessionId":"native-session","modelId":"configured-model","_meta":{"reasoningEffort":"high"}}));
                        configured.lock().unwrap().push((json!("model"), json!("configured-model")));
                        responder.respond(json!({}))
                    }
                    "session/set_config_option" => {
                        let params = request.params();
                        assert_eq!(params["sessionId"], "native-session");
                        configured.lock().unwrap().push((params["configId"].clone(), params["value"].clone()));
                        if let Some((id, message)) = &script.rejected && params["configId"] == *id {
                            return responder.respond_with_error(Error::new(-32602, message.clone()));
                        }
                        responder.respond(json!({"configOptions":[]}))
                    }
                    "session/close" => responder.respond(json!({})),
                    "session/prompt" => {
                        calls.lock().unwrap().push("prompt");
                        assert!(script.rejected.is_none());
                        assert_eq!(*configured.lock().unwrap(), script.options);
                        let request: PromptRequest = serde_json::from_value(request.params().clone())?;
                        let text = request.prompt.iter().filter_map(|block| match block {
                            ContentBlock::Text(text) => Some(text.text.as_str()),
                            _ => None,
                        }).collect::<String>();
                        assert!(text.starts_with(super::PROMPT_PREAMBLE));
                        assert!(text.contains(&expected_message));
                        if let Some(instructions) = &expected_instructions {
                            assert!(text.contains(instructions));
                        }
                        let script = script.clone();
                        cx.spawn({
                            let cx = cx.clone();
                            async move {
                                for (options, expected) in script.permissions {
                                    let response = cx.send_request(RequestPermissionRequest::new(
                                        request.session_id.clone(),
                                        ToolCallUpdate::new("tool-1", ToolCallUpdateFields::new()),
                                        options,
                                    )).block_task().await?;
                                    assert_eq!(response.outcome, expected);
                                }
                                for update in script.updates {
                                    cx.send_notification(update)?;
                                }
                                match script.response {
                                    Ok(response) => responder.respond(serde_json::to_value(response)?),
                                    Err(error) => responder.respond_with_error(error),
                                }
                            }
                        })
                    }
                    _ => responder.respond_with_error(Error::method_not_found()),
                }
            }, agent_client_protocol::on_receive_request!(),
        ).connect_to(server).await
    });
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        acp::run_connection(client, invocation, native),
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let calls = seen.lock().unwrap().clone();
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
async fn cancellation_notifies_the_active_session_and_keeps_partial_text() {
    use agent_client_protocol::schema::v1::CancelNotification;
    let mut invocation = invocation(true);
    invocation.cancellation = Default::default();
    let cancellation = invocation.cancellation.clone();
    let ended = Arc::new(tokio::sync::Notify::new());
    let cancelled = ended.clone();
    let (ready, started) = tokio::sync::oneshot::channel();
    let ready = Arc::new(Mutex::new(Some(ready)));
    let (client, server) = Channel::duplex();
    let task = tokio::spawn(async move {
        Agent
            .builder()
            .on_receive_notification(
                async move |request: CancelNotification, _cx| {
                    assert_eq!(request.session_id.to_string(), "native-session");
                    cancelled.notify_one();
                    Ok(())
                },
                agent_client_protocol::on_receive_notification!(),
            )
            .on_receive_request(
                async move |request: UntypedMessage, responder, cx| match request.method() {
                    "initialize" => responder.respond(json!({"protocolVersion":1})),
                    "session/new" => responder.respond(json!({"sessionId":"native-session"})),
                    "session/prompt" => {
                        cx.send_notification(message("partial answer"))?;
                        ready.lock().unwrap().take().unwrap().send(()).unwrap();
                        let ended = ended.clone();
                        cx.spawn(async move {
                            ended.notified().await;
                            responder.respond(json!({"stopReason":"cancelled"}))
                        })
                    }
                    _ => responder.respond_with_error(Error::method_not_found()),
                },
                agent_client_protocol::on_receive_request!(),
            )
            .connect_to(server)
            .await
    });
    let run = tokio::spawn(acp::run_connection(client, invocation, true));
    started.await.unwrap();
    cancellation.cancel();
    let output = tokio::time::timeout(Duration::from_secs(1), run)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        output.observed_session_id.as_deref(),
        Some("native-session")
    );
    assert_eq!(output.answer.as_deref(), Some("partial answer"));
    task.await.unwrap().unwrap();
}

mod config_tests;
