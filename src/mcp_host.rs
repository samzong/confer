mod config;

use crate::install_host::{InstallHost, supported_host_ids};
use config::{remove_mcp_config, write_mcp_config};

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, bail};

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
        agents,
        dry_run,
        HostAction::Install {
            bin: resolve_bin(bin)?,
        },
    )
}

pub(crate) fn uninstall(agents: &[String], dry_run: bool) -> Result<()> {
    run_hosts(
        resolve_hosts(agents)?,
        agents,
        dry_run,
        HostAction::Uninstall,
    )
}

fn resolve_hosts(agents: &[String]) -> Result<Vec<InstallHost>> {
    if agents.is_empty() || agents.iter().any(|agent| agent.trim() == "*") {
        return Ok(InstallHost::all());
    }
    let mut hosts = Vec::new();
    for agent in agents {
        let host = InstallHost::parse(agent).with_context(|| {
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
    if !path.is_absolute() && path.components().count() == 1 {
        return Ok(path.to_string_lossy().into_owned());
    }
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .context("failed to resolve current directory for --bin")?
            .join(path)
    };
    if !path.is_file() {
        bail!("--bin {} is not a file", path.display());
    }
    Ok(path.to_string_lossy().into_owned())
}

fn add_args(host: InstallHost, bin: &str) -> Option<Vec<&str>> {
    let InstallHost::Agent(host) = host else {
        return None;
    };
    let mut args = match host {
        AgentKind::Claude | AgentKind::Grok | AgentKind::Devin => {
            vec!["mcp", "add", "--scope", "user", SERVER_NAME, "--"]
        }
        AgentKind::Codex | AgentKind::Copilot => vec!["mcp", "add", SERVER_NAME, "--"],
        AgentKind::Agy => vec!["mcp", "add", SERVER_NAME],
        AgentKind::Cursor | AgentKind::Kimi | AgentKind::Opencode => return None,
    };
    args.extend([bin, SERVER_ARG]);
    Some(args)
}

fn remove_args(host: InstallHost) -> Option<Vec<&'static str>> {
    let InstallHost::Agent(host) = host else {
        return None;
    };
    Some(match host {
        AgentKind::Claude => vec!["mcp", "remove", SERVER_NAME, "--scope", "user"],
        AgentKind::Grok | AgentKind::Devin => {
            vec!["mcp", "remove", "--scope", "user", SERVER_NAME]
        }
        AgentKind::Codex | AgentKind::Agy | AgentKind::Copilot => {
            vec!["mcp", "remove", SERVER_NAME]
        }
        AgentKind::Cursor | AgentKind::Kimi | AgentKind::Opencode => return None,
    })
}

fn run_hosts(
    hosts: Vec<InstallHost>,
    agents: &[String],
    dry_run: bool,
    action: HostAction,
) -> Result<()> {
    let discover = agents.is_empty() || agents.iter().any(|agent| agent.trim() == "*");
    let mut changed = 0usize;
    let mut errors = Vec::new();
    for host in hosts {
        let program = host.program();
        if program.is_none() && (discover || !uses_config_file(host)) {
            eprintln!("skipped {}: executable is not on PATH", host.id());
            continue;
        }
        match apply_host(host, program.as_deref(), dry_run, &action) {
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

fn apply_host(
    host: InstallHost,
    program: Option<&str>,
    dry_run: bool,
    action: &HostAction,
) -> Result<()> {
    let removing = matches!(action, HostAction::Uninstall);
    let args = match action {
        HostAction::Install { bin } => add_args(host, bin),
        HostAction::Uninstall => remove_args(host),
    };
    if let Some(args) = args {
        let program = program.context("MCP host executable is not on PATH")?;
        if matches!(
            run_host_command(program, &args, dry_run, removing)?,
            HostCommandOutcome::AlreadyExists
        ) {
            run_host_command(
                program,
                &remove_args(host).context("missing native MCP remove command")?,
                false,
                true,
            )?;
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
    } else {
        let path = host.config_path()?;
        if dry_run {
            println!(
                "{} {} ({SERVER_NAME})",
                if removing { "remove" } else { "write" },
                path.display()
            );
        } else {
            match action {
                HostAction::Install { bin } => write_mcp_config(host, &path, bin)?,
                HostAction::Uninstall => remove_mcp_config(host, &path)?,
            }
            println!(
                "{} {}",
                if removing { "uninstalled" } else { "installed" },
                host.id()
            );
        }
    }
    Ok(())
}

fn run_host_command(
    program: &str,
    args: &[&str],
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

fn uses_config_file(host: InstallHost) -> bool {
    add_args(host, DEFAULT_BIN).is_none()
}

pub(crate) fn host_help() -> String {
    format!(
        "Target host: {}. Repeat for multiple hosts. Use '*' for all installed CLIs.",
        supported_host_ids()
    )
}

fn display_command(program: &str, args: &[&str]) -> String {
    std::iter::once(program)
        .chain(args.iter().copied())
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
    use super::config::read_mcp_config;
    use super::{InstallHost, add_args, remove_args, uses_config_file};
    use crate::types::AgentKind;

    #[test]
    fn native_commands_match_host_contracts() {
        for (host, add, remove) in [
            (
                AgentKind::Claude,
                "mcp add --scope user confer -- confer mcp",
                "mcp remove confer --scope user",
            ),
            (
                AgentKind::Grok,
                "mcp add --scope user confer -- confer mcp",
                "mcp remove --scope user confer",
            ),
            (
                AgentKind::Codex,
                "mcp add confer -- confer mcp",
                "mcp remove confer",
            ),
            (
                AgentKind::Copilot,
                "mcp add confer -- confer mcp",
                "mcp remove confer",
            ),
            (
                AgentKind::Agy,
                "mcp add confer confer mcp",
                "mcp remove confer",
            ),
            (
                AgentKind::Devin,
                "mcp add --scope user confer -- confer mcp",
                "mcp remove --scope user confer",
            ),
        ] {
            assert_eq!(
                add_args(InstallHost::Agent(host), "confer").unwrap(),
                add.split_whitespace().collect::<Vec<_>>()
            );
            assert_eq!(
                remove_args(InstallHost::Agent(host)).unwrap(),
                remove.split_whitespace().collect::<Vec<_>>()
            );
            assert!(!uses_config_file(InstallHost::Agent(host)));
        }
        for host in [AgentKind::Cursor, AgentKind::Kimi, AgentKind::Opencode] {
            assert!(add_args(InstallHost::Agent(host), "confer").is_none());
            assert!(remove_args(InstallHost::Agent(host)).is_none());
            assert!(uses_config_file(InstallHost::Agent(host)));
        }
    }

    #[cfg(unix)]
    #[test]
    fn cursor_registration_without_cli_requires_explicit_selection() {
        if std::env::var_os("CONFER_TEST_CURSOR_REGISTRATION").is_none() {
            let dir = tempfile::Builder::new()
                .prefix("confer-registration-test-")
                .tempdir()
                .unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "mcp_host::tests::cursor_registration_without_cli_requires_explicit_selection",
                    "--nocapture",
                ])
                .env_clear()
                .env("CONFER_TEST_CURSOR_REGISTRATION", dir.path())
                .env("TMPDIR", dir.path().parent().unwrap())
                .env("HOME", dir.path())
                .env("PATH", dir.path())
                .current_dir(dir.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(dir.path().join(".cursor/mcp.json").is_file());
            return;
        }

        let isolated =
            std::path::PathBuf::from(std::env::var_os("CONFER_TEST_CURSOR_REGISTRATION").unwrap());
        assert!(isolated.is_absolute());
        assert_eq!(std::env::var_os("PATH").unwrap(), isolated.as_os_str());
        let isolated = isolated.canonicalize().unwrap();
        assert_eq!(
            isolated.parent().unwrap(),
            std::env::temp_dir().canonicalize().unwrap()
        );
        assert!(
            isolated
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("confer-registration-test-")
        );
        assert_eq!(dirs::home_dir().unwrap().canonicalize().unwrap(), isolated);
        assert_eq!(
            std::env::current_dir().unwrap().canonicalize().unwrap(),
            isolated
        );
        let path = InstallHost::Agent(AgentKind::Cursor).config_path().unwrap();
        let kimi_path = InstallHost::Agent(AgentKind::Kimi).config_path().unwrap();
        assert_eq!(
            kimi_path,
            dirs::home_dir()
                .unwrap()
                .join(".kimi-code")
                .join("mcp.json")
        );
        let custom = isolated.join("custom-kimi-home");
        unsafe {
            std::env::set_var("KIMI_CODE_HOME", &custom);
        }
        assert_eq!(
            InstallHost::Agent(AgentKind::Kimi).config_path().unwrap(),
            custom.join("mcp.json")
        );
        unsafe {
            std::env::set_var("KIMI_CODE_HOME", "relative-kimi-home");
        }
        assert_eq!(
            InstallHost::Agent(AgentKind::Kimi).config_path().unwrap(),
            isolated.join("relative-kimi-home").join("mcp.json")
        );
        unsafe {
            std::env::remove_var("KIMI_CODE_HOME");
        }
        let agents = ["cursor".into()];
        for dry_run in [true, false] {
            for selection in [vec![], vec!["*".into()], vec!["cursor".into(), "*".into()]] {
                assert!(super::install(&selection, dry_run, None).is_err());
                assert!(super::uninstall(&selection, dry_run).is_err());
                assert!(!path.exists());
                assert!(!kimi_path.exists());
            }
            for host in AgentKind::ALL {
                if !uses_config_file(InstallHost::Agent(host)) {
                    assert!(super::install(&[host.id().into()], dry_run, None).is_err());
                    assert!(super::uninstall(&[host.id().into()], dry_run).is_err());
                }
            }
            assert!(super::install(&agents, dry_run, Some(path.clone())).is_err());
            assert!(!path.exists());
            assert!(!kimi_path.exists());
        }

        for (host, path) in [(AgentKind::Cursor, path), (AgentKind::Kimi, kimi_path)] {
            let agents = [host.id().into()];
            super::install(&agents, true, None).unwrap();
            super::uninstall(&agents, true).unwrap();
            super::uninstall(&agents, false).unwrap();
            assert!(!path.exists());
            super::install(&agents, false, None).unwrap();
            assert_eq!(
                read_mcp_config(&path).unwrap()["mcpServers"]["confer"]["command"],
                "confer"
            );
            let installed = std::fs::read(&path).unwrap();
            super::install(&agents, true, Some("other-bin".into())).unwrap();
            super::uninstall(&agents, true).unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), installed);
            super::uninstall(&agents, false).unwrap();
            assert!(
                read_mcp_config(&path).unwrap()["mcpServers"]
                    .get("confer")
                    .is_none()
            );
            assert!(!crate::adapters::check_readiness(host).locally_ready);
        }
    }
}
