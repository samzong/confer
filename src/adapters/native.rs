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
    let (mut child, stderr) = match super::process::spawn(&mut command, &invocation, true) {
        Ok(process) => process,
        Err(error) => return AdapterOutput::failed(error.to_string()),
    };
    let transport = ByteStreams::new(
        child.stdin.take().expect("piped stdin").compat_write(),
        child.stdout.take().expect("piped stdout").compat(),
    );
    let cancellation = invocation.cancellation.clone();
    let mut result = acp::run_connection(transport, invocation, true).await;
    let status = super::process::reap_with_cancellation(&mut child, &cancellation).await;
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
            let kimi_home =
                super::resolve_kimi_home(std::env::var_os("KIMI_CODE_HOME"), dirs::home_dir())
                    .map_err(|error| error.to_string())?;
            command.env("KIMI_CODE_HOME", &kimi_home);
            command.arg("acp");
        }
        AgentKind::Opencode => {
            command
                .env("OPENCODE_PERMISSION", r#"{"*":"allow"}"#)
                .env_remove("OPENCODE_ENABLE_QUESTION_TOOL")
                .args([
                    "acp",
                    "--port",
                    "0",
                    "--hostname",
                    "127.0.0.1",
                    "--mdns=false",
                ]);
        }
        AgentKind::Rx => {
            super::rx::apply_native_args(invocation, command, dirs::home_dir().as_deref())?;
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
    fn rx_native_command_pins_home_and_starts_acp_without_prompting_for_installation() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        std::fs::create_dir_all(home.join(".recall")).unwrap();
        std::fs::create_dir_all(home.join(".rx")).unwrap();
        std::fs::write(
            home.join(".recall/rx.toml"),
            "default_provider = \"tokener\"\n",
        )
        .unwrap();
        std::fs::write(
            home.join(".recall/rx.keys"),
            "tokener = \"not-a-real-key\"\n",
        )
        .unwrap();
        std::fs::write(
            home.join(".rx/auth.json"),
            r#"{"openai-codex":{"type":"oauth","access":"a","refresh":"r","expires":1}}"#,
        )
        .unwrap();

        let build = |model: Option<&str>, env: &[(&str, &str)]| {
            let invocation = Invocation {
                agent: AgentKind::Rx,
                executable: PathBuf::from("rxa"),
                workspace: PathBuf::from("/workspace"),
                native_session_id: None,
                model: model.map(str::to_owned),
                reasoning_effort: Some("high".into()),
                instructions: None,
                message: "Implement this".into(),
                first_message: true,
                cancellation: Default::default(),
                session_observer: Default::default(),
            };
            let mut command = invocation.command();
            super::super::rx::apply_native_args_with(
                &invocation,
                &mut command,
                Some(home),
                None,
                env.iter().map(|(k, v)| ((*k).into(), (*v).into())),
            )
            .unwrap();
            command
        };

        let command = build(None, &[]);
        let debug = format!("{command:?}");
        assert!(debug.contains("RX_HOME"), "{debug}");
        assert!(debug.contains("RX_NO_UPDATE=\"1\""), "{debug}");
        assert!(debug.contains("RX_NO_INSTALL=\"1\""), "{debug}");
        assert!(debug.contains("\"rxa\" \"acp\""), "{debug}");
        assert!(!debug.contains("--model"), "{debug}");
        assert!(!debug.contains("--thinking"), "{debug}");
        let envs = command
            .as_std()
            .get_envs()
            .map(|(k, v)| (k.to_string_lossy().into_owned(), v.is_some()))
            .collect::<Vec<_>>();
        for stripped in ["RX_AGENT_PROVIDER", "RX_HOST_REQUEST", "RX_NO_YOLO"] {
            assert!(envs.contains(&(stripped.into(), false)), "{envs:?}");
        }

        for (model, env, expected) in [
            (
                "tokener/kimi-k3.1",
                &[][..],
                "\"--provider\" \"tokener\" \"acp\" \"--model\" \"tokener/kimi-k3.1\"",
            ),
            (
                "openai-codex/gpt-5.4",
                &[][..],
                "\"--provider\" \"none\" \"acp\" \"--model\" \"openai-codex/gpt-5.4\"",
            ),
            (
                "anthropic/claude-opus-4",
                &[("ANTHROPIC_API_KEY", "not-a-real-key")][..],
                "\"--provider\" \"none\" \"acp\" \"--model\" \"anthropic/claude-opus-4\"",
            ),
        ] {
            let debug = format!("{:?}", build(Some(model), env));
            assert!(debug.contains(expected), "{debug}");
            assert!(!debug.contains("--thinking"), "{debug}");
        }
    }

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
            cancellation: Default::default(),
            session_observer: Default::default(),
        };
        let mut command = invocation.command();
        apply_native_args(&invocation, &mut command).unwrap();
        let debug = format!("{command:?}");
        assert!(debug.contains("acp"), "{debug}");
        assert!(!debug.contains("-p"), "{debug}");
        assert!(debug.contains("KIMI_CODE_HOME"), "{debug}");
    }
}
