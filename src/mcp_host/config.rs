use std::{fs, io, path::Path};

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};

use super::{SERVER_ARG, SERVER_NAME};
use crate::types::AgentKind;

pub(super) fn write_mcp_config(host: AgentKind, path: &Path, bin: &str) -> Result<()> {
    update_mcp_config(path, |config| {
        let servers = config
            .as_object_mut()
            .and_then(|root| {
                root.entry(server_key(host))
                    .or_insert_with(|| Value::Object(Map::new()))
                    .as_object_mut()
            })
            .with_context(|| {
                format!(
                    "invalid {} MCP config: server map must be an object",
                    host.id()
                )
            })?;
        let entry = servers
            .entry(SERVER_NAME)
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .with_context(|| {
                format!("invalid {} MCP config: confer must be an object", host.id())
            })?;
        if uses_non_stdio_transport(host, entry) {
            bail!(
                "cannot install {} MCP config: existing confer entry uses a non-stdio transport",
                host.id()
            );
        }
        if host == AgentKind::Opencode {
            entry.insert("type".into(), Value::String("local".into()));
            entry.insert("command".into(), serde_json::json!([bin, SERVER_ARG]));
            entry.insert("enabled".into(), Value::Bool(true));
        } else {
            entry.insert("type".into(), Value::String("stdio".into()));
            entry.insert("command".into(), Value::String(bin.into()));
            entry.insert("args".into(), serde_json::json!([SERVER_ARG]));
        }
        Ok(())
    })
}

pub(super) fn remove_mcp_config(host: AgentKind, path: &Path) -> Result<()> {
    if !path.is_file() {
        return Ok(());
    }
    update_mcp_config(path, |config| {
        if let Some(servers) = config
            .get_mut(server_key(host))
            .and_then(Value::as_object_mut)
        {
            if servers
                .get(SERVER_NAME)
                .and_then(Value::as_object)
                .is_some_and(|entry| uses_non_stdio_transport(host, entry))
            {
                bail!(
                    "cannot uninstall {} MCP config: existing confer entry uses a non-stdio transport",
                    host.id()
                );
            }
            servers.remove(SERVER_NAME);
        }
        Ok(())
    })
}

fn update_mcp_config(path: &Path, change: impl FnOnce(&mut Value) -> Result<()>) -> Result<()> {
    let mut config = read_mcp_config(path)?;
    change(&mut config)?;
    write_mcp_config_file(path, &config)
}

pub(super) fn read_mcp_config(path: &Path) -> Result<Value> {
    match fs::read_to_string(path) {
        Ok(body) if body.trim().is_empty() => Ok(serde_json::json!({})),
        Ok(body) => serde_json::from_str(&body)
            .with_context(|| format!("failed to parse {}", path.display())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(serde_json::json!({})),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

fn write_mcp_config_file(path: &Path, config: &Value) -> Result<()> {
    let target = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Some(
            fs::canonicalize(path)
                .with_context(|| format!("failed to resolve symbolic link {}", path.display()))?,
        ),
        Ok(_) => None,
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", path.display()));
        }
    };
    let path = target.as_deref().unwrap_or(path);
    crate::state::write_json_atomic(path, config).context("failed to write MCP config")
}

fn server_key(host: AgentKind) -> &'static str {
    if host == AgentKind::Opencode {
        "mcp"
    } else {
        "mcpServers"
    }
}

fn uses_non_stdio_transport(host: AgentKind, entry: &Map<String, Value>) -> bool {
    let transport = if host == AgentKind::Opencode {
        "local"
    } else {
        "stdio"
    };
    entry.contains_key("url") || entry.get("type").is_some_and(|value| value != transport)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_config_update_preserves_unrelated_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        std::fs::write(
            &path,
            r#"{"mcpServers":{"other":{"command":"other"},"confer":{"env":{"A":"B"}}}}"#,
        )
        .unwrap();
        write_mcp_config(AgentKind::Cursor, &path, "/tmp/confer").unwrap();
        let config = read_mcp_config(&path).unwrap();
        assert_eq!(config["mcpServers"]["other"]["command"], "other");
        assert_eq!(config["mcpServers"]["confer"]["env"]["A"], "B");
        assert_eq!(config["mcpServers"]["confer"]["type"], "stdio");
        assert_eq!(config["mcpServers"]["confer"]["command"], "/tmp/confer");
        assert_eq!(
            config["mcpServers"]["confer"]["args"],
            serde_json::json!(["mcp"])
        );
        remove_mcp_config(AgentKind::Cursor, &path).unwrap();
        assert_eq!(
            read_mcp_config(&path).unwrap()["mcpServers"]["other"]["command"],
            "other"
        );
        assert!(
            read_mcp_config(&path).unwrap()["mcpServers"]
                .get("confer")
                .is_none()
        );
    }

    #[test]
    fn mcp_config_update_accepts_empty_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        std::fs::write(&path, " \n").unwrap();

        write_mcp_config(AgentKind::Kimi, &path, "confer").unwrap();

        let config = read_mcp_config(&path).unwrap();
        assert_eq!(config["mcpServers"]["confer"]["command"], "confer");
    }

    #[test]
    fn opencode_registration_preserves_config_and_uses_local_command_array() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.json");
        let original = serde_json::json!({
            "model": "opencode/big-pickle",
            "permission": {"bash": "ask"},
            "mcp": {
                "other": {"type": "remote", "url": "https://example.com/mcp"},
                "confer": {"type": "local", "command": ["old"], "enabled": false, "environment": {"A": "B"}}
            }
        });
        std::fs::write(&path, original.to_string()).unwrap();
        for bin in ["confer", "/path with spaces/confer"] {
            write_mcp_config(AgentKind::Opencode, &path, bin).unwrap();
            let config = read_mcp_config(&path).unwrap();
            assert_eq!(config["model"], original["model"]);
            assert_eq!(config["permission"], original["permission"]);
            assert_eq!(config["mcp"]["other"], original["mcp"]["other"]);
            assert_eq!(config["mcp"]["confer"]["type"], "local");
            assert_eq!(
                config["mcp"]["confer"]["command"],
                serde_json::json!([bin, "mcp"])
            );
            assert_eq!(config["mcp"]["confer"]["enabled"], true);
            assert_eq!(
                config["mcp"]["confer"]["environment"],
                original["mcp"]["confer"]["environment"]
            );
            assert!(config.get("mcpServers").is_none());
        }
        remove_mcp_config(AgentKind::Opencode, &path).unwrap();
        let config = read_mcp_config(&path).unwrap();
        assert!(config["mcp"].get("confer").is_none());
        assert_eq!(config["mcp"]["other"], original["mcp"]["other"]);
    }

    #[test]
    fn opencode_registration_refuses_remote_or_malformed_config_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.json");
        for body in [
            r#"{"mcp":{"confer":{"type":"remote","url":"https://example.com/mcp"}}}"#,
            r#"{"mcp":[]}"#,
            r#"{"mcp":{"confer":false}}"#,
            "invalid",
        ] {
            std::fs::write(&path, body).unwrap();
            assert!(write_mcp_config(AgentKind::Opencode, &path, "confer").is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), body);
        }
    }
}
