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
fn model_or_effort_without_agent_is_rejected() {
    for (model, effort) in [
        (Some("claude-opus-5-5"), None),
        (None, Some("high")),
        (Some("gpt-6-sol"), Some("medium")),
    ] {
        let request = SeatSpecInput {
            agent: None,
            model: model.map(str::to_owned),
            reasoning_effort: effort.map(str::to_owned),
            name: Some("opus".into()),
            instructions: None,
        };
        let error = select_seats(
            vec![request],
            1,
            Some("cursor"),
            &[ready(AgentKind::Claude), ready(AgentKind::Codex)],
            HashSet::new(),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("sets model or reasoning_effort without an agent"),
            "{error}"
        );
    }
}

#[test]
fn blank_model_and_effort_are_unset() {
    let request = SeatSpecInput {
        agent: None,
        model: Some("  ".into()),
        reasoning_effort: Some("".into()),
        name: None,
        instructions: None,
    };
    let seats = select_seats(
        vec![request],
        1,
        None,
        &[ready(AgentKind::Codex)],
        HashSet::new(),
    )
    .unwrap();
    assert_eq!(seats[0].agent, AgentKind::Codex);
    assert!(seats[0].model.is_none());
    assert!(seats[0].reasoning_effort.is_none());
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

#[tokio::test]
async fn retiring_codex_seats_archives_their_native_threads() {
    if std::env::var_os("CONFER_TEST_RETIRE_ARCHIVE").is_none() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let bin = directory.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let executable = bin.join("codex");
        std::fs::write(
            &executable,
            r#"#!/bin/sh
log="$CONFER_TEST_RETIRE_ARCHIVE/archive-requests"
read -r line
printf '%s\n' '{"id":1,"result":{}}'
read -r line
read -r line
printf '%s\n' "$line" >> "$log"
case "$line" in
*rejected-thread*) printf '%s\n' '{"id":2,"error":{"code":-32600,"message":"thread rejected-thread already has an active writer"}}' ;;
*) printf '%s\n' '{"id":2,"result":{}}' ;;
esac
"#,
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = std::env::join_paths(
            std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "mcp::rooms::tests::retiring_codex_seats_archives_their_native_threads",
                "--nocapture",
            ])
            .env("CONFER_TEST_RETIRE_ARCHIVE", directory.path())
            .env("PATH", path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("active writer"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let directory =
        std::path::PathBuf::from(std::env::var_os("CONFER_TEST_RETIRE_ARCHIVE").unwrap());
    let workspace = directory.canonicalize().unwrap();
    crate::test_support::git_init(&workspace);
    let store = StateStore::new(directory.join("rooms.json"));
    let mut record = room("room-1", &workspace.to_string_lossy());
    for (id, agent, session) in [
        ("archived", AgentKind::Codex, Some("archived-thread")),
        ("rejected", AgentKind::Codex, Some("rejected-thread")),
        ("busy", AgentKind::Codex, Some("busy-thread")),
        ("unstarted", AgentKind::Codex, None),
        ("claude", AgentKind::Claude, Some("claude-session")),
    ] {
        let mut member = seat(id, id, agent);
        member.native_session_id = session.map(Into::into);
        record.seats.push(member);
    }
    store
        .mutate(|state| {
            state.rooms.push(record);
            Ok(())
        })
        .unwrap();
    let service = ConferMcp::with_store(store.clone());
    let retire = |seat: &str| RetireSeatArgs {
        workspace: workspace.clone(),
        room_id: "room-1".into(),
        seat: seat.into(),
    };

    let delivery = store.try_acquire_seat_lease("room-1", "busy").unwrap();
    let busy = service.retire_seat_inner(retire("busy")).await.unwrap_err();
    assert!(busy.to_string().starts_with("seat_busy:"), "{busy:#}");
    drop(delivery);

    for seat in ["archived", "rejected", "unstarted", "claude"] {
        service.retire_seat_inner(retire(seat)).await.unwrap();
    }

    let seats = &store.load().unwrap().rooms[0].seats;
    for seat in seats {
        let expected = if seat.id == "busy" {
            SeatStatus::Active
        } else {
            SeatStatus::Retired
        };
        assert_eq!(seat.status, expected, "{}", seat.id);
    }
    let archived = std::fs::read_to_string(directory.join("archive-requests"))
        .unwrap()
        .lines()
        .map(|line| {
            let request: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(request["method"], "thread/archive");
            request["params"]["threadId"].as_str().unwrap().to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(archived, ["archived-thread", "rejected-thread"]);
}
