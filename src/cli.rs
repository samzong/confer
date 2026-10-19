use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use crate::{
    install_host::{self, InstallHost},
    mcp, mcp_host,
    types::AgentKind,
};

#[derive(Parser)]
#[command(name = "confer", version, about = "Local multi-agent rooms over MCP")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    #[command(about = "Show local MCP processes and running seats")]
    Status,
    #[command(about = "Serve and manage the Confer MCP server")]
    Mcp {
        #[command(subcommand)]
        command: Option<McpCommands>,
    },
    #[command(about = "Manage the bundled Confer Agent Skill")]
    Skill {
        #[command(subcommand)]
        command: SkillCommands,
    },
}

#[derive(Subcommand)]
enum McpCommands {
    #[command(about = "Print Confer MCP capabilities and tools")]
    Capabilities {
        #[arg(long, value_enum, default_value_t = mcp::CapabilitiesFormat::Text)]
        format: mcp::CapabilitiesFormat,
    },
    #[command(about = "Register Confer with supported local MCP hosts")]
    Install {
        #[arg(
            long = "agent",
            help = mcp_host::host_help()
        )]
        agents: Vec<String>,
        #[arg(long, help = "Print host changes without applying them")]
        dry_run: bool,
        #[arg(long, help = "Confer binary to register instead of PATH confer")]
        bin: Option<PathBuf>,
    },
    #[command(about = "Unregister Confer from supported local MCP hosts")]
    Uninstall {
        #[arg(
            long = "agent",
            help = mcp_host::host_help()
        )]
        agents: Vec<String>,
        #[arg(long, help = "Print host changes without applying them")]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum SkillCommands {
    #[command(about = "Install the bundled Confer Agent Skill through Kitup")]
    Install {
        #[arg(long, help = "Install scope: user or project")]
        scope: Option<String>,
        #[arg(
            long = "agent",
            help = format!("Target host: {}. Repeat for multiple hosts. Use '*' for all.", install_host::supported_host_ids())
        )]
        agents: Vec<String>,
        #[arg(long, help = "Show the Kitup install plan without writing")]
        dry_run: bool,
        #[arg(long, help = "Skip interactive confirmation")]
        yes: bool,
    },
}

pub(crate) fn run() -> Result<()> {
    match Cli::parse().command {
        Commands::Status => crate::status::run(),
        Commands::Mcp { command } => match command {
            None => mcp::run(),
            Some(McpCommands::Capabilities { format }) => mcp::run_capabilities(format),
            Some(McpCommands::Install {
                agents,
                dry_run,
                bin,
            }) => mcp_host::install(&agents, dry_run, bin),
            Some(McpCommands::Uninstall { agents, dry_run }) => {
                mcp_host::uninstall(&agents, dry_run)
            }
        },
        Commands::Skill {
            command:
                SkillCommands::Install {
                    scope,
                    agents,
                    dry_run,
                    yes,
                },
        } => run_skill_install(scope, agents, dry_run, yes),
    }
}

fn run_skill_install(
    scope: Option<String>,
    agents: Vec<String>,
    dry_run: bool,
    yes: bool,
) -> Result<()> {
    let scope_set = scope.is_some();
    let flags = kitup::parse_install_flags(kitup::InstallFlagValues {
        scope,
        scope_set,
        agents,
        yes,
        dry_run,
        force: false,
    });
    kitup::install_flag_error(&flags.errors)?;
    let agents = supported_skill_agents(flags.agents, flags.scope)?;
    let hosts_file = install_host::skill_hosts_file(flags.scope, &agents)?;
    let base = kitup::BaseOptions {
        hosts_file: hosts_file.as_ref().map(|file| file.path().to_path_buf()),
        ..Default::default()
    };
    let report = kitup::run_bundled_skill_install(&kitup::InstallWorkflowOptions {
        install: kitup::InstallOptions {
            base,
            app_id: "confer".into(),
            skill_bundle: skill_bundle(),
            scope: flags.scope,
            agents,
            force: false,
        },
        yes: flags.yes,
        dry_run: flags.dry_run,
        stdin_tty: std::io::stdin().is_terminal(),
        current_agent: None,
        default_scope: Some(kitup::Scope::User),
        scope_set: flags.scope_set,
        prompt_scope: true,
    })?;
    kitup::install_workflow_error(&report)?;
    Ok(())
}

fn supported_skill_agents(
    selector: kitup::AgentSelector,
    scope: kitup::Scope,
) -> Result<kitup::AgentSelector> {
    let supported = InstallHost::all()
        .into_iter()
        .map(InstallHost::skill_host_id)
        .collect::<Vec<_>>();
    let selected = match selector {
        kitup::AgentSelector::Auto => {
            let mut detected = kitup::detect_hosts(&kitup::BaseOptions::default(), Some(scope))
                .context("failed to detect local Skill hosts")?
                .into_iter()
                .map(|host| host.id)
                .filter(|id| supported.contains(&id.as_str()))
                .collect::<Vec<_>>();
            for host in InstallHost::ADDITIONAL {
                if let Some(present) = host.skill_auto_detection(scope)? {
                    detected.retain(|id| id != host.skill_host_id());
                    if present {
                        detected.push(host.skill_host_id().into());
                    }
                }
            }
            if !detected.iter().any(|id| id == "kimi-cli")
                && crate::adapters::check_readiness(AgentKind::Kimi).locally_ready
            {
                detected.push("kimi-cli".into());
            }
            detected
        }
        kitup::AgentSelector::All => supported.iter().map(ToString::to_string).collect(),
        kitup::AgentSelector::Explicit(values) => {
            let mut selected = Vec::new();
            for value in values {
                let agent = InstallHost::parse(&value).with_context(|| {
                    format!(
                        "unsupported Skill host '{value}'; supported hosts: {}",
                        install_host::supported_host_ids()
                    )
                })?;
                let mapped = agent.skill_host_id();
                if !selected.iter().any(|item| item == mapped) {
                    selected.push(mapped.to_string());
                }
            }
            selected
        }
    };
    if selected.is_empty() {
        bail!("no supported Skill hosts were detected");
    }
    Ok(kitup::AgentSelector::Explicit(selected))
}

fn skill_bundle() -> kitup::SkillBundle {
    kitup::files_bundle(vec![
        kitup::SkillFile {
            path: "SKILL.md".into(),
            contents: include_bytes!("../skills/confer/SKILL.md").to_vec(),
            mode: None,
        },
        kitup::SkillFile {
            path: "agents/openai.yaml".into(),
            contents: include_bytes!("../skills/confer/agents/openai.yaml").to_vec(),
            mode: None,
        },
    ])
}

#[cfg(test)]
mod tests {
    use super::{Cli, Commands, McpCommands, SkillCommands, supported_skill_agents};
    use clap::{CommandFactory, Parser};

    #[test]
    fn mcp_without_subcommand_serves() {
        let cli = Cli::try_parse_from(["confer", "mcp"]).unwrap();
        assert!(matches!(cli.command, Commands::Mcp { command: None }));
    }

    #[test]
    fn install_commands_accept_agent_selection() {
        let cli = Cli::try_parse_from([
            "confer",
            "mcp",
            "install",
            "--agent",
            "claude",
            "--agent",
            "grok",
            "--dry-run",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Commands::Mcp {
                command: Some(McpCommands::Install { .. })
            }
        ));

        let cli = Cli::try_parse_from(["confer", "skill", "install", "--agent", "codex", "--yes"])
            .unwrap();
        assert!(matches!(
            cli.command,
            Commands::Skill {
                command: SkillCommands::Install { .. }
            }
        ));

        for (aliases, expected) in [
            (vec!["agy"], "antigravity-cli"),
            (vec!["copilot", "github-copilot"], "github-copilot"),
            (vec!["kimi", "kimi-cli"], "kimi-cli"),
        ] {
            assert_eq!(
                supported_skill_agents(
                    kitup::AgentSelector::Explicit(aliases.into_iter().map(String::from).collect()),
                    kitup::Scope::User,
                )
                .unwrap(),
                kitup::AgentSelector::Explicit(vec![expected.into()])
            );
        }
    }

    #[test]
    fn root_help_is_available() {
        Cli::command().debug_assert();
    }
}
