use super::{StateStore, canonical_workspace, normalize_workspace};
use crate::types::SeatStatus;

#[test]
fn discovered_state_persists_under_xdg() {
    if let Some(root) = std::env::var_os("CONFER_TEST_STATE_ROOT") {
        let root = std::path::PathBuf::from(root).join("confer");
        let store = StateStore::discover().unwrap();
        store.mutate(|_| Ok(())).unwrap();
        assert_eq!(store.path(), root.join("rooms.json"));
        let persisted: serde_json::Value =
            serde_json::from_slice(&std::fs::read(store.path()).unwrap()).unwrap();
        assert_eq!(persisted["schema_version"], 3);
        let lease = store.try_acquire_seat_lease("room", "seat").unwrap();
        let reopened = StateStore::discover().unwrap();
        assert!(reopened.try_acquire_seat_lease("room", "seat").is_err());
        drop(lease);
        assert!(reopened.try_acquire_seat_lease("room", "seat").is_ok());
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().canonicalize().unwrap();
    let custom = home.join("custom state");
    for xdg in [None, Some(""), Some("relative/state"), custom.to_str()] {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "state::tests::discovered_state_persists_under_xdg",
            ])
            .env("HOME", &home)
            .env_remove("XDG_STATE_HOME")
            .env(
                "CONFER_TEST_STATE_ROOT",
                if xdg == custom.to_str() {
                    custom.clone()
                } else {
                    home.join(".local/state")
                },
            );
        if let Some(xdg) = xdg {
            command.env("XDG_STATE_HOME", xdg);
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{output:?}");
    }
}

#[test]
fn cache_round_trip_preserves_rooms() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().canonicalize().unwrap();
    let store = StateStore::new(dir.path().join("rooms.json"));
    store
        .mutate(|state| {
            let mut room = crate::test_support::room("room-1", &workspace.to_string_lossy());
            room.name = "Review".into();
            state.rooms.push(room);
            Ok(())
        })
        .unwrap();

    let state = store.load().unwrap();
    assert_eq!(state.rooms.len(), 1);
    assert_eq!(state.rooms[0].id, "room-1");
    assert!(store.room_for_workspace("room-1", &workspace).is_ok());
    let other = tempfile::tempdir().unwrap();
    assert!(
        store
            .room_for_workspace("room-1", &other.path().canonicalize().unwrap())
            .is_err()
    );
    let persisted: serde_json::Value =
        serde_json::from_slice(&std::fs::read(store.path()).unwrap()).unwrap();
    assert_eq!(persisted["schema_version"], 3);
    assert!(persisted["rooms"][0].get("status").is_none());
}

#[test]
fn workspace_requires_an_existing_absolute_directory() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().canonicalize().unwrap();
    assert_eq!(canonical_workspace(dir.path()).unwrap(), workspace);
    assert_eq!(normalize_workspace(dir.path()).unwrap(), workspace);
    let file = dir.path().join("file");
    std::fs::write(&file, "content").unwrap();
    for path in [
        std::path::PathBuf::new(),
        ".".into(),
        "src".into(),
        dir.path().join("missing"),
        file,
    ] {
        assert!(canonical_workspace(&path).is_err());
        assert!(normalize_workspace(&path).is_err());
    }
    #[cfg(unix)]
    {
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&workspace, &link).unwrap();
        assert_eq!(canonical_workspace(&link).unwrap(), workspace);
        assert_eq!(normalize_workspace(&link).unwrap(), workspace);
    }
}

#[test]
fn workspace_normalization_ignores_inherited_git_location() {
    if let Some(workspace) = std::env::var_os("CONFER_TEST_WORKSPACE") {
        let workspace = std::path::PathBuf::from(workspace);
        let subdir = workspace.join("src");
        assert_eq!(canonical_workspace(&subdir).unwrap(), subdir);
        assert_eq!(normalize_workspace(&subdir).unwrap(), workspace);
        let outside = workspace.parent().unwrap();
        assert_eq!(normalize_workspace(outside).unwrap(), outside);
        let checkout = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        assert_eq!(
            normalize_workspace(&checkout.join("src")).unwrap(),
            checkout.canonicalize().unwrap()
        );
        #[cfg(unix)]
        {
            assert_eq!(
                normalize_workspace(&outside.join("link")).unwrap(),
                workspace
            );
        }
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("task");
    let other = dir.path().join("other");
    for path in [&workspace, &other] {
        crate::test_support::git_init(path);
    }
    std::fs::create_dir(workspace.join("src")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(workspace.join("src"), dir.path().join("link")).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "state::tests::workspace_normalization_ignores_inherited_git_location",
            "--nocapture",
        ])
        .env("CONFER_TEST_WORKSPACE", workspace.canonicalize().unwrap())
        .env("GIT_DIR", other.join(".git"))
        .env("GIT_WORK_TREE", &other)
        .current_dir(&other)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn cache_rejects_unknown_schema() {
    let dir = tempfile::tempdir().unwrap();
    let store = StateStore::new(dir.path().join("rooms.json"));
    std::fs::write(store.path(), r#"{"schema_version":4,"rooms":[]}"#).unwrap();
    assert!(store.load().is_err());
}

#[test]
fn cache_mutation_upgrades_supported_legacy_schemas() {
    let dir = tempfile::tempdir().unwrap();
    let store = StateStore::new(dir.path().join("rooms.json"));
    for schema_version in [1, 2] {
        std::fs::write(
            store.path(),
            format!(r#"{{"schema_version":{schema_version},"rooms":[]}}"#),
        )
        .unwrap();
        store.mutate(|_| Ok(())).unwrap();
        assert_eq!(store.load().unwrap().schema_version, 3);
    }
}

#[test]
fn cache_reads_legacy_room_without_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let store = StateStore::new(dir.path().join("rooms.json"));
    std::fs::write(
        store.path(),
        r#"{"schema_version":1,"rooms":[{"id":"room-1","name":"Room","workspace":"/tmp/project","status":"inactive","host":{"agent":"codex"},"seats":[{"id":"seat-1","name":"reviewer","agent":"claude","model":null,"reasoning_effort":null,"instructions":null,"native_session_id":null}],"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}]}"#,
    )
    .unwrap();

    let state = store.load().unwrap();
    assert_eq!(state.rooms[0].seats[0].status, SeatStatus::Active);
    assert!(
        store
            .room_for_workspace("room-1", std::path::Path::new("/tmp/project"))
            .is_ok()
    );
}

#[test]
fn seat_lease_is_exclusive_across_store_instances() {
    let dir = tempfile::tempdir().unwrap();
    let first_store = StateStore::new(dir.path().join("rooms.json"));
    let second_store = StateStore::new(dir.path().join("rooms.json"));
    let first = first_store
        .try_acquire_seat_lease("room-1", "seat-1")
        .unwrap();

    let error = second_store
        .try_acquire_seat_lease("room-1", "seat-1")
        .unwrap_err();
    assert!(error.to_string().contains("seat_busy"));

    drop(first);
    assert!(
        second_store
            .try_acquire_seat_lease("room-1", "seat-1")
            .is_ok()
    );
}
