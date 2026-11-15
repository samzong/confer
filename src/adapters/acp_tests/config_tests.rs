use super::*;

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
    crate::adapters::validate_invocation(&invocation).unwrap();
    let first_message = invocation.first_message;
    let attach = match (invocation.first_message, invocation.agent) {
        (true, _) => "new",
        (false, AgentKind::Kimi | AgentKind::Grok | AgentKind::Opencode) => "resume",
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
        let expected_session = (!first_message).then_some("native-session");
        assert_eq!(
            output.observed_session_id.as_deref(),
            expected_session,
            "{output:?}"
        );
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
async fn opencode_sets_build_mode_then_model_then_effort_and_resumes() {
    for (first, model, effort) in [
        (true, Some("opencode/big-pickle"), None),
        (true, Some("provider/model"), Some("high")),
        (false, Some("provider/model"), Some("default")),
        (false, None, Some("low")),
    ] {
        let mut invocation = invocation(first);
        invocation.agent = AgentKind::Opencode;
        invocation.model = model.map(str::to_owned);
        invocation.reasoning_effort = effort.map(str::to_owned);
        crate::adapters::validate_invocation(&invocation).unwrap();
        let expected = std::iter::once(("mode", "build"))
            .chain(model.map(|value| ("model", value)))
            .chain(effort.map(|value| ("effort", value)))
            .collect();
        configured_session(invocation, expected, None).await;
    }
}

#[tokio::test]
async fn opencode_unsupported_model_effort_keeps_session_without_prompting() {
    let mut invocation = invocation(true);
    invocation.agent = AgentKind::Opencode;
    invocation.model = Some("opencode/big-pickle".into());
    invocation.reasoning_effort = Some("high".into());
    configured_session(
        invocation,
        vec![],
        Some(("effort", "Invalid params: effort not found: high")),
    )
    .await;
}

#[tokio::test]
async fn rx_applies_model_then_thinking_on_create_and_load() {
    for (first, model, effort) in [
        (true, Some("tokener/kimi-k3.1"), Some("high")),
        (false, Some("tokener/claude-opus-5"), Some("medium")),
        (false, None, Some("off")),
        (true, None, None),
    ] {
        let mut invocation = invocation(first);
        invocation.agent = AgentKind::Rx;
        invocation.model = model.map(str::to_owned);
        invocation.reasoning_effort = effort.map(str::to_owned);
        let expected = model
            .map(|value| ("model", value))
            .into_iter()
            .chain(effort.map(|value| ("thinking", value)))
            .collect();
        configured_session(invocation, expected, None).await;
    }
}

#[tokio::test]
async fn rx_unsupported_thinking_fails_before_the_prompt() {
    let mut invocation = invocation(true);
    invocation.agent = AgentKind::Rx;
    invocation.model = Some("tokener/kimi-k3.1".into());
    invocation.reasoning_effort = Some("low".into());
    configured_session(
        invocation,
        vec![],
        Some((
            "thinking",
            "thinking level is not supported by the selected model",
        )),
    )
    .await;
}

#[tokio::test]
async fn non_rx_acp_agents_keep_their_configuration_and_recovery_paths() {
    let mut invocation = super::invocation(false);
    invocation.agent = AgentKind::Kimi;
    invocation.model = Some("kimi-code/k3".into());
    invocation.reasoning_effort = Some("high".into());
    configured_session(
        invocation,
        vec![
            ("mode", "auto"),
            ("model", "kimi-code/k3"),
            ("thinking", "high"),
        ],
        None,
    )
    .await;

    let mut invocation = super::invocation(false);
    invocation.agent = AgentKind::Opencode;
    invocation.model = Some("opencode/big-pickle".into());
    configured_session(
        invocation,
        vec![("mode", "build"), ("model", "opencode/big-pickle")],
        None,
    )
    .await;
}

#[tokio::test]
async fn configuration_failure_before_the_prompt_keeps_native_session() {
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
