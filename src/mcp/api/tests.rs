use super::{
    AddSeatArgs, CancelDeliveryArgs, CreateRoomArgs, ListRoomsArgs, RoomScope, SendMessageArgs,
    WaitOutputParams,
};
use crate::mcp::ConferMcp;
use crate::state::StateStore;

use crate::test_support::room;

#[test]
fn all_scope_lists_rooms_across_workspaces() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().canonicalize().unwrap();
    let other = tempfile::tempdir().unwrap();
    let store = StateStore::new(dir.path().join("rooms.json"));
    store
        .mutate(|state| {
            state.rooms = vec![
                room("current", &workspace.to_string_lossy()),
                room(
                    "other",
                    &other.path().canonicalize().unwrap().to_string_lossy(),
                ),
            ];
            Ok(())
        })
        .unwrap();
    let server = ConferMcp::with_store(store);
    crate::test_support::git_init(&workspace);
    let subdir = workspace.join("src");
    std::fs::create_dir(&subdir).unwrap();

    for scope in [serde_json::Value::Null, serde_json::json!("current")] {
        let args = serde_json::from_value(serde_json::json!({
            "scope": scope,
            "workspace": subdir,
        }))
        .unwrap();
        let output = server.list_rooms_inner(args).unwrap();
        assert_eq!(output.workspace.as_deref(), workspace.to_str());
        assert_eq!(output.rooms.len(), 1);
        assert_eq!(output.rooms[0].id, "current");
    }
    for value in [
        serde_json::json!({}),
        serde_json::json!({"scope": "current"}),
        serde_json::json!({"scope": null, "workspace": null}),
    ] {
        assert!(
            server
                .list_rooms_inner(serde_json::from_value(value).unwrap())
                .is_err()
        );
    }
    for value in [
        serde_json::json!({"scope": "all"}),
        serde_json::json!({"scope": "all", "workspace": "relative/missing"}),
    ] {
        let output = server
            .list_rooms_inner(serde_json::from_value(value).unwrap())
            .unwrap();
        assert!(output.workspace.is_none());
        assert_eq!(output.rooms.len(), 2);
    }
}

#[test]
fn null_room_scope_defaults_to_current() {
    let args: ListRoomsArgs = serde_json::from_value(serde_json::json!({ "scope": null }))
        .expect("null scope should deserialize");

    assert!(matches!(args.scope.unwrap_or_default(), RoomScope::Current));
}

#[test]
fn dual_mode_nested_inputs_succeed_and_corrupted_values_return_circuit_breaker_errors() {
    let create_native: CreateRoomArgs = serde_json::from_value(serde_json::json!({
        "workspace": "/tmp/ws",
        "seats": [{"agent": "claude", "name": "reviewer"}]
    }))
    .unwrap();
    assert_eq!(create_native.seats.len(), 1);
    assert_eq!(create_native.seats[0].agent.as_deref(), Some("claude"));

    let create_stringified: CreateRoomArgs = serde_json::from_value(serde_json::json!({
        "workspace": "/tmp/ws",
        "seats": "[{\"agent\":\"codex\",\"model\":\"gpt-5.6-sol\"}]"
    }))
    .unwrap();
    assert_eq!(create_stringified.seats.len(), 1);
    assert_eq!(
        create_stringified.seats[0].model.as_deref(),
        Some("gpt-5.6-sol")
    );

    let create_item_stringified: CreateRoomArgs = serde_json::from_value(serde_json::json!({
        "workspace": "/tmp/ws",
        "seats": ["{\"agent\":\"claude\",\"name\":\"r1\"}"]
    }))
    .unwrap();
    assert_eq!(create_item_stringified.seats[0].name.as_deref(), Some("r1"));

    let create_with_target_and_empty_seats: CreateRoomArgs =
        serde_json::from_value(serde_json::json!({
            "workspace": "/tmp/ws",
            "target_size": 2,
            "seats": "[]"
        }))
        .unwrap();
    assert!(create_with_target_and_empty_seats.seats.is_empty());

    let add_stringified: AddSeatArgs = serde_json::from_value(serde_json::json!({
        "workspace": "/tmp/ws",
        "room_id": "room-1",
        "seat": "{\"agent\":\"claude\",\"reasoning_effort\":\"high\"}"
    }))
    .unwrap();
    assert_eq!(
        add_stringified.seat.reasoning_effort.as_deref(),
        Some("high")
    );

    let send_stringified: SendMessageArgs = serde_json::from_value(serde_json::json!({
        "workspace": "/tmp/ws",
        "room_id": "room-1",
        "recipients": "[\"reviewer\",\"fast\"]",
        "message": "ping"
    }))
    .unwrap();
    assert_eq!(send_stringified.recipients, ["reviewer", "fast"]);

    let wait_stringified: WaitOutputParams = serde_json::from_value(serde_json::json!({
        "workspace": "/tmp/ws",
        "room_id": "room-1",
        "delivery_ids": "[\"d1\"]",
        "full_output": true
    }))
    .unwrap();
    assert_eq!(wait_stringified.delivery_ids, ["d1"]);
    assert_eq!(wait_stringified.full_output, Some(true));

    let wait_empty: WaitOutputParams = serde_json::from_value(serde_json::json!({
        "workspace": "/tmp/ws",
        "room_id": "room-1",
        "delivery_ids": "[]"
    }))
    .unwrap();
    assert!(wait_empty.delivery_ids.is_empty());

    let cancel_stringified: CancelDeliveryArgs = serde_json::from_value(serde_json::json!({
        "workspace": "/tmp/ws",
        "room_id": "room-1",
        "delivery_ids": "[\"d1\",\"d2\"]"
    }))
    .unwrap();
    assert_eq!(cancel_stringified.delivery_ids, ["d1", "d2"]);

    for (bad_seats, expected_field, expected_fragment) in [
        (serde_json::json!(true), "seats", "boolean (true)"),
        (serde_json::json!("true"), "seats", "boolean (true)"),
        (serde_json::json!(null), "seats", "null"),
        (serde_json::json!("null"), "seats", "null"),
        (serde_json::json!([true]), "seats[0]", "boolean (true)"),
        (serde_json::json!("[true]"), "seats[0]", "boolean (true)"),
        (serde_json::json!([null]), "seats[0]", "null"),
        (
            serde_json::json!([]),
            "seats",
            "provide target_size or at least one seat",
        ),
        (
            serde_json::json!("[]"),
            "seats",
            "provide target_size or at least one seat",
        ),
    ] {
        let err = serde_json::from_value::<CreateRoomArgs>(serde_json::json!({
            "workspace": "/tmp/ws",
            "seats": bad_seats
        }))
        .unwrap_err()
        .to_string();
        assert!(err.contains("create_room"), "{err}");
        assert!(err.contains(expected_field), "{err}");
        assert!(err.contains(expected_fragment), "{err}");
        assert!(
            err.contains("Do NOT retry the same create_room payload unchanged"),
            "{err}"
        );
    }

    for (bad_seat, expected_fragment) in [
        (serde_json::json!(true), "boolean (true)"),
        (serde_json::json!("false"), "boolean (false)"),
        (serde_json::json!(null), "null"),
        (serde_json::json!("null"), "null"),
        (serde_json::json!([]), "empty array ([])"),
        (serde_json::json!([true]), "array"),
    ] {
        let err = serde_json::from_value::<AddSeatArgs>(serde_json::json!({
            "workspace": "/tmp/ws",
            "room_id": "room-1",
            "seat": bad_seat
        }))
        .unwrap_err()
        .to_string();
        assert!(err.contains("add_seat"), "{err}");
        assert!(err.contains("seat"), "{err}");
        assert!(err.contains(expected_fragment), "{err}");
        assert!(
            err.contains("Do NOT retry the same add_seat payload unchanged"),
            "{err}"
        );
    }

    for (bad_recipients, expected_fragment) in [
        (serde_json::json!([]), "recipients must not be empty"),
        (serde_json::json!("[]"), "recipients must not be empty"),
        (serde_json::json!([true]), "boolean (true)"),
        (serde_json::json!("[true]"), "boolean (true)"),
        (serde_json::json!(null), "null"),
    ] {
        let err = serde_json::from_value::<SendMessageArgs>(serde_json::json!({
            "workspace": "/tmp/ws",
            "room_id": "room-1",
            "recipients": bad_recipients,
            "message": "ping"
        }))
        .unwrap_err()
        .to_string();
        assert!(err.contains("send_message"), "{err}");
        assert!(err.contains("recipients"), "{err}");
        assert!(err.contains(expected_fragment), "{err}");
        assert!(
            err.contains("Do NOT retry the same send_message payload unchanged"),
            "{err}"
        );
    }

    for (bad_ids, expected_fragment) in [
        (serde_json::json!(null), "null"),
        (serde_json::json!(true), "boolean (true)"),
        (serde_json::json!([true]), "boolean (true)"),
        (serde_json::json!("[null]"), "null"),
    ] {
        let err = serde_json::from_value::<WaitOutputParams>(serde_json::json!({
            "workspace": "/tmp/ws",
            "room_id": "room-1",
            "delivery_ids": bad_ids
        }))
        .unwrap_err()
        .to_string();
        assert!(err.contains("wait_output"), "{err}");
        assert!(err.contains("delivery_ids"), "{err}");
        assert!(err.contains(expected_fragment), "{err}");
        assert!(
            err.contains("Do NOT retry the same wait_output payload unchanged"),
            "{err}"
        );
    }

    for (bad_ids, expected_fragment) in [
        (serde_json::json!([]), "delivery_ids must not be empty"),
        (serde_json::json!("[]"), "delivery_ids must not be empty"),
        (serde_json::json!(null), "null"),
        (serde_json::json!("[true]"), "boolean (true)"),
    ] {
        let err = serde_json::from_value::<CancelDeliveryArgs>(serde_json::json!({
            "workspace": "/tmp/ws",
            "room_id": "room-1",
            "delivery_ids": bad_ids
        }))
        .unwrap_err()
        .to_string();
        assert!(err.contains("cancel_delivery"), "{err}");
        assert!(err.contains("delivery_ids"), "{err}");
        assert!(err.contains(expected_fragment), "{err}");
        assert!(
            err.contains("Do NOT retry the same cancel_delivery payload unchanged"),
            "{err}"
        );
    }
}
