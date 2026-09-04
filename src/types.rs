use serde::{Deserialize, Serialize};

pub(crate) const ROOMS_SCHEMA_VERSION: u32 = 3;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum AgentKind {
    Claude,
    Codex,
    Cursor,
    Grok,
    Agy,
}

impl AgentKind {
    pub(crate) const ALL: [Self; 5] = [
        Self::Claude,
        Self::Codex,
        Self::Cursor,
        Self::Grok,
        Self::Agy,
    ];

    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Cursor => "cursor",
            Self::Grok => "grok",
            Self::Agy => "agy",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "claude" | "claude-code" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "cursor" | "cursor-agent" | "agent" => Some(Self::Cursor),
            "grok" | "grok-build" => Some(Self::Grok),
            "agy" | "antigravity" | "antigravity-cli" => Some(Self::Agy),
            _ => None,
        }
    }

    pub(crate) fn binary_names(self) -> &'static [&'static str] {
        match self {
            Self::Claude => &["claude"],
            Self::Codex => &["codex"],
            Self::Cursor => &["agent", "cursor-agent"],
            Self::Grok => &["grok"],
            Self::Agy => &["agy"],
        }
    }

    pub(crate) fn skill_host_id(self) -> Option<&'static str> {
        match self {
            Self::Claude => Some("claude-code"),
            Self::Codex => Some("codex"),
            Self::Cursor => Some("cursor"),
            Self::Grok => Some("grok"),
            Self::Agy => Some("antigravity-cli"),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct HostRecord {
    pub(crate) agent: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct SeatRecord {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) agent: AgentKind,
    pub(crate) model: Option<String>,
    pub(crate) reasoning_effort: Option<String>,
    pub(crate) instructions: Option<String>,
    pub(crate) native_session_id: Option<String>,
    #[serde(default)]
    pub(crate) status: SeatStatus,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SeatStatus {
    #[default]
    Active,
    Retired,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct RoomRecord {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) workspace: String,
    pub(crate) host: HostRecord,
    pub(crate) seats: Vec<SeatRecord>,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct RoomsFile {
    pub(crate) schema_version: u32,
    pub(crate) rooms: Vec<RoomRecord>,
}

impl Default for RoomsFile {
    fn default() -> Self {
        Self {
            schema_version: ROOMS_SCHEMA_VERSION,
            rooms: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Readiness {
    pub(crate) agent: AgentKind,
    pub(crate) locally_ready: bool,
    pub(crate) executable: Option<String>,
    pub(crate) reason: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Replacement {
    pub(crate) seat_name: String,
    pub(crate) requested_agent: String,
    pub(crate) replacement_agent: String,
    pub(crate) reason: String,
}
