use anyhow::{Result, bail};

use super::Invocation;
use crate::types::AgentKind;

pub(super) fn validate_invocation(invocation: &Invocation) -> Result<()> {
    if invocation.message.trim().is_empty() {
        bail!("message must not be empty");
    }
    validate_seat_config(
        invocation.agent,
        invocation.model.as_deref(),
        invocation.reasoning_effort.as_deref(),
    )
}

pub(crate) fn validate_seat_config(
    agent: AgentKind,
    model: Option<&str>,
    effort: Option<&str>,
) -> Result<()> {
    if let Some(effort) = effort {
        let general = [
            "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
        ];
        let label = match agent {
            AgentKind::Kimi => {
                (!["on", "low", "high", "max"].contains(&effort)).then_some("Kimi thinking")
            }
            AgentKind::Devin => Some("Devin reasoning_effort"),
            _ if !general.contains(&effort) => Some("reasoning_effort"),
            AgentKind::Agy if !["low", "medium", "high"].contains(&effort) => {
                Some("Antigravity reasoning_effort")
            }
            AgentKind::Copilot if effort == "ultra" => Some("Copilot reasoning_effort"),
            _ => None,
        };
        if let Some(label) = label {
            bail!("unsupported {label} '{effort}'");
        }
    }
    if agent == AgentKind::Cursor {
        cursor_config(model, effort)?;
    }
    Ok(())
}

pub(super) fn cursor_config<'a>(
    model: Option<&'a str>,
    effort: Option<&'a str>,
) -> Result<Vec<(&'a str, &'a str)>> {
    if effort.is_some() && model.is_some_and(|model| model.contains('[')) {
        bail!("Cursor model already encodes options; omit reasoning_effort");
    }
    let mut options = Vec::new();
    if let Some(model) = model {
        if let Some((base, parameters)) = model.split_once('[') {
            let Some(parameters) = parameters.strip_suffix(']') else {
                bail!("invalid Cursor model options");
            };
            if base.is_empty() {
                bail!("Cursor model must not be empty");
            }
            options.push(("model", base));
            for parameter in parameters.split(',') {
                let Some((name, value)) = parameter.split_once('=') else {
                    bail!("invalid Cursor model option");
                };
                if name.is_empty() || value.is_empty() {
                    bail!("invalid Cursor model option");
                }
                options.push((name, value));
            }
        } else {
            options.push(("model", model));
        }
    }
    if let Some(effort) = effort {
        options.push(("effort", effort));
    }
    Ok(options)
}

pub(super) fn session_options(invocation: &Invocation) -> Result<Vec<(&str, &str)>> {
    let model = invocation.model.as_deref();
    let effort = invocation.reasoning_effort.as_deref();
    let (mode, effort_id) = match invocation.agent {
        AgentKind::Cursor => return cursor_config(model, effort),
        AgentKind::Copilot => (None, "reasoning_effort"),
        AgentKind::Kimi => (Some(("mode", "auto")), "thinking"),
        _ => return Ok(Vec::new()),
    };
    Ok(mode
        .into_iter()
        .chain(model.map(|value| ("model", value)))
        .chain(effort.map(|value| (effort_id, value)))
        .collect())
}
