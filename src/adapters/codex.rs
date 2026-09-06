use std::process::Stdio;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{ChildStdin, ChildStdout};

use super::{AdapterOutput, Invocation, error_text};

struct Connection {
    input: ChildStdin,
    output: Lines<BufReader<ChildStdout>>,
    next_id: u64,
    events: Vec<Value>,
}

impl Connection {
    async fn write(&mut self, value: Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(&value)?;
        bytes.push(b'\n');
        self.input.write_all(&bytes).await?;
        self.input.flush().await?;
        Ok(())
    }

    async fn read(&mut self) -> Result<Value> {
        let line = self
            .output
            .next_line()
            .await?
            .context("Codex app-server closed its output")?;
        serde_json::from_str(&line).context("invalid Codex app-server JSON")
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        self.write(json!({"id":id,"method":method,"params":params}))
            .await?;
        loop {
            let value = self.read().await?;
            if value.get("id") == Some(&json!(id)) && value.get("method").is_none() {
                if let Some(error) = value.get("error") {
                    bail!(
                        "{method}: {}",
                        error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("native request failed")
                    );
                }
                return value
                    .get("result")
                    .cloned()
                    .context("Codex response has no result");
            }
            if value.get("id").is_some() {
                self.respond_to_request(&value).await?;
            } else if method == "turn/start"
                && matches!(
                    value["method"].as_str(),
                    Some("item/completed" | "turn/completed")
                )
            {
                self.events.push(value);
            }
        }
    }

    async fn respond_to_request(&mut self, request: &Value) -> Result<()> {
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        let response = match method {
            "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                json!({"id":request["id"],"result":{"decision":"accept"}})
            }
            "item/tool/requestUserInput" => json!({"id":request["id"],"result":{"answers":{}}}),
            _ => {
                json!({"id":request["id"],"error":{"code":-32601,"message":"unsupported non-interactive request"}})
            }
        };
        self.write(response).await
    }

    async fn execute(
        &mut self,
        invocation: &Invocation,
        prompt: &str,
        binding: &mut Option<String>,
    ) -> Result<String> {
        self.request(
            "initialize",
            json!({
                "clientInfo":{"name":"confer","version":env!("CARGO_PKG_VERSION")},
                "capabilities":{"experimentalApi":false}
            }),
        )
        .await?;
        self.write(json!({"method":"initialized"})).await?;
        let mut params = json!({
            "cwd":invocation.workspace,
            "approvalPolicy":"never",
            "sandbox":"danger-full-access"
        });
        if let Some(model) = &invocation.model {
            params["model"] = json!(model);
        }
        let method = if invocation.first_message {
            "thread/start"
        } else {
            params["threadId"] = json!(
                invocation
                    .native_session_id
                    .as_deref()
                    .context("Codex resume requires a native session ID")?
            );
            params["excludeTurns"] = json!(true);
            "thread/resume"
        };
        let response = self.request(method, params).await?;
        let thread = response
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .context("Codex returned no native thread ID")?
            .to_owned();
        *binding = Some(thread.clone());
        if !invocation.first_message && invocation.native_session_id.as_deref() != Some(&thread) {
            bail!("Codex resumed a different native thread");
        }
        let mut params = json!({
            "threadId":thread,
            "input":[{"type":"text","text":prompt}],
            "approvalPolicy":"never",
            "sandboxPolicy":{"type":"dangerFullAccess"}
        });
        if let Some(model) = &invocation.model {
            params["model"] = json!(model);
        }
        if let Some(effort) = &invocation.reasoning_effort {
            params["effort"] = json!(effort);
        }
        let response = self.request("turn/start", params).await?;
        let turn = response
            .pointer("/turn/id")
            .and_then(Value::as_str)
            .context("Codex returned no turn ID")?
            .to_owned();
        let mut pending = std::mem::take(&mut self.events).into_iter();
        let mut answer = None;
        loop {
            let event = match pending.next() {
                Some(event) => event,
                None => self.read().await?,
            };
            if event.get("id").is_some() {
                self.respond_to_request(&event).await?;
                continue;
            }
            let params = &event["params"];
            if params["threadId"].as_str() != Some(&thread) {
                continue;
            }
            match event["method"].as_str() {
                Some("item/completed") if params["turnId"].as_str() == Some(&turn) => {
                    let item = &params["item"];
                    if item["type"] == "agentMessage"
                        && item["phase"] != "commentary"
                        && let Some(text) = item["text"].as_str()
                    {
                        answer = Some(text.to_owned());
                    }
                }
                Some("turn/completed") if params["turn"]["id"].as_str() == Some(&turn) => {
                    if params["turn"]["status"] != "completed" {
                        bail!(
                            "Codex turn {}: {}",
                            params["turn"]["status"],
                            params["turn"]["error"]["message"]
                                .as_str()
                                .unwrap_or("native turn did not complete")
                        );
                    }
                    if let Some(items) = params["turn"]["items"].as_array() {
                        for item in items {
                            if item["type"] == "agentMessage"
                                && item["phase"] != "commentary"
                                && let Some(text) = item["text"].as_str()
                            {
                                answer = Some(text.to_owned());
                            }
                        }
                    }
                    tokio::time::timeout(
                        std::time::Duration::from_secs(3),
                        self.request("thread/unsubscribe", json!({"threadId":thread})),
                    )
                    .await
                    .context("Codex unsubscribe timed out")??;
                    return answer
                        .filter(|s| !s.is_empty())
                        .context("Codex returned no final answer");
                }
                _ => {}
            }
        }
    }
}

pub(super) async fn run(invocation: Invocation, prompt: &str) -> AdapterOutput {
    let mut command = invocation.command();
    command
        .arg("app-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(false);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return AdapterOutput::failed(format!("failed to start codex: {error}")),
    };
    let stderr = super::StderrCapture::start(child.stderr.take().expect("piped stderr"));
    let mut connection = Connection {
        input: child.stdin.take().expect("piped stdin"),
        output: BufReader::new(child.stdout.take().expect("piped stdout")).lines(),
        next_id: 0,
        events: Vec::new(),
    };
    let mut observed_session_id = None;
    let result = connection
        .execute(&invocation, prompt, &mut observed_session_id)
        .await;
    drop(connection);
    let status = super::reap(&mut child).await;
    let stderr = stderr.finish().await;
    let stderr = String::from_utf8_lossy(&stderr);
    let result = match status {
        Ok(None) => result,
        Ok(Some(status)) if status.success() => result,
        Ok(Some(status)) => result.and_then(|_| Err(anyhow::anyhow!("codex exited with {status}"))),
        Err(error) => result.and_then(|_| Err(error.into())),
    };
    match result {
        Ok(answer) => AdapterOutput {
            observed_session_id,
            answer: Some(answer),
            error: None,
        },
        Err(error) => AdapterOutput {
            observed_session_id,
            answer: None,
            error: Some(error_text(&error.to_string(), &stderr, "")),
        },
    }
}
