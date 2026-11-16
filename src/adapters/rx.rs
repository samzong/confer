use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use tokio::process::Command;

use super::Invocation;

const RX_BUNDLED_PROVIDERS: &[(&str, &str)] = &[
    ("openrouter", "OPENROUTER_API_KEY"),
    ("tokener", "TOKENER_API_KEY"),
    ("abacus", "ABACUS_API_KEY"),
    ("alibaba", "DASHSCOPE_API_KEY"),
    ("alibaba-cn", "DASHSCOPE_API_KEY"),
    ("alibaba-coding-plan", "ALIBABA_CODING_PLAN_API_KEY"),
    ("alibaba-coding-plan-cn", "ALIBABA_CODING_PLAN_API_KEY"),
    ("alibaba-token-plan", "ALIBABA_TOKEN_PLAN_API_KEY"),
    ("alibaba-token-plan-cn", "ALIBABA_TOKEN_PLAN_API_KEY"),
    ("ambient", "AMBIENT_API_KEY"),
    ("cortecs", "CORTECS_API_KEY"),
    ("deepseek", "DEEPSEEK_API_KEY"),
    ("echo", "ECHO_API_KEY"),
    ("empiriolabs", "EMPIRIOLABS_API_KEY"),
    ("fastrouter", "FASTROUTER_API_KEY"),
    ("freemodel", "FREEMODEL_API_KEY"),
    ("friendli", "FRIENDLI_TOKEN"),
    ("helicone", "HELICONE_API_KEY"),
    ("huggingface", "HF_TOKEN"),
    ("hyper", "HYPER_API_KEY"),
    ("impossibl", "IMPOSSIBL_API_KEY"),
    ("kenari", "KENARI_API_KEY"),
    ("kilo", "KILO_API_KEY"),
    ("lilac", "LILAC_API_KEY"),
    ("llmgateway", "LLMGATEWAY_API_KEY"),
    ("llmgateway-providers", "LLMGATEWAY_API_KEY"),
    ("llmtr", "LLMTR_API_KEY"),
    ("minimax", "MINIMAX_API_KEY"),
    ("minimax-cn", "MINIMAX_API_KEY"),
    ("moonshotai", "MOONSHOT_API_KEY"),
    ("moonshotai-cn", "MOONSHOT_API_KEY"),
    ("nearai", "NEARAI_API_KEY"),
    ("ollama-cloud", "OLLAMA_API_KEY"),
    ("opencode", "OPENCODE_API_KEY"),
    ("opencode-go", "OPENCODE_API_KEY"),
    ("pioneer", "PIONEER_API_KEY"),
    ("poe", "POE_API_KEY"),
    ("requesty", "REQUESTY_API_KEY"),
    ("routing-run", "ROUTING_RUN_API_KEY"),
    ("siliconflow", "SILICONFLOW_API_KEY"),
    ("siliconflow-cn", "SILICONFLOW_CN_API_KEY"),
    ("the-grid-ai", "THEGRID_API_KEY"),
    ("trustedrouter", "TRUSTEDROUTER_API_KEY"),
    ("zai", "ZHIPU_API_KEY"),
    ("zai-coding-plan", "ZHIPU_API_KEY"),
    ("zenmux", "ZENMUX_API_KEY"),
    ("zhipuai", "ZHIPU_API_KEY"),
    ("zhipuai-coding-plan", "ZHIPU_API_KEY"),
    ("magpie", "MAGPIE_API_KEY"),
];

const RX_NATIVE_ENV_KEYS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_OAUTH_TOKEN",
    "ANT_LING_API_KEY",
    "QWEN_TOKEN_PLAN_API_KEY",
    "QWEN_TOKEN_PLAN_CN_API_KEY",
    "OPENAI_API_KEY",
    "AZURE_OPENAI_API_KEY",
    "NVIDIA_API_KEY",
    "DEEPSEEK_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_CLOUD_API_KEY",
    "GROQ_API_KEY",
    "CEREBRAS_API_KEY",
    "XAI_API_KEY",
    "TYPESAFE_API_KEY",
    "RADIUS_API_KEY",
    "OPENROUTER_API_KEY",
    "AI_GATEWAY_API_KEY",
    "ZAI_API_KEY",
    "ZAI_CODING_CN_API_KEY",
    "MISTRAL_API_KEY",
    "MINIMAX_API_KEY",
    "MINIMAX_CN_API_KEY",
    "MOONSHOT_API_KEY",
    "HF_TOKEN",
    "FIREWORKS_API_KEY",
    "TOGETHER_API_KEY",
    "BASETEN_API_KEY",
    "OPENCODE_API_KEY",
    "KIMI_API_KEY",
    "META_API_KEY",
    "CLOUDFLARE_API_KEY",
    "XIAOMI_API_KEY",
    "XIAOMI_TOKEN_PLAN_CN_API_KEY",
    "XIAOMI_TOKEN_PLAN_AMS_API_KEY",
    "XIAOMI_TOKEN_PLAN_SGP_API_KEY",
    "COPILOT_GITHUB_TOKEN",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AWS_BEARER_TOKEN_BEDROCK",
];

const RX_EXTRA_SECRET_ENV_KEYS: &[&str] = &["ANTHROPIC_AUTH_TOKEN"];

#[derive(Deserialize)]
struct RxConfig {
    default_provider: Option<String>,
    #[serde(default)]
    provider: BTreeMap<String, RxProvider>,
}

#[derive(Deserialize)]
struct RxProvider {
    base_url: Option<String>,
    anthropic_base: Option<String>,
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
        Some("none") => rx_native_ready(&rx_home, &env_get),
        Some(provider) => launcher_provider_ready(provider, config.as_ref(), &keys, &env_get),
        None => {
            launcher_provider_ready("openrouter", config.as_ref(), &keys, &env_get)
                || rx_native_ready(&rx_home, &env_get)
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
        .env("RX_NO_INSTALL", "1");
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

pub(super) fn secret_env_names(
    home: Option<&Path>,
    extra_env_keys: impl IntoIterator<Item = String>,
) -> Vec<String> {
    let mut names = BTreeSet::new();
    for (_, env) in RX_BUNDLED_PROVIDERS {
        names.insert((*env).to_owned());
    }
    for env in RX_NATIVE_ENV_KEYS
        .iter()
        .chain(RX_EXTRA_SECRET_ENV_KEYS.iter())
    {
        names.insert((*env).to_owned());
    }
    for key in extra_env_keys {
        if key.starts_with("RX_PROVIDER_") && key.ends_with("_API_KEY") {
            names.insert(key);
        }
    }
    if let Some(home) = home
        && let Ok(body) = std::fs::read_to_string(home.join(".recall/rx.toml"))
        && let Ok(config) = toml::from_str::<RxConfig>(&body)
    {
        for (id, entry) in config.provider {
            if let Some(env) = entry.env.filter(|value| !value.is_empty()) {
                names.insert(env);
            }
            names.insert(generated_env(&id));
        }
    }
    names.into_iter().collect()
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

fn rx_native_ready(rx_home: &Path, env_get: &impl Fn(&str) -> Option<String>) -> bool {
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
    !provider.is_empty() && !model_id.is_empty() && native_provider_env_ready(provider, env_get)
}

fn has_nonempty_env(env_get: &impl Fn(&str) -> Option<String>, name: &str) -> bool {
    env_get(name).is_some_and(|value| !value.is_empty())
}

fn native_provider_env_ready(provider: &str, env_get: &impl Fn(&str) -> Option<String>) -> bool {
    match provider {
        "github-copilot" => has_nonempty_env(env_get, "COPILOT_GITHUB_TOKEN"),
        "anthropic" => {
            has_nonempty_env(env_get, "ANTHROPIC_OAUTH_TOKEN")
                || has_nonempty_env(env_get, "ANTHROPIC_API_KEY")
        }
        "ant-ling" => has_nonempty_env(env_get, "ANT_LING_API_KEY"),
        "qwen-token-plan" | "qwen-token-plan-individual" => {
            has_nonempty_env(env_get, "QWEN_TOKEN_PLAN_API_KEY")
        }
        "qwen-token-plan-cn" => has_nonempty_env(env_get, "QWEN_TOKEN_PLAN_CN_API_KEY"),
        "openai" => has_nonempty_env(env_get, "OPENAI_API_KEY"),
        "azure" => has_nonempty_env(env_get, "AZURE_OPENAI_API_KEY"),
        "nvidia" => has_nonempty_env(env_get, "NVIDIA_API_KEY"),
        "deepseek" => has_nonempty_env(env_get, "DEEPSEEK_API_KEY"),
        "google" => has_nonempty_env(env_get, "GEMINI_API_KEY"),
        "google-vertex" => has_nonempty_env(env_get, "GOOGLE_CLOUD_API_KEY"),
        "groq" => has_nonempty_env(env_get, "GROQ_API_KEY"),
        "cerebras" => has_nonempty_env(env_get, "CEREBRAS_API_KEY"),
        "xai" => has_nonempty_env(env_get, "XAI_API_KEY"),
        "typesafe" => has_nonempty_env(env_get, "TYPESAFE_API_KEY"),
        "radius" => has_nonempty_env(env_get, "RADIUS_API_KEY"),
        "openrouter" => has_nonempty_env(env_get, "OPENROUTER_API_KEY"),
        "vercel-ai-gateway" => has_nonempty_env(env_get, "AI_GATEWAY_API_KEY"),
        "zai" => has_nonempty_env(env_get, "ZAI_API_KEY"),
        "zai-coding-cn" => has_nonempty_env(env_get, "ZAI_CODING_CN_API_KEY"),
        "mistral" => has_nonempty_env(env_get, "MISTRAL_API_KEY"),
        "minimax" => has_nonempty_env(env_get, "MINIMAX_API_KEY"),
        "minimax-cn" => has_nonempty_env(env_get, "MINIMAX_CN_API_KEY"),
        "moonshotai" | "moonshotai-cn" => has_nonempty_env(env_get, "MOONSHOT_API_KEY"),
        "huggingface" => has_nonempty_env(env_get, "HF_TOKEN"),
        "fireworks" => has_nonempty_env(env_get, "FIREWORKS_API_KEY"),
        "together" => has_nonempty_env(env_get, "TOGETHER_API_KEY"),
        "baseten" => has_nonempty_env(env_get, "BASETEN_API_KEY"),
        "opencode" | "opencode-go" => has_nonempty_env(env_get, "OPENCODE_API_KEY"),
        "kimi-coding" => has_nonempty_env(env_get, "KIMI_API_KEY"),
        "meta" => has_nonempty_env(env_get, "META_API_KEY"),
        "cloudflare-workers-ai" | "cloudflare-ai-gateway" => {
            has_nonempty_env(env_get, "CLOUDFLARE_API_KEY")
        }
        "xiaomi" => has_nonempty_env(env_get, "XIAOMI_API_KEY"),
        "xiaomi-token-plan-cn" => has_nonempty_env(env_get, "XIAOMI_TOKEN_PLAN_CN_API_KEY"),
        "xiaomi-token-plan-ams" => has_nonempty_env(env_get, "XIAOMI_TOKEN_PLAN_AMS_API_KEY"),
        "xiaomi-token-plan-sgp" => has_nonempty_env(env_get, "XIAOMI_TOKEN_PLAN_SGP_API_KEY"),
        "amazon-bedrock" => {
            has_nonempty_env(env_get, "AWS_BEARER_TOKEN_BEDROCK")
                || (has_nonempty_env(env_get, "AWS_ACCESS_KEY_ID")
                    && has_nonempty_env(env_get, "AWS_SECRET_ACCESS_KEY"))
                || has_nonempty_env(env_get, "AWS_PROFILE")
        }
        _ => false,
    }
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
