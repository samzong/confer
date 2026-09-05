use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use super::{Invocation, run};
use crate::types::AgentKind;

fn invocation(directory: &Path, agent: AgentKind, script: &str) -> Invocation {
    let executable = directory.join("native-agent");
    std::fs::write(&executable, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    Invocation {
        agent,
        executable,
        workspace: directory.to_owned(),
        native_session_id: None,
        model: None,
        reasoning_effort: None,
        instructions: Some("Private seat instructions".into()),
        message: "Current task".into(),
        first_message: true,
    }
}

#[tokio::test]
async fn cli_bridge_preserves_native_identity_and_prompt() {
    for agent in [AgentKind::Claude, AgentKind::Agy] {
        let directory = tempfile::tempdir().unwrap();
        let mut invocation = invocation(
            directory.path(),
            agent,
            r#"
printf '%s\n' "$@" > arguments
printf '%s\n' '{"session_id":"native-session","result":"Final answer"}'
"#,
        );
        let output = run(invocation.clone()).await;
        assert_eq!(
            output.observed_session_id.as_deref(),
            Some("native-session")
        );
        assert_eq!(output.answer.as_deref(), Some("Final answer"));
        assert!(output.error.is_none(), "{output:?}");
        let arguments = std::fs::read_to_string(directory.path().join("arguments")).unwrap();
        assert!(arguments.contains("Private seat instructions\n\nCurrent task"));
        invocation.first_message = false;
        invocation.native_session_id = output.observed_session_id;
        invocation.message = "Follow-up task".into();
        let output = run(invocation).await;
        assert!(output.error.is_none(), "{output:?}");
        assert_eq!(
            output.observed_session_id.as_deref(),
            Some("native-session")
        );
        let arguments = std::fs::read_to_string(directory.path().join("arguments")).unwrap();
        assert!(arguments.contains("Private seat instructions\n\nFollow-up task"));
        assert!(!arguments.contains("Current task"));
    }
}

#[tokio::test]
async fn cli_bridge_does_not_turn_a_failed_result_into_success() {
    let directory = tempfile::tempdir().unwrap();
    let invocation = invocation(
        directory.path(),
        AgentKind::Claude,
        r#"
printf '%s\n' '{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Partial answer"}]},"session_id":"failed-session"}'
printf '%s\n' '{"type":"result","is_error":true,"errors":["Model request failed"],"session_id":"failed-session"}'
"#,
    );
    let output = run(invocation).await;
    assert_eq!(
        output.observed_session_id.as_deref(),
        Some("failed-session")
    );
    assert!(
        output
            .error
            .as_deref()
            .is_some_and(|error| error.contains("Model request failed")),
        "{output:?}"
    );
}

#[tokio::test]
async fn codex_bridge_correlates_turns_and_uses_terminal_status() {
    for failed in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let terminal = if failed { "failed" } else { "completed" };
        let script = r#"
read -r line
printf '%s\n' '{"id":1,"result":{}}'
read -r line
read -r line
printf '%s\n' '{"id":2,"result":{"thread":{"id":"native-thread"}}}'
read -r line
printf '%s\n' "$line" > turn-request
printf '%s\n' '{"id":3,"result":{"turn":{"id":"active-turn"}}}'
printf '%s\n' '{"method":"item/completed","params":{"threadId":"native-thread","turnId":"active-turn","item":{"type":"agentMessage","phase":"commentary","text":"Working"}}}'
printf '%s\n' '{"method":"item/completed","params":{"threadId":"native-thread","turnId":"old-turn","item":{"type":"agentMessage","text":"Wrong answer"}}}'
printf '%s\n' '{"method":"item/completed","params":{"threadId":"native-thread","turnId":"active-turn","item":{"type":"agentMessage","phase":"final_answer","text":"Correct answer"}}}'
printf '%s\n' '{"method":"turn/completed","params":{"threadId":"native-thread","turn":{"id":"active-turn","status":"TERMINAL","error":{"message":"Tool execution failed"},"items":[]}}}'
read -r line
printf '%s\n' '{"id":4,"result":{"status":"unsubscribed"}}'
"#.replace("TERMINAL", terminal);
        let mut invocation = invocation(directory.path(), AgentKind::Codex, &script);
        invocation.model = Some("test-model".into());
        invocation.reasoning_effort = Some("high".into());
        let output = run(invocation).await;
        assert_eq!(output.observed_session_id.as_deref(), Some("native-thread"));
        if failed {
            assert!(
                output
                    .error
                    .as_deref()
                    .is_some_and(|error| error.contains("Tool execution failed")),
                "{output:?}"
            );
        } else {
            assert_eq!(output.answer.as_deref(), Some("Correct answer"));
            assert!(output.error.is_none(), "{output:?}");
        }
        let request: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(directory.path().join("turn-request")).unwrap(),
        )
        .unwrap();
        assert_eq!(request["params"]["model"], "test-model");
        assert_eq!(request["params"]["effort"], "high");
        assert_eq!(request["params"]["approvalPolicy"], "never");
        assert_eq!(
            request["params"]["sandboxPolicy"]["type"],
            "dangerFullAccess"
        );
    }
}

#[tokio::test]
async fn completed_transport_reaps_a_child_that_ignores_eof() {
    let mut child = tokio::process::Command::new("sh")
        .args(["-c", "exec sleep 30"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    assert!(super::reap(&mut child).await.unwrap().is_none());
    assert!(child.try_wait().unwrap().is_some());
}

#[tokio::test(start_paused = true)]
async fn stderr_keeps_error_tail_without_waiting_for_inherited_writer() {
    use tokio::io::AsyncWriteExt;
    let (mut writer, reader) = tokio::io::duplex(1024);
    let capture = super::StderrCapture::start(reader);
    let mut message = vec![b'x'; 100000];
    message.extend_from_slice(b"FINAL_ERROR_DETAIL");
    writer.write_all(&message).await.unwrap();
    let captured = tokio::time::timeout(std::time::Duration::from_secs(2), capture.finish())
        .await
        .unwrap();
    assert!(!captured.is_empty() && captured.len() < message.len());
    assert!(captured.ends_with(b"FINAL_ERROR_DETAIL"));
    assert!(writer.write_all(b"late").await.is_err());
}
