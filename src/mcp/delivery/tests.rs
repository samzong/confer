use super::{
    DeliveryRuntime, DeliveryState, DeliveryStatus, deliveries_completed, finish_delivery,
    native_session_outcome, resolve_recipients,
};
use crate::mcp::ConferMcp;
use crate::mcp::api::WaitOutputArgs;
use crate::state::StateStore;
use crate::test_support::seat;
use crate::types::{AgentKind, SeatStatus};
use std::time::Duration;

use crate::test_support::room;

pub(super) fn delivery(id: &str, status: DeliveryStatus) -> DeliveryState {
    DeliveryState {
        delivery_id: id.into(),
        room_id: "room-1".into(),
        seat_id: "seat-1".into(),
        seat_name: "reviewer".into(),
        agent: AgentKind::Claude,
        status,
        final_answer: None,
        error: None,
        resume_command: None,
        answer_incomplete: false,
        previous_delivery_uncertain: false,
        question: None,
        uncertain_delivery_id: None,
        cancel_requested: false,
        cancellation: Default::default(),
    }
}

#[test]
fn empty_delivery_set_is_not_completed_work() {
    assert!(!deliveries_completed(&[]));
}

#[test]
fn queued_delivery_is_not_terminal() {
    let delivery = delivery("delivery-1", DeliveryStatus::Queued);

    assert!(!delivery.terminal());
    assert!(!deliveries_completed(&[delivery]));
}

#[tokio::test(start_paused = true)]
async fn wait_output_wakes_on_updates_and_honors_deadline() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().canonicalize().unwrap();
    let store = StateStore::new(directory.path().join("rooms.json"));
    store
        .mutate(|state| {
            state
                .rooms
                .push(room("room-1", &workspace.to_string_lossy()));
            Ok(())
        })
        .unwrap();
    let runtime = DeliveryRuntime::new();
    runtime
        .deliveries
        .insert(delivery("delivery-1", DeliveryStatus::Running));
    let server = ConferMcp {
        store,
        runtime: runtime.clone(),
        tool_router: ConferMcp::tool_router(),
    };
    let started = tokio::time::Instant::now();
    let waiting_server = server.clone();
    let waiting_workspace = workspace.clone();
    let waiter = tokio::spawn(async move {
        let output = waiting_server
            .wait_output_inner(WaitOutputArgs {
                workspace: waiting_workspace,
                room_id: "room-1".into(),
                delivery_ids: vec!["delivery-1".into()],
                timeout_ms: Some(1_000),
            })
            .await
            .unwrap();
        (started.elapsed(), output)
    });
    for _ in 0..10 {
        if runtime.deliveries.subscriber_count() == 1 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(runtime.deliveries.subscriber_count(), 1);

    runtime.deliveries.set_failed("delivery-1", "failed".into());
    let (elapsed, output) = waiter.await.unwrap();

    assert_eq!(elapsed, Duration::ZERO);
    assert!(output.completed);
    assert!(!output.timed_out);
    assert!(matches!(
        output.deliveries[0].status,
        DeliveryStatus::Failed
    ));

    runtime
        .deliveries
        .insert(delivery("delivery-2", DeliveryStatus::Running));
    let timeout_started = tokio::time::Instant::now();
    let output = server
        .wait_output_inner(WaitOutputArgs {
            workspace,
            room_id: "room-1".into(),
            delivery_ids: vec!["delivery-2".into()],
            timeout_ms: Some(125),
        })
        .await
        .unwrap();

    assert_eq!(timeout_started.elapsed(), Duration::from_millis(125));
    assert!(!output.completed);
    assert!(output.timed_out);
}

#[test]
fn retired_seat_is_not_addressable_or_broadcast() {
    let mut room = room("room-1", "/tmp/project");
    let mut retired = seat("retired", "retired", AgentKind::Grok);
    retired.native_session_id = Some("session-1".into());
    retired.status = SeatStatus::Retired;
    room.seats = vec![seat("active", "active", AgentKind::Claude), retired];

    assert_eq!(resolve_recipients(&room, &["*".into()]).unwrap().len(), 1);
    assert!(
        resolve_recipients(&room, &["retired".into()])
            .unwrap_err()
            .to_string()
            .contains("retired")
    );
    room.seats[0].status = SeatStatus::Retired;
    assert!(resolve_recipients(&room, &["*".into()]).is_err());
}

#[test]
fn observed_native_session_survives_a_failed_delivery() {
    let (mismatch, persistable) = native_session_outcome(None, Some("session-1"));
    assert!(mismatch.is_none());
    assert_eq!(persistable, Some("session-1"));

    let (mismatch, persistable) = native_session_outcome(Some("session-1"), Some("session-2"));
    assert!(mismatch.is_some());
    assert!(persistable.is_none());

    let (mismatch, persistable) = native_session_outcome(Some("session-1"), None);
    assert!(mismatch.is_none());
    assert!(persistable.is_none());
}

#[test]
fn completed_answer_survives_session_persistence_failure() {
    let mut delivery = delivery("delivery-1", DeliveryStatus::Running);

    finish_delivery(
        &mut delivery,
        None,
        Some("session persistence failed".into()),
        None,
        Some("completed review".into()),
    );

    assert!(matches!(delivery.status, DeliveryStatus::Failed));
    assert_eq!(delivery.final_answer.as_deref(), Some("completed review"));
    assert_eq!(
        delivery.error.as_deref(),
        Some("session persistence failed")
    );
}

#[test]
fn snapshots_preserve_requested_order_and_room_isolation() {
    let tracker = super::DeliveryTracker::new();
    for id in ["b", "a"] {
        tracker.insert(delivery(id, DeliveryStatus::Queued));
    }
    let mut other = delivery("c", DeliveryStatus::Completed);
    other.room_id = "other".into();
    tracker.insert(other);
    for requested in [vec![], vec!["b".into(), "a".into(), "b".into()]] {
        let expected = if requested.is_empty() {
            vec!["a", "b"]
        } else {
            vec!["b", "a", "b"]
        };
        let snapshots = tracker.snapshots("room-1", &requested).unwrap();
        assert_eq!(
            snapshots
                .iter()
                .map(|item| item.delivery_id.as_str())
                .collect::<Vec<_>>(),
            expected
        );
    }
    for id in ["c", "missing"] {
        assert!(tracker.snapshots("room-1", &[id.into()]).is_err());
    }
}

#[tokio::test]
async fn queued_cancellation_skips_dispatch_and_leaves_other_deliveries_untouched() {
    use super::{QueuedDelivery, run_seat_worker};
    use crate::mcp::api::CancelDeliveryArgs;
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().canonicalize().unwrap();
    let store = StateStore::new(directory.path().join("rooms.json"));
    store
        .mutate(|state| {
            let mut record = room("room-1", &workspace.to_string_lossy());
            record
                .seats
                .push(seat("seat-1", "worker", AgentKind::Claude));
            state.rooms.push(record);
            Ok(())
        })
        .unwrap();
    let server = ConferMcp::with_store(store.clone());
    let cancelled = delivery("cancel", DeliveryStatus::Queued);
    let cancellation = cancelled.cancellation.clone();
    server.runtime.deliveries.insert(cancelled);
    server
        .runtime
        .deliveries
        .insert(delivery("keep", DeliveryStatus::Completed));
    let result = server
        .cancel_delivery_inner(CancelDeliveryArgs {
            workspace: workspace.clone(),
            room_id: "room-1".into(),
            delivery_ids: vec!["cancel".into(), "keep".into(), "cancel".into()],
        })
        .await
        .unwrap();
    assert_eq!(result.deliveries[0].status, DeliveryStatus::Cancelled);
    assert_eq!(result.deliveries[1].status, DeliveryStatus::Completed);
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    sender
        .send(QueuedDelivery {
            delivery_id: "cancel".into(),
            room_id: "room-1".into(),
            seat_id: "seat-1".into(),
            message: "must never start".into(),
            workspace: workspace.clone(),
            cancellation,
        })
        .unwrap();
    drop(sender);
    let lease = store.try_acquire_seat_lease("room-1", "seat-1").unwrap();
    tokio::time::timeout(
        Duration::from_millis(100),
        run_seat_worker(
            receiver,
            store.clone(),
            server.runtime.deliveries.clone(),
            None,
        ),
    )
    .await
    .unwrap();
    assert_eq!(lease.metadata().unwrap().len(), 0);
    assert!(!store.seat_dispatch_uncertain("room-1", "seat-1"));
    for ids in [vec![], vec!["unknown".into()]] {
        assert!(
            server
                .cancel_delivery_inner(CancelDeliveryArgs {
                    workspace: workspace.clone(),
                    room_id: "room-1".into(),
                    delivery_ids: ids,
                })
                .await
                .is_err()
        );
    }
    assert!(
        server
            .cancel_delivery_inner(CancelDeliveryArgs {
                workspace: std::env::temp_dir(),
                room_id: "room-1".into(),
                delivery_ids: vec!["keep".into()],
            })
            .await
            .is_err()
    );
}

#[test]
fn cancellation_after_adapter_return_keeps_the_complete_answer() {
    let runtime = DeliveryRuntime::new();
    let delivery = delivery("finished", DeliveryStatus::Running);
    delivery.cancellation.finish();
    runtime.deliveries.insert(delivery);
    runtime.deliveries.cancel("finished");
    runtime.deliveries.finish(
        "finished",
        None,
        None,
        crate::adapters::AdapterOutput {
            observed_session_id: None,
            answer: Some("complete answer".into()),
            error: None,
            question: None,
        },
        None,
    );
    let result = runtime
        .deliveries
        .snapshots("room-1", &["finished".into()])
        .unwrap();
    assert_eq!(result[0].status, DeliveryStatus::Completed);
    assert_eq!(result[0].final_answer.as_deref(), Some("complete answer"));
    assert!(!result[0].answer_incomplete);
}

#[tokio::test(start_paused = true)]
async fn cancellation_returns_uncertain_when_cleanup_never_finishes() {
    use crate::mcp::api::CancelDeliveryArgs;
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().canonicalize().unwrap();
    let store = StateStore::new(directory.path().join("rooms.json"));
    store
        .mutate(|state| {
            state
                .rooms
                .push(room("room-1", &workspace.to_string_lossy()));
            Ok(())
        })
        .unwrap();
    let server = ConferMcp::with_store(store);
    server
        .runtime
        .deliveries
        .insert(delivery("stuck", DeliveryStatus::Running));
    let start = tokio::time::Instant::now();
    let result = server
        .cancel_delivery_inner(CancelDeliveryArgs {
            workspace,
            room_id: "room-1".into(),
            delivery_ids: vec!["stuck".into()],
        })
        .await
        .unwrap();
    assert_eq!(
        start.elapsed(),
        Duration::from_millis(super::CANCEL_WAIT_MS)
    );
    assert!(result.timed_out);
    assert!(!result.completed);
    assert_eq!(result.deliveries[0].status, DeliveryStatus::Running);
    assert!(result.deliveries[0].previous_delivery_uncertain);
}

#[cfg(unix)]
#[tokio::test]
async fn uncancelled_worker_failures_clear_dispatch_lease() {
    use super::{QueuedDelivery, run_seat_worker};
    if std::env::var_os("CONFER_TEST_WORKER_FAILURES").is_none() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("claude");
        std::fs::write(
            &executable,
            r#"#!/usr/bin/env python3
import json,sys,time,os
sid=sys.argv[sys.argv.index('--session-id')+1]
if 'mismatch-case' in sys.argv[-1]:
    sid='foreign-session'
else:
    open('adapter-ready','w').write('yes')
    while not os.path.exists('adapter-release'):
        time.sleep(0.01)
print(json.dumps({'session_id':sid,'type':'result','result':'answer'}),flush=True)
"#,
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = std::env::join_paths(
            std::iter::once(directory.path().to_owned())
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "mcp::delivery::tests::uncancelled_worker_failures_clear_dispatch_lease",
                "--nocapture",
            ])
            .env("CONFER_TEST_WORKER_FAILURES", directory.path())
            .env("ANTHROPIC_API_KEY", "test-only")
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
        std::path::PathBuf::from(std::env::var_os("CONFER_TEST_WORKER_FAILURES").unwrap());
    for case in ["mismatch-case", "persistence-case"] {
        let workspace = directory.join(case);
        std::fs::create_dir(&workspace).unwrap();
        let workspace = workspace.canonicalize().unwrap();
        let store = StateStore::new(workspace.join("rooms.json"));
        store
            .mutate(|state| {
                let mut record = room("room-1", &workspace.to_string_lossy());
                record
                    .seats
                    .push(seat("seat-1", "worker", AgentKind::Claude));
                state.rooms.push(record);
                Ok(())
            })
            .unwrap();
        let tracker = super::DeliveryTracker::new();
        let state = delivery(case, DeliveryStatus::Queued);
        let cancellation = state.cancellation.clone();
        tracker.insert(state);
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        sender
            .send(QueuedDelivery {
                delivery_id: case.into(),
                room_id: "room-1".into(),
                seat_id: "seat-1".into(),
                message: case.into(),
                workspace: workspace.clone(),
                cancellation,
            })
            .unwrap();
        drop(sender);
        let task = tokio::spawn(run_seat_worker(
            receiver,
            store.clone(),
            tracker.clone(),
            None,
        ));
        if case == "persistence-case" {
            tokio::time::timeout(Duration::from_secs(5), async {
                while !workspace.join("adapter-ready").exists() {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            assert!(
                !std::fs::read(workspace.join("seat-locks/room-1-seat-1.lock"))
                    .unwrap()
                    .is_empty()
            );
            store
                .mutate(|state| {
                    state.rooms[0].seats[0].native_session_id = Some("competing-session".into());
                    Ok(())
                })
                .unwrap();
            std::fs::write(workspace.join("adapter-release"), "yes").unwrap();
        }
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
        let states = tracker.snapshots("room-1", &[case.into()]).unwrap();
        assert_eq!(states[0].status, DeliveryStatus::Failed);
        assert!(!states[0].previous_delivery_uncertain);
        let error = states[0].error.as_deref().unwrap();
        assert!(
            error.contains(if case == "mismatch-case" {
                "native session changed"
            } else {
                "could not be persisted"
            }),
            "{error}"
        );
        let lease = store.try_acquire_seat_lease("room-1", "seat-1").unwrap();
        assert_eq!(lease.metadata().unwrap().len(), 0);
        assert!(!store.seat_dispatch_uncertain("room-1", "seat-1"));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn cancellation_while_waiting_for_lease_never_dispatches() {
    use super::{QueuedDelivery, run_seat_worker};
    use std::future::Future;
    if std::env::var_os("CONFER_TEST_CANCEL_LEASE").is_none() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("opencode");
        std::fs::write(&executable, "#!/bin/sh\ntouch adapter-spawned\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = std::env::join_paths(
            std::iter::once(directory.path().to_owned())
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "mcp::delivery::tests::cancellation_while_waiting_for_lease_never_dispatches",
                "--nocapture",
            ])
            .env("CONFER_TEST_CANCEL_LEASE", directory.path())
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
    let directory = std::path::PathBuf::from(std::env::var_os("CONFER_TEST_CANCEL_LEASE").unwrap());
    for case in ["lease-wait", "readiness-wait"] {
        let workspace = directory.join(case);
        std::fs::create_dir(&workspace).unwrap();
        let workspace = workspace.canonicalize().unwrap();
        let store = StateStore::new(workspace.join("rooms.json"));
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
        let state_lock = std::fs::File::open(workspace.join("rooms.json.lock")).unwrap();
        state_lock.lock().unwrap();
        let tracker = super::DeliveryTracker::new();
        let mut state = delivery(case, DeliveryStatus::Queued);
        state.agent = AgentKind::Opencode;
        let cancellation = state.cancellation.clone();
        tracker.insert(state);
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        sender
            .send(QueuedDelivery {
                delivery_id: case.into(),
                room_id: "room-1".into(),
                seat_id: "seat-1".into(),
                message: "must never start".into(),
                workspace: workspace.clone(),
                cancellation,
            })
            .unwrap();
        drop(sender);
        let (polled_sender, polled_receiver) = std::sync::mpsc::channel();
        let (done_sender, done_receiver) = tokio::sync::oneshot::channel();
        let worker_store = store.clone();
        let worker_tracker = tracker.clone();
        let thread = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    let worker = run_seat_worker(receiver, worker_store, worker_tracker, None);
                    tokio::pin!(worker);
                    let mut polled_sender = Some(polled_sender);
                    std::future::poll_fn(|cx| {
                        let result = worker.as_mut().poll(cx);
                        if let Some(sender) = polled_sender.take() {
                            assert!(result.is_pending());
                            sender.send(()).unwrap();
                        }
                        result
                    })
                    .await;
                });
            done_sender.send(()).unwrap();
        });
        polled_receiver
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        assert_eq!(
            tracker.snapshots("room-1", &[case.into()]).unwrap()[0].status,
            DeliveryStatus::Queued
        );
        if case == "lease-wait" {
            tracker.cancel(case);
            drop(lease);
        } else {
            drop(lease);
            tokio::time::timeout(Duration::from_secs(5), async {
                while std::fs::read(workspace.join("seat-locks/room-1-seat-1.lock"))
                    .unwrap()
                    .is_empty()
                {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            assert_eq!(
                tracker.snapshots("room-1", &[case.into()]).unwrap()[0].status,
                DeliveryStatus::Queued
            );
            tracker.cancel(case);
        }
        drop(state_lock);
        tokio::time::timeout(Duration::from_secs(5), done_receiver)
            .await
            .unwrap()
            .unwrap();
        thread.join().unwrap();
        let states = tracker.snapshots("room-1", &[case.into()]).unwrap();
        assert!(
            !workspace.join("adapter-spawned").exists(),
            "adapter spawned in {case}"
        );
        assert_eq!(states[0].status, DeliveryStatus::Cancelled);
        assert!(
            !states[0].previous_delivery_uncertain,
            "uncertain in {case}"
        );
        let lease = store.try_acquire_seat_lease("room-1", "seat-1").unwrap();
        assert_eq!(
            lease.metadata().unwrap().len(),
            0,
            "lease retained in {case}"
        );
        drop(lease);
        assert!(!store.seat_dispatch_uncertain("room-1", "seat-1"));
    }
}
