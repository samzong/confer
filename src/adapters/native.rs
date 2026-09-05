use std::process::Stdio;

use agent_client_protocol::ByteStreams;
use tokio::process::Command;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use super::{AdapterOutput, Invocation, acp, error_text};
use crate::types::AgentKind;

pub(super) async fn run(invocation: Invocation) -> AdapterOutput {
    let mut command = Command::new(&invocation.executable);
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
        _ => return AdapterOutput::failed("agent has no native ACP transport".into()),
    }
    command
        .current_dir(&invocation.workspace)
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
