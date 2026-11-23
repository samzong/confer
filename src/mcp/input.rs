use std::path::PathBuf;

use serde::Deserialize;
use serde::de::Error as _;
use serde_json::Value;

use super::api::{
    AddSeatArgs, CancelDeliveryArgs, CreateRoomArgs, SeatSpecInput, SendMessageArgs,
    WaitOutputParams,
};

pub(super) const CREATE_ROOM_FALLBACK: &str = "Pass target_size: 1 or one seat and add more seats with add_seat, or pass seats as a JSON string.";
pub(super) const ADD_SEAT_FALLBACK: &str =
    "Pass seat as an object with scalar fields, or pass seat as a JSON object string.";
pub(super) const SEND_MESSAGE_FALLBACK: &str = "Use \"*\" to broadcast, send to one recipient at a time, or pass recipients as a JSON array string.";
pub(super) const WAIT_OUTPUT_FALLBACK: &str = "Omit delivery_ids to wait for all room deliveries, pass one ID, or pass delivery_ids as a JSON array string.";
pub(super) const CANCEL_DELIVERY_FALLBACK: &str =
    "Cancel one delivery ID per call, or pass delivery_ids as a JSON array string.";

pub(super) fn circuit_breaker_error(
    tool: &str,
    field: &str,
    received: &str,
    constraint: Option<&str>,
    fallback: &str,
) -> String {
    match constraint {
        Some(rule) => format!(
            "{tool} invalid parameter `{field}`: received {received}, {rule}. Do NOT retry the same {tool} payload unchanged because untyped call_mcp_tool calls can collapse nested arrays and objects. {fallback}"
        ),
        None => format!(
            "{tool} invalid parameter `{field}`: received {received}. Do NOT retry the same {tool} payload unchanged because untyped call_mcp_tool calls can collapse nested arrays and objects. {fallback}"
        ),
    }
}

pub(super) fn empty_create_room_error() -> String {
    circuit_breaker_error(
        "create_room",
        "seats",
        "no target_size and no seats",
        Some("provide target_size or at least one seat"),
        CREATE_ROOM_FALLBACK,
    )
}

pub(super) fn empty_recipients_error() -> String {
    circuit_breaker_error(
        "send_message",
        "recipients",
        "empty array ([])",
        Some("recipients must not be empty"),
        SEND_MESSAGE_FALLBACK,
    )
}

pub(super) fn empty_cancel_delivery_ids_error() -> String {
    circuit_breaker_error(
        "cancel_delivery",
        "delivery_ids",
        "empty array ([])",
        Some("delivery_ids must not be empty"),
        CANCEL_DELIVERY_FALLBACK,
    )
}

fn deserialize_present_value<'de, D>(deserializer: D) -> Result<Option<Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Value::deserialize(deserializer).map(Some)
}

fn describe_value(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(flag) => format!("boolean ({flag})"),
        Value::Number(number) => format!("number ({number})"),
        Value::Array(items) if items.is_empty() => "empty array ([])".into(),
        Value::Array(_) => "array".into(),
        Value::Object(_) => "object".into(),
        Value::String(text) => match serde_json::from_str::<Value>(text.trim()) {
            Ok(Value::Null) => "JSON string containing null".into(),
            Ok(Value::Bool(flag)) => format!("JSON string containing boolean ({flag})"),
            Ok(Value::Number(number)) => format!("JSON string containing number ({number})"),
            Ok(Value::Array(items)) if items.is_empty() => {
                "JSON string containing empty array ([])".into()
            }
            Ok(Value::Array(_)) => "JSON string containing array".into(),
            Ok(Value::Object(_)) => "JSON string containing object".into(),
            Ok(Value::String(_)) => "nested JSON string scalar".into(),
            Err(_) => "non-JSON string".into(),
        },
    }
}

fn extract_optional_string(
    map: &serde_json::Map<String, Value>,
    key: &str,
    tool: &str,
    field_prefix: &str,
    fallback: &str,
) -> Result<Option<String>, String> {
    let Some(value) = map.get(key) else {
        return Ok(None);
    };
    match value {
        Value::Null => Ok(None),
        Value::String(text) => Ok(Some(text.clone())),
        other => Err(circuit_breaker_error(
            tool,
            &format!("{field_prefix}.{key}"),
            &describe_value(other),
            None,
            fallback,
        )),
    }
}

fn parse_seat_object(
    map: &serde_json::Map<String, Value>,
    tool: &str,
    field_path: &str,
    fallback: &str,
) -> Result<SeatSpecInput, String> {
    Ok(SeatSpecInput {
        agent: extract_optional_string(map, "agent", tool, field_path, fallback)?,
        model: extract_optional_string(map, "model", tool, field_path, fallback)?,
        reasoning_effort: extract_optional_string(
            map,
            "reasoning_effort",
            tool,
            field_path,
            fallback,
        )?,
        name: extract_optional_string(map, "name", tool, field_path, fallback)?,
        instructions: extract_optional_string(map, "instructions", tool, field_path, fallback)?,
    })
}

fn parse_seat_spec(
    value: Value,
    tool: &str,
    field_path: &str,
    fallback: &str,
) -> Result<SeatSpecInput, String> {
    match &value {
        Value::Object(map) => parse_seat_object(map, tool, field_path, fallback),
        Value::String(text) => match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(map)) => parse_seat_object(&map, tool, field_path, fallback),
            Ok(_) => Err(circuit_breaker_error(
                tool,
                field_path,
                &describe_value(&value),
                None,
                fallback,
            )),
            Err(_) => Err(circuit_breaker_error(
                tool,
                field_path,
                "non-JSON string",
                None,
                fallback,
            )),
        },
        other => Err(circuit_breaker_error(
            tool,
            field_path,
            &describe_value(other),
            None,
            fallback,
        )),
    }
}

fn parse_seat_list(value: Value, target_size: Option<usize>) -> Result<Vec<SeatSpecInput>, String> {
    let items = match value {
        Value::Array(items) => items,
        Value::String(ref text) => match serde_json::from_str::<Value>(text) {
            Ok(Value::Array(items)) => items,
            _ => {
                return Err(circuit_breaker_error(
                    "create_room",
                    "seats",
                    &describe_value(&value),
                    None,
                    CREATE_ROOM_FALLBACK,
                ));
            }
        },
        other => {
            return Err(circuit_breaker_error(
                "create_room",
                "seats",
                &describe_value(&other),
                None,
                CREATE_ROOM_FALLBACK,
            ));
        }
    };
    if items.is_empty() && target_size.is_none() {
        return Err(circuit_breaker_error(
            "create_room",
            "seats",
            "empty array ([])",
            Some("provide target_size or at least one seat"),
            CREATE_ROOM_FALLBACK,
        ));
    }
    let mut parsed = Vec::with_capacity(items.len());
    for (index, item) in items.into_iter().enumerate() {
        let path = format!("seats[{index}]");
        parsed.push(parse_seat_spec(
            item,
            "create_room",
            &path,
            CREATE_ROOM_FALLBACK,
        )?);
    }
    Ok(parsed)
}

fn parse_string_list(
    value: Value,
    tool: &str,
    field: &str,
    empty_rule: Option<&str>,
    fallback: &str,
) -> Result<Vec<String>, String> {
    let items = match value {
        Value::Array(items) => items,
        Value::String(ref text) => match serde_json::from_str::<Value>(text) {
            Ok(Value::Array(items)) => items,
            _ => {
                return Err(circuit_breaker_error(
                    tool,
                    field,
                    &describe_value(&value),
                    None,
                    fallback,
                ));
            }
        },
        other => {
            return Err(circuit_breaker_error(
                tool,
                field,
                &describe_value(&other),
                None,
                fallback,
            ));
        }
    };
    if items.is_empty()
        && let Some(rule) = empty_rule
    {
        return Err(circuit_breaker_error(
            tool,
            field,
            "empty array ([])",
            Some(rule),
            fallback,
        ));
    }
    let mut parsed = Vec::with_capacity(items.len());
    for (index, item) in items.into_iter().enumerate() {
        match item {
            Value::String(text) => parsed.push(text),
            other => {
                return Err(circuit_breaker_error(
                    tool,
                    &format!("{field}[{index}]"),
                    &describe_value(&other),
                    None,
                    fallback,
                ));
            }
        }
    }
    Ok(parsed)
}

#[derive(Deserialize)]
struct CreateRoomArgsWire {
    workspace: PathBuf,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    target_size: Option<usize>,
    #[serde(default)]
    host_agent: Option<String>,
    #[serde(default, deserialize_with = "deserialize_present_value")]
    seats: Option<Value>,
}

impl<'de> Deserialize<'de> for CreateRoomArgs {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = CreateRoomArgsWire::deserialize(deserializer)?;
        let seats = match wire.seats {
            None => Vec::new(),
            Some(raw) => parse_seat_list(raw, wire.target_size).map_err(D::Error::custom)?,
        };
        Ok(Self {
            workspace: wire.workspace,
            name: wire.name,
            target_size: wire.target_size,
            host_agent: wire.host_agent,
            seats,
        })
    }
}

#[derive(Deserialize)]
struct AddSeatArgsWire {
    workspace: PathBuf,
    room_id: String,
    #[serde(default, deserialize_with = "deserialize_present_value")]
    seat: Option<Value>,
}

impl<'de> Deserialize<'de> for AddSeatArgs {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = AddSeatArgsWire::deserialize(deserializer)?;
        let raw = wire.seat.ok_or_else(|| D::Error::missing_field("seat"))?;
        let seat = parse_seat_spec(raw, "add_seat", "seat", ADD_SEAT_FALLBACK)
            .map_err(D::Error::custom)?;
        Ok(Self {
            workspace: wire.workspace,
            room_id: wire.room_id,
            seat,
        })
    }
}

#[derive(Deserialize)]
struct SendMessageArgsWire {
    workspace: PathBuf,
    room_id: String,
    #[serde(default, deserialize_with = "deserialize_present_value")]
    recipients: Option<Value>,
    message: String,
}

impl<'de> Deserialize<'de> for SendMessageArgs {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = SendMessageArgsWire::deserialize(deserializer)?;
        let raw = wire
            .recipients
            .ok_or_else(|| D::Error::missing_field("recipients"))?;
        let recipients = parse_string_list(
            raw,
            "send_message",
            "recipients",
            Some("recipients must not be empty"),
            SEND_MESSAGE_FALLBACK,
        )
        .map_err(D::Error::custom)?;
        Ok(Self {
            workspace: wire.workspace,
            room_id: wire.room_id,
            recipients,
            message: wire.message,
        })
    }
}

#[derive(Deserialize)]
struct WaitOutputArgsWire {
    workspace: PathBuf,
    room_id: String,
    #[serde(default, deserialize_with = "deserialize_present_value")]
    delivery_ids: Option<Value>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    full_output: Option<bool>,
}

impl<'de> Deserialize<'de> for WaitOutputParams {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = WaitOutputArgsWire::deserialize(deserializer)?;
        let delivery_ids = match wire.delivery_ids {
            None => Vec::new(),
            Some(raw) => parse_string_list(
                raw,
                "wait_output",
                "delivery_ids",
                None,
                WAIT_OUTPUT_FALLBACK,
            )
            .map_err(D::Error::custom)?,
        };
        Ok(Self {
            workspace: wire.workspace,
            room_id: wire.room_id,
            delivery_ids,
            timeout_ms: wire.timeout_ms,
            full_output: wire.full_output,
        })
    }
}

#[derive(Deserialize)]
struct CancelDeliveryArgsWire {
    workspace: PathBuf,
    room_id: String,
    #[serde(default, deserialize_with = "deserialize_present_value")]
    delivery_ids: Option<Value>,
}

impl<'de> Deserialize<'de> for CancelDeliveryArgs {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = CancelDeliveryArgsWire::deserialize(deserializer)?;
        let raw = wire
            .delivery_ids
            .ok_or_else(|| D::Error::missing_field("delivery_ids"))?;
        let delivery_ids = parse_string_list(
            raw,
            "cancel_delivery",
            "delivery_ids",
            Some("delivery_ids must not be empty"),
            CANCEL_DELIVERY_FALLBACK,
        )
        .map_err(D::Error::custom)?;
        Ok(Self {
            workspace: wire.workspace,
            room_id: wire.room_id,
            delivery_ids,
        })
    }
}
