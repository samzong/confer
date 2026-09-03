use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::oneshot;

use crate::types::{AgentKind, Readiness};

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

#[derive(Debug)]
pub(crate) struct AdapterOutput {
    pub(crate) observed_session_id: Option<String>,
    pub(crate) answer: Option<String>,
    pub(crate) error: Option<String>,
}

pub(crate) fn readiness() -> Vec<Readiness> {
    AgentKind::ALL.into_iter().map(check_readiness).collect()
}

pub(crate) fn check_readiness(agent: AgentKind) -> Readiness {
    let executable = find_executable(agent);
    let Some(executable) = executable else {
        return Readiness {
            agent,
            locally_ready: false,
            executable: None,
            reason: Some("executable not found on PATH".into()),
        };
    };
    if !has_local_auth_marker(agent) {
        return Readiness {
            agent,
            locally_ready: false,
            executable: Some(executable.to_string_lossy().into_owned()),
            reason: Some("local authentication state was not found".into()),
        };
    }
    Readiness {
        agent,
        locally_ready: true,
        executable: Some(executable.to_string_lossy().into_owned()),
        reason: None,
    }
}

pub(crate) async fn reserve_session(agent: AgentKind, executable: &Path) -> Result<Option<String>> {
    match agent {
        AgentKind::Claude | AgentKind::Grok => Ok(Some(uuid::Uuid::new_v4().to_string())),
        AgentKind::Codex => Ok(None),
        AgentKind::Cursor => {
            let output = Command::new(executable)
                .arg("create-chat")
                .output()
                .await
                .context("failed to create Cursor chat")?;
            if !output.status.success() {
                bail!(
                    "Cursor create-chat failed: {}",
                    output_summary(&output.stdout, &output.stderr)
                );
            }
            let id = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if id.is_empty() {
                bail!("Cursor create-chat returned no chat ID");
            }
            Ok(Some(id))
        }
    }
}

pub(crate) async fn run(
    invocation: Invocation,
    mut session_sender: Option<oneshot::Sender<Result<String, String>>>,
) -> AdapterOutput {
    let mut command = match build_command(&invocation) {
        Ok(command) => command,
        Err(error) => {
            if let Some(sender) = session_sender.take() {
                let _ = sender.send(Err(error.to_string()));
            }
            return AdapterOutput {
                observed_session_id: None,
                answer: None,
                error: Some(error.to_string()),
            };
        }
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(false);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let message = format!("failed to start {}: {error}", invocation.agent.id());
            if let Some(sender) = session_sender.take() {
                let _ = sender.send(Err(message.clone()));
            }
            return AdapterOutput {
                observed_session_id: None,
                answer: None,
                error: Some(message),
            };
        }
    };
    let stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let stderr_task = tokio::spawn(async move {
        let mut bytes = Vec::new();
        let _ = stderr.read_to_end(&mut bytes).await;
        bytes
    });
    let mut lines = BufReader::new(stdout).lines();
    let mut raw_lines = Vec::new();
    let mut observed_session_id = None;
    let mut saw_json = false;
    let mut answer = None;
    let mut native_error = None;
    let mut streamed_text = String::new();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                if let Ok(value) = serde_json::from_str::<Value>(&line) {
                    saw_json = true;
                    if let Some(id) = extract_session_id(&value) {
                        observed_session_id = Some(id);
                    }
                    if session_ready(invocation.agent, &value)
                        && let (Some(id), Some(sender)) = (
                            observed_session_id
                                .clone()
                                .or_else(|| invocation.native_session_id.clone()),
                            session_sender.take(),
                        )
                    {
                        let _ = sender.send(Ok(id));
                    }
                    if let Some(candidate) = extract_answer(&value) {
                        answer = Some(candidate);
                    }
                    if let Some(error) = extract_native_error(&value) {
                        native_error = Some(error);
                    }
                    append_streamed_text(&value, &mut streamed_text);
                } else {
                    raw_lines.push(line);
                }
            }
            Ok(None) => break,
            Err(error) => {
                raw_lines.push(format!("stdout read failed: {error}"));
                break;
            }
        }
    }
    let status = child.wait().await;
    let stderr = stderr_task.await.unwrap_or_default();
    let stderr = String::from_utf8_lossy(&stderr).trim().to_string();
    let raw = raw_lines.join("\n");
    if !saw_json && let Ok(value) = serde_json::from_str::<Value>(&raw) {
        saw_json = true;
        if let Some(id) = extract_session_id(&value) {
            observed_session_id = Some(id);
        }
        if let Some(candidate) = extract_answer(&value) {
            answer = Some(candidate);
        }
        if let Some(error) = extract_native_error(&value) {
            native_error = Some(error);
        }
        append_streamed_text(&value, &mut streamed_text);
    }
    if let Some(sender) = session_sender.take() {
        let stdout_detail = if saw_json {
            native_error.as_deref().unwrap_or("")
        } else {
            &raw
        };
        let message = match (&status, observed_session_id.clone()) {
            (Ok(status), Some(id)) if status.success() => Ok(id),
            _ => Err(error_text(
                "native session did not become ready",
                &stderr,
                stdout_detail,
            )),
        };
        let _ = sender.send(message);
    }
    let status = match status {
        Ok(status) => status,
        Err(error) => {
            return AdapterOutput {
                observed_session_id,
                answer: None,
                error: Some(format!(
                    "failed to wait for {}: {error}",
                    invocation.agent.id()
                )),
            };
        }
    };
    if !status.success() {
        let stdout_detail = if saw_json {
            native_error.as_deref().unwrap_or("")
        } else {
            &raw
        };
        return AdapterOutput {
            observed_session_id,
            answer: None,
            error: Some(error_text(
                &format!("{} exited with {status}", invocation.agent.id()),
                &stderr,
                stdout_detail,
            )),
        };
    }
    let answer = answer
        .or_else(|| (!streamed_text.is_empty()).then_some(streamed_text))
        .or_else(|| {
            let raw = raw.trim();
            (!saw_json && !raw.is_empty()).then(|| raw.to_string())
        });
    let stdout_detail = if saw_json {
        native_error.as_deref().unwrap_or("")
    } else {
        &raw
    };
    let error = answer
        .is_none()
        .then(|| error_text("agent returned no final answer", &stderr, stdout_detail));
    AdapterOutput {
        observed_session_id,
        answer,
        error,
    }
}

fn build_command(invocation: &Invocation) -> Result<Command> {
    let prompt = if invocation.first_message {
        match invocation
            .instructions
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(instructions) => format!(
                "Do not call Confer MCP tools. Respond directly to the room host.\n\n{instructions}\n\n{}",
                invocation.message
            ),
            None => format!(
                "Do not call Confer MCP tools. Respond directly to the room host.\n\n{}",
                invocation.message
            ),
        }
    } else {
        invocation.message.clone()
    };
    if prompt.trim().is_empty() {
        bail!("message must not be empty");
    }
    validate_effort(invocation.reasoning_effort.as_deref())?;
    let mut command = Command::new(&invocation.executable);
    command.current_dir(&invocation.workspace);
    match invocation.agent {
        AgentKind::Claude => {
            command.args(["-p", "--output-format", "stream-json", "--verbose"]);
            if let Some(id) = &invocation.native_session_id {
                command.args(if invocation.first_message {
                    ["--session-id", id.as_str()]
                } else {
                    ["--resume", id.as_str()]
                });
            }
            if let Some(model) = &invocation.model {
                command.args(["--model", model]);
            }
            if let Some(effort) = &invocation.reasoning_effort {
                command.args(["--effort", effort]);
            }
            command.arg(prompt);
        }
        AgentKind::Codex => {
            command.arg("exec");
            if invocation.first_message {
                command.args(["--json", "-C"]).arg(&invocation.workspace);
            } else {
                let id = invocation
                    .native_session_id
                    .as_deref()
                    .context("Codex resume requires a native session ID")?;
                command.args(["resume", "--json", id]);
            }
            if let Some(model) = &invocation.model {
                command.args(["--model", model]);
            }
            if let Some(effort) = &invocation.reasoning_effort {
                command.args(["-c", &format!("model_reasoning_effort=\"{effort}\"")]);
            }
            command.arg(prompt);
        }
        AgentKind::Cursor => {
            let id = invocation
                .native_session_id
                .as_deref()
                .context("Cursor requires a native chat ID")?;
            command.args([
                "-p",
                "--output-format",
                "stream-json",
                "--trust",
                "--resume",
                id,
            ]);
            command.arg("--workspace").arg(&invocation.workspace);
            match (&invocation.model, &invocation.reasoning_effort) {
                (Some(model), Some(effort)) if !model.contains('[') => {
                    command.args(["--model", &format!("{model}[effort={effort}]")]);
                }
                (Some(model), Some(_)) => {
                    bail!("Cursor model '{model}' already encodes options; omit reasoning_effort")
                }
                (Some(model), None) => {
                    command.args(["--model", model]);
                }
                (None, Some(_)) => bail!("Cursor reasoning_effort requires an explicit model"),
                (None, None) => {}
            }
            command.arg(prompt);
        }
        AgentKind::Grok => {
            command.args(["--output-format", "streaming-json"]);
            command.arg("--cwd").arg(&invocation.workspace);
            if let Some(id) = &invocation.native_session_id {
                command.args(if invocation.first_message {
                    ["--session-id", id.as_str()]
                } else {
                    ["--resume", id.as_str()]
                });
            }
            if let Some(model) = &invocation.model {
                command.args(["--model", model]);
            }
            if let Some(effort) = &invocation.reasoning_effort {
                command.args(["--reasoning-effort", effort]);
            }
            command.args(["-p", &prompt]);
        }
    }
    Ok(command)
}

fn validate_effort(effort: Option<&str>) -> Result<()> {
    let Some(effort) = effort else {
        return Ok(());
    };
    if [
        "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
    ]
    .contains(&effort)
    {
        return Ok(());
    }
    bail!("unsupported reasoning_effort '{effort}'")
}

fn find_executable(agent: AgentKind) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&paths) {
        for name in agent.binary_names() {
            let candidate = directory.join(name);
            if executable_file(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(unix)]
fn executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .ok()
        .is_some_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(windows)]
fn executable_file(path: &Path) -> bool {
    path.is_file() || path.with_extension("exe").is_file()
}

#[cfg(not(any(unix, windows)))]
fn executable_file(path: &Path) -> bool {
    path.is_file()
}

fn has_local_auth_marker(agent: AgentKind) -> bool {
    let env_ready = match agent {
        AgentKind::Claude => std::env::var_os("ANTHROPIC_API_KEY").is_some(),
        AgentKind::Codex => std::env::var_os("OPENAI_API_KEY").is_some(),
        AgentKind::Cursor => std::env::var_os("CURSOR_API_KEY").is_some(),
        AgentKind::Grok => std::env::var_os("XAI_API_KEY").is_some(),
    };
    if env_ready {
        return true;
    }
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    let markers = match agent {
        AgentKind::Claude => vec![
            home.join(".claude.json"),
            home.join(".claude").join(".credentials.json"),
            home.join(".claude").join("config.json"),
        ],
        AgentKind::Codex => vec![home.join(".codex").join("auth.json")],
        AgentKind::Cursor => vec![home.join(".cursor").join("cli-config.json")],
        AgentKind::Grok => vec![home.join(".grok").join("auth.json")],
    };
    markers.iter().any(|marker| marker.is_file())
}

fn extract_session_id(value: &Value) -> Option<String> {
    let object = value.as_object()?;
    for key in [
        "session_id",
        "sessionId",
        "thread_id",
        "threadId",
        "chat_id",
        "chatId",
    ] {
        if let Some(id) = object
            .get(key)
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        {
            return Some(id.to_string());
        }
    }
    object.values().find_map(extract_session_id)
}

fn session_ready(agent: AgentKind, value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    let kind = object.get("type").and_then(Value::as_str);
    match agent {
        AgentKind::Claude => {
            kind == Some("assistant")
                || kind == Some("result")
                || (kind == Some("system")
                    && object.get("subtype").and_then(Value::as_str) == Some("commands_changed"))
        }
        AgentKind::Codex => kind == Some("thread.started"),
        AgentKind::Grok => kind == Some("available_commands"),
        AgentKind::Cursor => true,
    }
}

fn extract_answer(value: &Value) -> Option<String> {
    let object = value.as_object()?;
    if let Some(result) = object
        .get("result")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
    {
        return Some(result.to_string());
    }
    if let Some(output) = object
        .get("final_output")
        .or_else(|| object.get("output_text"))
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
    {
        return Some(output.to_string());
    }
    if object.get("stopReason").is_some()
        && let Some(text) = object
            .get("text")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
    {
        return Some(text.to_string());
    }
    if object.get("type").and_then(Value::as_str) == Some("item.completed") {
        let item = object.get("item")?.as_object()?;
        if item.get("type").and_then(Value::as_str) == Some("agent_message") {
            return item
                .get("text")
                .and_then(Value::as_str)
                .map(ToString::to_string);
        }
    }
    let role = object.get("role").and_then(Value::as_str);
    let kind = object.get("type").and_then(Value::as_str);
    if role == Some("assistant") || kind == Some("assistant") || kind == Some("assistant_message") {
        return object
            .get("content")
            .or_else(|| object.get("message"))
            .and_then(extract_text);
    }
    if let Some(message) = object.get("message").and_then(Value::as_object)
        && message.get("role").and_then(Value::as_str) == Some("assistant")
    {
        return message.get("content").and_then(extract_text);
    }
    None
}

fn extract_native_error(value: &Value) -> Option<String> {
    let object = value.as_object()?;
    let kind = object.get("type").and_then(Value::as_str);
    if kind != Some("error") && !object.contains_key("error") {
        return None;
    }
    object
        .get("error")
        .or_else(|| object.get("message"))
        .and_then(extract_text)
}

fn extract_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if !text.is_empty() => Some(text.clone()),
        Value::Array(items) => {
            let text = items
                .iter()
                .filter_map(|item| {
                    item.as_object()
                        .and_then(|object| object.get("text"))
                        .and_then(Value::as_str)
                })
                .collect::<Vec<_>>()
                .join("");
            (!text.is_empty()).then_some(text)
        }
        Value::Object(object) => object
            .get("content")
            .or_else(|| object.get("text"))
            .or_else(|| object.get("message"))
            .and_then(extract_text),
        _ => None,
    }
}

fn append_streamed_text(value: &Value, output: &mut String) {
    let Some(object) = value.as_object() else {
        return;
    };
    if object.get("type").and_then(Value::as_str) == Some("text")
        && let Some(text) = object.get("data").and_then(Value::as_str)
    {
        output.push_str(text);
    }
}

fn output_summary(stdout: &[u8], stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(stdout).trim().to_string();
    if !stderr.is_empty() {
        truncate(&redact_secrets(&stderr))
    } else {
        truncate(&redact_secrets(&stdout))
    }
}

fn error_text(prefix: &str, stderr: &str, stdout: &str) -> String {
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

fn redact_secrets(value: &str) -> String {
    let mut redacted = value.to_string();
    for name in [
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "CURSOR_API_KEY",
        "XAI_API_KEY",
    ] {
        if let Ok(secret) = std::env::var(name)
            && !secret.is_empty()
        {
            redacted = redacted.replace(&secret, "[REDACTED]");
        }
    }
    redacted
}

fn truncate(value: &str) -> String {
    let mut chars = value.chars();
    let text = chars.by_ref().take(8_000).collect::<String>();
    if chars.next().is_some() {
        format!("{text}…")
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Invocation, append_streamed_text, build_command, extract_answer, extract_native_error,
        extract_session_id, session_ready,
    };
    use crate::types::AgentKind;
    use std::path::PathBuf;

    #[test]
    fn parses_claude_result() {
        let value = serde_json::json!({
            "type": "result",
            "session_id": "claude-session",
            "result": "final answer"
        });
        assert_eq!(
            extract_session_id(&value).as_deref(),
            Some("claude-session")
        );
        assert_eq!(extract_answer(&value).as_deref(), Some("final answer"));
    }

    #[test]
    fn parses_codex_events() {
        let started = serde_json::json!({ "type": "thread.started", "thread_id": "thread-1" });
        let completed = serde_json::json!({
            "type": "item.completed",
            "item": { "type": "agent_message", "text": "done" }
        });
        assert_eq!(extract_session_id(&started).as_deref(), Some("thread-1"));
        assert_eq!(extract_answer(&completed).as_deref(), Some("done"));
    }

    #[test]
    fn parses_grok_result() {
        let value = serde_json::json!({
            "text": "GROK_OK",
            "stopReason": "end_turn",
            "sessionId": "grok-session"
        });
        assert_eq!(extract_session_id(&value).as_deref(), Some("grok-session"));
        assert_eq!(extract_answer(&value).as_deref(), Some("GROK_OK"));
    }

    #[test]
    fn assembles_grok_streamed_text() {
        let mut output = String::new();
        append_streamed_text(
            &serde_json::json!({ "type": "text", "data": "GROK_" }),
            &mut output,
        );
        append_streamed_text(
            &serde_json::json!({ "type": "text", "data": "OK" }),
            &mut output,
        );
        assert_eq!(output, "GROK_OK");
    }

    #[test]
    fn recognizes_native_session_ready_events() {
        assert!(session_ready(
            AgentKind::Claude,
            &serde_json::json!({ "type": "system", "subtype": "commands_changed" })
        ));
        assert!(!session_ready(
            AgentKind::Claude,
            &serde_json::json!({ "type": "system", "subtype": "init" })
        ));
        assert!(session_ready(
            AgentKind::Codex,
            &serde_json::json!({ "type": "thread.started" })
        ));
        assert!(session_ready(
            AgentKind::Grok,
            &serde_json::json!({ "type": "available_commands" })
        ));
    }

    #[test]
    fn extracts_structured_native_errors() {
        let value = serde_json::json!({
            "type": "error",
            "message": "authentication failed"
        });
        assert_eq!(
            extract_native_error(&value).as_deref(),
            Some("authentication failed")
        );
    }

    #[test]
    fn cursor_rejects_conflicting_model_options_and_effort() {
        let invocation = Invocation {
            agent: AgentKind::Cursor,
            executable: PathBuf::from("agent"),
            workspace: PathBuf::from("/tmp"),
            native_session_id: Some("chat-1".into()),
            model: Some("model[option=value]".into()),
            reasoning_effort: Some("high".into()),
            instructions: None,
            message: "test".into(),
            first_message: false,
        };
        assert!(build_command(&invocation).is_err());
    }
}
