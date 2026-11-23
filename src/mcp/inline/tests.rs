use rmcp::handler::server::wrapper::Parameters;
use serde_json::Value;

use super::{
    INLINE_MAX_BYTES, json_escaped_str_len, render_create_room, render_send_message, structured_err,
};
use crate::mcp::ConferMcp;
use crate::mcp::api::{
    CancelDeliveryArgs, ClearUncertainDeliveryArgs, CreateRoomOutput, ListRoomsArgs,
    RetireSeatArgs, RoomScope, room_view,
};
use crate::mcp::delivery::{DeliveryState, DeliveryStatus, SendMessageOutput, SendReceipt};
use crate::state::{StateStore, mark_seat_dispatch};
use crate::test_support::{room, seat};
use crate::types::{AgentKind, Readiness, SeatStatus};

fn completed_state(
    delivery_id: &str,
    room_id: &str,
    seat_id: &str,
    seat_name: &str,
    agent: AgentKind,
    final_answer: Option<String>,
    error: Option<String>,
) -> DeliveryState {
    DeliveryState {
        delivery_id: delivery_id.into(),
        room_id: room_id.into(),
        seat_id: seat_id.into(),
        seat_name: seat_name.into(),
        agent,
        status: if error.is_some() && final_answer.is_none() {
            DeliveryStatus::Failed
        } else {
            DeliveryStatus::Completed
        },
        final_answer,
        error,
        resume_command: None,
        answer_incomplete: false,
        previous_delivery_uncertain: false,
        question: None,
        uncertain_delivery_id: None,
        cancel_requested: false,
        cancellation: Default::default(),
    }
}

fn call_result_json_text(result: &rmcp::model::CallToolResult) -> (&str, Value) {
    let text = result.content[0]
        .as_text()
        .expect("structured result must contain text content")
        .text
        .as_str();
    assert!(
        text.len() <= INLINE_MAX_BYTES,
        "serialized text exceeded {INLINE_MAX_BYTES} bytes: {}",
        text.len()
    );
    let parsed: Value = serde_json::from_str(text).expect("tool output must be valid JSON");
    if let Some(structured) = &result.structured_content {
        let structured_serialized = serde_json::to_string(structured).unwrap();
        assert_eq!(structured_serialized.len(), text.len());
    }
    (text, parsed)
}

#[test]
fn json_escaped_length_matches_serde_json_across_control_quotes_cjk_and_emoji() {
    let sample = "\x00\x01\x08\x0c\n\r\t\"\\/Hello 世界 🚀 लाइन\u{2028}\u{2029}";
    let serialized = serde_json::to_string(sample).unwrap();
    assert_eq!(json_escaped_str_len(sample) + 2, serialized.len());
}

#[tokio::test]
async fn wait_output_and_cancel_delivery_enforce_inline_budget_water_filling_and_lossless_recovery()
{
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().canonicalize().unwrap();
    let store = StateStore::new(dir.path().join("rooms.json"));
    store
        .mutate(|state| {
            let mut record = room("room-1", &workspace.to_string_lossy());
            record.seats.push(seat("s-fast", "fast", AgentKind::Rx));
            record
                .seats
                .push(seat("s-long-a", "reviewer-a", AgentKind::Claude));
            record
                .seats
                .push(seat("s-long-b", "reviewer-b", AgentKind::Codex));
            state.rooms.push(record);
            Ok(())
        })
        .unwrap();
    let server = ConferMcp::with_store(store.clone());

    let raw_fast = "\x1b[32mDownloading 10%\rDownloading 100%\r\n\x1b[0m\x1b]8;;https://example.com\x07Fast verdict: PASS\x1b]8;;\x07";
    let mut long_a = String::from("HEAD-A-ROOT-CAUSE: schema mismatch in line 1\n");
    for idx in 0..180 {
        long_a.push_str(&format!(
            "line-{idx}: quote=\"value\" slash=\\ ctrl=\x01 中文诊断🚀 detail\n"
        ));
    }
    long_a.push_str("TAIL-A-CONCLUSION: fix src/mcp/api.rs");

    let mut long_b = String::from("\x1b[31mstep 1\rstep 2\rHEAD-B-CONTEXT: startup trace\x1b[0m\n");
    for idx in 0..220 {
        long_b.push_str(&format!(
            "trace-{idx}: \"escaped\" \\path\\ CJK分析🧪 extra payload\n"
        ));
    }
    long_b.push_str("TAIL-B-VERDICT: REQUEST_CHANGES");

    server.runtime.insert_test_delivery(completed_state(
        "d-fast",
        "room-1",
        "s-fast",
        "fast",
        AgentKind::Rx,
        Some(raw_fast.into()),
        None,
    ));
    let mut state_a = completed_state(
        "d-long-a",
        "room-1",
        "s-long-a",
        "reviewer-a",
        AgentKind::Claude,
        Some(long_a.clone()),
        Some("\x1b[33mminor stderr note\x1b[0m".into()),
    );
    state_a.question = Some(serde_json::json!({"prompt": "Confirm target?"}));
    server.runtime.insert_test_delivery(state_a);
    server.runtime.insert_test_delivery(completed_state(
        "d-long-b",
        "room-1",
        "s-long-b",
        "reviewer-b",
        AgentKind::Codex,
        Some(long_b.clone()),
        None,
    ));

    let inline_res = server
        .wait_output(Parameters(
            serde_json::from_value(serde_json::json!({
                "workspace": workspace,
                "room_id": "room-1",
                "delivery_ids": ["d-fast", "d-long-a", "d-long-b"],
                "timeout_ms": 0
            }))
            .unwrap(),
        ))
        .await
        .unwrap();
    let (_, inline_json) = call_result_json_text(&inline_res);
    let deliveries = inline_json["deliveries"].as_array().unwrap();
    assert_eq!(deliveries.len(), 3);

    assert_eq!(
        deliveries[0]["final_answer"].as_str().unwrap(),
        "Downloading 100%\nFast verdict: PASS"
    );
    assert!(deliveries[0].get("output_folded").is_none());
    assert_eq!(deliveries[0]["answer_incomplete"], false);

    assert_eq!(deliveries[1]["output_folded"], true);
    assert_eq!(deliveries[1]["answer_incomplete"], false);
    assert_eq!(
        deliveries[1]["error"].as_str().unwrap(),
        "minor stderr note"
    );
    assert_eq!(
        deliveries[1]["question"],
        serde_json::json!({"prompt": "Confirm target?"})
    );
    let folded_a = deliveries[1]["final_answer"].as_str().unwrap();
    assert!(folded_a.starts_with("HEAD-A-ROOT-CAUSE:"));
    assert!(folded_a.ends_with("TAIL-A-CONCLUSION: fix src/mcp/api.rs"));
    assert!(folded_a.contains("full_output: true"));
    assert!(folded_a.contains("\"d-long-a\""));

    assert_eq!(deliveries[2]["output_folded"], true);
    assert_eq!(deliveries[2]["answer_incomplete"], false);
    let folded_b = deliveries[2]["final_answer"].as_str().unwrap();
    assert!(folded_b.starts_with("HEAD-B-CONTEXT: startup trace"));
    assert!(folded_b.ends_with("TAIL-B-VERDICT: REQUEST_CHANGES"));
    assert!(!folded_b.contains('\x1b'));

    let full_res = server
        .wait_output(Parameters(
            serde_json::from_value(serde_json::json!({
                "workspace": workspace,
                "room_id": "room-1",
                "delivery_ids": ["d-fast", "d-long-a", "d-long-b"],
                "timeout_ms": 0,
                "full_output": true
            }))
            .unwrap(),
        ))
        .await
        .unwrap();
    let full_text = full_res.content[0].as_text().unwrap().text.as_str();
    assert!(full_text.len() > INLINE_MAX_BYTES);
    let full_json: Value = serde_json::from_str(full_text).unwrap();
    assert_eq!(
        full_json["deliveries"][0]["final_answer"].as_str().unwrap(),
        raw_fast
    );
    assert_eq!(
        full_json["deliveries"][1]["final_answer"].as_str().unwrap(),
        long_a
    );
    assert_eq!(
        full_json["deliveries"][2]["final_answer"].as_str().unwrap(),
        long_b
    );
    assert!(full_json["deliveries"][1].get("output_folded").is_none());

    let mut partial_cancelled = completed_state(
        "d-cancel",
        "room-1",
        "s-long-a",
        "reviewer-a",
        AgentKind::Claude,
        Some(long_a.clone()),
        None,
    );
    partial_cancelled.status = DeliveryStatus::Cancelled;
    partial_cancelled.answer_incomplete = true;
    server.runtime.insert_test_delivery(partial_cancelled);
    let cancel_res = server
        .cancel_delivery(Parameters(CancelDeliveryArgs {
            workspace: workspace.clone(),
            room_id: "room-1".into(),
            delivery_ids: vec!["d-cancel".into()],
        }))
        .await
        .unwrap();
    let (_, cancel_json) = call_result_json_text(&cancel_res);
    assert_eq!(cancel_json["deliveries"][0]["output_folded"], true);
    assert_eq!(cancel_json["deliveries"][0]["answer_incomplete"], true);

    for idx in 0..45 {
        server.runtime.insert_test_delivery(completed_state(
            &format!("d-many-{idx:02}"),
            "room-1",
            "s-fast",
            "fast-worker-with-long-name",
            AgentKind::Claude,
            Some("answer".into()),
            None,
        ));
    }
    let many_res = server
        .wait_output(Parameters(
            serde_json::from_value(serde_json::json!({
                "workspace": workspace,
                "room_id": "room-1",
                "timeout_ms": 0
            }))
            .unwrap(),
        ))
        .await
        .unwrap();
    let (_, many_json) = call_result_json_text(&many_res);
    assert!(many_json["omitted_deliveries"].as_u64().unwrap() > 0);

    let entries = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    for name in entries {
        assert!(
            matches!(
                name.as_str(),
                "rooms.json" | "rooms.json.lock" | "seat-locks"
            ),
            "unexpected state file written: {name}"
        );
    }
}

#[tokio::test]
async fn room_tools_and_errors_enforce_inline_budget_and_keep_priority_seats() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().canonicalize().unwrap();
    crate::test_support::git_init(&workspace);
    let store = StateStore::new(dir.path().join("rooms.json"));

    store
        .mutate(|state| {
            for idx in 0..35 {
                let mut historical = room(&format!("hist-{idx:02}"), &workspace.to_string_lossy());
                historical.updated_at = format!("2026-01-01T00:{idx:02}:00.000Z");
                for s_idx in 0..4 {
                    historical.seats.push(seat(
                        &format!("h-{idx}-{s_idx}"),
                        &format!("seat-{s_idx}"),
                        AgentKind::Claude,
                    ));
                }
                state.rooms.push(historical);
            }
            Ok(())
        })
        .unwrap();
    let server = ConferMcp::with_store(store.clone());

    let list_inline = server
        .list_rooms(Parameters(ListRoomsArgs {
            workspace: Some(workspace.clone()),
            scope: Some(RoomScope::Current),
            full_output: None,
        }))
        .await
        .unwrap();
    let (_, list_json) = call_result_json_text(&list_inline);
    let kept_count = list_json["rooms"].as_array().unwrap().len();
    let omitted_count = list_json["omitted_rooms"].as_u64().unwrap() as usize;
    assert!(kept_count > 0);
    assert!(omitted_count > 0);
    assert_eq!(kept_count + omitted_count, 35);
    assert_eq!(list_json["rooms"][0]["id"], "hist-34");

    let list_full = server
        .list_rooms(Parameters(ListRoomsArgs {
            workspace: Some(workspace.clone()),
            scope: Some(RoomScope::Current),
            full_output: Some(true),
        }))
        .await
        .unwrap();
    let list_full_json: Value =
        serde_json::from_str(&list_full.content[0].as_text().unwrap().text).unwrap();
    assert_eq!(list_full_json["rooms"].as_array().unwrap().len(), 35);
    assert!(list_full_json.get("omitted_rooms").is_none());

    let mut huge_room = room("huge-room", &workspace.to_string_lossy());
    huge_room.updated_at = "2026-12-31T23:59:59.000Z".into();
    for idx in 0..120 {
        let mut member = seat(
            &format!("seat-{idx:03}"),
            &format!("worker-{idx:03}"),
            AgentKind::Claude,
        );
        member.native_session_id = Some(format!("native-session-id-{idx:03}"));
        if idx < 90 {
            member.status = SeatStatus::Retired;
        }
        huge_room.seats.push(member);
    }
    store
        .mutate(|state| {
            state.rooms.push(huge_room.clone());
            Ok(())
        })
        .unwrap();
    let lease = store
        .try_acquire_seat_lease("huge-room", "seat-005")
        .unwrap();
    mark_seat_dispatch(&lease, "uncertain-5").unwrap();
    drop(lease);

    let huge_list = server
        .list_rooms(Parameters(ListRoomsArgs {
            workspace: Some(workspace.clone()),
            scope: Some(RoomScope::Current),
            full_output: None,
        }))
        .await
        .unwrap();
    let (_, huge_list_json) = call_result_json_text(&huge_list);
    assert_eq!(huge_list_json["rooms"].as_array().unwrap().len(), 1);
    assert_eq!(huge_list_json["omitted_rooms"], 35);
    assert!(
        huge_list_json["rooms"][0]["omitted_seats"]
            .as_u64()
            .unwrap()
            > 0
    );
    let listed_seat_ids: Vec<&str> = huge_list_json["rooms"][0]["seats"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap())
        .collect();
    assert!(listed_seat_ids.contains(&"seat-005"));

    let created_view = render_create_room(
        CreateRoomOutput {
            room: room_view(store.load().unwrap().rooms.last().unwrap(), &store),
            readiness: AgentKind::ALL
                .into_iter()
                .map(|agent| Readiness {
                    agent,
                    locally_ready: true,
                    executable: Some(agent.id().into()),
                    reason: None,
                })
                .collect(),
        },
        Some("seat-119"),
    );
    let created_serialized = serde_json::to_string(&created_view).unwrap();
    assert!(created_serialized.len() <= INLINE_MAX_BYTES);
    assert!(created_view.room.omitted_seats > 0);
    assert!(created_view.room.seats.iter().any(|s| s.id == "seat-119"));
    assert!(created_view.room.seats.iter().any(|s| s.id == "seat-005"));

    let cleared = server
        .clear_uncertain_delivery(Parameters(ClearUncertainDeliveryArgs {
            workspace: workspace.clone(),
            room_id: "huge-room".into(),
            seat: "seat-005".into(),
            delivery_id: "uncertain-5".into(),
        }))
        .await
        .unwrap();
    let (_, cleared_json) = call_result_json_text(&cleared);
    assert!(cleared_json["room"]["omitted_seats"].as_u64().unwrap() > 0);
    assert!(
        cleared_json["room"]["seats"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == "seat-005")
    );

    let retired = server
        .retire_seat(Parameters(RetireSeatArgs {
            workspace: workspace.clone(),
            room_id: "huge-room".into(),
            seat: "seat-118".into(),
        }))
        .await
        .unwrap();
    let (_, retired_json) = call_result_json_text(&retired);
    assert!(
        retired_json["room"]["seats"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == "seat-118" && item["status"] == "retired")
    );

    let send_view = render_send_message(SendMessageOutput {
        room_id: "huge-room".into(),
        deliveries: (0..120)
            .map(|idx| SendReceipt {
                delivery_id: format!("del-{idx:03}"),
                seat_id: format!("seat-{idx:03}"),
                seat_name: format!("worker-{idx:03}"),
                agent: AgentKind::Codex,
                accepted: true,
                previous_delivery_uncertain: false,
                uncertain_delivery_id: None,
                resume_command: Some(format!("cd /tmp && codex resume session-{idx:03}")),
                error: None,
            })
            .collect(),
    });
    let send_serialized = serde_json::to_string(&send_view).unwrap();
    assert!(send_serialized.len() <= INLINE_MAX_BYTES);
    assert!(send_view.omitted_deliveries > 0);

    let huge_err = anyhow::anyhow!(
        "\x1b[31mFATAL-HEAD\x1b[0m\n{}\nFATAL-TAIL",
        "error-line-with-\"quotes\"-and-中文\n".repeat(400)
    );
    let err_res = structured_err(huge_err);
    assert_eq!(err_res.is_error, Some(true));
    let (_, err_json) = call_result_json_text(&err_res);
    let err_str = err_json["error"].as_str().unwrap();
    assert!(err_str.starts_with("FATAL-HEAD"));
    assert!(err_str.ends_with("FATAL-TAIL"));
    assert!(!err_str.contains('\x1b'));
}
