use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub(super) const RX_BUNDLED_PROVIDERS: &[(&str, &str)] = &[
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

pub(super) const RX_NATIVE_ENV_KEYS: &[&str] = &[
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

pub(super) const RX_EXTRA_SECRET_ENV_KEYS: &[&str] = &["ANTHROPIC_AUTH_TOKEN"];

fn has_nonempty_env(env_get: &impl Fn(&str) -> Option<String>, name: &str) -> bool {
    env_get(name).is_some_and(|value| !value.is_empty())
}

pub(super) fn native_provider_env_ready(
    provider: &str,
    home: &Path,
    env_get: &impl Fn(&str) -> Option<String>,
) -> bool {
    match provider {
        "github-copilot" => has_nonempty_env(env_get, "COPILOT_GITHUB_TOKEN"),
        "anthropic" => {
            has_nonempty_env(env_get, "ANTHROPIC_AUTH_TOKEN")
                || has_nonempty_env(env_get, "ANTHROPIC_OAUTH_TOKEN")
                || has_nonempty_env(env_get, "ANTHROPIC_API_KEY")
                || (has_nonempty_env(env_get, "ANTHROPIC_FEDERATION_RULE_ID")
                    && has_nonempty_env(env_get, "ANTHROPIC_ORGANIZATION_ID")
                    && has_nonempty_env(env_get, "ANTHROPIC_IDENTITY_TOKEN_FILE"))
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
        "google-vertex" => {
            has_nonempty_env(env_get, "GOOGLE_CLOUD_API_KEY")
                || ((has_nonempty_env(env_get, "GOOGLE_CLOUD_PROJECT")
                    || has_nonempty_env(env_get, "GCLOUD_PROJECT"))
                    && has_nonempty_env(env_get, "GOOGLE_CLOUD_LOCATION")
                    && env_get("GOOGLE_APPLICATION_CREDENTIALS")
                        .filter(|value| !value.is_empty())
                        .map_or_else(
                            || home.join(".config/gcloud/application_default_credentials.json"),
                            PathBuf::from,
                        )
                        .is_file())
        }
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
        "cloudflare-workers-ai" => {
            has_nonempty_env(env_get, "CLOUDFLARE_API_KEY")
                && has_nonempty_env(env_get, "CLOUDFLARE_ACCOUNT_ID")
        }
        "cloudflare-ai-gateway" => {
            has_nonempty_env(env_get, "CLOUDFLARE_API_KEY")
                && has_nonempty_env(env_get, "CLOUDFLARE_ACCOUNT_ID")
                && has_nonempty_env(env_get, "CLOUDFLARE_GATEWAY_ID")
        }
        "xiaomi" => has_nonempty_env(env_get, "XIAOMI_API_KEY"),
        "xiaomi-token-plan-cn" => has_nonempty_env(env_get, "XIAOMI_TOKEN_PLAN_CN_API_KEY"),
        "xiaomi-token-plan-ams" => has_nonempty_env(env_get, "XIAOMI_TOKEN_PLAN_AMS_API_KEY"),
        "xiaomi-token-plan-sgp" => has_nonempty_env(env_get, "XIAOMI_TOKEN_PLAN_SGP_API_KEY"),
        "amazon-bedrock" => {
            has_nonempty_env(env_get, "AWS_BEARER_TOKEN_BEDROCK")
                || has_nonempty_env(env_get, "AWS_PROFILE")
                || (has_nonempty_env(env_get, "AWS_ACCESS_KEY_ID")
                    && has_nonempty_env(env_get, "AWS_SECRET_ACCESS_KEY"))
                || has_nonempty_env(env_get, "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI")
                || has_nonempty_env(env_get, "AWS_CONTAINER_CREDENTIALS_FULL_URI")
                || has_nonempty_env(env_get, "AWS_WEB_IDENTITY_TOKEN_FILE")
        }
        _ => false,
    }
}

pub(super) fn is_secret_env_key(key: &str, secret_names: &BTreeSet<String>) -> bool {
    secret_names.contains(key)
        || key.ends_with("_API_KEY")
        || key.ends_with("_SECRET_ACCESS_KEY")
        || key.ends_with("_AUTH_TOKEN")
        || key.ends_with("_OAUTH_TOKEN")
        || key.ends_with("_BEARER_TOKEN")
        || key.ends_with("_SESSION_TOKEN")
}

pub(super) fn generated_env(id: &str) -> String {
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
