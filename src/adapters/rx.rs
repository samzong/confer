use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use tokio::process::Command;

use super::Invocation;

mod providers;
use providers::{
    RX_BUNDLED_PROVIDERS, RX_EXTRA_SECRET_ENV_KEYS, RX_NATIVE_ENV_KEYS, generated_env,
    is_secret_env_key, native_provider_env_ready,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RxConfig {
    default_provider: Option<String>,
    #[serde(default)]
    provider: BTreeMap<String, RxProvider>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RxProvider {
    base_url: Option<String>,
    anthropic_base: Option<String>,
    auth: Option<String>,
    env: Option<String>,
    #[serde(rename = "model")]
    _model: Option<String>,
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
    path_env: Option<&std::ffi::OsStr>,
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
    let Some(version) = state.current.as_deref().filter(|v| is_stable_version(v)) else {
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
    rx_has_credentials_with(home, std::env::var_os("RX_HOME"), |name| {
        std::env::var(name).ok()
    })
}

pub(super) fn rx_has_credentials_with(
    home: &Path,
    rx_home_override: Option<OsString>,
    env_get: impl Fn(&str) -> Option<String>,
) -> bool {
    let Ok(rx_home) = resolve_rx_home(rx_home_override, Some(home.to_path_buf())) else {
        return false;
    };
    let Some((config, keys)) = load_launcher_state(Some(home)) else {
        return false;
    };
    match config.as_ref().and_then(|c| c.default_provider.as_deref()) {
        Some("none") => rx_native_ready(home, &rx_home, &env_get),
        Some(provider) => launcher_provider_ready(provider, config.as_ref(), &keys, &env_get),
        None => {
            launcher_provider_ready("openrouter", config.as_ref(), &keys, &env_get)
                || rx_native_ready(home, &rx_home, &env_get)
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
        |name| std::env::var(name).ok(),
    )
}

pub(super) fn apply_native_args_with(
    invocation: &Invocation,
    command: &mut Command,
    home: Option<&Path>,
    rx_home_override: Option<OsString>,
    env_get: impl Fn(&str) -> Option<String>,
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
        let provider_arg = if let Some((provider, model_id)) = spec.split_once('/')
            && !provider.is_empty()
            && !model_id.is_empty()
        {
            let (config, keys) = load_launcher_state(home).unwrap_or((None, BTreeMap::new()));
            if launcher_provider_ready(provider, config.as_ref(), &keys, &env_get) {
                Some(provider)
            } else if auth_json_providers(&rx_home.join("auth.json"))
                .is_some_and(|providers| providers.contains(provider))
            {
                Some("none")
            } else if config
                .as_ref()
                .is_some_and(|cfg| cfg.provider.contains_key(provider))
            {
                Some(provider)
            } else {
                Some("none")
            }
        } else {
            None
        };
        if let Some(provider) = provider_arg {
            command.args(["--provider", provider]);
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
    env_map: &std::collections::HashMap<String, String>,
) -> Vec<String> {
    let mut secret_names = BTreeSet::new();
    for (_, env) in RX_BUNDLED_PROVIDERS {
        secret_names.insert((*env).to_owned());
    }
    for env in RX_NATIVE_ENV_KEYS
        .iter()
        .chain(RX_EXTRA_SECRET_ENV_KEYS.iter())
    {
        secret_names.insert((*env).to_owned());
    }
    for key in env_map.keys() {
        if key.starts_with("RX_PROVIDER_") && key.ends_with("_API_KEY") {
            secret_names.insert(key.clone());
        }
    }
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
    let mut secrets = secret_names
        .iter()
        .filter_map(|name| env_map.get(name).cloned())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if let Some(home) = home
        && let Ok(body) = std::fs::read_to_string(home.join(".recall/rx.keys"))
        && let Ok(value) = toml::from_str::<toml::Value>(&body)
        && let Some(table) = value.as_table()
    {
        for val in table.values() {
            if let Some(secret) = val.as_str().filter(|value| !value.is_empty()) {
                secrets.push(secret.to_owned());
            }
        }
    }
    if let Ok(rx_home) = resolve_rx_home(rx_home_override, home.map(Path::to_path_buf))
        && let Ok(body) = std::fs::read_to_string(rx_home.join("auth.json"))
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(&body)
        && let Some(map) = value.as_object()
    {
        for entry in map.values() {
            let Some(obj) = entry.as_object() else {
                continue;
            };
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
                        && is_secret_env_key(env_key, &secret_names)
                    {
                        secrets.push(secret.to_owned());
                    }
                }
            }
        }
    }
    secrets
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
    env_get: &impl Fn(&str) -> Option<String>,
) -> bool {
    if !is_valid_provider_id(id) {
        return false;
    }
    let bundled_env = RX_BUNDLED_PROVIDERS
        .iter()
        .find(|(provider_id, _)| *provider_id == id)
        .map(|(_, env)| *env);
    let entry = config.and_then(|cfg| cfg.provider.get(id));
    if bundled_env.is_none()
        && entry
            .and_then(|e| e.base_url.as_deref())
            .is_none_or(str::is_empty)
    {
        return false;
    }
    let env_name = entry
        .and_then(|e| e.env.as_deref())
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .or_else(|| bundled_env.map(str::to_owned))
        .unwrap_or_else(|| generated_env(id));
    let stored = keys.get(id).is_some_and(|value| !value.is_empty());
    let environment = env_get(&env_name).is_some_and(|value| !value.is_empty());
    let local_magpie = id == "magpie"
        && bundled_env.is_some()
        && entry
            .and_then(|e| e.base_url.as_deref())
            .is_none_or(is_loopback_http_url)
        && entry
            .and_then(|e| e.anthropic_base.as_deref())
            .is_none_or(is_loopback_http_url);
    match entry.and_then(|e| e.auth.as_deref()).unwrap_or("api_key") {
        "api_key" => stored || (bundled_env.is_some() && environment) || local_magpie,
        "env" => environment || local_magpie,
        _ => false,
    }
}

fn rx_native_ready(home: &Path, rx_home: &Path, env_get: &impl Fn(&str) -> Option<String>) -> bool {
    if auth_json_providers(&rx_home.join("auth.json"))
        .is_some_and(|providers| !providers.is_empty())
    {
        return true;
    }
    let model = env_get("RX_MODEL")
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .or_else(|| config_json_model(&rx_home.join("config.json")));
    let Some((provider, model_id)) = model.as_deref().and_then(|value| value.split_once('/'))
    else {
        return false;
    };
    !provider.is_empty()
        && !model_id.is_empty()
        && native_provider_env_ready(provider, home, env_get)
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
                if key_ok != Some(true) && env_ok != Some(true) {
                    return None;
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
            }
            _ => return None,
        }
        providers.insert(id.clone());
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

fn is_loopback_http_url(endpoint: &str) -> bool {
    let Some((scheme, rest)) = endpoint.split_once("://") else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return false;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') {
        return false;
    }
    let host = if let Some(bracketed) = authority.strip_prefix('[') {
        let Some((ipv6, tail)) = bracketed.split_once(']') else {
            return false;
        };
        if !tail.is_empty() && !tail.strip_prefix(':').is_some_and(all_ascii_digits) {
            return false;
        }
        ipv6
    } else {
        match authority.split_once(':') {
            Some((host, port)) if all_ascii_digits(port) => host,
            Some(_) => return false,
            None => authority,
        }
    };
    host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}
