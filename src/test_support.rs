use crate::types::{AgentKind, HostRecord, RoomRecord, SeatRecord, SeatStatus};

pub(crate) fn room(id: &str, workspace: &str) -> RoomRecord {
    RoomRecord {
        id: id.into(),
        name: id.into(),
        workspace: workspace.into(),
        host: HostRecord {
            agent: Some("codex".into()),
        },
        seats: Vec::new(),
        created_at: "2026-01-01T00:00:00Z".into(),
        updated_at: "2026-01-01T00:00:00Z".into(),
    }
}

pub(crate) fn seat(id: &str, name: &str, agent: AgentKind) -> SeatRecord {
    SeatRecord {
        id: id.into(),
        name: name.into(),
        agent,
        model: None,
        reasoning_effort: None,
        instructions: None,
        native_session_id: None,
        status: SeatStatus::Active,
    }
}

pub(crate) fn git_init(path: &std::path::Path) {
    let output = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .arg(path)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}
