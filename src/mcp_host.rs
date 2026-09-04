use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};

use crate::types::AgentKind;

const SERVER_NAME: &str = "confer";
const DEFAULT_BIN: &str = "confer";
const SERVER_ARG: &str = "mcp";

enum HostAction {
    Install { bin: String },
    Uninstall,
}

enum HostCommandOutcome {
    Applied,
    AlreadyExists,
}

pub(crate) fn install(agents: &[String], dry_run: bool, bin: Option<PathBuf>) -> Result<()> {
    run_hosts(
        resolve_hosts(agents)?,
        dry_run,
        HostAction::Install {
            bin: resolve_bin(bin)?,
        },
    )
}

pub(crate) fn uninstall(agents: &[String], dry_run: bool) -> Result<()> {
    run_hosts(resolve_hosts(agents)?, dry_run, HostAction::Uninstall)
}

fn resolve_hosts(agents: &[String]) -> Result<Vec<AgentKind>> {
    if agents.is_empty() || agents.iter().any(|agent| agent.trim() == "*") {
        return Ok(AgentKind::ALL.to_vec());
    }
    let mut hosts = Vec::new();
    for agent in agents {
        let host = AgentKind::parse(agent).with_context(|| {
            format!(
                "unknown MCP host '{}'; supported hosts: {}",
                agent.trim(),
                supported_host_ids()
            )
        })?;
        if !hosts.contains(&host) {
            hosts.push(host);
        }
    }
    Ok(hosts)
}

fn resolve_bin(bin: Option<PathBuf>) -> Result<String> {
    let Some(path) = bin else {
        return Ok(DEFAULT_BIN.into());
    };
    if path.as_os_str().is_empty() {
        bail!("--bin must not be empty");
    }
    if path.is_absolute() {
        ensure_bin_file(&path)?;
        return Ok(path.to_string_lossy().into_owned());
    }
    if path.components().count() == 1 {
        return Ok(path.to_string_lossy().into_owned());
    }
    let absolute = std::env::current_dir()
        .context("failed to resolve current directory for --bin")?
        .join(path);
    ensure_bin_file(&absolute)?;
    Ok(absolute.to_string_lossy().into_owned())
}

fn ensure_bin_file(path: &Path) -> Result<()> {
    if path.is_file() {
        Ok(())
    } else {
        bail!("--bin {} is not a file", path.display())
    }
}

fn add_args(host: AgentKind, bin: &str) -> Option<Vec<String>> {
    match host {
        AgentKind::Claude => Some(vec![
            "mcp".into(),
            "add".into(),
            "--scope".into(),
            "user".into(),
            SERVER_NAME.into(),
            "--".into(),
            bin.into(),
            SERVER_ARG.into(),
        ]),
        AgentKind::Codex => Some(vec![
            "mcp".into(),
            "add".into(),
            SERVER_NAME.into(),
            "--".into(),
            bin.into(),
            SERVER_ARG.into(),
        ]),
        AgentKind::Cursor => None,
        AgentKind::Grok => Some(vec![
            "mcp".into(),
            "add".into(),
            "--scope".into(),
            "user".into(),
            SERVER_NAME.into(),
            "--".into(),
            bin.into(),
            SERVER_ARG.into(),
        ]),
        AgentKind::Agy => Some(vec![
            "mcp".into(),
            "add".into(),
            SERVER_NAME.into(),
            bin.into(),
            SERVER_ARG.into(),
        ]),
    }
}

fn remove_args(host: AgentKind) -> Option<Vec<String>> {
    match host {
        AgentKind::Claude => Some(vec![
            "mcp".into(),
            "remove".into(),
            SERVER_NAME.into(),
            "--scope".into(),
            "user".into(),
        ]),
        AgentKind::Codex => Some(vec!["mcp".into(), "remove".into(), SERVER_NAME.into()]),
        AgentKind::Cursor => None,
        AgentKind::Grok => Some(vec![
            "mcp".into(),
            "remove".into(),
            "--scope".into(),
            "user".into(),
            SERVER_NAME.into(),
        ]),
        AgentKind::Agy => Some(vec!["mcp".into(), "remove".into(), SERVER_NAME.into()]),
    }
}

fn run_hosts(hosts: Vec<AgentKind>, dry_run: bool, action: HostAction) -> Result<()> {
    let mut changed = 0usize;
    let mut errors = Vec::new();
    for host in hosts {
        let Some(program) = host_program(host) else {
            eprintln!("skipped {}: executable is not on PATH", host.id());
            continue;
        };
        match apply_host(host, &program, dry_run, &action) {
            Ok(()) => changed += 1,
            Err(error) => errors.push(error),
        }
    }
    if changed == 0 && errors.is_empty() {
        bail!(
            "no supported MCP hosts found on PATH ({})",
            supported_host_ids()
        );
    }
    if errors.is_empty() {
        Ok(())
    } else {
        let detail = errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ");
        if changed == 0 {
            bail!("failed to update Confer MCP: {detail}");
        }
        bail!("updated some hosts, but failed: {detail}")
    }
}

fn apply_host(host: AgentKind, program: &str, dry_run: bool, action: &HostAction) -> Result<()> {
    match action {
        HostAction::Install { bin } => match add_args(host, bin) {
            Some(args) => {
                if matches!(
                    run_host_command(program, &args, dry_run, false)?,
                    HostCommandOutcome::AlreadyExists
                ) {
                    let remove = remove_args(host).with_context(|| {
                        format!("{} has no native MCP remove command", host.id())
                    })?;
                    run_host_command(program, &remove, false, true)?;
                    if matches!(
                        run_host_command(program, &args, false, false)?,
                        HostCommandOutcome::AlreadyExists
                    ) {
                        bail!(
                            "{} MCP registration still exists after replacement",
                            host.id()
                        );
                    }
                }
                Ok(())
            }
            None => {
                let path = cursor_config_path()?;
                if dry_run {
                    println!("write {} ({SERVER_NAME})", path.display());
                } else {
                    write_cursor_config(&path, bin)?;
                    println!("installed {}", host.id());
                }
                Ok(())
            }
        },
        HostAction::Uninstall => match remove_args(host) {
            Some(args) => {
                run_host_command(program, &args, dry_run, true)?;
                Ok(())
            }
            None => {
                let path = cursor_config_path()?;
                if dry_run {
                    println!("remove {} ({SERVER_NAME})", path.display());
                } else {
                    remove_cursor_config(&path)?;
                    println!("uninstalled {}", host.id());
                }
                Ok(())
            }
        },
    }
}

fn run_host_command(
    program: &str,
    args: &[String],
    dry_run: bool,
    removing: bool,
) -> Result<HostCommandOutcome> {
    if dry_run {
        println!("{}", display_command(program, args));
        return Ok(HostCommandOutcome::Applied);
    }
    let output = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("failed to run {program}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}\n{stderr}");
    if !removing && looks_like_already_exists(&combined) {
        return Ok(HostCommandOutcome::AlreadyExists);
    }
    if output.status.success()
        || (removing
            && combined.to_ascii_lowercase().contains(SERVER_NAME)
            && looks_like_not_found(&combined))
    {
        println!(
            "{} {}",
            if removing { "uninstalled" } else { "installed" },
            program
        );
        return Ok(HostCommandOutcome::Applied);
    }
    let detail = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    bail!("{}: {}", display_command(program, args), detail)
}

fn cursor_config_path() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .context("cannot determine home directory")?
        .join(".cursor/mcp.json"))
}

fn write_cursor_config(path: &Path, bin: &str) -> Result<()> {
    update_cursor_config(path, |config| {
        let servers = config
            .as_object_mut()
            .and_then(|root| {
                root.entry("mcpServers")
                    .or_insert_with(|| Value::Object(Map::new()))
                    .as_object_mut()
            })
            .context("invalid Cursor MCP config: mcpServers must be an object")?;
        let entry = servers
            .entry(SERVER_NAME)
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .context("invalid Cursor MCP config: mcpServers.confer must be an object")?;
        if uses_non_stdio_transport(entry) {
            bail!("cannot install Cursor MCP: existing confer entry uses a non-stdio transport");
        }
        entry.insert("type".into(), Value::String("stdio".into()));
        entry.insert("command".into(), Value::String(bin.into()));
        entry.insert("args".into(), serde_json::json!([SERVER_ARG]));
        Ok(())
    })
}

fn remove_cursor_config(path: &Path) -> Result<()> {
    if !path.is_file() {
        return Ok(());
    }
    update_cursor_config(path, |config| {
        if let Some(servers) = config.get_mut("mcpServers").and_then(Value::as_object_mut) {
            if servers
                .get(SERVER_NAME)
                .and_then(Value::as_object)
                .is_some_and(uses_non_stdio_transport)
            {
                bail!(
                    "cannot uninstall Cursor MCP: existing confer entry uses a non-stdio transport"
                );
            }
            servers.remove(SERVER_NAME);
        }
        Ok(())
    })
}

fn update_cursor_config(path: &Path, change: impl FnOnce(&mut Value) -> Result<()>) -> Result<()> {
    let mut config = read_cursor_config(path)?;
    change(&mut config)?;
    write_cursor_config_file(path, &config)
}

fn read_cursor_config(path: &Path) -> Result<Value> {
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

fn write_cursor_config_file(path: &Path, config: &Value) -> Result<()> {
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
    let parent = path
        .parent()
        .context("Cursor MCP config has no parent directory")?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("failed to create temporary file in {}", parent.display()))?;
    let body = format!("{}\n", serde_json::to_string_pretty(config)?);
    temp.write_all(body.as_bytes())
        .context("failed to write Cursor MCP config")?;
    temp.as_file()
        .sync_all()
        .context("failed to sync Cursor MCP config")?;
    temp.persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to replace {}", path.display()))?;
    Ok(())
}

fn uses_non_stdio_transport(entry: &Map<String, Value>) -> bool {
    entry.contains_key("url") || entry.get("type").is_some_and(|value| value != "stdio")
}

fn host_program(host: AgentKind) -> Option<String> {
    let paths = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&paths) {
        for name in host.binary_names() {
            let path = directory.join(name);
            if executable_file(&path) {
                return Some((*name).to_string());
            }
        }
    }
    None
}

fn supported_host_ids() -> String {
    AgentKind::ALL.map(AgentKind::id).join(", ")
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

fn display_command(program: &str, args: &[String]) -> String {
    std::iter::once(program)
        .chain(args.iter().map(String::as_str))
        .map(quote_arg)
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote_arg(value: &str) -> String {
    if value
        .bytes()
        .any(|byte| byte.is_ascii_whitespace() || byte == b'\'')
    {
        format!("'{}'", value.replace('\'', "'\\''"))
    } else {
        value.into()
    }
}

fn looks_like_not_found(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    [
        "not found",
        "does not exist",
        "not registered",
        "not configured",
        "no mcp server",
    ]
    .iter()
    .any(|needle| value.contains(needle))
}

fn looks_like_already_exists(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    value.contains("already exists") || value.contains("already configured")
}

#[cfg(test)]
mod tests {
    use super::{
        add_args, read_cursor_config, remove_args, remove_cursor_config, write_cursor_config,
    };
    use crate::types::AgentKind;

    #[test]
    fn native_commands_match_host_contracts() {
        assert_eq!(
            add_args(AgentKind::Claude, "confer").unwrap(),
            [
                "mcp", "add", "--scope", "user", "confer", "--", "confer", "mcp"
            ]
        );
        assert_eq!(
            add_args(AgentKind::Grok, "confer").unwrap(),
            [
                "mcp", "add", "--scope", "user", "confer", "--", "confer", "mcp"
            ]
        );
        assert_eq!(
            add_args(AgentKind::Agy, "confer").unwrap(),
            ["mcp", "add", "confer", "confer", "mcp"]
        );
        assert_eq!(
            remove_args(AgentKind::Agy).unwrap(),
            ["mcp", "remove", "confer"]
        );
    }

    #[test]
    fn cursor_update_preserves_unrelated_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        std::fs::write(
            &path,
            r#"{"mcpServers":{"other":{"command":"other"},"confer":{"env":{"A":"B"}}}}"#,
        )
        .unwrap();
        write_cursor_config(&path, "/tmp/confer").unwrap();
        let config = read_cursor_config(&path).unwrap();
        assert_eq!(config["mcpServers"]["other"]["command"], "other");
        assert_eq!(config["mcpServers"]["confer"]["env"]["A"], "B");
        remove_cursor_config(&path).unwrap();
        assert!(
            read_cursor_config(&path).unwrap()["mcpServers"]
                .get("confer")
                .is_none()
        );
    }

    #[test]
    fn cursor_update_accepts_empty_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        std::fs::write(&path, " \n").unwrap();

        write_cursor_config(&path, "confer").unwrap();

        let config = read_cursor_config(&path).unwrap();
        assert_eq!(config["mcpServers"]["confer"]["command"], "confer");
    }
}
