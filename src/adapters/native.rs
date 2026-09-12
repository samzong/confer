use std::process::Stdio;

use agent_client_protocol::ByteStreams;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use super::{AdapterOutput, Invocation, acp, error_text};
use crate::types::AgentKind;
use tokio::process::Command;

pub(super) async fn run(invocation: Invocation) -> AdapterOutput {
    let mut command = invocation.command();
    if let Err(error) = apply_native_args(&invocation, &mut command) {
        return AdapterOutput::failed(error);
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(false);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return AdapterOutput::failed(format!(
                "failed to start {}: {error}",
                invocation.agent.id()
            ));
        }
    };
    let stderr = super::StderrCapture::start(child.stderr.take().expect("piped stderr"));
    let transport = ByteStreams::new(
        child.stdin.take().expect("piped stdin").compat_write(),
        child.stdout.take().expect("piped stdout").compat(),
    );
    let mut result = acp::run_connection(transport, invocation, true).await;
    let status = super::reap(&mut child).await;
    let stderr = stderr.finish().await;
    let stderr = String::from_utf8_lossy(&stderr);
    let failure = match status {
        Ok(Some(status)) if !status.success() => {
            Some(format!("native ACP agent exited with {status}"))
        }
        Err(error) => Some(format!("failed to wait for native ACP agent: {error}")),
        _ => None,
    };
    if let Some(error) = result.error.as_ref().or(failure.as_ref()) {
        result.error = Some(error_text(error, &stderr, ""));
    }
    result
}

fn apply_native_args(invocation: &Invocation, command: &mut Command) -> Result<(), String> {
    match invocation.agent {
        AgentKind::Grok => {
            command.args(["agent", "--no-leader", "--always-approve"]);
            if let Some(model) = &invocation.model {
                command.args(["--model", model]);
            }
            if let Some(effort) = &invocation.reasoning_effort {
                command.args(["--reasoning-effort", effort]);
            }
            command.arg("stdio");
        }
        AgentKind::Cursor => {
            command
                .args(["--trust", "--force", "--workspace"])
                .arg(&invocation.workspace);
            command.arg("acp");
        }
        AgentKind::Copilot => {
            command.args(["--acp", "--allow-all"]);
        }
        AgentKind::Kimi => {
            // Pin the data root so readiness, MCP registration, and the
            // ACP child agree even when KIMI_CODE_HOME is unset or relative.
            let kimi_home =
                super::resolve_kimi_home(std::env::var_os("KIMI_CODE_HOME"), dirs::home_dir())
                    .map_err(|error| error.to_string())?;
            command.env("KIMI_CODE_HOME", &kimi_home);
            command.arg("acp");
        }
        _ => return Err("agent has no native ACP transport".into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn kimi_native_command_pins_home_and_starts_acp() {
        let invocation = Invocation {
            agent: AgentKind::Kimi,
            executable: PathBuf::from("kimi"),
            workspace: PathBuf::from("/workspace"),
            native_session_id: None,
            model: Some("kimi-code/k3".into()),
            reasoning_effort: None,
            instructions: None,
            message: "Analyze this".into(),
            first_message: true,
        };
        let mut command = invocation.command();
        apply_native_args(&invocation, &mut command).unwrap();
        let debug = format!("{command:?}");
        assert!(debug.contains("acp"), "{debug}");
        assert!(!debug.contains("-p"), "{debug}");
        assert!(debug.contains("KIMI_CODE_HOME"), "{debug}");
    }
}
