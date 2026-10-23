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

fn recorded_requests(directory: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(directory.join("requests"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn methods(requests: &[serde_json::Value]) -> Vec<&str> {
    requests
        .iter()
        .map(|request| request["method"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn codex_resume_unarchives_only_an_archived_thread() {
    for archived in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let first_resume = if archived {
            r#"{"id":2,"error":{"code":-32600,"message":"session native-thread is archived"}}"#
        } else {
            r#"{"id":2,"result":{"thread":{"id":"native-thread"}}}"#
        };
        let unarchive = if archived {
            r#"
read -r line
printf '%s\n' "$line" >> requests
printf '%s\n' '{"id":3,"result":{"thread":{"id":"native-thread"}}}'
read -r line
printf '%s\n' "$line" >> requests
printf '%s\n' '{"id":4,"result":{"thread":{"id":"native-thread"}}}'
"#
        } else {
            ""
        };
        let next = if archived { 5 } else { 3 };
        let script = format!(
            r#"
read -r line
printf '%s\n' "$line" >> requests
printf '%s\n' '{{"id":1,"result":{{}}}}'
read -r line
printf '%s\n' "$line" >> requests
read -r line
printf '%s\n' "$line" >> requests
printf '%s\n' '{first_resume}'
{unarchive}
read -r line
printf '%s\n' "$line" >> requests
printf '%s\n' '{{"id":{next},"result":{{"turn":{{"id":"active-turn"}}}}}}'
printf '%s\n' '{{"method":"turn/completed","params":{{"threadId":"native-thread","turn":{{"id":"active-turn","status":"completed","items":[{{"type":"agentMessage","phase":"final_answer","text":"resumed answer"}}]}}}}}}'
read -r line
printf '%s\n' "$line" >> requests
printf '%s\n' '{{"id":{after},"result":{{}}}}'
"#,
            after = next + 1
        );
        let mut invocation = invocation(directory.path(), AgentKind::Codex, &script);
        invocation.first_message = false;
        invocation.native_session_id = Some("native-thread".into());
        let output = run(invocation).await;
        assert!(output.error.is_none(), "{output:?}");
        assert_eq!(output.answer.as_deref(), Some("resumed answer"));
        let requests = recorded_requests(directory.path());
        let expected = if archived {
            vec![
                "initialize",
                "initialized",
                "thread/resume",
                "thread/unarchive",
                "thread/resume",
                "turn/start",
                "thread/unsubscribe",
            ]
        } else {
            vec![
                "initialize",
                "initialized",
                "thread/resume",
                "turn/start",
                "thread/unsubscribe",
            ]
        };
        assert_eq!(methods(&requests), expected);
        if archived {
            assert_eq!(requests[3]["params"]["threadId"], "native-thread");
            assert_eq!(requests[4]["params"], requests[2]["params"]);
        }
    }
}

#[tokio::test]
async fn codex_resume_failure_of_an_unarchived_thread_is_not_retried() {
    let directory = tempfile::tempdir().unwrap();
    let mut invocation = invocation(
        directory.path(),
        AgentKind::Codex,
        r#"
read -r line
printf '%s\n' "$line" >> requests
printf '%s\n' '{"id":1,"result":{}}'
read -r line
printf '%s\n' "$line" >> requests
read -r line
printf '%s\n' "$line" >> requests
printf '%s\n' '{"id":2,"error":{"code":-32600,"message":"no rollout found for thread id native-thread"}}'
read -r line
printf '%s\n' "$line" >> requests
printf '%s\n' '{"id":3,"error":{"code":-32600,"message":"no archived rollout found for thread id native-thread"}}'
read -r line && printf '%s\n' "$line" >> requests
"#,
    );
    invocation.first_message = false;
    invocation.native_session_id = Some("native-thread".into());
    let output = run(invocation).await;
    let error = output.error.as_deref().unwrap_or_default();
    assert!(
        error.contains("thread/resume: no rollout found")
            && error.contains("thread/unarchive: no archived rollout found"),
        "{output:?}"
    );
    assert_eq!(
        methods(&recorded_requests(directory.path())),
        [
            "initialize",
            "initialized",
            "thread/resume",
            "thread/unarchive"
        ]
    );
}

#[tokio::test]
async fn codex_archive_requests_the_native_thread_and_reports_rejection() {
    for rejected in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let response = if rejected {
            r#"{"id":2,"error":{"code":-32600,"message":"thread native-thread already has an active writer"}}"#
        } else {
            r#"{"id":2,"result":{}}"#
        };
        let invocation = invocation(
            directory.path(),
            AgentKind::Codex,
            &format!(
                r#"
read -r line
printf '%s\n' "$line" >> requests
printf '%s\n' '{{"id":1,"result":{{}}}}'
read -r line
printf '%s\n' "$line" >> requests
read -r line
printf '%s\n' "$line" >> requests
printf '%s\n' '{response}'
"#
            ),
        );
        let result = crate::adapters::codex::archive(
            &invocation.executable,
            directory.path(),
            "native-thread",
        )
        .await;
        let requests = recorded_requests(directory.path());
        assert_eq!(
            methods(&requests),
            ["initialize", "initialized", "thread/archive"]
        );
        assert_eq!(requests[2]["params"]["threadId"], "native-thread");
        if rejected {
            assert!(
                result
                    .as_ref()
                    .is_err_and(|error| error.to_string().contains("active writer")),
                "{result:?}"
            );
        } else {
            assert!(result.is_ok(), "{result:?}");
        }
    }
}
