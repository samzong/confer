use super::*;
use crate::mcp::api::{CancelDeliveryArgs, SendMessageArgs};
use crate::test_support::{room, seat};

#[tokio::test]
async fn runtime_cancellation_preserves_session_and_queue_order() {
    if std::env::var_os("CONFER_TEST_CANCEL_RUNTIME").is_none() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("opencode");
        std::fs::write(&executable, r#"#!/usr/bin/env python3
import json,sys,os,time,fcntl
active=open('agent-active','w')
try:
    fcntl.flock(active,fcntl.LOCK_EX|fcntl.LOCK_NB)
except BlockingIOError:
    open('adapter-overlap','w').write('yes')
pending=None
for line in sys.stdin:
    v=json.loads(line)
    method=v.get('method')
    result={}
    if method=='initialize':
        if os.path.exists('delay-next-initialize'):
            os.unlink('delay-next-initialize')
            open('initializing','w').write('yes')
            time.sleep(60)
        result={'protocolVersion':1,'agentCapabilities':{'loadSession':True,'sessionCapabilities':{'close':{}}}}
    elif method=='session/new':
        result={'sessionId':'retained-native-session'}
    elif method=='session/load':
        assert v['params']['sessionId']=='retained-native-session'
    elif method=='session/set_config_option':
        result={'configOptions':[]}
    elif method=='session/prompt':
        text=v['params']['prompt'][0]['text']
        with open('dispatched','a') as f:f.write(text+'\n')
        if 'long-task' in text or 'uncertain-queued' in text:
            pending=v['id']
            print(json.dumps({'jsonrpc':'2.0','method':'session/update','params':{'sessionId':'retained-native-session','update':{'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':'partial progress'}}}}),flush=True)
            open('started','w').write('yes')
            continue
        result={'stopReason':'end_turn'}
        print(json.dumps({'jsonrpc':'2.0','method':'session/update','params':{'sessionId':'retained-native-session','update':{'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':'continued'}}}}),flush=True)
    elif method=='session/close':
        if 'blocked-close' in text:
            open('close-blocked','w').write('yes')
            while not os.path.exists('release-close'):
                time.sleep(0.01)
        if 'answer-first' in text:
            open('closing','w').write('yes')
            time.sleep(0.5)
    elif method=='session/cancel':
        if 'uncertain-queued' in text:
            open('cancel-received','w').write('yes')
            while not os.path.exists('release-cancel'):
                time.sleep(0.01)
        print(json.dumps({'jsonrpc':'2.0','id':pending,'result':{'stopReason':'cancelled'}}),flush=True)
        continue
    if 'id' in v:
        print(json.dumps({'jsonrpc':'2.0','id':v['id'],'result':result}),flush=True)
"#).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = std::env::join_paths(
            std::iter::once(directory.path().to_owned())
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "mcp::delivery::runtime_tests::runtime_cancellation_preserves_session_and_queue_order",
                "--nocapture",
            ])
            .env("CONFER_TEST_CANCEL_RUNTIME", directory.path())
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
    let directory =
        std::path::PathBuf::from(std::env::var_os("CONFER_TEST_CANCEL_RUNTIME").unwrap());
    let workspace = directory.canonicalize().unwrap();
    let store = StateStore::new(directory.join("rooms.json"));
    store
        .mutate(|state| {
            let mut record = room("room-1", &workspace.to_string_lossy());
            record
                .seats
                .push(seat("seat-1", "worker", AgentKind::Opencode));
            state.rooms.push(record);
            Ok(())
        })
        .unwrap();
    let lease = store.try_acquire_seat_lease("room-1", "seat-1").unwrap();
    crate::state::mark_seat_dispatch(&lease, "crashed-before-native-id").unwrap();
    drop(lease);
    let server = ConferMcp::with_store(store.clone());
    let send = |message: &str| SendMessageArgs {
        workspace: workspace.clone(),
        room_id: "room-1".into(),
        recipients: vec!["worker".into()],
        message: message.into(),
    };
    let receipt = server.send_message_inner(send("long-task")).await.unwrap();
    assert!(
        !receipt.deliveries[0].accepted,
        "uncertain work must block dispatch"
    );
    assert!(receipt.deliveries[0].previous_delivery_uncertain);
    assert_eq!(
        receipt.deliveries[0].uncertain_delivery_id.as_deref(),
        Some("crashed-before-native-id")
    );
    let clear = |id: &str| crate::mcp::api::ClearUncertainDeliveryArgs {
        workspace: workspace.clone(),
        room_id: "room-1".into(),
        seat: "worker".into(),
        delivery_id: id.into(),
    };
    assert!(
        server
            .clear_uncertain_delivery_inner(clear("wrong-id"))
            .is_err()
    );
    assert!(store.seat_dispatch_uncertain("room-1", "seat-1"));
    server
        .clear_uncertain_delivery_inner(clear("crashed-before-native-id"))
        .unwrap();
    assert!(!directory.join("dispatched").exists());
    let receipt = server.send_message_inner(send("long-task")).await.unwrap();
    assert!(receipt.deliveries[0].accepted);
    let running = receipt.deliveries[0].delivery_id.clone();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !directory.join("started").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let removed = server
        .send_message_inner(send("removed-task"))
        .await
        .unwrap()
        .deliveries[0]
        .delivery_id
        .clone();
    let kept = server
        .send_message_inner(send("kept-task"))
        .await
        .unwrap()
        .deliveries[0]
        .delivery_id
        .clone();
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        server.cancel_delivery_inner(CancelDeliveryArgs {
            workspace: workspace.clone(),
            room_id: "room-1".into(),
            delivery_ids: vec![running, removed],
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result.deliveries[0].status, DeliveryStatus::Cancelled);
    assert!(result.deliveries[0].answer_incomplete);
    assert_eq!(
        result.deliveries[0].final_answer.as_deref(),
        Some("partial progress")
    );
    assert_eq!(result.deliveries[1].status, DeliveryStatus::Cancelled);
    let result = server
        .wait_output_inner(WaitOutputArgs {
            workspace: workspace.clone(),
            room_id: "room-1".into(),
            delivery_ids: vec![kept],
            timeout_ms: Some(5000),
        })
        .await
        .unwrap();
    assert_eq!(result.deliveries[0].status, DeliveryStatus::Completed);
    let dispatched = std::fs::read_to_string(directory.join("dispatched")).unwrap();
    assert!(!dispatched.contains("removed-task"));
    assert!(dispatched.contains("kept-task"));
    let room = store.room_for_workspace("room-1", &workspace).unwrap();
    assert_eq!(
        room.seats[0].native_session_id.as_deref(),
        Some("retained-native-session")
    );
    assert_eq!(room.seats[0].status, SeatStatus::Active);
    assert!(!store.seat_dispatch_uncertain("room-1", "seat-1"));
    assert_eq!(
        store
            .try_acquire_seat_lease("room-1", "seat-1")
            .unwrap()
            .metadata()
            .unwrap()
            .len(),
        0
    );
    let finished = server
        .send_message_inner(send("answer-first"))
        .await
        .unwrap()
        .deliveries[0]
        .delivery_id
        .clone();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !directory.join("closing").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let result = server
        .cancel_delivery_inner(CancelDeliveryArgs {
            workspace: workspace.clone(),
            room_id: "room-1".into(),
            delivery_ids: vec![finished],
        })
        .await
        .unwrap();
    assert_eq!(result.deliveries[0].status, DeliveryStatus::Completed);
    assert!(!result.deliveries[0].answer_incomplete);
    assert_eq!(
        result.deliveries[0].final_answer.as_deref(),
        Some("continued")
    );

    store
        .mutate(|state| {
            state.rooms[0]
                .seats
                .push(seat("seat-2", "new-worker", AgentKind::Opencode));
            Ok(())
        })
        .unwrap();
    std::fs::write(directory.join("delay-next-initialize"), "yes").unwrap();
    let first_send = || {
        let mut args = send("first-without-id");
        args.recipients = vec!["new-worker".into()];
        args
    };
    let first = server
        .send_message_inner(first_send())
        .await
        .unwrap()
        .deliveries[0]
        .delivery_id
        .clone();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !directory.join("initializing").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let result = server
        .cancel_delivery_inner(CancelDeliveryArgs {
            workspace: workspace.clone(),
            room_id: "room-1".into(),
            delivery_ids: vec![first],
        })
        .await
        .unwrap();
    assert_eq!(result.deliveries[0].status, DeliveryStatus::Cancelled);
    assert!(result.deliveries[0].previous_delivery_uncertain);
    assert!(
        result.deliveries[0]
            .error
            .as_deref()
            .unwrap()
            .contains("no native session was established")
    );
    assert!(
        store
            .room_for_workspace("room-1", &workspace)
            .unwrap()
            .seats[1]
            .native_session_id
            .is_none()
    );
    let next = server.send_message_inner(first_send()).await.unwrap();
    assert!(!next.deliveries[0].accepted);
    assert!(next.deliveries[0].previous_delivery_uncertain);
    server
        .clear_uncertain_delivery_inner(crate::mcp::api::ClearUncertainDeliveryArgs {
            workspace: workspace.clone(),
            room_id: "room-1".into(),
            seat: "new-worker".into(),
            delivery_id: next.deliveries[0].uncertain_delivery_id.clone().unwrap(),
        })
        .unwrap();
    let next = server.send_message_inner(first_send()).await.unwrap();
    assert!(next.deliveries[0].accepted);
    let result = server
        .wait_output_inner(WaitOutputArgs {
            workspace: workspace.clone(),
            room_id: "room-1".into(),
            delivery_ids: vec![next.deliveries[0].delivery_id.clone()],
            timeout_ms: Some(5000),
        })
        .await
        .unwrap();
    assert_eq!(result.deliveries[0].status, DeliveryStatus::Completed);
    assert!(
        store
            .room_for_workspace("room-1", &workspace)
            .unwrap()
            .seats[1]
            .native_session_id
            .is_some()
    );

    let uncertain = server
        .send_message_inner(send("uncertain-queued"))
        .await
        .unwrap()
        .deliveries[0]
        .delivery_id
        .clone();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let text = std::fs::read_to_string(directory.join("dispatched")).unwrap();
            if text.contains("uncertain-queued") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let follower = server
        .send_message_inner(send("uncertain-follower"))
        .await
        .unwrap()
        .deliveries[0]
        .delivery_id
        .clone();
    let signal = server
        .runtime
        .deliveries
        .states
        .borrow()
        .get(&uncertain)
        .unwrap()
        .cancellation
        .clone();
    signal.mark_uncertain();
    let cancel = server.cancel_delivery_inner(CancelDeliveryArgs {
        workspace: workspace.clone(),
        room_id: "room-1".into(),
        delivery_ids: vec![uncertain.clone()],
    });
    tokio::pin!(cancel);
    let receive_cancel = async {
        while !directory.join("cancel-received").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };
    tokio::select! {
        result = &mut cancel => panic!("cancel returned before native acknowledgement: {result:?}"),
        received = tokio::time::timeout(Duration::from_secs(2), receive_cancel) => received.unwrap()
    }
    assert_eq!(
        server
            .runtime
            .deliveries
            .snapshots("room-1", std::slice::from_ref(&follower))
            .unwrap()[0]
            .status,
        DeliveryStatus::Queued
    );
    assert!(
        !std::fs::read_to_string(directory.join("dispatched"))
            .unwrap()
            .contains("uncertain-follower")
    );
    std::fs::write(directory.join("release-cancel"), "yes").unwrap();
    let cancelled = cancel.await.unwrap();
    assert_eq!(cancelled.deliveries[0].status, DeliveryStatus::Cancelled);
    assert!(cancelled.deliveries[0].previous_delivery_uncertain);
    let continued = server
        .wait_output_inner(WaitOutputArgs {
            workspace: workspace.clone(),
            room_id: "room-1".into(),
            delivery_ids: vec![follower],
            timeout_ms: Some(5000),
        })
        .await
        .unwrap();
    assert_eq!(continued.deliveries[0].status, DeliveryStatus::Failed);
    assert!(continued.deliveries[0].previous_delivery_uncertain);
    assert!(
        continued.deliveries[0]
            .resume_command
            .as_deref()
            .unwrap()
            .contains("retained-native-session")
    );
    assert!(
        !std::fs::read_to_string(directory.join("dispatched"))
            .unwrap()
            .contains("uncertain-follower")
    );
    assert!(store.seat_dispatch_uncertain("room-1", "seat-1"));
    server
        .clear_uncertain_delivery_inner(clear(&uncertain))
        .unwrap();
    assert!(!store.seat_dispatch_uncertain("room-1", "seat-1"));
    let third = server
        .send_message_inner(send("after-uncertain"))
        .await
        .unwrap();
    assert!(!third.deliveries[0].previous_delivery_uncertain);
    server
        .wait_output_inner(WaitOutputArgs {
            workspace: workspace.clone(),
            room_id: "room-1".into(),
            delivery_ids: vec![third.deliveries[0].delivery_id.clone()],
            timeout_ms: Some(5000),
        })
        .await
        .unwrap();

    let closing = server
        .send_message_inner(send("blocked-close"))
        .await
        .unwrap()
        .deliveries[0]
        .delivery_id
        .clone();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !directory.join("close-blocked").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let follower = server
        .send_message_inner(send("after-timeout"))
        .await
        .unwrap()
        .deliveries[0]
        .delivery_id
        .clone();
    tokio::time::pause();
    let cancel = server.cancel_delivery_inner(CancelDeliveryArgs {
        workspace: workspace.clone(),
        room_id: "room-1".into(),
        delivery_ids: vec![closing.clone()],
    });
    tokio::pin!(cancel);
    std::future::poll_fn(|cx| {
        assert!(std::future::Future::poll(cancel.as_mut(), cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    tokio::time::advance(Duration::from_secs(8)).await;
    let result = cancel.await.unwrap();
    tokio::time::resume();
    assert!(result.timed_out);
    assert_eq!(result.deliveries[0].status, DeliveryStatus::Running);
    assert!(result.deliveries[0].previous_delivery_uncertain);
    assert_eq!(
        server
            .runtime
            .deliveries
            .snapshots("room-1", std::slice::from_ref(&follower))
            .unwrap()[0]
            .status,
        DeliveryStatus::Queued
    );
    assert!(
        !std::fs::read_to_string(directory.join("dispatched"))
            .unwrap()
            .contains("after-timeout")
    );
    assert!(!directory.join("adapter-overlap").exists());
    let active = std::fs::File::open(directory.join("agent-active")).unwrap();
    assert!(active.try_lock().is_err());
    assert!(store.try_acquire_seat_lease("room-1", "seat-1").is_err());
    std::fs::write(directory.join("release-close"), "yes").unwrap();
    let finished = server
        .wait_output_inner(WaitOutputArgs {
            workspace: workspace.clone(),
            room_id: "room-1".into(),
            delivery_ids: vec![closing.clone(), follower],
            timeout_ms: Some(5000),
        })
        .await
        .unwrap();
    assert!(finished.deliveries[0].terminal());
    assert_eq!(finished.deliveries[1].status, DeliveryStatus::Failed);
    assert!(finished.deliveries[0].previous_delivery_uncertain);
    assert!(finished.deliveries[1].previous_delivery_uncertain);
    assert_eq!(
        store.uncertain_delivery_id("room-1", "seat-1").as_deref(),
        Some(closing.as_str())
    );
    assert!(
        !std::fs::read_to_string(directory.join("dispatched"))
            .unwrap()
            .contains("after-timeout")
    );
    server
        .clear_uncertain_delivery_inner(crate::mcp::api::ClearUncertainDeliveryArgs {
            workspace: workspace.clone(),
            room_id: "room-1".into(),
            seat: "worker".into(),
            delivery_id: closing,
        })
        .unwrap();
    let resumed = server
        .send_message_inner(send("after-timeout-explicit"))
        .await
        .unwrap();
    let finished = server
        .wait_output_inner(WaitOutputArgs {
            workspace: workspace.clone(),
            room_id: "room-1".into(),
            delivery_ids: vec![resumed.deliveries[0].delivery_id.clone()],
            timeout_ms: Some(5000),
        })
        .await
        .unwrap();
    assert_eq!(finished.deliveries[0].status, DeliveryStatus::Completed);
    assert!(!store.seat_dispatch_uncertain("room-1", "seat-1"));
    assert!(!directory.join("adapter-overlap").exists());
}
