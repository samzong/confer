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
                            if matches!(agent, AgentKind::Kimi | AgentKind::Grok) {
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
async fn grok_effort_uses_the_model_from_legacy_new_and_resume_responses() {
    for first in [true, false] {
        let mut invocation = invocation(first);
        invocation.agent = AgentKind::Grok;
        invocation.reasoning_effort = Some("high".into());
        configured_session(invocation, vec![("model", "configured-model")], None).await;
    }
}

async fn configured_session(
    invocation: Invocation,
    expected: Vec<(&str, &str)>,
    rejected: Option<(&str, &str)>,
) {
    super::validate_invocation(&invocation).unwrap();
    let attach = match (invocation.first_message, invocation.agent) {
        (true, _) => "new",
        (false, AgentKind::Kimi | AgentKind::Grok) => "resume",
        _ => "load",
    };
    let script = Script {
        configure: true,
        load_session: true,
        options: expected
            .into_iter()
            .map(|(id, value)| (json!(id), json!(value)))
            .collect(),
        rejected: rejected.map(|(id, message)| (id.to_owned(), message.to_owned())),
        replay: vec![message("previous answer")],
        updates: vec![message("configured answer")],
        ..Script::default()
    };
    let (output, calls) = run_script(invocation, true, script).await;
    if let Some((_, message)) = rejected {
        assert!(!calls.contains(&"prompt"));
        assert!(output.observed_session_id.is_none(), "{output:?}");
        assert!(output.answer.is_none());
        assert!(
            output
                .error
                .as_deref()
                .is_some_and(|error| error.contains(message)),
            "{output:?}"
        );
    } else {
        assert!(output.error.is_none(), "{output:?}");
        assert_eq!(output.answer.as_deref(), Some("configured answer"));
        assert_eq!(
            output.observed_session_id.as_deref(),
            Some("native-session")
        );
        assert_eq!(calls, ["initialize", attach, "prompt"]);
    }
}

#[tokio::test]
async fn cursor_negotiates_parameterized_model_configuration() {
    for model in [None, Some("cursor-model[effort=high]")] {
        let mut invocation = invocation(true);
        invocation.model = model.map(str::to_owned);
        invocation.reasoning_effort = model.is_none().then(|| "high".into());
        let expected = if model.is_some() {
            vec![("model", "cursor-model"), ("effort", "high")]
        } else {
            vec![("effort", "high")]
        };
        configured_session(invocation, expected, None).await;
    }
}

#[tokio::test]
async fn copilot_applies_model_and_effort_through_config_options() {
    for (first, model, effort) in [
        (true, Some("gpt-5.4"), Some("high")),
        (false, Some("gpt-5.4"), None),
        (false, None, Some("low")),
        (true, None, None),
    ] {
        let mut invocation = invocation(first);
        invocation.agent = AgentKind::Copilot;
        invocation.model = model.map(str::to_owned);
        invocation.reasoning_effort = effort.map(str::to_owned);
        let expected = model
            .map(|value| ("model", value))
            .into_iter()
            .chain(effort.map(|value| ("reasoning_effort", value)))
            .collect();
        configured_session(invocation, expected, None).await;
    }
}

#[tokio::test]
async fn kimi_sends_auto_mode_then_model_then_thinking() {
    for (first, model, effort) in [
        (true, "kimi-code/k3", None),
        (true, "kimi-code/k3", Some("low")),
        (true, "kimi-code/k3", Some("high")),
        (true, "kimi-code/k3", Some("max")),
        (true, "kimi-code/k3", Some("on")),
        (false, "kimi-code/kimi-for-coding", Some("on")),
    ] {
        let mut invocation = invocation(first);
        invocation.agent = AgentKind::Kimi;
        invocation.model = Some(model.to_owned());
        invocation.reasoning_effort = effort.map(str::to_owned);
        let expected = [("mode", "auto"), ("model", model)]
            .into_iter()
            .chain(effort.map(|value| ("thinking", value)))
            .collect();
        configured_session(invocation, expected, None).await;
    }
}

#[tokio::test]
async fn kimi_unknown_thinking_fails_before_the_prompt() {
    let mut invocation = invocation(true);
    invocation.agent = AgentKind::Kimi;
    invocation.model = Some("kimi-code/kimi-for-coding".into());
    invocation.reasoning_effort = Some("high".into());
    configured_session(
        invocation,
        vec![],
        Some(("thinking", "Invalid params: Unknown thinking value: high")),
    )
    .await;
}

#[tokio::test]
async fn configuration_failure_before_the_prompt_records_no_native_session() {
    let mut invocation = invocation(true);
    invocation.agent = AgentKind::Copilot;
    invocation.model = Some("gpt-5.4".into());
    invocation.reasoning_effort = Some("low".into());
    configured_session(
        invocation,
        vec![],
        Some((
            "reasoning_effort",
            "The selected model does not support reasoning_effort configuration.",
        )),
    )
    .await;
}
