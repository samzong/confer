use super::Invocation;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};

#[cfg(unix)]
async fn kill_target(id: i32) -> std::io::Result<()> {
    unsafe extern "C" {
        fn kill(pid: std::os::raw::c_int, signal: std::os::raw::c_int) -> std::os::raw::c_int;
    }
    if unsafe { kill(id, 9) } == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::NotFound || error.raw_os_error() == Some(3) {
        return Ok(());
    }
    if error.kind() == std::io::ErrorKind::PermissionDenied
        && process_table()
            .await?
            .iter()
            .filter(|row| {
                if id > 0 {
                    row.id == id
                } else {
                    row.group == -id
                }
            })
            .all(|row| row.zombie)
    {
        Ok(())
    } else {
        Err(error)
    }
}

#[cfg(unix)]
struct ProcessRow {
    id: i32,
    parent: i32,
    group: i32,
    zombie: bool,
}

#[cfg(unix)]
async fn process_table() -> std::io::Result<Vec<ProcessRow>> {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        Command::new("/bin/ps")
            .args(["-A", "-o", "pid=,ppid=,pgid=,stat="])
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "process descendant scan exceeded 1s",
        )
    })??;
    if !output.status.success() {
        return Err(std::io::Error::other("process descendant scan failed"));
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| {
            let mut fields = line.split_whitespace();
            Some(ProcessRow {
                id: fields.next()?.parse().ok()?,
                parent: fields.next()?.parse().ok()?,
                group: fields.next()?.parse().ok()?,
                zombie: fields.next()?.starts_with('Z'),
            })
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| std::io::Error::other("invalid process table"))
}

#[cfg(unix)]
async fn descendant_targets(root: u32, include_root: bool) -> std::io::Result<Vec<i32>> {
    use std::collections::HashSet;
    let rows = process_table().await?;
    let mut descendants = HashSet::from([root as i32]);
    loop {
        let before = descendants.len();
        for row in &rows {
            if descendants.contains(&row.parent) {
                descendants.insert(row.id);
            }
        }
        if descendants.len() == before {
            break;
        }
    }
    let mut targets = HashSet::new();
    if include_root {
        targets.insert(-(root as i32));
    }
    for row in rows {
        if row.id != root as i32 && descendants.contains(&row.id) {
            targets.insert(
                if row.group != root as i32 && descendants.contains(&row.group) {
                    -row.group
                } else {
                    row.id
                },
            );
        }
    }
    Ok(targets.into_iter().collect())
}

pub(super) async fn stop_cancelled_descendants(cancellation: &super::Cancellation) {
    #[cfg(unix)]
    if let Some(root) = cancellation.process_id() {
        match descendant_targets(root, false).await {
            Ok(targets) => {
                for target in targets {
                    if kill_target(target).await.is_err() {
                        cancellation.mark_uncertain();
                    }
                }
            }
            Err(_) => cancellation.mark_uncertain(),
        }
    }
    #[cfg(not(unix))]
    let _ = cancellation;
}

pub(super) async fn terminate(child: &mut Child) -> std::io::Result<()> {
    #[cfg(unix)]
    if let Some(id) = child.id() {
        let targets = descendant_targets(id, true).await;
        let mut result = kill_target(-(id as i32)).await;
        match targets {
            Ok(targets) => {
                for target in targets.into_iter().filter(|target| *target != -(id as i32)) {
                    let killed = kill_target(target).await;
                    if result.is_ok() {
                        result = killed;
                    }
                }
            }
            Err(error) => result = Err(error),
        }
        let killed = if child.try_wait()?.is_some() {
            Ok(())
        } else {
            child.start_kill().or_else(|error| {
                if child.try_wait()?.is_some() {
                    Ok(())
                } else {
                    Err(error)
                }
            })
        };
        return result.and(killed);
    }
    child.start_kill()
}

pub(super) async fn wait_after_kill(
    child: &mut Child,
) -> std::io::Result<std::process::ExitStatus> {
    tokio::time::timeout(std::time::Duration::from_secs(3), child.wait())
        .await
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "process did not exit within 3s after termination",
            )
        })?
}

pub(super) async fn reap(child: &mut Child) -> std::io::Result<Option<std::process::ExitStatus>> {
    match tokio::time::timeout(std::time::Duration::from_secs(3), child.wait()).await {
        Ok(status) => status.map(Some),
        Err(_) => {
            child.start_kill()?;
            child.wait().await?;
            Ok(None)
        }
    }
}

pub(super) async fn reap_with_cancellation(
    child: &mut Child,
    cancellation: &super::Cancellation,
) -> std::io::Result<Option<std::process::ExitStatus>> {
    #[cfg(unix)]
    let group = child.id();
    let result = tokio::select! {
        status = reap(child) => status,
        _ = cancellation.cancelled() => {
            stop_cancelled_descendants(cancellation).await;
            tokio::select! {
                status = child.wait() => status.map(Some),
                _ = cancellation.grace_expired() => {
                    let terminated = terminate(child).await;
                    let reaped = wait_after_kill(child).await.map(|_| None);
                    terminated.and(reaped)
                }
            }
        }
    };
    #[cfg(unix)]
    let result = if cancellation.is_cancelled() && matches!(&result, Ok(Some(_))) {
        match group {
            Some(group) => kill_target(-(group as i32)).await.and(result),
            None => result,
        }
    } else {
        result
    };
    if result.is_err() && cancellation.is_cancelled() {
        cancellation.mark_uncertain();
    }
    result
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
        "WINDSURF_API_KEY",
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
    #[cfg(unix)]
    command.process_group(0);
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
    if let Some(id) = child.id() {
        invocation.cancellation.set_process_id(id);
    }
    let stderr = StderrCapture::start(child.stderr.take().expect("piped stderr"));
    Ok((child, stderr))
}

#[cfg(all(test, unix))]
#[path = "process_tests.rs"]
mod tests;
