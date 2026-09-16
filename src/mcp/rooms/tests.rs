use super::select_seats;
use crate::mcp::ConferMcp;
use crate::mcp::api::{RetireSeatArgs, SeatSpecInput};
use crate::state::StateStore;
use crate::test_support::seat;
use crate::types::{AgentKind, Readiness, SeatStatus};
use std::collections::HashSet;

fn ready(agent: AgentKind) -> Readiness {
    Readiness {
        agent,
        locally_ready: true,
        executable: Some(agent.id().into()),
        reason: None,
    }
}

use crate::test_support::room;

#[test]
fn automatic_seats_prefer_other_agents_and_repeat_with_unique_addresses() {
    let readiness = vec![
        ready(AgentKind::Claude),
        ready(AgentKind::Codex),
        ready(AgentKind::Grok),
    ];
    let seats = select_seats(Vec::new(), 20, Some("codex"), &readiness, HashSet::new()).unwrap();
    assert_eq!(seats.len(), 20);
    assert_eq!(seats[0].agent, AgentKind::Claude);
    assert_eq!(seats[1].agent, AgentKind::Grok);
    assert_eq!(seats[2].agent, AgentKind::Codex);
    assert_eq!(seats[3].agent, AgentKind::Claude);
    assert_eq!(
        seats
            .iter()
            .map(|seat| &seat.id)
            .collect::<HashSet<_>>()
            .len(),
        seats.len()
    );
    assert_eq!(
        seats
            .iter()
            .map(|seat| &seat.name)
            .collect::<HashSet<_>>()
            .len(),
        seats.len()
    );
}

#[test]
fn unavailable_requested_agent_fails_even_when_an_alternative_is_ready() {
    for readiness in [vec![ready(AgentKind::Claude)], Vec::new()] {
        let request = SeatSpecInput {
            agent: Some("cursor".into()),
            model: Some("cursor-model".into()),
            reasoning_effort: Some("high".into()),
            name: Some("reviewer".into()),
            instructions: Some("Review only".into()),
        };
        assert!(select_seats(vec![request], 1, Some("codex"), &readiness, HashSet::new()).is_err());
    }
    assert!(select_seats(Vec::new(), 1, None, &[], HashSet::new()).is_err());
}

#[test]
fn added_seat_uses_a_unique_room_address() {
    let names = ["claude".to_string()].into_iter().collect();
    let request = SeatSpecInput {
        agent: Some("claude".into()),
        model: None,
        reasoning_effort: None,
        name: None,
        instructions: None,
    };
    let seats = select_seats(
        vec![request],
        1,
        Some("codex"),
        &[ready(AgentKind::Claude)],
        names,
    )
    .unwrap();

    assert_eq!(seats[0].name, "claude-2");
    assert_eq!(seats[0].status, SeatStatus::Active);
}

#[test]
fn identical_configurations_create_separate_seats() {
    let requests = (0..2)
        .map(|_| SeatSpecInput {
            agent: Some("claude".into()),
            model: Some("sonnet".into()),
            reasoning_effort: Some("high".into()),
            name: None,
            instructions: Some("Review only".into()),
        })
        .collect();
    let seats = select_seats(
        requests,
        1,
        Some("claude"),
        &[ready(AgentKind::Claude)],
        HashSet::new(),
    )
    .unwrap();
    assert_eq!(seats.len(), 2);
    assert_ne!(seats[0].id, seats[1].id);
    assert_ne!(seats[0].name, seats[1].name);
    for seat in seats {
        assert_eq!(seat.agent, AgentKind::Claude);
        assert_eq!(seat.model.as_deref(), Some("sonnet"));
        assert_eq!(seat.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(seat.instructions.as_deref(), Some("Review only"));
        assert!(seat.native_session_id.is_none());
    }
}

#[test]
fn seat_selection_validates_the_final_agent_configuration() {
    for (agent, model, effort) in [
        (AgentKind::Cursor, Some("model[effort=high]"), Some("high")),
        (AgentKind::Cursor, Some("model[effort"), None),
        (AgentKind::Claude, None, Some("invalid")),
        (AgentKind::Agy, None, Some("xhigh")),
        (AgentKind::Kimi, None, Some("none")),
    ] {
        let request = SeatSpecInput {
            agent: Some(agent.id().into()),
            model: model.map(str::to_owned),
            reasoning_effort: effort.map(str::to_owned),
            name: None,
            instructions: None,
        };
        assert!(select_seats(vec![request], 1, None, &[ready(agent)], HashSet::new()).is_err());
    }
}

#[tokio::test]
async fn retiring_seat_preserves_its_native_session() {
    let dir = tempfile::tempdir().unwrap();
    let store = StateStore::new(dir.path().join("rooms.json"));
    let workspace = dir.path().canonicalize().unwrap();
    let mut record = room("room-1", &workspace.to_string_lossy());
    let mut reviewer = seat("seat-1", "reviewer", AgentKind::Claude);
    reviewer.native_session_id = Some("session-1".into());
    record.seats.push(reviewer);
    store
        .mutate(|state| {
            state.rooms.push(record);
            Ok(())
        })
        .unwrap();
    let service = ConferMcp::with_store(store.clone());
    let mut worker = service.runtime.register_worker("room-1", "seat-1").await;
    assert!(service.runtime.has_worker("room-1", "seat-1").await);

    crate::test_support::git_init(&workspace);
    let subdir = workspace.join("src");
    std::fs::create_dir(&subdir).unwrap();
    let other = tempfile::tempdir().unwrap();
    let before = std::fs::read(store.path()).unwrap();
    for caller in [other.path().canonicalize().unwrap(), subdir] {
        assert!(
            service
                .add_seat_inner(
                    serde_json::from_value(serde_json::json!({
                        "workspace": caller,
                        "room_id": "room-1",
                        "seat": {"agent": "claude"},
                    }))
                    .unwrap()
                )
                .is_err()
        );
        assert!(
            service
                .retire_seat_inner(
                    serde_json::from_value(serde_json::json!({
                        "workspace": caller,
                        "room_id": "room-1",
                        "seat": "reviewer",
                    }))
                    .unwrap()
                )
                .await
                .is_err()
        );
        assert!(
            service
                .send_message_inner(
                    serde_json::from_value(serde_json::json!({
                        "workspace": caller,
                        "room_id": "room-1",
                        "recipients": ["reviewer"],
                        "message": "Review this change",
                    }))
                    .unwrap()
                )
                .await
                .is_err()
        );
        assert!(
            service
                .wait_output_inner(
                    serde_json::from_value(serde_json::json!({
                        "workspace": caller,
                        "room_id": "room-1",
                        "timeout_ms": 0,
                    }))
                    .unwrap()
                )
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(store.path()).unwrap(), before);
        assert!(service.runtime.has_worker("room-1", "seat-1").await);
    }

    service
        .retire_seat_inner(RetireSeatArgs {
            workspace,
            room_id: "room-1".into(),
            seat: "reviewer".into(),
        })
        .await
        .unwrap();

    let seat = &store.load().unwrap().rooms[0].seats[0];
    assert_eq!(seat.status, SeatStatus::Retired);
    assert_eq!(seat.native_session_id.as_deref(), Some("session-1"));
    assert!(!service.runtime.has_worker("room-1", "seat-1").await);
    assert!(worker.closed().await);
}
