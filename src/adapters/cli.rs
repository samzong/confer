use super::{AdapterOutput, Invocation, error_text, validate_invocation};
use crate::types::AgentKind;
use anyhow::{Result, bail};
use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
};

#[derive(Default)]
struct CapturedOutput {
    session: Option<String>,
    answer: Option<String>,
    error: Option<String>,
    streamed: String,
    saw_json: bool,
}

impl CapturedOutput {
    fn ingest(&mut self, value: &Value) {
        self.saw_json = true;
        self.session = extract_session_id(value).or(self.session.take());
        self.answer = extract_answer(value).or(self.answer.take());
        self.error = extract_native_error(value).or(self.error.take());
        append_streamed_text(value, &mut self.streamed);
    }
}

pub(super) async fn run(invocation: Invocation, prompt: &str) -> AdapterOutput {
    let mut command = match build_command(&invocation, prompt) {
        Ok(command) => command,
        Err(error) => return AdapterOutput::failed(error.to_string()),
    };
    let (mut child, stderr) = match super::process::spawn(&mut command, &invocation, false) {
        Ok(process) => process,
        Err(error) => return AdapterOutput::failed(error.to_string()),
    };
    let mut lines = BufReader::new(child.stdout.take().expect("piped stdout")).lines();
    let mut raw_lines = Vec::new();
    let mut captured = CapturedOutput::default();
    let mut terminated = false;
    loop {
        let next = tokio::select! {
            biased;
            _ = invocation.cancellation.cancelled() => {
                if !terminated && super::process::terminate(&mut child).await.is_err() {
                    invocation.cancellation.mark_uncertain();
                }
                terminated = true;
                break;
            }
            next = lines.next_line() => next,
        };
        match next {
            Ok(Some(line)) => match serde_json::from_str::<Value>(&line) {
                Ok(value) => {
                    let previous_session = captured.session.clone();
                    captured.ingest(&value);
                    if captured.session != previous_session
                        && let Some(id) = captured.session.as_deref()
                        && let Err(error) = invocation.session_observer.observe(id)
                    {
                        captured.error = Some(error.to_string());
                        if super::process::terminate(&mut child).await.is_err() {
                            invocation.cancellation.mark_uncertain();
                        }
                        terminated = true;
                        break;
                    }
                    if captured.error.is_none()
                        && captured.answer.is_some()
                        && is_final_result(&value)
                    {
                        invocation.cancellation.finish();
                    }
                }
                Err(_) => raw_lines.push(line),
            },
            Ok(None) => break,
            Err(error) => {
                raw_lines.push(format!("stdout read failed: {error}"));
                break;
            }
        }
    }
    let status = if terminated {
        super::process::wait_after_kill(&mut child).await.map(Some)
    } else {
        tokio::select! {
            status = child.wait() => status.map(Some),
            _ = invocation.cancellation.cancelled() => {
                if !terminated && super::process::terminate(&mut child).await.is_err() {
                        invocation.cancellation.mark_uncertain();
                    }
                super::process::wait_after_kill(&mut child).await.map(Some)
            }
        }
    };
    if status.is_err() && invocation.cancellation.is_cancelled() {
        invocation.cancellation.mark_uncertain();
    }
    let stderr = stderr.finish().await;
    let stderr = String::from_utf8_lossy(&stderr);
    let raw = raw_lines.join("\n");
    if !captured.saw_json
        && let Ok(value) = serde_json::from_str::<Value>(&raw)
    {
        captured.ingest(&value);
    }
    let detail = if captured.saw_json {
        captured.error.as_deref().unwrap_or("")
    } else {
        &raw
    };
    let result = match status {
        Err(error) => Err(format!(
            "failed to wait for {}: {error}",
            invocation.agent.id()
        )),
        Ok(Some(status)) if !status.success() => Err(error_text(
            &format!("{} exited with {status}", invocation.agent.id()),
            &stderr,
            detail,
        )),
        _ if captured.error.is_some() => Err(error_text(detail, &stderr, "")),
        _ => captured
            .answer
            .or_else(|| (!captured.streamed.is_empty()).then_some(captured.streamed))
            .or_else(|| {
                (!captured.saw_json && !raw.trim().is_empty()).then(|| raw.trim().to_owned())
            })
            .ok_or_else(|| error_text("agent returned no final answer", &stderr, detail)),
    };
    AdapterOutput::from_result(captured.session, result)
}

pub(super) fn build_command(invocation: &Invocation, prompt: &str) -> Result<Command> {
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
        AgentKind::Codex
        | AgentKind::Grok
        | AgentKind::Cursor
        | AgentKind::Copilot
        | AgentKind::Kimi
        | AgentKind::Devin
        | AgentKind::Opencode => {
            bail!("agent uses a dedicated adapter")
        }
    }
    Ok(command)
}

pub(super) fn extract_session_id(value: &Value) -> Option<String> {
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

pub(super) fn is_final_result(value: &Value) -> bool {
    ["result", "response", "final_output", "output_text"]
        .iter()
        .any(|key| value.get(key).is_some_and(Value::is_string))
        || value.get("stopReason").is_some()
}

pub(super) fn extract_answer(value: &Value) -> Option<String> {
    let object = value.as_object()?;
    for value in [
        object.get("result"),
        object.get("response"),
        object
            .get("final_output")
            .or_else(|| object.get("output_text")),
    ] {
        if let Some(text) = value
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            return Some(text.to_owned());
        }
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

pub(super) fn extract_native_error(value: &Value) -> Option<String> {
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

pub(super) fn extract_text(value: &Value) -> Option<String> {
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

pub(super) fn append_streamed_text(value: &Value, output: &mut String) {
    let Some(object) = value.as_object() else {
        return;
    };
    if object.get("type").and_then(Value::as_str) == Some("text")
        && let Some(text) = object.get("data").and_then(Value::as_str)
    {
        output.push_str(text);
    }
}
