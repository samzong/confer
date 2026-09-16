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
                root.entry("mcpServers")
                    .or_insert_with(|| Value::Object(Map::new()))
                    .as_object_mut()
            })
            .with_context(|| {
                format!(
                    "invalid {} MCP config: mcpServers must be an object",
                    host.id()
                )
            })?;
        let entry = servers
            .entry(SERVER_NAME)
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .with_context(|| {
                format!(
                    "invalid {} MCP config: mcpServers.confer must be an object",
                    host.id()
                )
            })?;
        if uses_non_stdio_transport(entry) {
            bail!(
                "cannot install {} MCP config: existing confer entry uses a non-stdio transport",
                host.id()
            );
        }
        entry.insert("type".into(), Value::String("stdio".into()));
        entry.insert("command".into(), Value::String(bin.into()));
        entry.insert("args".into(), serde_json::json!([SERVER_ARG]));
        Ok(())
    })
}

pub(super) fn remove_mcp_config(host: AgentKind, path: &Path) -> Result<()> {
    if !path.is_file() {
        return Ok(());
    }
    update_mcp_config(path, |config| {
        if let Some(servers) = config.get_mut("mcpServers").and_then(Value::as_object_mut) {
            if servers
                .get(SERVER_NAME)
                .and_then(Value::as_object)
                .is_some_and(uses_non_stdio_transport)
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
        Ok(body) if body.trim().is_empty() => Ok(serde_json::json!({ "mcpServers": {} })),
        Ok(body) => serde_json::from_str(&body)
            .with_context(|| format!("failed to parse {}", path.display())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Ok(serde_json::json!({ "mcpServers": {} }))
        }
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

fn uses_non_stdio_transport(entry: &Map<String, Value>) -> bool {
    entry.contains_key("url") || entry.get("type").is_some_and(|value| value != "stdio")
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
}
