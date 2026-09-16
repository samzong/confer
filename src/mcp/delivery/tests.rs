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

fn delivery(id: &str, status: DeliveryStatus) -> DeliveryState {
    DeliveryState {
        delivery_id: id.into(),
        room_id: "room-1".into(),
        seat_id: "seat-1".into(),
        seat_name: "reviewer".into(),
        agent: AgentKind::Claude,
        status,
        final_answer: None,
        error: None,
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
