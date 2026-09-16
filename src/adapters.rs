mod acp;
#[cfg(test)]
mod acp_tests;
mod bridge;
mod cli;
#[cfg(all(test, unix))]
mod cli_tests;
mod codex;
mod config;
mod native;
mod process;
mod readiness;
#[cfg(test)]
mod tests;

use crate::types::AgentKind;
use config::validate_invocation;
pub(crate) use config::validate_seat_config;
#[cfg(all(test, unix))]
use process::StderrCapture;
use process::{error_text, reap, redact_secrets, truncate};
pub(crate) use readiness::{check_readiness, find_executable, readiness, resolve_kimi_home};
use std::path::PathBuf;
use tokio::process::Command;

#[derive(Clone, Debug)]
pub(crate) struct Invocation {
    pub(crate) agent: AgentKind,
    pub(crate) executable: PathBuf,
    pub(crate) workspace: PathBuf,
    pub(crate) native_session_id: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) reasoning_effort: Option<String>,
    pub(crate) instructions: Option<String>,
    pub(crate) message: String,
    pub(crate) first_message: bool,
}

impl Invocation {
    fn command(&self) -> Command {
        let mut command = Command::new(&self.executable);
        command
            .current_dir(&self.workspace)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE");
        command
    }
}

#[derive(Debug)]
pub(crate) struct AdapterOutput {
    pub(crate) observed_session_id: Option<String>,
    pub(crate) answer: Option<String>,
    pub(crate) error: Option<String>,
}

pub(crate) fn reserve_session(agent: AgentKind) -> Option<String> {
    match agent {
        AgentKind::Claude | AgentKind::Grok => Some(uuid::Uuid::new_v4().to_string()),
        AgentKind::Codex
        | AgentKind::Cursor
        | AgentKind::Agy
        | AgentKind::Copilot
        | AgentKind::Kimi => None,
    }
}

pub(crate) async fn run(invocation: Invocation) -> AdapterOutput {
    if let Err(error) = validate_invocation(&invocation) {
        return AdapterOutput::failed(error.to_string());
    }
    match invocation.agent {
        AgentKind::Grok | AgentKind::Cursor | AgentKind::Copilot | AgentKind::Kimi => {
            native::run(invocation).await
        }
        _ => bridge::run(invocation).await,
    }
}

impl AdapterOutput {
    fn failed(error: String) -> Self {
        Self::from_result(None, Err(error))
    }

    fn from_result(observed_session_id: Option<String>, result: Result<String, String>) -> Self {
        let (answer, error) = match result {
            Ok(answer) => (Some(answer), None),
            Err(error) => (None, Some(error)),
        };
        Self {
            observed_session_id,
            answer,
            error,
        }
    }
}

fn prompt_text(invocation: &Invocation) -> String {
    let instructions = invocation
        .instructions
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    std::iter::once("Do not call Confer MCP tools. Respond directly to the room host.")
        .chain(instructions)
        .chain(std::iter::once(invocation.message.as_str()))
        .collect::<Vec<_>>()
        .join("\n\n")
}
