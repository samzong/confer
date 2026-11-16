use super::Invocation;
use super::cli::{
    append_streamed_text, build_command, extract_answer, extract_native_error, extract_session_id,
};
use crate::types::AgentKind;
use std::path::PathBuf;

fn invocation(agent: AgentKind) -> Invocation {
    Invocation {
        agent,
        executable: agent.binary_names()[0].into(),
        workspace: "/workspace".into(),
        native_session_id: None,
        model: None,
        reasoning_effort: None,
        instructions: None,
        message: "Analyze this".into(),
        first_message: true,
        cancellation: Default::default(),
        session_observer: Default::default(),
    }
}

#[test]
fn prompt_includes_preamble_before_instructions_and_message() {
    for first_message in [true, false] {
        for instructions in [None, Some(""), Some("  "), Some(" Private instructions ")] {
            let invocation = Invocation {
                first_message,
                instructions: instructions.map(String::from),
                ..invocation(AgentKind::Codex)
            };
            let prompt = super::prompt_text(&invocation);
            let mut parts = prompt.split("\n\n");
            assert_eq!(parts.next(), Some(super::PROMPT_PREAMBLE));
            if instructions.is_some_and(|value| !value.trim().is_empty()) {
                assert_eq!(parts.next(), Some("Private instructions"));
            }
            assert_eq!(parts.next(), Some(invocation.message.as_str()));
            assert_eq!(parts.next(), None);
        }
    }
}

#[test]
fn parses_native_results() {
    for (value, session, answer) in [
        (
            serde_json::json!({"type":"result","session_id":"claude-session","result":"final answer"}),
            "claude-session",
            "final answer",
        ),
        (
            serde_json::json!({"text":"GROK_OK","stopReason":"end_turn","sessionId":"grok-session"}),
            "grok-session",
            "GROK_OK",
        ),
        (
            serde_json::json!({"conversation_id":"agy-conv-123","status":"SUCCESS","response":"AGY_OK\n","duration_seconds":1.2}),
            "agy-conv-123",
            "AGY_OK\n",
        ),
    ] {
        assert_eq!(extract_session_id(&value).as_deref(), Some(session));
        assert_eq!(extract_answer(&value).as_deref(), Some(answer));
    }
    let started = serde_json::json!({"type":"thread.started","thread_id":"thread-1"});
    let completed =
        serde_json::json!({"type":"item.completed","item":{"type":"agent_message","text":"done"}});
    assert_eq!(extract_session_id(&started).as_deref(), Some("thread-1"));
    assert_eq!(extract_answer(&completed).as_deref(), Some("done"));
}

#[test]
fn assembles_grok_streamed_text() {
    let mut output = String::new();
    append_streamed_text(
        &serde_json::json!({ "type": "text", "data": "GROK_" }),
        &mut output,
    );
    append_streamed_text(
        &serde_json::json!({ "type": "text", "data": "OK" }),
        &mut output,
    );
    assert_eq!(output, "GROK_OK");
}

#[test]
fn extracts_structured_native_errors() {
    let value = serde_json::json!({
        "type": "error",
        "message": "authentication failed"
    });
    assert_eq!(
        extract_native_error(&value).as_deref(),
        Some("authentication failed")
    );
}

#[test]
fn cursor_rejects_conflicting_model_options_and_effort() {
    let invocation = Invocation {
        workspace: PathBuf::from("/tmp"),
        native_session_id: Some("chat-1".into()),
        model: Some("model[option=value]".into()),
        reasoning_effort: Some("high".into()),
        message: "test".into(),
        first_message: false,
        cancellation: Default::default(),
        session_observer: Default::default(),
        ..invocation(AgentKind::Cursor)
    };
    assert!(super::validate_invocation(&invocation).is_err());
}

#[test]
fn copilot_accepts_only_its_native_effort_levels() {
    for (effort, valid) in [
        ("none", true),
        ("minimal", true),
        ("xhigh", true),
        ("max", true),
        ("ultra", false),
        ("extreme", false),
    ] {
        let result = super::validate_seat_config(AgentKind::Copilot, None, Some(effort));
        assert_eq!(result.is_ok(), valid, "{effort}: {result:?}");
    }
    assert!(
        build_command(
            &Invocation {
                message: "test".into(),
                ..invocation(AgentKind::Copilot)
            },
            "prompt"
        )
        .is_err()
    );
}

#[test]
fn builds_agy_command_for_first_and_resume_messages() {
    let first = Invocation {
        model: Some("gemini-3.8-flash-high".into()),
        reasoning_effort: Some("high".into()),
        instructions: Some("Do not edit files.".into()),
        ..invocation(AgentKind::Agy)
    };
    let command = build_command(&first, &super::prompt_text(&first)).unwrap();
    let debug = format!("{command:?}");
    assert!(debug.contains("--add-dir"));
    assert!(debug.contains("/workspace"));
    assert!(debug.contains("--model"));
    assert!(debug.contains("gemini-3.8-flash-high"));
    assert!(debug.contains("--effort"));
    assert!(debug.contains("high"));
    assert!(debug.contains("--output-format"));
    assert!(debug.contains("json"));

    let resume = Invocation {
        native_session_id: Some("conv-456".into()),
        message: "Next step".into(),
        first_message: false,
        cancellation: Default::default(),
        session_observer: Default::default(),
        ..invocation(AgentKind::Agy)
    };
    let command = build_command(&resume, &super::prompt_text(&resume)).unwrap();
    let debug = format!("{command:?}");
    assert!(debug.contains("--conversation"));
    assert!(debug.contains("conv-456"));

    let invalid_resume = Invocation {
        message: "Next step".into(),
        first_message: false,
        cancellation: Default::default(),
        session_observer: Default::default(),
        ..invocation(AgentKind::Agy)
    };
    assert!(build_command(&invalid_resume, &super::prompt_text(&invalid_resume)).is_err());

    let invalid_effort = Invocation {
        reasoning_effort: Some("xhigh".into()),
        ..invocation(AgentKind::Agy)
    };
    assert!(build_command(&invalid_effort, &super::prompt_text(&invalid_effort)).is_err());
}

#[test]
fn kimi_requires_native_acp_transport() {
    let first = Invocation {
        model: Some("kimi-code/k3".into()),
        ..invocation(AgentKind::Kimi)
    };
    let error = build_command(&first, &super::prompt_text(&first))
        .unwrap_err()
        .to_string();
    assert!(error.contains("dedicated adapter"), "{error}");
}

#[test]
fn kimi_accepts_thinking_values_as_reasoning_effort() {
    assert!(super::validate_seat_config(AgentKind::Copilot, None, Some("none")).is_ok());
    for effort in [None, Some("on"), Some("low"), Some("high"), Some("max")] {
        assert!(
            super::validate_seat_config(AgentKind::Kimi, None, effort).is_ok(),
            "{effort:?}"
        );
    }
    for effort in [
        "none",
        "off",
        "medium",
        "minimal",
        "xhigh",
        "ultra",
        "not-a-level",
    ] {
        assert!(
            super::validate_seat_config(AgentKind::Kimi, None, Some(effort)).is_err(),
            "{effort}"
        );
    }
}

#[test]
fn devin_rejects_reasoning_effort() {
    assert!(super::validate_seat_config(AgentKind::Devin, Some("opus"), None).is_ok());
    assert!(super::validate_seat_config(AgentKind::Devin, None, Some("high")).is_err());
    assert!(super::validate_seat_config(AgentKind::Devin, None, Some("ultra")).is_err());
    assert!(
        build_command(&invocation(AgentKind::Devin), "prompt")
            .unwrap_err()
            .to_string()
            .contains("dedicated adapter")
    );
}

#[test]
fn rx_home_respects_env_override_and_default() {
    use std::ffi::OsString;
    let home = PathBuf::from("/home/test");
    assert_eq!(
        super::resolve_rx_home(Some(OsString::from("/custom/rx")), Some(home.clone())).unwrap(),
        PathBuf::from("/custom/rx")
    );
    assert_eq!(
        super::resolve_rx_home(None, Some(home)).unwrap(),
        PathBuf::from("/home/test/.rx")
    );
    assert!(super::resolve_rx_home(None, None).is_err());
    let cwd = std::env::current_dir().unwrap();
    assert_eq!(
        super::resolve_rx_home(Some(OsString::from("relative/rx")), None).unwrap(),
        cwd.join("relative/rx")
    );
}

#[test]
fn rx_auth_marker_accepts_native_store_and_launcher_provider_without_reading_keys() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let ready = |env: &[(&str, &str)]| {
        super::rx::rx_has_credentials_with(
            home,
            None,
            env.iter().map(|(k, v)| ((*k).into(), (*v).into())),
        )
    };
    assert!(!ready(&[]));

    std::fs::create_dir_all(home.join(".rx")).unwrap();
    std::fs::write(home.join(".rx/auth.json"), "{}").unwrap();
    assert!(!ready(&[]));
    std::fs::write(
        home.join(".rx/auth.json"),
        r#"{"amazon-bedrock":{"type":"api_key"}}"#,
    )
    .unwrap();
    assert!(!ready(&[]));
    std::fs::write(
        home.join(".rx/auth.json"),
        r#"{"amazon-bedrock":{"type":"api_key"},"openai-codex":{"type":"oauth","access":"tok","refresh":"ref","expires":9999999999}}"#,
    )
    .unwrap();
    assert!(ready(&[]));

    std::fs::remove_file(home.join(".rx/auth.json")).unwrap();
    assert!(!ready(&[("TOKENER_API_KEY", "not-a-real-key")]));
    assert!(ready(&[("OPENROUTER_API_KEY", "not-a-real-key")]));

    std::fs::create_dir_all(home.join(".recall")).unwrap();
    std::fs::write(
        home.join(".recall/rx.toml"),
        "default_provider = \"none\"\n",
    )
    .unwrap();
    assert!(!ready(&[("OPENROUTER_API_KEY", "not-a-real-key")]));

    std::fs::write(
        home.join(".rx/config.json"),
        r#"{"model":"openai-codex/gpt-5.4"}"#,
    )
    .unwrap();
    assert!(!ready(&[("ANTHROPIC_API_KEY", "not-a-real-key")]));
    std::fs::write(
        home.join(".rx/config.json"),
        r#"{"model":"anthropic/claude-opus-4"}"#,
    )
    .unwrap();
    assert!(ready(&[("ANTHROPIC_AUTH_TOKEN", "not-a-real-key")]));
    std::fs::remove_file(home.join(".rx/config.json")).unwrap();

    std::fs::write(
        home.join(".recall/rx.toml"),
        "default_provider = \"tokener\"\n",
    )
    .unwrap();
    assert!(!ready(&[]));
    std::fs::write(
        home.join(".recall/rx.keys"),
        "tokener = \"not-a-real-key\"\n",
    )
    .unwrap();
    assert!(ready(&[]));

    std::fs::remove_file(home.join(".recall/rx.keys")).unwrap();
    std::fs::write(
        home.join(".recall/rx.toml"),
        "default_provider = \"customgw\"\n[provider.customgw]\nbase_url = \"https://example.invalid/v1\"\nauth = \"env\"\nenv = \"CUSTOMGW_API_KEY\"\n",
    )
    .unwrap();
    assert!(!ready(&[]));
    assert!(ready(&[("CUSTOMGW_API_KEY", "not-a-real-key")]));
}

#[test]
fn rx_agent_installation_check_verifies_managed_binary() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let missing = home.join("isolated-bin/rxa");
    let binary_name = if cfg!(windows) {
        "rx-agent.exe"
    } else {
        "rx-agent"
    };
    assert!(!super::rx::rx_agent_installed_with(
        &missing,
        Some(home),
        None
    ));

    let version_dir = home.join(".recall/rx-agent/versions/0.2.0");
    std::fs::create_dir_all(&version_dir).unwrap();
    std::fs::write(
        home.join(".recall/rx-agent/install.toml"),
        "current = \"0.2.0\"\n",
    )
    .unwrap();
    assert!(!super::rx::rx_agent_installed_with(
        &missing,
        Some(home),
        None
    ));
    std::fs::write(version_dir.join(binary_name), "").unwrap();
    assert!(super::rx::rx_agent_installed_with(
        &missing,
        Some(home),
        None
    ));
}

#[test]
fn rx_secrets_are_redacted_including_custom_provider_envs() {
    use std::ffi::OsString;

    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    std::fs::create_dir_all(home.join(".recall")).unwrap();
    std::fs::write(
        home.join(".recall/rx.toml"),
        "[provider.customgw]\nbase_url = \"https://example.invalid/v1\"\nauth = \"env\"\nenv = \"CUSTOM_CRED\"\n",
    )
    .unwrap();
    std::fs::write(
        home.join(".recall/rx.keys"),
        "openrouter = \"or-file-secret\"\n",
    )
    .unwrap();
    let custom_rx = home.join("custom-rx");
    std::fs::create_dir_all(&custom_rx).unwrap();
    std::fs::write(
        custom_rx.join("auth.json"),
        r#"{"bedrock":{"type":"api_key","key":"ak-secret","env":{"AWS_SECRET_ACCESS_KEY":"aws-sec","AWS_PROFILE":"default","GOOGLE_CLOUD_LOCATION":"global"}},"codex":{"type":"oauth","access":"oa-acc","refresh":"oa-ref","expires":1}}"#,
    )
    .unwrap();

    let scrubbed = super::process::redact_secrets_with(
        "tokener=tok-secret custom=cust-secret or=or-file-secret ak=ak-secret aws=aws-sec prof=default loc=global acc=oa-acc ref=oa-ref",
        Some(home),
        Some(OsString::from(&custom_rx)),
        [
            ("TOKENER_API_KEY".into(), "tok-secret".into()),
            ("CUSTOM_CRED".into(), "cust-secret".into()),
        ],
    );
    assert_eq!(
        scrubbed,
        "tokener=[REDACTED] custom=[REDACTED] or=[REDACTED] ak=[REDACTED] aws=[REDACTED] prof=default loc=global acc=[REDACTED] ref=[REDACTED]"
    );
}

#[test]
fn rx_accepts_native_thinking_and_rejects_unsupported_values() {
    for effort in [
        None,
        Some("off"),
        Some("minimal"),
        Some("low"),
        Some("medium"),
        Some("high"),
        Some("xhigh"),
        Some("max"),
    ] {
        assert!(
            super::validate_seat_config(AgentKind::Rx, None, effort).is_ok(),
            "{effort:?}"
        );
    }
    for effort in ["none", "on", "ultra", "not-a-level"] {
        assert!(
            super::validate_seat_config(AgentKind::Rx, None, Some(effort)).is_err(),
            "{effort}"
        );
    }
}

#[test]
fn rx_resume_uses_the_native_session() {
    assert_eq!(
        super::resume_command(AgentKind::Rx, "/workspace", "session-1").as_deref(),
        Some("cd /workspace && rxa --resume session-1")
    );
}

#[test]
fn kimi_home_respects_env_override_and_default() {
    use std::ffi::OsString;
    let home = PathBuf::from("/home/test");
    assert_eq!(
        super::resolve_kimi_home(Some(OsString::from("/custom/kimi")), Some(home.clone())).unwrap(),
        PathBuf::from("/custom/kimi")
    );
    assert_eq!(
        super::resolve_kimi_home(None, Some(home.clone())).unwrap(),
        home.join(".kimi-code")
    );
    assert_eq!(
        super::resolve_kimi_home(Some(OsString::new()), Some(home.clone())).unwrap(),
        home.join(".kimi-code")
    );
    assert!(super::resolve_kimi_home(None, None).is_err());
    let cwd = std::env::current_dir().unwrap();
    assert_eq!(
        super::resolve_kimi_home(Some(OsString::from("relative/kimi")), Some(home.clone()))
            .unwrap(),
        cwd.join("relative/kimi")
    );
}

#[test]
fn kimi_auth_marker_requires_credentials_or_a_key() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    assert!(!super::readiness::kimi_home_has_auth(home));
    for (config, ready) in [
        ("[providers.\"managed:kimi-code\"]\napi_key = \"\"\n", false),
        (
            "[services.moonshot_search]\nbase_url = \"https://api.moonshot.cn/v1/search\"\napi_key = \"sk-search\"\n",
            false,
        ),
        (
            "[providers.vertexai.env]\nGOOGLE_CLOUD_PROJECT = \"my-gcp-project\"\nGOOGLE_CLOUD_LOCATION = \"us-central1\"\n",
            false,
        ),
        ("[providers.kimi.env]\nKIMI_API_KEY = \"sk-env\"\n", true),
        (
            "[providers.custom]\ntype = \"openai\"\napi_key = \"sk-test\"\n",
            true,
        ),
    ] {
        std::fs::write(home.join("config.toml"), config).unwrap();
        assert_eq!(
            super::readiness::kimi_home_has_auth(home),
            ready,
            "{config}"
        );
    }
    std::fs::remove_file(home.join("config.toml")).unwrap();
    std::fs::create_dir_all(home.join("credentials/mcp")).unwrap();
    std::fs::write(home.join("credentials/mcp/tool.json"), "{}").unwrap();
    assert!(!super::readiness::kimi_home_has_auth(home));
    std::fs::write(home.join("credentials/kimi-code.json"), "{}").unwrap();
    assert!(super::readiness::kimi_home_has_auth(home));
}

#[cfg(unix)]
#[test]
fn shell_quote_survives_shell_parsing() {
    let workspace = "/tmp/it's a repo";
    let printed = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("printf %s {}", super::shell_quote(workspace)))
        .output()
        .unwrap();
    assert_eq!(printed.stdout, workspace.as_bytes());
    assert!(super::resume_command(AgentKind::Cursor, workspace, "session-1").is_none());
}
