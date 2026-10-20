use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::types::AgentKind;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InstallHost {
    Agent(AgentKind),
    GeminiCli,
    Qwen,
    Droid,
    Qoder,
    Kiro,
}

impl InstallHost {
    pub(crate) const ADDITIONAL: [Self; 5] = [
        Self::GeminiCli,
        Self::Qwen,
        Self::Droid,
        Self::Qoder,
        Self::Kiro,
    ];

    pub(crate) fn all() -> Vec<Self> {
        AgentKind::ALL
            .into_iter()
            .map(Self::Agent)
            .chain(Self::ADDITIONAL)
            .collect()
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "gemini-cli" => Some(Self::GeminiCli),
            "qwen" | "qwen-code" => Some(Self::Qwen),
            "droid" | "factory-droid" => Some(Self::Droid),
            "qoder" | "qoder-cli" => Some(Self::Qoder),
            "kiro" | "kiro-cli" => Some(Self::Kiro),
            _ => AgentKind::parse(value).map(Self::Agent),
        }
    }

    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::Agent(agent) => agent.id(),
            Self::GeminiCli => "gemini-cli",
            Self::Qwen => "qwen",
            Self::Droid => "droid",
            Self::Qoder => "qoder",
            Self::Kiro => "kiro",
        }
    }

    pub(crate) fn skill_host_id(self) -> &'static str {
        match self {
            Self::Agent(agent) => agent.skill_host_id(),
            Self::GeminiCli => "gemini-cli",
            Self::Qwen => "qwen-code",
            Self::Droid => "droid",
            Self::Qoder => "qoder",
            Self::Kiro => "kiro-cli",
        }
    }

    pub(crate) fn program(self) -> Option<String> {
        match self {
            Self::Agent(agent) => crate::adapters::find_executable(agent),
            Self::GeminiCli => crate::adapters::find_executable_by_names(&["gemini"]),
            Self::Qwen => crate::adapters::find_executable_by_names(&["qwen"]),
            Self::Droid => crate::adapters::find_executable_by_names(&["droid"]),
            Self::Qoder => crate::adapters::find_executable_by_names(&["qoder", "qodercli"]),
            Self::Kiro => crate::adapters::find_executable_by_names(&["kiro-cli", "kiro"]),
        }
        .map(|path| path.to_string_lossy().into_owned())
    }

    pub(crate) fn skill_auto_detection(self, scope: kitup::Scope) -> Result<Option<bool>> {
        if self == Self::GeminiCli {
            return Ok(Some(self.program().is_some()));
        }
        if scope == kitup::Scope::User
            && let Some((_, variable)) = Self::SKILL_DIRECTORY_OVERRIDES
                .iter()
                .find(|(host, _)| *host == self)
            && std::env::var_os(variable).is_some_and(|value| !value.is_empty())
        {
            return Ok(Some(
                self.config_path()?
                    .parent()
                    .is_some_and(|path| path.is_dir()),
            ));
        }
        Ok(None)
    }

    const SKILL_DIRECTORY_OVERRIDES: [(Self, &'static str); 3] = [
        (Self::GeminiCli, "GEMINI_CLI_HOME"),
        (Self::Qwen, "QWEN_HOME"),
        (Self::Qoder, "QODER_CONFIG_DIR"),
    ];

    pub(crate) fn config_path(self) -> Result<PathBuf> {
        let home = || dirs::home_dir().context("cannot determine home directory");
        match self {
            Self::Agent(AgentKind::Cursor) => Ok(home()?.join(".cursor/mcp.json")),
            Self::Agent(AgentKind::Kimi) => Ok(crate::adapters::resolve_kimi_home(
                std::env::var_os("KIMI_CODE_HOME"),
                dirs::home_dir(),
            )?
            .join("mcp.json")),
            Self::Agent(AgentKind::Opencode) => Ok(std::env::var_os("XDG_CONFIG_HOME")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
                .context("cannot determine OpenCode config directory")?
                .join("opencode/opencode.json")),
            Self::GeminiCli => Ok(std::env::var_os("GEMINI_CLI_HOME")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .unwrap_or(home()?)
                .join(".gemini/settings.json")),
            Self::Qwen => {
                let root = match std::env::var_os("QWEN_HOME").filter(|value| !value.is_empty()) {
                    Some(value) if value == "~" => home()?,
                    Some(value) => {
                        match value.to_str().and_then(|value| value.strip_prefix("~/")) {
                            Some(relative) => home()?.join(relative),
                            None => PathBuf::from(value),
                        }
                    }
                    None => home()?.join(".qwen"),
                };
                Ok(root.join("settings.json"))
            }
            Self::Droid => Ok(home()?.join(".factory/mcp.json")),
            Self::Qoder => Ok(std::env::var_os("QODER_CONFIG_DIR")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .unwrap_or(home()?.join(".qoder"))
                .join("settings.json")),
            Self::Kiro => Ok(home()?.join(".kiro/settings/mcp.json")),
            Self::Agent(_) => bail!("{} does not use an MCP config file", self.id()),
        }
    }
}

pub(crate) fn supported_host_ids() -> String {
    InstallHost::all()
        .into_iter()
        .map(InstallHost::id)
        .collect::<Vec<_>>()
        .join(", ")
}

pub(crate) fn skill_hosts_file(
    scope: kitup::Scope,
    selector: &kitup::AgentSelector,
) -> Result<Option<tempfile::NamedTempFile>> {
    if scope == kitup::Scope::Project {
        return Ok(None);
    }
    let mut hosts = kitup::load_host_spec(None)?;
    let mut changed = false;
    for (host, variable) in InstallHost::SKILL_DIRECTORY_OVERRIDES {
        if let kitup::AgentSelector::Explicit(values) = selector
            && !values
                .iter()
                .any(|value| InstallHost::parse(value) == Some(host))
        {
            continue;
        }
        if std::env::var_os(variable).is_none_or(|value| value.is_empty()) {
            continue;
        }
        let config = host.config_path()?;
        let directory = config
            .parent()
            .context("host config has no parent directory")?;
        let directory = if directory.is_absolute() {
            directory.to_path_buf()
        } else {
            std::env::current_dir()?.join(directory)
        };
        let home = dirs::home_dir().context("cannot determine home directory")?;
        let canonical_home = home.canonicalize().unwrap_or_else(|_| home.clone());
        let relative = directory
            .strip_prefix(&home)
            .or_else(|_| directory.strip_prefix(&canonical_home))
            .with_context(|| {
                format!(
                    "Kitup requires {} Skill paths inside HOME; {variable} resolves outside HOME",
                    host.id()
                )
            })?;
        let root = if relative.as_os_str().is_empty() {
            "~".to_string()
        } else {
            format!("~/{}", relative.to_string_lossy())
        };
        let entry = hosts
            .iter_mut()
            .find(|entry| entry.id == host.skill_host_id())
            .with_context(|| format!("Kitup does not support {}", host.id()))?;
        entry.user_skills_dirs = vec![format!("{root}/skills")];
        entry.detect = if root == "~" { Vec::new() } else { vec![root] };
        changed = true;
    }
    if !changed {
        return Ok(None);
    }
    let hosts = hosts
        .into_iter()
        .map(|host| {
            serde_json::json!({
                "id": host.id,
                "displayName": host.display_name,
                "aliases": host.aliases,
                "projectSkillsDirs": host.project_skills_dirs,
                "userSkillsDirs": host.user_skills_dirs,
                "detect": host.detect,
                "status": host.status,
                "notes": host.notes,
            })
        })
        .collect::<Vec<_>>();
    let mut file = tempfile::NamedTempFile::new().context("failed to prepare Kitup host paths")?;
    serde_json::to_writer(file.as_file_mut(), &serde_json::json!({"hosts": hosts}))?;
    Ok(Some(file))
}
