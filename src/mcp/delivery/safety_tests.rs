use super::*;
use crate::mcp::api::{ClearUncertainDeliveryArgs, SendMessageArgs};
use crate::test_support::{room, seat};
use std::os::unix::fs::PermissionsExt;

#[tokio::test]
async fn observer_failure_with_uncertain_cleanup_blocks_next_dispatch() {
    if std::env::var_os("CONFER_TEST_OBSERVER_CLEANUP").is_none() {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("claude");
        std::fs::write(
            &executable,
            r#"#!/bin/sh
touch started
while ! test -f release; do sleep 0.01; done
printf '%s\n' '{"session_id":"unexpected-session","type":"system"}'
while :; do sleep 1; done
"#,
        )
        .unwrap();
        std::fs::set_permissions(executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = std::env::join_paths(
            std::iter::once(directory.path().to_owned())
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "mcp::delivery::safety_tests::observer_failure_with_uncertain_cleanup_blocks_next_dispatch", "--nocapture"])
            .env("CONFER_TEST_OBSERVER_CLEANUP", directory.path())
            .env("PATH", path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let directory = PathBuf::from(std::env::var_os("CONFER_TEST_OBSERVER_CLEANUP").unwrap());
    let workspace = directory.canonicalize().unwrap();
    let store = StateStore::new(directory.join("rooms.json"));
    store
        .mutate(|state| {
            let mut record = room("room-1", &workspace.to_string_lossy());
            let mut worker = seat("seat-1", "worker", AgentKind::Claude);
            worker.native_session_id = Some("expected-session".into());
            record.seats.push(worker);
            state.rooms.push(record);
            Ok(())
        })
        .unwrap();
    let server = ConferMcp::with_store(store.clone());
    let send = || SendMessageArgs {
        workspace: workspace.clone(),
        room_id: "room-1".into(),
        recipients: vec!["worker".into()],
        message: "work".into(),
    };
    let receipt = server.send_message_inner(send()).await.unwrap();
    let id = receipt.deliveries[0].delivery_id.clone();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !directory.join("started").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let signal = server
        .runtime
        .deliveries
        .snapshots("room-1", std::slice::from_ref(&id))
        .unwrap()[0]
        .cancellation
        .clone();
    signal.mark_uncertain();
    assert!(!signal.is_cancelled());
    std::fs::write(directory.join("release"), "yes").unwrap();
    let result = server
        .wait_output_inner(WaitOutputArgs {
            workspace: workspace.clone(),
            room_id: "room-1".into(),
            delivery_ids: vec![id.clone()],
            timeout_ms: Some(5000),
        })
        .await
        .unwrap();
    assert!(result.completed, "{result:?}");
    assert_eq!(result.deliveries[0].status, DeliveryStatus::Failed);
    assert!(
        result.deliveries[0]
            .error
            .as_deref()
            .unwrap()
            .contains("unexpected-session")
    );
    assert!(!signal.is_cancelled());
    assert!(
        result.deliveries[0].previous_delivery_uncertain,
        "{result:?}"
    );
    assert_eq!(
        store.uncertain_delivery_id("room-1", "seat-1").as_deref(),
        Some(id.as_str())
    );
    let refused = server.send_message_inner(send()).await.unwrap();
    assert!(!refused.deliveries[0].accepted);
    assert_eq!(
        refused.deliveries[0].uncertain_delivery_id.as_deref(),
        Some(id.as_str())
    );
}

#[tokio::test]
async fn native_identity_survives_process_exit_before_adapter_returns() {
    if let Some(directory) = std::env::var_os("CONFER_TEST_ID_CRASH") {
        let directory = PathBuf::from(directory);
        let workspace = directory.canonicalize().unwrap();
        let store = StateStore::new(directory.join("rooms.json"));
        let server = ConferMcp::with_store(store);
        server
            .send_message_inner(SendMessageArgs {
                workspace,
                room_id: "room-1".into(),
                recipients: vec!["worker".into()],
                message: "hold".into(),
            })
            .await
            .unwrap();
        std::future::pending::<()>().await;
        return;
    }
    for (agent, binary) in [
        (AgentKind::Codex, "codex"),
        (AgentKind::Opencode, "opencode"),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().canonicalize().unwrap();
        let store = StateStore::new(directory.path().join("rooms.json"));
        store
            .mutate(|state| {
                let mut record = room("room-1", &workspace.to_string_lossy());
                record.seats.push(seat("seat-1", "worker", agent));
                state.rooms.push(record);
                Ok(())
            })
            .unwrap();
        let executable = directory.path().join(binary);
        std::fs::write(
            &executable,
            r#"#!/usr/bin/env python3
import json,sys,os
for line in sys.stdin:
    value=json.loads(line)
    method=value.get('method')
    result={}
    if method=='initialize':
        result={'protocolVersion':1,'agentCapabilities':{'loadSession':True}}
    elif method=='thread/start':
        result={'thread':{'id':'surviving-native-session'}}
    elif method=='session/new':
        result={'sessionId':'surviving-native-session'}
    elif method=='session/set_config_option':
        result={'configOptions':[]}
    elif method in ('turn/start','session/prompt'):
        open('prompt-started','w').write('yes')
        continue
    if 'id' in value:
        print(json.dumps({'jsonrpc':'2.0','id':value['id'],'result':result}),flush=True)
"#,
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = std::env::join_paths(
            std::iter::once(directory.path().to_owned())
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "mcp::delivery::safety_tests::native_identity_survives_process_exit_before_adapter_returns", "--nocapture"])
            .env("CONFER_TEST_ID_CRASH", directory.path()).env("PATH", &path)
            .env("OPENAI_API_KEY", "fixture-not-a-credential")
            .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped()).spawn().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !directory.path().join("prompt-started").exists() {
            if std::time::Instant::now() >= deadline || child.try_wait().unwrap().is_some() {
                let _ = child.kill();
                panic!(
                    "{} did not reach the held native turn: {:?}",
                    agent.id(),
                    child.wait_with_output().unwrap()
                );
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let id_before_exit = store
            .room_for_workspace("room-1", &workspace)
            .unwrap()
            .seats[0]
            .native_session_id
            .clone();
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(
            id_before_exit.as_deref(),
            Some("surviving-native-session"),
            "{} lost the ID while adapter was running",
            agent.id()
        );
        let room = store.room_for_workspace("room-1", &workspace).unwrap();
        assert_eq!(
            room.seats[0].native_session_id.as_deref(),
            Some("surviving-native-session")
        );
        assert!(store.seat_dispatch_uncertain("room-1", "seat-1"));
        let server = ConferMcp::with_store(store.clone());
        let args = SendMessageArgs {
            workspace: workspace.clone(),
            room_id: "room-1".into(),
            recipients: vec!["worker".into()],
            message: "must not run".into(),
        };
        let receipt = server.send_message_inner(args).await.unwrap();
        assert!(!receipt.deliveries[0].accepted);
        assert!(
            receipt.deliveries[0]
                .resume_command
                .as_deref()
                .unwrap()
                .contains("surviving-native-session")
        );
        let clear = || ClearUncertainDeliveryArgs {
            workspace: workspace.clone(),
            room_id: "room-1".into(),
            seat: "worker".into(),
            delivery_id: receipt.deliveries[0].uncertain_delivery_id.clone().unwrap(),
        };
        let lease = store.try_acquire_seat_lease("room-1", "seat-1").unwrap();
        assert!(
            server
                .clear_uncertain_delivery_inner(clear())
                .unwrap_err()
                .to_string()
                .contains("seat_busy:")
        );
        drop(lease);
        let foreign = tempfile::tempdir().unwrap();
        let mut wrong = clear();
        wrong.workspace = foreign.path().to_path_buf();
        assert!(server.clear_uncertain_delivery_inner(wrong).is_err());
        server.clear_uncertain_delivery_inner(clear()).unwrap();
        assert!(!store.seat_dispatch_uncertain("room-1", "seat-1"));
        eprintln!(
            "{}: ID persisted before prompt reply; killed MCP process; restart blocked dispatch with resume command; checked clear succeeded",
            agent.id()
        );
    }
}
