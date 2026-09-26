use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use super::{AdapterOutput, Invocation, cli, error_text, validate_invocation};

pub(super) async fn run(invocation: Invocation, prompt: &str) -> AdapterOutput {
    let export_dir = match private_export_dir() {
        Ok(dir) => dir,
        Err(error) => return AdapterOutput::failed(error.to_string()),
    };
    let export = export_dir.path().join("atif-export.json");
    let prompt_file = export_dir.path().join("prompt.txt");
    if let Err(error) = std::fs::write(&prompt_file, prompt) {
        return AdapterOutput::failed(format!("failed to write the devin prompt file: {error}"));
    }
    let mut command = match build_command(&invocation, &prompt_file, &export) {
        Ok(command) => command,
        Err(error) => return AdapterOutput::failed(error.to_string()),
    };
    let (mut child, stderr) = match super::process::spawn(&mut command, &invocation, false) {
        Ok(process) => process,
        Err(error) => return AdapterOutput::failed(error.to_string()),
    };
    let mut lines = BufReader::new(child.stdout.take().expect("piped stdout")).lines();
    let mut raw_lines = Vec::new();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => raw_lines.push(line),
            Ok(None) => break,
            Err(error) => {
                raw_lines.push(format!("stdout read failed: {error}"));
                break;
            }
        }
    }
    let status = super::reap(&mut child).await;
    let stderr = String::from_utf8_lossy(&stderr.finish().await).into_owned();
    let exported = read_export(&export);
    let stdout = raw_lines.join("\n");
    let stdout = stdout.trim();
    let (exported_session, exported_answer) = exported.unwrap_or_default();
    // A resumed turn whose export reports a different session id means devin
    // forked or lost the seat's session; fail the delivery instead of letting
    // the foreign id surface and strand the seat.
    let session_mismatch = match (&invocation.native_session_id, &exported_session) {
        (Some(expected), Some(observed)) if expected != observed => Some(format!(
            "devin resumed session '{expected}' but its export reported '{observed}'"
        )),
        _ => None,
    };
    // A resumed turn whose export omits the session id keeps the seat's
    // existing native session instead of failing the delivery.
    let session = match session_mismatch {
        Some(_) => invocation.native_session_id.clone(),
        None => exported_session.or_else(|| invocation.native_session_id.clone()),
    };
    let result = match status {
        Err(error) => Err(format!(
            "failed to wait for {}: {error}",
            invocation.agent.id()
        )),
        Ok(None) => Err(error_text(
            &format!(
                "{} did not exit within 3s of stdout EOF; terminated",
                invocation.agent.id()
            ),
            &stderr,
            stdout,
        )),
        Ok(Some(status)) if !status.success() => Err(error_text(
            &format!("{} exited with {status}", invocation.agent.id()),
            &stderr,
            stdout,
        )),
        _ if session.is_none() => Err(error_text(
            "devin completed without a session id in its ATIF export",
            &stderr,
            stdout,
        )),
        _ => match session_mismatch {
            Some(mismatch) => Err(error_text(&mismatch, &stderr, stdout)),
            None => exported_answer
                .or_else(|| (!stdout.is_empty()).then(|| stdout.to_owned()))
                .ok_or_else(|| error_text("agent returned no final answer", &stderr, "")),
        },
    };
    AdapterOutput::from_result(session, result)
}

fn private_export_dir() -> Result<tempfile::TempDir> {
    // The export transcript carries private seat instructions; tempfile
    // tempdirs default to 0777 & ~umask, so pin 0700 explicitly.
    let mut builder = tempfile::Builder::new();
    builder.prefix("confer-devin-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    builder
        .tempdir()
        .context("failed to create a private devin export directory")
}

pub(super) fn build_command(
    invocation: &Invocation,
    prompt_file: &Path,
    export: &Path,
) -> Result<Command> {
    validate_invocation(invocation)?;
    let mut command = invocation.command();
    // Seat model and permission mode come from the invocation only, never
    // inherited devin config env vars.
    command
        .env_remove("DEVIN_MODEL")
        .env_remove("DEVIN_PERMISSION_MODE")
        .env_remove("DEVIN_SANDBOX");
    command.args([
        "--permission-mode",
        "dangerous",
        "--respect-workspace-trust",
        "false",
        "--export",
    ]);
    command.arg(export);
    if let Some(id) = &invocation.native_session_id {
        command.arg(format!("--resume={id}"));
    } else if !invocation.first_message {
        bail!("Devin resume requires a native session ID");
    }
    if let Some(model) = &invocation.model {
        command.args(["--model", model]);
    }
    command.arg("-p").arg("--prompt-file").arg(prompt_file);
    Ok(command)
}

fn read_export(path: &Path) -> Option<(Option<String>, Option<String>)> {
    let value: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    // Only the root session_id is authoritative; nested objects may carry ids
    // for subagent trajectories or unrelated sessions.
    let session = value
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_owned);
    let answer = value
        .get("steps")
        .and_then(Value::as_array)
        .and_then(|steps| {
            // Resumed sessions replay earlier turns in the export; only steps
            // after the final user step belong to this delivery.
            let current = steps
                .iter()
                .rposition(|step| step.get("source").and_then(Value::as_str) == Some("user"))?;
            steps[current + 1..].iter().rev().find_map(|step| {
                if step.get("source").and_then(Value::as_str) != Some("agent") {
                    return None;
                }
                step.get("message")
                    .and_then(cli::extract_text)
                    .map(|text| text.trim().to_owned())
                    .filter(|text| !text.is_empty())
            })
        });
    Some((session, answer))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::AgentKind;

    fn invocation() -> Invocation {
        Invocation {
            agent: AgentKind::Devin,
            executable: Path::new("devin").to_owned(),
            workspace: "/workspace".into(),
            native_session_id: None,
            model: None,
            reasoning_effort: None,
            instructions: None,
            message: "Analyze this".into(),
            first_message: true,
        }
    }

    fn args(command: &Command) -> Vec<String> {
        command
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn builds_print_command_with_export_and_resume() {
        let export = Path::new("/tmp/export.json");
        let prompt_file = Path::new("/tmp/prompt.txt");
        let command = build_command(&invocation(), prompt_file, export).unwrap();
        assert_eq!(
            args(&command),
            [
                "--permission-mode",
                "dangerous",
                "--respect-workspace-trust",
                "false",
                "--export",
                "/tmp/export.json",
                "-p",
                "--prompt-file",
                "/tmp/prompt.txt",
            ]
        );

        let mut resume = invocation();
        resume.native_session_id = Some("devin-123".into());
        resume.first_message = false;
        resume.model = Some("opus".into());
        let command = build_command(&resume, prompt_file, export).unwrap();
        assert_eq!(
            args(&command),
            [
                "--permission-mode",
                "dangerous",
                "--respect-workspace-trust",
                "false",
                "--export",
                "/tmp/export.json",
                "--resume=devin-123",
                "--model",
                "opus",
                "-p",
                "--prompt-file",
                "/tmp/prompt.txt",
            ]
        );

        let envs: Vec<_> = command.as_std().get_envs().collect();
        for name in ["DEVIN_MODEL", "DEVIN_PERMISSION_MODE", "DEVIN_SANDBOX"] {
            assert!(
                envs.iter()
                    .any(|(key, value)| **key == *name && value.is_none()),
                "{name} was not cleared: {envs:?}"
            );
        }

        let mut missing = invocation();
        missing.first_message = false;
        assert!(build_command(&missing, prompt_file, export).is_err());

        let mut effort = invocation();
        effort.reasoning_effort = Some("high".into());
        assert!(build_command(&effort, prompt_file, export).is_err());
    }

    #[test]
    fn reads_session_and_current_turn_answer_from_atif_export() {
        let directory = tempfile::tempdir().unwrap();
        let export = directory.path().join("export.json");
        std::fs::write(
            &export,
            serde_json::json!({
                "schema_version": "ATIF-v1.7",
                "session_id": "devin-session-9",
                "agent": {"name": "devin", "version": "3000.10.31"},
                "steps": [
                    {"step_id": 1, "source": "user", "message": "first task"},
                    {"step_id": 2, "source": "agent", "message": "Prior answer"},
                    {"step_id": 3, "source": "user", "message": "current task"},
                    {"step_id": 4, "source": "agent", "message": "Working on it"},
                    {"step_id": 5, "source": "agent", "tool_calls": []},
                    {"step_id": 6, "source": "agent", "message": [{"type": "text", "text": "Final "}, {"type": "text", "text": "answer"}]},
                    {"step_id": 7, "source": "system", "message": "done"}
                ]
            })
            .to_string(),
        )
        .unwrap();
        let (session, answer) = read_export(&export).unwrap();
        assert_eq!(session.as_deref(), Some("devin-session-9"));
        assert_eq!(answer.as_deref(), Some("Final answer"));

        // A turn without an agent message yields no answer rather than leaking
        // the previous turn's text from a resumed export.
        std::fs::write(
            &export,
            serde_json::json!({
                "session_id": "devin-session-9",
                "steps": [
                    {"source": "user", "message": "first task"},
                    {"source": "agent", "message": "Prior answer"},
                    {"source": "user", "message": "current task"},
                    {"source": "agent", "tool_calls": []}
                ]
            })
            .to_string(),
        )
        .unwrap();
        let (session, answer) = read_export(&export).unwrap();
        assert_eq!(session.as_deref(), Some("devin-session-9"));
        assert_eq!(answer, None);

        // Nested session ids never override the root one.
        std::fs::write(
            &export,
            serde_json::json!({
                "steps": [],
                "nested": {"session_id": "subagent-session"}
            })
            .to_string(),
        )
        .unwrap();
        let (session, answer) = read_export(&export).unwrap();
        assert_eq!(session, None);
        assert_eq!(answer, None);

        std::fs::write(&export, "not json").unwrap();
        assert!(read_export(&export).is_none());
        assert!(read_export(&directory.path().join("missing.json")).is_none());
    }
}
