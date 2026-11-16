use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use tokio::process::Command;

use super::Invocation;

#[derive(Deserialize)]
struct RxConfig {
    default_provider: Option<String>,
    #[serde(default)]
    provider: BTreeMap<String, RxProvider>,
}

#[derive(Deserialize)]
struct RxProvider {
    base_url: Option<String>,
    auth: Option<String>,
    env: Option<String>,
}

#[derive(Deserialize)]
struct RxAgentInstallState {
    current: Option<String>,
}

pub(crate) fn resolve_rx_home(rx_home: Option<OsString>, home: Option<PathBuf>) -> Result<PathBuf> {
    match rx_home.filter(|value| !value.is_empty()) {
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
            .map(|home| home.join(".rx"))
            .context("cannot determine home directory"),
    }
}

pub(super) fn rx_agent_installed(rxa_executable: &Path, home: Option<&Path>) -> bool {
    rx_agent_installed_with(rxa_executable, home, std::env::var_os("PATH").as_deref())
}

pub(super) fn rx_agent_installed_with(
    rxa_executable: &Path,
    home: Option<&Path>,
    path_env: Option<&OsStr>,
) -> bool {
    if path_env
        .and_then(|paths| super::readiness::find_executable_in_paths(&["rx-agent"], paths))
        .is_some()
    {
        return true;
    }
    for dir in [
        rxa_executable.parent().map(Path::to_path_buf),
        std::fs::canonicalize(rxa_executable)
            .ok()
            .and_then(|resolved| resolved.parent().map(Path::to_path_buf)),
    ]
    .into_iter()
    .flatten()
    {
        if super::readiness::executable_file(&dir.join("rx-agent")) {
            return true;
        }
    }
    let Some(home) = home else {
        return false;
    };
    let Ok(body) = std::fs::read_to_string(home.join(".recall/rx-agent/install.toml")) else {
        return false;
    };
    let Ok(state) = toml::from_str::<RxAgentInstallState>(&body) else {
        return false;
    };
    let Some(version) = state
        .current
        .as_deref()
        .filter(|version| is_stable_version(version))
    else {
        return false;
    };
    let agent_name = if cfg!(windows) {
        "rx-agent.exe"
    } else {
        "rx-agent"
    };
    home.join(".recall/rx-agent/versions")
        .join(version)
        .join(agent_name)
        .is_file()
}

pub(super) fn rx_has_credentials(home: &Path) -> bool {
    rx_has_credentials_with(home, std::env::var_os("RX_HOME"), std::env::vars())
}

pub(super) fn rx_has_credentials_with(
    home: &Path,
    rx_home_override: Option<OsString>,
    env_vars: impl IntoIterator<Item = (String, String)>,
) -> bool {
    let Ok(rx_home) = resolve_rx_home(rx_home_override, Some(home.to_path_buf())) else {
        return false;
    };
    let Some((config, keys)) = load_launcher_state(Some(home)) else {
        return false;
    };
    let env_map = nonempty_env_map(env_vars);
    match config.as_ref().and_then(|c| c.default_provider.as_deref()) {
        Some("none") => rx_native_ready(&rx_home, &env_map),
        Some(provider) => launcher_provider_ready(provider, config.as_ref(), &keys, &env_map),
        None => {
            launcher_provider_ready("openrouter", config.as_ref(), &keys, &env_map)
                || rx_native_ready(&rx_home, &env_map)
        }
    }
}

pub(super) fn apply_native_args(
    invocation: &Invocation,
    command: &mut Command,
    home: Option<&Path>,
) -> Result<(), String> {
    apply_native_args_with(
        invocation,
        command,
        home,
        std::env::var_os("RX_HOME"),
        std::env::vars(),
    )
}

pub(super) fn apply_native_args_with(
    invocation: &Invocation,
    command: &mut Command,
    home: Option<&Path>,
    rx_home_override: Option<OsString>,
    env_vars: impl IntoIterator<Item = (String, String)>,
) -> Result<(), String> {
    let rx_home = resolve_rx_home(rx_home_override, home.map(Path::to_path_buf))
        .map_err(|error| error.to_string())?;
    command
        .env("RX_HOME", &rx_home)
        .env("RX_NO_UPDATE", "1")
        .env("RX_NO_INSTALL", "1")
        .env_remove("RX_AGENT_PROVIDER")
        .env_remove("RX_HOST_REQUEST")
        .env_remove("RX_NO_YOLO");
    if let Some(spec) = invocation.model.as_deref() {
        if let Some((provider, model_id)) = spec.split_once('/')
            && !provider.is_empty()
            && !model_id.is_empty()
        {
            let env_map = nonempty_env_map(env_vars);
            let (config, keys) = load_launcher_state(home).unwrap_or_default();
            let in_auth = auth_json_providers(&rx_home.join("auth.json"))
                .is_some_and(|providers| providers.contains(provider));
            let explicit_launcher = keys.get(provider).is_some_and(|value| !value.is_empty())
                || config.as_ref().is_some_and(|cfg| {
                    cfg.default_provider.as_deref() == Some(provider)
                        || cfg.provider.contains_key(provider)
                });
            let env_launcher = !in_auth
                && !is_native_only_provider(provider)
                && launcher_provider_ready(provider, config.as_ref(), &keys, &env_map);
            let flag = if explicit_launcher || env_launcher {
                provider
            } else {
                "none"
            };
            command.args(["--provider", flag]);
        }
        command.args(["acp", "--model", spec]);
    } else {
        command.arg("acp");
    }
    Ok(())
}

pub(super) fn collect_secrets(
    home: Option<&Path>,
    rx_home_override: Option<OsString>,
    env_map: &HashMap<String, String>,
) -> Vec<String> {
    let mut secret_names = BTreeSet::new();
    if let Some(home) = home
        && let Ok(body) = std::fs::read_to_string(home.join(".recall/rx.toml"))
        && let Ok(value) = toml::from_str::<toml::Value>(&body)
        && let Some(providers) = value.get("provider").and_then(toml::Value::as_table)
    {
        for (id, entry) in providers {
            if let Some(env) = entry
                .get("env")
                .and_then(toml::Value::as_str)
                .filter(|value| !value.is_empty())
            {
                secret_names.insert(env.to_owned());
            }
            secret_names.insert(generated_env(id));
        }
    }
    let mut secrets = env_map
        .iter()
        .filter(|(key, value)| {
            !value.is_empty() && (secret_names.contains(*key) || is_secret_env_key(key))
        })
        .map(|(_, value)| value.clone())
        .collect::<Vec<_>>();
    if let Some(home) = home
        && let Ok(body) = std::fs::read_to_string(home.join(".recall/rx.keys"))
        && let Ok(value) = toml::from_str::<toml::Value>(&body)
        && let Some(table) = value.as_table()
    {
        secrets.extend(
            table
                .values()
                .filter_map(toml::Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
        );
    }
    if let Ok(rx_home) = resolve_rx_home(rx_home_override, home.map(Path::to_path_buf))
        && let Ok(body) = std::fs::read_to_string(rx_home.join("auth.json"))
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(&body)
        && let Some(map) = value.as_object()
    {
        for obj in map.values().filter_map(serde_json::Value::as_object) {
            for field in ["key", "access", "refresh"] {
                if let Some(secret) = obj
                    .get(field)
                    .and_then(serde_json::Value::as_str)
                    .filter(|value| !value.is_empty())
                {
                    secrets.push(secret.to_owned());
                }
            }
            if let Some(env_obj) = obj.get("env").and_then(serde_json::Value::as_object) {
                for (env_key, env_val) in env_obj {
                    if let Some(secret) = env_val.as_str().filter(|value| !value.is_empty())
                        && (secret_names.contains(env_key) || is_secret_env_key(env_key))
                    {
                        secrets.push(secret.to_owned());
                    }
                }
            }
        }
    }
    secrets
}

fn nonempty_env_map(
    env_vars: impl IntoIterator<Item = (String, String)>,
) -> HashMap<String, String> {
    env_vars
        .into_iter()
        .filter(|(_, value)| !value.is_empty())
        .collect()
}

fn is_secret_env_key(key: &str) -> bool {
    !key.ends_with("_FILE")
        && (key.ends_with("_API_KEY")
            || key.ends_with("_TOKEN")
            || key.ends_with("_SECRET")
            || key.ends_with("_SECRET_ACCESS_KEY")
            || key == "AWS_BEARER_TOKEN_BEDROCK")
}

fn is_native_only_provider(provider: &str) -> bool {
    [
        "anthropic",
        "openai",
        "openai-codex",
        "azure",
        "google",
        "google-vertex",
        "amazon-bedrock",
        "github-copilot",
        "xai",
        "groq",
        "cerebras",
        "mistral",
        "baseten",
        "fireworks",
        "together",
        "nvidia",
        "meta",
        "kimi-coding",
        "typesafe",
        "radius",
        "vercel-ai-gateway",
    ]
    .contains(&provider)
        || provider.starts_with("cloudflare-")
        || provider.starts_with("xiaomi")
        || provider.starts_with("qwen-token-plan")
        || provider.starts_with("ant-ling")
}

fn load_launcher_state(
    home: Option<&Path>,
) -> Option<(Option<RxConfig>, BTreeMap<String, String>)> {
    let Some(home) = home else {
        return Some((None, BTreeMap::new()));
    };
    let launcher = home.join(".recall");
    let keys = match std::fs::read_to_string(launcher.join("rx.keys")) {
        Ok(body) => toml::from_str::<BTreeMap<String, String>>(&body).ok()?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
        Err(_) => return None,
    };
    let config = match std::fs::read_to_string(launcher.join("rx.toml")) {
        Ok(body) => Some(toml::from_str::<RxConfig>(&body).ok()?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(_) => return None,
    };
    Some((config, keys))
}

fn launcher_provider_ready(
    id: &str,
    config: Option<&RxConfig>,
    keys: &BTreeMap<String, String>,
    env_map: &HashMap<String, String>,
) -> bool {
    if !is_valid_provider_id(id) {
        return false;
    }
    let entry = config.and_then(|cfg| cfg.provider.get(id));
    let stored = keys.get(id).is_some_and(|value| !value.is_empty());
    let environment = match entry
        .and_then(|e| e.env.as_deref())
        .filter(|name| !name.is_empty())
    {
        Some(name) => env_map.contains_key(name),
        None => {
            let direct = format!("{}_API_KEY", id.replace('-', "_").to_ascii_uppercase());
            env_map.contains_key(&generated_env(id)) || env_map.contains_key(&direct)
        }
    };
    let local_magpie = id == "magpie" && entry.and_then(|e| e.base_url.as_deref()).is_none();
    match entry.and_then(|e| e.auth.as_deref()).unwrap_or("api_key") {
        "api_key" => stored || environment || local_magpie,
        "env" => environment || local_magpie,
        _ => false,
    }
}

fn rx_native_ready(rx_home: &Path, env_map: &HashMap<String, String>) -> bool {
    if auth_json_providers(&rx_home.join("auth.json"))
        .is_some_and(|providers| !providers.is_empty())
    {
        return true;
    }
    let model = env_map
        .get("RX_MODEL")
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .or_else(|| config_json_model(&rx_home.join("config.json")));
    let Some((provider, model_id)) = model.as_deref().and_then(|value| value.split_once('/'))
    else {
        return false;
    };
    !provider.is_empty()
        && !model_id.is_empty()
        && provider != "openai-codex"
        && provider_has_env_credential(provider, env_map)
}

fn provider_has_env_credential(provider: &str, env_map: &HashMap<String, String>) -> bool {
    let default_prefix;
    let default_ref;
    let prefixes: &[&str] = match provider {
        "github-copilot" => &["COPILOT_GITHUB_TOKEN"],
        "google" | "google-vertex" => &["GEMINI_", "GOOGLE_"],
        "huggingface" => &["HF_TOKEN"],
        "vercel-ai-gateway" => &["AI_GATEWAY_"],
        "moonshotai" | "moonshotai-cn" => &["MOONSHOT_"],
        "qwen-token-plan" | "qwen-token-plan-individual" | "qwen-token-plan-cn" => &["QWEN_"],
        "opencode" | "opencode-go" => &["OPENCODE_"],
        "kimi-coding" => &["KIMI_"],
        "cloudflare-workers-ai" | "cloudflare-ai-gateway" => &["CLOUDFLARE_"],
        "xiaomi" | "xiaomi-token-plan-cn" | "xiaomi-token-plan-ams" | "xiaomi-token-plan-sgp" => {
            &["XIAOMI_"]
        }
        "amazon-bedrock" => &["AWS_"],
        _ => {
            default_prefix = format!("{}_", provider.replace('-', "_").to_ascii_uppercase());
            default_ref = default_prefix.as_str();
            std::slice::from_ref(&default_ref)
        }
    };
    env_map.keys().any(|key| {
        prefixes.iter().any(|prefix| key.starts_with(prefix))
            && (is_secret_env_key(key)
                || key == "AWS_PROFILE"
                || key == "GOOGLE_APPLICATION_CREDENTIALS")
    })
}

fn auth_json_providers(path: &Path) -> Option<BTreeSet<String>> {
    let body = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&body).ok()?;
    let map = value.as_object()?;
    let mut providers = BTreeSet::new();
    for (id, entry) in map {
        let obj = entry.as_object()?;
        match obj.get("type").and_then(serde_json::Value::as_str) {
            Some("api_key") => {
                let key_ok = obj
                    .get("key")
                    .map(|v| v.as_str().is_some_and(|s| !s.is_empty()));
                let env_ok = obj
                    .get("env")
                    .map(|v| v.as_object().is_some_and(|m| !m.is_empty()));
                if matches!(key_ok, Some(false)) || matches!(env_ok, Some(false)) {
                    return None;
                }
                if key_ok == Some(true) || env_ok == Some(true) {
                    providers.insert(id.clone());
                }
            }
            Some("oauth") => {
                if obj
                    .get("access")
                    .and_then(serde_json::Value::as_str)
                    .is_none_or(str::is_empty)
                    || !obj.get("refresh").is_some_and(serde_json::Value::is_string)
                    || !obj.get("expires").is_some_and(serde_json::Value::is_number)
                {
                    return None;
                }
                providers.insert(id.clone());
            }
            _ => return None,
        }
    }
    Some(providers)
}

fn config_json_model(path: &Path) -> Option<String> {
    let body = std::fs::read_to_string(path).ok()?;
    let value = serde_json::from_str::<serde_json::Value>(&body).ok()?;
    value
        .as_object()
        .and_then(|map| map.get("model"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_owned)
}

fn is_valid_provider_id(id: &str) -> bool {
    !id.is_empty()
        && id != "none"
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn generated_env(id: &str) -> String {
    let normalized = id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("RX_PROVIDER_{normalized}_API_KEY")
}

fn is_stable_version(version: &str) -> bool {
    let mut parts = version.split('.');
    matches!(
        (parts.next(), parts.next(), parts.next(), parts.next()),
        (Some(a), Some(b), Some(c), None)
            if all_ascii_digits(a) && all_ascii_digits(b) && all_ascii_digits(c)
    )
}

fn all_ascii_digits(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}
