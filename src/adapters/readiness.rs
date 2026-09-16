use crate::types::{AgentKind, Readiness};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub(crate) fn readiness() -> Vec<Readiness> {
    AgentKind::ALL.into_iter().map(check_readiness).collect()
}

pub(crate) fn check_readiness(agent: AgentKind) -> Readiness {
    let executable = super::find_executable(agent);
    let reason = match &executable {
        None => Some("executable not found on PATH"),
        Some(_) if !has_local_auth_marker(agent) => {
            Some("local authentication state was not found")
        }
        _ => None,
    };
    Readiness {
        agent,
        locally_ready: reason.is_none(),
        executable: executable.map(|path| path.to_string_lossy().into_owned()),
        reason: reason.map(str::to_owned),
    }
}

pub(crate) fn find_executable(agent: AgentKind) -> Option<PathBuf> {
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
        AgentKind::Agy | AgentKind::Kimi => false,
        AgentKind::Copilot => [
            "COPILOT_GITHUB_TOKEN",
            "GH_TOKEN",
            "GITHUB_TOKEN",
            "COPILOT_PROVIDER_BASE_URL",
        ]
        .iter()
        .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty())),
    };
    if env_ready {
        return true;
    }
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    let markers: &[&str] = match agent {
        AgentKind::Claude => &[
            ".claude.json",
            ".claude/.credentials.json",
            ".claude/config.json",
        ],
        AgentKind::Codex => &[".codex/auth.json"],
        AgentKind::Cursor => &[".cursor/cli-config.json"],
        AgentKind::Grok => &[".grok/auth.json"],
        AgentKind::Agy => &[
            ".gemini/antigravity-cli/jetski_state.pbtxt",
            ".gemini/antigravity-cli/settings.json",
            ".gemini/antigravity-cli/installation_id",
        ],
        AgentKind::Copilot => return copilot_home(&home).join("config.json").is_file(),
        AgentKind::Kimi => {
            return resolve_kimi_home(std::env::var_os("KIMI_CODE_HOME"), Some(home))
                .is_ok_and(|home| kimi_home_has_auth(&home));
        }
    };
    markers.iter().any(|marker| home.join(marker).is_file())
}

#[derive(serde::Deserialize)]
struct KimiConfig {
    #[serde(default)]
    providers: std::collections::BTreeMap<String, KimiProvider>,
}

#[derive(serde::Deserialize)]
struct KimiProvider {
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    env: std::collections::BTreeMap<String, toml::Value>,
}

impl KimiProvider {
    fn has_credential(&self) -> bool {
        if self.api_key.as_deref().is_some_and(|key| !key.is_empty()) {
            return true;
        }
        self.env.iter().any(|(name, value)| {
            name.ends_with("API_KEY") && value.as_str().is_some_and(|value| !value.is_empty())
        })
    }
}

pub(super) fn kimi_home_has_auth(kimi_home: &Path) -> bool {
    let credentials = kimi_home.join("credentials");
    if let Ok(entries) = std::fs::read_dir(&credentials) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().is_some_and(|ext| ext == "json") {
                return true;
            }
        }
    }
    let Ok(config) = std::fs::read_to_string(kimi_home.join("config.toml")) else {
        return false;
    };
    let Ok(config) = toml::from_str::<KimiConfig>(&config) else {
        return false;
    };
    config.providers.values().any(KimiProvider::has_credential)
}

fn copilot_home(home: &Path) -> PathBuf {
    std::env::var_os("COPILOT_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".copilot"))
}

pub(crate) fn resolve_kimi_home(
    kimi_code_home: Option<std::ffi::OsString>,
    home: Option<PathBuf>,
) -> Result<PathBuf> {
    match kimi_code_home.filter(|value| !value.is_empty()) {
        Some(dir) => {
            let dir = PathBuf::from(dir);
            if dir.is_absolute() {
                Ok(dir)
            } else {
                let cwd = std::env::current_dir().context("cannot determine current directory")?;
                Ok(cwd.join(dir))
            }
        }
        None => home
            .map(|home| home.join(".kimi-code"))
            .context("cannot determine home directory"),
    }
}
