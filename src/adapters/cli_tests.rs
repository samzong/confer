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
        cancellation: Default::default(),
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
        assert!(arguments.contains(super::PROMPT_PREAMBLE));
        assert!(arguments.contains("Private seat instructions\n\nCurrent task"));
        invocation.first_message = false;
        invocation.native_session_id = output.observed_session_id;
        invocation.message = "Follow-up task".into();
        let cancellation = invocation.cancellation.clone();
        let output = run(invocation).await;
        cancellation.cancel();
        assert!(!cancellation.is_cancelled());
        assert!(output.error.is_none(), "{output:?}");
        assert_eq!(
            output.observed_session_id.as_deref(),
            Some("native-session")
        );
        let arguments = std::fs::read_to_string(directory.path().join("arguments")).unwrap();
        assert!(arguments.contains(super::PROMPT_PREAMBLE));
        assert!(arguments.contains("Private seat instructions\n\nFollow-up task"));
        assert!(!arguments.contains("Current task"));
    }
}

#[tokio::test]
async fn native_git_uses_the_invocation_workspace() {
    if std::env::var_os("CONFER_TEST_NATIVE_GIT").is_none() {
        let foreign = tempfile::tempdir().unwrap();
        let initialized = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .arg(foreign.path())
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .status()
            .unwrap();
        assert!(initialized.success());
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "adapters::cli_tests::native_git_uses_the_invocation_workspace",
                "--nocapture",
            ])
            .env("CONFER_TEST_NATIVE_GIT", "1")
            .env("GIT_DIR", foreign.path().join(".git"))
            .env("GIT_WORK_TREE", foreign.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("NATIVE_GIT_SCOPE_VERIFIED"));
        return;
    }
    assert!(std::env::var_os("GIT_DIR").is_some());
    assert!(std::env::var_os("GIT_WORK_TREE").is_some());
    for agent in AgentKind::ALL {
        let directory = tempfile::tempdir().unwrap();
        let initialized = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .arg(directory.path())
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .status()
            .unwrap();
        assert!(initialized.success());
        let invocation = invocation(
            directory.path(),
            agent,
            "git rev-parse --show-toplevel > observed-root\nexit 1",
        );
        let output = run(invocation).await;
        assert!(output.error.is_some());
        let root = std::fs::read_to_string(directory.path().join("observed-root")).unwrap();
        assert_eq!(
            Path::new(root.trim()),
            directory.path().canonicalize().unwrap(),
            "{}",
            agent.id()
        );
    }
    println!("NATIVE_GIT_SCOPE_VERIFIED");
}

#[tokio::test]
async fn devin_bridge_reads_session_and_answer_from_atif_export() {
    let directory = tempfile::tempdir().unwrap();
    let mut invocation = invocation(
        directory.path(),
        AgentKind::Devin,
        r#"
printf '%s\n' "$@" > arguments
export_path=""
prompt_path=""
resumed=0
while [ $# -gt 0 ]; do
  if [ "$1" = "--export" ]; then export_path="$2"; fi
  if [ "$1" = "--prompt-file" ]; then prompt_path="$2"; fi
  case "$1" in --resume=*) resumed=1 ;; esac
  shift
done
cat "$prompt_path" > captured-prompt
if [ "$resumed" = 1 ]; then
  printf '%s' '{"schema_version":"ATIF-v1.7","session_id":"devin-session","agent":{"name":"devin"},"steps":[{"source":"user","message":"task"},{"source":"agent","message":"Stale prior-turn answer"},{"source":"user","message":"follow-up"},{"source":"agent","tool_calls":[]}]}' > "$export_path"
  printf '%s\n' 'Follow-up answer'
else
  printf '%s' '{"schema_version":"ATIF-v1.7","session_id":"devin-session","agent":{"name":"devin"},"steps":[{"source":"user","message":"task"},{"source":"agent","message":"Working"},{"source":"agent","message":"Final answer"}]}' > "$export_path"
fi
"#,
    );
    let output = run(invocation.clone()).await;
    assert_eq!(output.observed_session_id.as_deref(), Some("devin-session"));
    assert_eq!(output.answer.as_deref(), Some("Final answer"));
    assert!(output.error.is_none(), "{output:?}");
    let arguments = std::fs::read_to_string(directory.path().join("arguments")).unwrap();
    assert!(arguments.contains("--prompt-file"));
    let prompt = std::fs::read_to_string(directory.path().join("captured-prompt")).unwrap();
    assert!(prompt.contains("Private seat instructions\n\nCurrent task"));

    invocation.first_message = false;
    invocation.native_session_id = output.observed_session_id;
    invocation.message = "Follow-up task".into();
    let output = run(invocation).await;
    assert!(output.error.is_none(), "{output:?}");
    assert_eq!(output.observed_session_id.as_deref(), Some("devin-session"));
    // The resumed export has no current-turn agent message; the answer falls
    // back to this turn's stdout rather than a stale agent step.
    assert_eq!(output.answer.as_deref(), Some("Follow-up answer"));
    let arguments = std::fs::read_to_string(directory.path().join("arguments")).unwrap();
    assert!(arguments.contains("--resume=devin-session"));
    let prompt = std::fs::read_to_string(directory.path().join("captured-prompt")).unwrap();
    assert!(prompt.contains("Private seat instructions\n\nFollow-up task"));
}

#[tokio::test]
async fn devin_bridge_falls_back_to_stdout_for_current_turn() {
    let directory = tempfile::tempdir().unwrap();
    let invocation = invocation(
        directory.path(),
        AgentKind::Devin,
        r#"
export_path=""
while [ $# -gt 0 ]; do
  if [ "$1" = "--export" ]; then export_path="$2"; fi
  shift
done
printf '%s' '{"session_id":"devin-session","steps":[{"source":"user","message":"task"},{"source":"agent","tool_calls":[]}]}' > "$export_path"
printf '%s\n' 'Printed answer'
"#,
    );
    let output = run(invocation).await;
    assert_eq!(output.answer.as_deref(), Some("Printed answer"));
    assert_eq!(output.observed_session_id.as_deref(), Some("devin-session"));
    assert!(output.error.is_none(), "{output:?}");
}

#[tokio::test]
async fn devin_bridge_keeps_the_seat_session_when_a_resumed_export_omits_it() {
    let directory = tempfile::tempdir().unwrap();
    let mut invocation = invocation(
        directory.path(),
        AgentKind::Devin,
        r#"
export_path=""
while [ $# -gt 0 ]; do
  if [ "$1" = "--export" ]; then export_path="$2"; fi
  shift
done
printf '%s' '{"steps":[{"source":"user","message":"follow-up"},{"source":"agent","message":"Resumed answer"}]}' > "$export_path"
"#,
    );
    invocation.first_message = false;
    invocation.native_session_id = Some("devin-session".into());
    let output = run(invocation).await;
    assert_eq!(output.observed_session_id.as_deref(), Some("devin-session"));
    assert_eq!(output.answer.as_deref(), Some("Resumed answer"));
    assert!(output.error.is_none(), "{output:?}");
}

#[tokio::test]
async fn devin_bridge_fails_a_resumed_export_with_a_different_session() {
    let directory = tempfile::tempdir().unwrap();
    let mut invocation = invocation(
        directory.path(),
        AgentKind::Devin,
        r#"
export_path=""
while [ $# -gt 0 ]; do
  if [ "$1" = "--export" ]; then export_path="$2"; fi
  shift
done
printf '%s' '{"session_id":"other-session","steps":[{"source":"user","message":"follow-up"},{"source":"agent","message":"Resumed answer"}]}' > "$export_path"
"#,
    );
    invocation.first_message = false;
    invocation.native_session_id = Some("devin-session".into());
    let output = run(invocation).await;
    assert_eq!(output.observed_session_id.as_deref(), Some("devin-session"));
    assert!(output.answer.is_none());
    assert!(
        output
            .error
            .as_deref()
            .is_some_and(|error| error.contains("devin-session") && error.contains("other-session")),
        "{output:?}"
    );
}

#[tokio::test]
async fn devin_bridge_preserves_the_session_after_a_failed_exit() {
    let directory = tempfile::tempdir().unwrap();
    let invocation = invocation(
        directory.path(),
        AgentKind::Devin,
        r#"
export_path=""
while [ $# -gt 0 ]; do
  if [ "$1" = "--export" ]; then export_path="$2"; fi
  shift
done
printf '%s' '{"session_id":"devin-session","steps":[{"source":"user","message":"task"}]}' > "$export_path"
printf '%s\n' 'boom' >&2
exit 1
"#,
    );
    let output = run(invocation).await;
    assert_eq!(output.observed_session_id.as_deref(), Some("devin-session"));
    assert!(output.answer.is_none());
    assert!(output.error.is_some(), "{output:?}");
}

#[tokio::test]
async fn devin_bridge_fails_without_a_session_in_the_export() {
    let directory = tempfile::tempdir().unwrap();
    let invocation = invocation(
        directory.path(),
        AgentKind::Devin,
        "printf '%s\\n' 'Printed answer'\n",
    );
    let output = run(invocation).await;
    assert!(output.answer.is_none());
    assert!(output.observed_session_id.is_none());
    assert!(
        output
            .error
            .as_deref()
            .is_some_and(|error| error.contains("session id")),
        "{output:?}"
    );
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

async fn wait_for_file(path: &Path) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !path.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn cli_cancellation_terminates_the_process_and_preserves_observed_identity() {
    let directory = tempfile::tempdir().unwrap();
    let invocation = invocation(
        directory.path(),
        AgentKind::Claude,
        r#"
printf '%s\n' '{"session_id":"cancelled-session","type":"system"}'
python3 -c 'import os,subprocess; os.setsid(); subprocess.Popen(["sh","-c","sleep 1; printf leaked > late-write"]); open("tool-started","w").write("ready"); import time; time.sleep(60)' &
while [ ! -f tool-started ]; do sleep 0.01; done
printf '%s' "$$" > child-pid
exec sleep 60
"#,
    );
    let cancellation = invocation.cancellation.clone();
    let task = tokio::spawn(run(invocation));
    wait_for_file(&directory.path().join("child-pid")).await;
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    cancellation.cancel();
    let output = tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        output.observed_session_id.as_deref(),
        Some("cancelled-session")
    );
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    assert!(!directory.path().join("late-write").exists());
    let pid = std::fs::read_to_string(directory.path().join("child-pid")).unwrap();
    assert!(
        !std::process::Command::new("kill")
            .args(["-0", &pid])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    );
}

#[tokio::test]
async fn native_cancellation_kills_an_agent_that_ignores_cancel_after_the_grace() {
    let directory = tempfile::tempdir().unwrap();
    let invocation = invocation(directory.path(), AgentKind::Cursor, "exit 1");
    let script = r#"#!/usr/bin/env python3
import json,os,sys,subprocess
open('child-pid','w').write(str(os.getpid()))
for line in sys.stdin:
    v=json.loads(line)
    if v.get('method')=='initialize':
        print(json.dumps({'jsonrpc':'2.0','id':v['id'],'result':{'protocolVersion':1}}),flush=True)
    elif v.get('method')=='session/new':
        print(json.dumps({'jsonrpc':'2.0','id':v['id'],'result':{'sessionId':'native-session'}}),flush=True)
    elif v.get('method')=='session/prompt':
        subprocess.Popen(['/bin/sh','-c','sleep 4; printf leaked > late-write'])
        open('prompt-started','w').write('yes')
    elif v.get('method')=='session/cancel':
        open('cancel-seen','w').write(v['params']['sessionId'])
"#;
    std::fs::write(&invocation.executable, script).unwrap();
    let cancellation = invocation.cancellation.clone();
    let task = tokio::spawn(run(invocation));
    wait_for_file(&directory.path().join("prompt-started")).await;
    let start = std::time::Instant::now();
    cancellation.cancel();
    let output = tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    assert!(start.elapsed() >= std::time::Duration::from_secs(3));
    assert_eq!(
        std::fs::read_to_string(directory.path().join("cancel-seen")).unwrap(),
        "native-session"
    );
    assert_eq!(
        output.observed_session_id.as_deref(),
        Some("native-session")
    );
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    assert!(!directory.path().join("late-write").exists());
    let pid = std::fs::read_to_string(directory.path().join("child-pid")).unwrap();
    assert!(
        !std::process::Command::new("kill")
            .args(["-0", &pid])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    );
}

#[tokio::test]
async fn cli_final_result_prevents_cancellation_during_reaping() {
    let directory = tempfile::tempdir().unwrap();
    let invocation = invocation(
        directory.path(),
        AgentKind::Claude,
        r#"
printf '%s\n' '{"session_id":"finished-session","type":"result","result":"complete answer"}'
sleep 1
"#,
    );
    let cancellation = invocation.cancellation.clone();
    let task = tokio::spawn(run(invocation));
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !cancellation.is_finished() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(!task.is_finished());
    cancellation.cancel();
    let output = task.await.unwrap();
    assert!(!cancellation.is_cancelled());
    assert_eq!(output.answer.as_deref(), Some("complete answer"));
    assert!(output.error.is_none(), "{output:?}");
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

#[tokio::test]
async fn acknowledged_native_cancellation_stops_separate_group_tools() {
    let directory = tempfile::tempdir().unwrap();
    let invocation = invocation(directory.path(), AgentKind::Cursor, "exit 1");
    std::fs::write(&invocation.executable, r#"#!/usr/bin/env python3
import json,sys,subprocess
pending=None
for line in sys.stdin:
    v=json.loads(line)
    if v.get('method')=='initialize':
        print(json.dumps({'jsonrpc':'2.0','id':v['id'],'result':{'protocolVersion':1}}),flush=True)
    elif v.get('method')=='session/new':
        print(json.dumps({'jsonrpc':'2.0','id':v['id'],'result':{'sessionId':'native-session'}}),flush=True)
    elif v.get('method')=='session/prompt':
        pending=v['id']
        subprocess.Popen(['/bin/sh','-c','sleep 1.5; printf leaked > late-write'],start_new_session=True)
        open('prompt-started','w').write('yes')
    elif v.get('method')=='session/cancel':
        open('cancel-acknowledged','w').write('yes')
        print(json.dumps({'jsonrpc':'2.0','id':pending,'result':{'stopReason':'cancelled'}}),flush=True)
"#).unwrap();
    let cancellation = invocation.cancellation.clone();
    let task = tokio::spawn(run(invocation));
    wait_for_file(&directory.path().join("prompt-started")).await;
    cancellation.cancel();
    let output = tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    assert!(directory.path().join("cancel-acknowledged").exists());
    assert!(
        output
            .error
            .as_deref()
            .is_some_and(|error| error.contains("Cancelled")),
        "{output:?}"
    );
    assert_eq!(
        output.observed_session_id.as_deref(),
        Some("native-session")
    );
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    assert!(!directory.path().join("late-write").exists());
}

#[tokio::test]
async fn cli_final_result_waits_for_natural_exit_and_cleanup() {
    for close_stdout in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let script = format!(
            "printf '%s\\n' '{{\"session_id\":\"finished-session\",\"type\":\"result\",\"result\":\"complete answer\"}}'\n{}\nsleep 4\nprintf cleaned > cleanup-finished",
            if close_stdout { "exec 1>&-" } else { "" }
        );
        let invocation = invocation(directory.path(), AgentKind::Claude, &script);
        let output = run(invocation).await;
        assert_eq!(output.answer.as_deref(), Some("complete answer"));
        assert!(output.error.is_none(), "{output:?}");
        assert_eq!(
            std::fs::read_to_string(directory.path().join("cleanup-finished")).unwrap(),
            "cleaned"
        );
    }
}

#[tokio::test]
async fn devin_stdout_eof_prevents_cancellation_during_cleanup() {
    let directory = tempfile::tempdir().unwrap();
    let mut invocation = invocation(
        directory.path(),
        AgentKind::Devin,
        r#"
while [ $# -gt 0 ]; do
  if [ "$1" = "--export" ]; then export_path="$2"; fi
  shift
done
printf '%s' '{"session_id":"devin-session","steps":[]}' > "$export_path"
printf '%s\n' 'Printed answer'
exec 1>&-
sleep 1
printf cleaned > cleanup-finished
"#,
    );
    invocation.native_session_id = Some("devin-session".into());
    invocation.first_message = false;
    let cancellation = invocation.cancellation.clone();
    let task = tokio::spawn(run(invocation));
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !cancellation.is_finished() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(!task.is_finished());
    cancellation.cancel();
    let output = task.await.unwrap();
    assert!(!cancellation.is_cancelled());
    assert_eq!(output.answer.as_deref(), Some("Printed answer"));
    assert!(output.error.is_none(), "{output:?}");
    assert!(directory.path().join("cleanup-finished").exists());
}

#[tokio::test]
async fn uncancelled_reap_preserves_descendant_cleanup() {
    let directory = tempfile::tempdir().unwrap();
    let mut child = tokio::process::Command::new("python3")
        .args(["-c", r#"
import subprocess,sys,time
subprocess.Popen([sys.executable,'-c',"import pathlib,sys,time; time.sleep(4); path=pathlib.Path(sys.argv[1]); temporary=path.with_suffix('.tmp'); temporary.write_text('done'); temporary.replace(path)",sys.argv[1]],start_new_session=True,stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
open(sys.argv[2],'w').write('ready')
time.sleep(30)
"#])
        .arg(directory.path().join("cleanup-finished"))
        .arg(directory.path().join("ready"))
        .process_group(0)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn().unwrap();
    wait_for_file(&directory.path().join("ready")).await;
    let cancellation = super::Cancellation::default();
    assert!(
        super::process::reap_with_cancellation(&mut child, &cancellation)
            .await
            .unwrap()
            .is_none()
    );
    assert!(child.try_wait().unwrap().is_some());
    wait_for_file(&directory.path().join("cleanup-finished")).await;
    assert_eq!(
        std::fs::read_to_string(directory.path().join("cleanup-finished")).unwrap(),
        "done"
    );
}

#[tokio::test]
async fn cancellation_kills_same_group_children_created_after_scan() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let directory = tempfile::tempdir().unwrap();
    let mut child = tokio::process::Command::new("python3")
        .args(["-c", r#"
import signal,subprocess,sys
sentinel=subprocess.Popen([sys.executable,'-c','import time; time.sleep(60)'],stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
print('ready',flush=True)
assert sentinel.wait()==-signal.SIGKILL
print('scanned',flush=True)
assert sys.stdin.readline().strip()=='spawn'
subprocess.Popen([sys.executable,'-c',"import sys,time; time.sleep(1); open(sys.argv[1],'w').write('leaked')",sys.argv[1]],stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
print('spawned',flush=True)
"#])
        .arg(directory.path().join("late-write"))
        .process_group(0)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn().unwrap();
    let cancellation = super::Cancellation::default();
    cancellation.set_process_id(child.id().unwrap());
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap()).lines();
    assert_eq!(output.next_line().await.unwrap().as_deref(), Some("ready"));
    cancellation.cancel();
    let task = tokio::spawn(async move {
        let status = super::process::reap_with_cancellation(&mut child, &cancellation).await;
        (status, child)
    });
    let scanned = tokio::time::timeout(std::time::Duration::from_secs(2), output.next_line())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(scanned.as_deref(), Some("scanned"));
    input.write_all(b"spawn\n").await.unwrap();
    let spawned = tokio::time::timeout(std::time::Duration::from_secs(2), output.next_line())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(spawned.as_deref(), Some("spawned"));
    let (status, mut child) = tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
    assert!(status.unwrap().unwrap().success());
    assert!(child.try_wait().unwrap().is_some());
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    assert!(!directory.path().join("late-write").exists());
}
