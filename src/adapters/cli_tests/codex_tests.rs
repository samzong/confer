use super::*;

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
async fn codex_final_answer_prevents_cancellation_during_unsubscribe() {
    let directory = tempfile::tempdir().unwrap();
    let invocation = invocation(
        directory.path(),
        AgentKind::Codex,
        r#"
read -r line
printf '%s\n' '{"id":1,"result":{}}'
read -r line
read -r line
printf '%s\n' '{"id":2,"result":{"thread":{"id":"native-thread"}}}'
read -r line
printf '%s\n' '{"id":3,"result":{"turn":{"id":"active-turn"}}}'
printf '%s\n' '{"method":"turn/completed","params":{"threadId":"native-thread","turn":{"id":"active-turn","status":"completed","items":[{"type":"agentMessage","phase":"final_answer","text":"complete answer"}]}}}'
read -r line
printf '%s' ready > unsubscribing
sleep 1
printf '%s\n' '{"id":4,"result":{}}'
"#,
    );
    let cancellation = invocation.cancellation.clone();
    let task = tokio::spawn(run(invocation));
    wait_for_file(&directory.path().join("unsubscribing")).await;
    assert!(!task.is_finished());
    cancellation.cancel();
    let output = task.await.unwrap();
    assert!(!cancellation.is_cancelled());
    assert_eq!(output.answer.as_deref(), Some("complete answer"));
    assert!(output.error.is_none(), "{output:?}");
}

#[tokio::test]
async fn codex_cancellation_interrupts_the_active_turn() {
    let directory = tempfile::tempdir().unwrap();
    let invocation = invocation(
        directory.path(),
        AgentKind::Codex,
        r#"
read -r line
printf '%s\n' '{"id":1,"result":{}}'
read -r line
read -r line
printf '%s\n' '{"id":2,"result":{"thread":{"id":"native-thread"}}}'
read -r line
printf '%s\n' '{"id":3,"result":{"turn":{"id":"active-turn"}}}'
printf '%s' ready > turn-started
read -r line
printf '%s\n' "$line" > interrupt-request
printf '%s\n' '{"id":4,"result":{}}'
printf '%s\n' '{"method":"turn/completed","params":{"threadId":"native-thread","turn":{"id":"active-turn","status":"interrupted","items":[]}}}'
"#,
    );
    let cancellation = invocation.cancellation.clone();
    let task = tokio::spawn(run(invocation));
    wait_for_file(&directory.path().join("turn-started")).await;
    cancellation.cancel();
    let output = tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    let request: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(directory.path().join("interrupt-request")).unwrap(),
    )
    .unwrap();
    assert_eq!(request["method"], "turn/interrupt");
    assert_eq!(request["params"]["threadId"], "native-thread");
    assert_eq!(request["params"]["turnId"], "active-turn");
    assert_eq!(output.observed_session_id.as_deref(), Some("native-thread"));
    assert!(
        output
            .error
            .as_deref()
            .is_some_and(|error| error.contains("interrupted")),
        "{output:?}"
    );
}
