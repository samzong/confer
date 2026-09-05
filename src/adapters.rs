mod acp;
#[cfg(test)]
mod acp_tests;
mod bridge;
#[cfg(all(test, unix))]
mod cli_tests;
mod codex;
mod native;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use anyhow::{Result, bail};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;

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

pub(crate) fn reserve_session(agent: AgentKind) -> Option<String> {
    match agent {
        AgentKind::Claude | AgentKind::Grok => Some(uuid::Uuid::new_v4().to_string()),
        AgentKind::Codex | AgentKind::Cursor | AgentKind::Agy => None,
    }
}

pub(crate) async fn run(invocation: Invocation) -> AdapterOutput {
    if let Err(error) = validate_invocation(&invocation) {
        return AdapterOutput::failed(error.to_string());
    }
    match invocation.agent {
        AgentKind::Grok | AgentKind::Cursor => native::run(invocation).await,
        _ => bridge::run(invocation).await,
    }
}

impl AdapterOutput {
    fn failed(error: String) -> Self {
        Self {
            observed_session_id: None,
            answer: None,
            error: Some(error),
        }
    }
}

async fn reap(
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

struct StderrCapture {
    task: tokio::task::JoinHandle<()>,
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl StderrCapture {
    fn start(mut stderr: impl tokio::io::AsyncRead + Unpin + Send + 'static) -> Self {
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

    async fn finish(mut self) -> Vec<u8> {
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

async fn run_cli(invocation: Invocation, prompt: &str) -> AdapterOutput {
    let mut command = match build_command(&invocation, prompt) {
        Ok(command) => command,
        Err(error) => {
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
            return AdapterOutput {
                observed_session_id: None,
                answer: None,
                error: Some(message),
            };
        }
    };
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = StderrCapture::start(child.stderr.take().expect("piped stderr"));
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
    let stderr = stderr.finish().await;
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
    if let Some(error) = native_error {
        return AdapterOutput {
            observed_session_id,
            answer: None,
            error: Some(error_text(&error, &stderr, "")),
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

fn prompt_text(invocation: &Invocation) -> String {
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
}

fn validate_invocation(invocation: &Invocation) -> Result<()> {
    if invocation.message.trim().is_empty() {
        bail!("message must not be empty");
    }
    validate_seat_config(
        invocation.agent,
        invocation.model.as_deref(),
        invocation.reasoning_effort.as_deref(),
    )
}

pub(crate) fn validate_seat_config(
    agent: AgentKind,
    model: Option<&str>,
    effort: Option<&str>,
) -> Result<()> {
    validate_effort(effort)?;
    if agent == AgentKind::Agy
        && let Some(effort) = effort
        && !["low", "medium", "high"].contains(&effort)
    {
        bail!("unsupported Antigravity reasoning_effort '{effort}'");
    }
    if agent == AgentKind::Cursor {
        cursor_config(model, effort)?;
    }
    Ok(())
}

fn cursor_config<'a>(
    model: Option<&'a str>,
    effort: Option<&'a str>,
) -> Result<Vec<(&'a str, &'a str)>> {
    if effort.is_some() && model.is_some_and(|model| model.contains('[')) {
        bail!("Cursor model already encodes options; omit reasoning_effort");
    }
    let mut options = Vec::new();
    if let Some(model) = model {
        if let Some((base, parameters)) = model.split_once('[') {
            let Some(parameters) = parameters.strip_suffix(']') else {
                bail!("invalid Cursor model options");
            };
            if base.is_empty() {
                bail!("Cursor model must not be empty");
            }
            options.push(("model", base));
            for parameter in parameters.split(',') {
                let Some((name, value)) = parameter.split_once('=') else {
                    bail!("invalid Cursor model option");
                };
                if name.is_empty() || value.is_empty() {
                    bail!("invalid Cursor model option");
                }
                options.push((name, value));
            }
        } else {
            options.push(("model", model));
        }
    }
    if let Some(effort) = effort {
        options.push(("effort", effort));
    }
    Ok(options)
}

fn build_command(invocation: &Invocation, prompt: &str) -> Result<Command> {
    validate_invocation(invocation)?;
    let mut command = invocation.command();
    match invocation.agent {
        AgentKind::Claude => {
            command.args([
                "-p",
                "--output-format",
                "stream-json",
                "--verbose",
                "--dangerously-skip-permissions",
            ]);
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
        AgentKind::Agy => {
            command.args([
                "-p",
                prompt,
                "--output-format",
                "json",
                "--disable-slash-commands",
                "--dangerously-skip-permissions",
            ]);
            command.arg("--add-dir").arg(&invocation.workspace);
            if let Some(id) = &invocation.native_session_id {
                command.args(["--conversation", id]);
            } else if !invocation.first_message {
                bail!("Antigravity resume requires a native conversation ID");
            }
            if let Some(model) = &invocation.model {
                command.args(["--model", model]);
            }
            if let Some(effort) = &invocation.reasoning_effort {
                command.args(["--effort", effort]);
            }
        }
        AgentKind::Codex | AgentKind::Grok | AgentKind::Cursor => {
            bail!("agent requires its ACP transport")
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
        AgentKind::Agy => false,
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
        AgentKind::Agy => vec![
            home.join(".gemini")
                .join("antigravity-cli")
                .join("jetski_state.pbtxt"),
            home.join(".gemini")
                .join("antigravity-cli")
                .join("settings.json"),
            home.join(".gemini")
                .join("antigravity-cli")
                .join("installation_id"),
        ],
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
        "conversation_id",
        "conversationId",
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

fn extract_answer(value: &Value) -> Option<String> {
    let object = value.as_object()?;
    if let Some(result) = object
        .get("result")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
    {
        return Some(result.to_string());
    }
    if let Some(response) = object
        .get("response")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
    {
        return Some(response.to_string());
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
    if kind == Some("result") && object.get("is_error").and_then(Value::as_bool) == Some(true) {
        let errors = object
            .get("errors")
            .and_then(Value::as_array)
            .map(|errors| {
                errors
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .filter(|errors| !errors.is_empty());
        return errors
            .or_else(|| object.get("result").and_then(extract_text))
            .or_else(|| Some("native agent reported a failed result".into()));
    }
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
        extract_session_id,
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
    fn parses_agy_result() {
        let value = serde_json::json!({
            "conversation_id": "agy-conv-123",
            "status": "SUCCESS",
            "response": "AGY_OK\n",
            "duration_seconds": 1.2
        });
        assert_eq!(extract_session_id(&value).as_deref(), Some("agy-conv-123"));
        assert_eq!(extract_answer(&value).as_deref(), Some("AGY_OK\n"));
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
        assert!(super::validate_invocation(&invocation).is_err());
    }

    #[test]
    fn builds_agy_command_for_first_and_resume_messages() {
        let first = Invocation {
            agent: AgentKind::Agy,
            executable: PathBuf::from("agy"),
            workspace: PathBuf::from("/workspace"),
            native_session_id: None,
            model: Some("gemini-3.8-flash-high".into()),
            reasoning_effort: Some("high".into()),
            instructions: Some("Do not edit files.".into()),
            message: "Analyze this".into(),
            first_message: true,
        };
        let command = build_command(&first, &super::prompt_text(&first)).unwrap();
        let debug = format!("{command:?}");
        assert!(debug.contains("--add-dir"));
        assert!(debug.contains("/workspace"));
        assert!(debug.contains("--model"));
        assert!(debug.contains("gemini-3.8-flash-high"));
        assert!(debug.contains("--effort"));
        assert!(debug.contains("high"));
        assert!(debug.contains("--output-format"));
        assert!(debug.contains("json"));

        let resume = Invocation {
            agent: AgentKind::Agy,
            executable: PathBuf::from("agy"),
            workspace: PathBuf::from("/workspace"),
            native_session_id: Some("conv-456".into()),
            model: None,
            reasoning_effort: None,
            instructions: None,
            message: "Next step".into(),
            first_message: false,
        };
        let command = build_command(&resume, &super::prompt_text(&resume)).unwrap();
        let debug = format!("{command:?}");
        assert!(debug.contains("--conversation"));
        assert!(debug.contains("conv-456"));

        let invalid_resume = Invocation {
            agent: AgentKind::Agy,
            executable: PathBuf::from("agy"),
            workspace: PathBuf::from("/workspace"),
            native_session_id: None,
            model: None,
            reasoning_effort: None,
            instructions: None,
            message: "Next step".into(),
            first_message: false,
        };
        assert!(build_command(&invalid_resume, &super::prompt_text(&invalid_resume)).is_err());

        let invalid_effort = Invocation {
            agent: AgentKind::Agy,
            executable: PathBuf::from("agy"),
            workspace: PathBuf::from("/workspace"),
            native_session_id: None,
            model: None,
            reasoning_effort: Some("xhigh".into()),
            instructions: None,
            message: "Analyze this".into(),
            first_message: true,
        };
        assert!(build_command(&invalid_effort, &super::prompt_text(&invalid_effort)).is_err());
    }
}
