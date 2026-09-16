use super::Invocation;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};

pub(super) async fn reap(
    child: &mut tokio::process::Child,
) -> std::io::Result<Option<std::process::ExitStatus>> {
    match tokio::time::timeout(std::time::Duration::from_secs(3), child.wait()).await {
        Ok(status) => status.map(Some),
        Err(_) => {
            child.start_kill()?;
            child.wait().await?;
            Ok(None)
        }
    }
}

pub(super) struct StderrCapture {
    task: tokio::task::JoinHandle<()>,
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl StderrCapture {
    pub(super) fn start(mut stderr: impl tokio::io::AsyncRead + Unpin + Send + 'static) -> Self {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let captured = bytes.clone();
        let task = tokio::spawn(async move {
            let mut chunk = [0; 8192];
            while let Ok(count) = stderr.read(&mut chunk).await {
                if count == 0 {
                    break;
                }
                let mut bytes = captured.lock().expect("stderr capture lock");
                let excess = (bytes.len() + count).saturating_sub(65536);
                bytes.drain(..excess);
                bytes.extend_from_slice(&chunk[..count]);
            }
        });
        Self { task, bytes }
    }

    pub(super) async fn finish(mut self) -> Vec<u8> {
        if tokio::time::timeout(std::time::Duration::from_secs(1), &mut self.task)
            .await
            .is_err()
        {
            self.task.abort();
            let _ = self.task.await;
        }
        std::mem::take(&mut *self.bytes.lock().expect("stderr capture lock"))
    }
}

pub(super) fn error_text(prefix: &str, stderr: &str, stdout: &str) -> String {
    let detail = if !stderr.trim().is_empty() {
        stderr.trim()
    } else {
        stdout.trim()
    };
    if detail.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}: {}", truncate(&redact_secrets(detail)))
    }
}

pub(super) fn redact_secrets(value: &str) -> String {
    let mut redacted = value.to_string();
    for name in [
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "CURSOR_API_KEY",
        "XAI_API_KEY",
        "COPILOT_GITHUB_TOKEN",
        "GH_TOKEN",
        "GITHUB_TOKEN",
        "COPILOT_PROVIDER_API_KEY",
        "COPILOT_PROVIDER_BEARER_TOKEN",
    ] {
        if let Ok(secret) = std::env::var(name)
            && !secret.is_empty()
        {
            redacted = redacted.replace(&secret, "[REDACTED]");
        }
    }
    redacted
}

pub(super) fn truncate(value: &str) -> String {
    let mut chars = value.chars();
    let text = chars.by_ref().take(8_000).collect::<String>();
    if chars.next().is_some() {
        format!("{text}…")
    } else {
        text
    }
}

pub(super) fn spawn(
    command: &mut Command,
    invocation: &Invocation,
    interactive: bool,
) -> anyhow::Result<(Child, StderrCapture)> {
    let mut child = command
        .stdin(if interactive {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(false)
        .spawn()
        .map_err(|error| anyhow::anyhow!("failed to start {}: {error}", invocation.agent.id()))?;
    let stderr = StderrCapture::start(child.stderr.take().expect("piped stderr"));
    Ok((child, stderr))
}
