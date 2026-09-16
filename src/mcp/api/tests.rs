use super::{ListRoomsArgs, RoomScope};
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
