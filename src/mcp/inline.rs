mod rooms;

pub(super) use rooms::{
    render_create_room, render_list_rooms, render_room_output, render_send_message,
};

use rmcp::model::CallToolResult;
use serde::Serialize;
use serde_json::Value;

use super::delivery::{DeliveryState, DeliveryStatus, WaitOutput};
use crate::types::AgentKind;

pub(super) const INLINE_MAX_BYTES: usize = 8_800;

pub(super) fn is_zero(value: &usize) -> bool {
    *value == 0
}

#[derive(Debug, Serialize)]
struct ErrorOutput {
    error: String,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct DeliveryView {
    pub(super) delivery_id: String,
    pub(super) room_id: String,
    pub(super) seat_id: String,
    pub(super) seat_name: String,
    pub(super) agent: AgentKind,
    pub(super) status: DeliveryStatus,
    pub(super) final_answer: Option<String>,
    pub(super) error: Option<String>,
    pub(super) resume_command: Option<String>,
    pub(super) answer_incomplete: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(super) output_folded: bool,
    pub(super) previous_delivery_uncertain: bool,
    pub(super) question: Option<Value>,
    pub(super) uncertain_delivery_id: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct WaitOutputView {
    pub(super) room_id: String,
    pub(super) completed: bool,
    pub(super) timed_out: bool,
    pub(super) deliveries: Vec<DeliveryView>,
    #[serde(skip_serializing_if = "is_zero")]
    pub(super) omitted_deliveries: usize,
}

pub(super) fn sanitize_terminal_text(input: &str) -> String {
    let stripped = strip_ansi_sequences(input);
    if !stripped.contains('\r') {
        return stripped;
    }
    let lf_normalized = stripped.replace("\r\n", "\n");
    if !lf_normalized.contains('\r') {
        return lf_normalized;
    }
    lf_normalized
        .split('\n')
        .map(|line| {
            if !line.contains('\r') {
                line
            } else {
                line.split('\r')
                    .rfind(|frame| !frame.trim().is_empty())
                    .or_else(|| line.split('\r').rfind(|frame| !frame.is_empty()))
                    .unwrap_or("")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn strip_ansi_sequences(input: &str) -> String {
    if !input.contains('\x1b') {
        return input.to_string();
    }
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\x1b' {
            output.push(ch);
            continue;
        }
        match chars.peek().copied() {
            Some('[') => {
                chars.next();
                while let Some(&next) = chars.peek() {
                    let code = next as u32;
                    if (0x20..=0x3F).contains(&code) {
                        chars.next();
                    } else if (0x40..=0x7E).contains(&code) {
                        chars.next();
                        break;
                    } else {
                        break;
                    }
                }
            }
            Some(']' | 'P' | 'X' | '^' | '_') => {
                chars.next();
                while let Some(inner) = chars.next() {
                    if inner == '\x07' {
                        break;
                    }
                    if inner == '\x1b' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            Some('(' | ')' | '*' | '+' | '-' | '.' | '/') => {
                chars.next();
                if let Some(&next) = chars.peek()
                    && (0x20..=0x7E).contains(&(next as u32))
                {
                    chars.next();
                }
            }
            Some(next) if (0x30..=0x7E).contains(&(next as u32)) => {
                chars.next();
            }
            _ => {}
        }
    }
    output
}

pub(super) fn json_escaped_char_len(ch: char) -> usize {
    match ch {
        '"' | '\\' | '\x08' | '\x0c' | '\n' | '\r' | '\t' => 2,
        '\x00'..='\x1f' => 6,
        _ => ch.len_utf8(),
    }
}

pub(super) fn json_escaped_str_len(text: &str) -> usize {
    text.chars().map(json_escaped_char_len).sum()
}

fn take_prefix_by_escaped(text: &str, max_escaped: usize) -> &str {
    let mut used = 0usize;
    let mut end = 0usize;
    for (idx, ch) in text.char_indices() {
        let cost = json_escaped_char_len(ch);
        if used + cost > max_escaped {
            break;
        }
        used += cost;
        end = idx + ch.len_utf8();
    }
    &text[..end]
}

fn recovery_marker(folded_utf8_bytes: usize, delivery_id: Option<&str>) -> String {
    match delivery_id {
        Some(id) => format!(
            "\n...[folded {folded_utf8_bytes} UTF-8 bytes. Call wait_output with delivery_ids: [\"{id}\"], timeout_ms: 0, and full_output: true for the full text.]...\n"
        ),
        None => format!("\n...[folded {folded_utf8_bytes} UTF-8 bytes.]...\n"),
    }
}

pub(super) fn fold_text_to_json_budget(
    text: &str,
    max_escaped_bytes: usize,
    delivery_id: Option<&str>,
) -> (String, bool) {
    if json_escaped_str_len(text) <= max_escaped_bytes {
        return (text.to_string(), false);
    }
    let worst_marker = recovery_marker(text.len(), delivery_id);
    let worst_marker_cost = json_escaped_str_len(&worst_marker);
    if max_escaped_bytes <= worst_marker_cost {
        let compact_worst = format!("...[folded {} UTF-8 bytes]...", text.len());
        let compact_cost = json_escaped_str_len(&compact_worst);
        if compact_cost <= max_escaped_bytes {
            let prefix = take_prefix_by_escaped(text, max_escaped_bytes - compact_cost);
            let folded = text.len() - prefix.len();
            return (format!("{prefix}...[folded {folded} UTF-8 bytes]..."), true);
        }
        if max_escaped_bytes >= 3 {
            let prefix = take_prefix_by_escaped(text, max_escaped_bytes - 3);
            return (format!("{prefix}..."), true);
        }
        return (
            take_prefix_by_escaped(text, max_escaped_bytes).to_string(),
            true,
        );
    }

    let body_budget = max_escaped_bytes - worst_marker_cost;
    let head_budget = body_budget * 38 / 100;

    let mut raw_head_end = 0usize;
    let mut raw_head_used = 0usize;
    let mut aligned_head: Option<(usize, usize)> = None;
    for (idx, ch) in text.char_indices() {
        let cost = json_escaped_char_len(ch);
        if raw_head_used + cost > head_budget {
            break;
        }
        if ch == '\n' {
            aligned_head = Some((idx, raw_head_used));
        }
        raw_head_used += cost;
        raw_head_end = idx + ch.len_utf8();
    }
    let (head_end, head_used) = match aligned_head {
        Some((nl_idx, nl_used)) if nl_used * 2 >= head_budget => (nl_idx, nl_used),
        _ => (raw_head_end, raw_head_used),
    };

    let tail_budget = body_budget - head_used;
    let tail_pool = &text[head_end..];
    let mut raw_rel_start = tail_pool.len();
    let mut raw_tail_used = 0usize;
    let mut aligned_tail: Option<(usize, usize)> = None;
    for (rel_idx, ch) in tail_pool.char_indices().rev() {
        let cost = json_escaped_char_len(ch);
        if raw_tail_used + cost > tail_budget {
            break;
        }
        if ch == '\n' {
            aligned_tail = Some((rel_idx + 1, raw_tail_used));
        }
        raw_tail_used += cost;
        raw_rel_start = rel_idx;
    }
    let tail_start = match aligned_tail {
        Some((after_nl_rel, after_nl_used)) if after_nl_used * 2 >= tail_budget => {
            head_end + after_nl_rel
        }
        _ => head_end + raw_rel_start,
    };

    let folded_bytes = tail_start - head_end;
    let marker = recovery_marker(folded_bytes, delivery_id);
    (
        format!("{}{}{}", &text[..head_end], marker, &text[tail_start..]),
        true,
    )
}

pub(super) fn water_fill(needs: &[usize], total_budget: usize) -> Vec<usize> {
    let count = needs.len();
    if count == 0 {
        return Vec::new();
    }
    let mut indices: Vec<usize> = (0..count).collect();
    indices.sort_by_key(|&idx| needs[idx]);
    let mut allocations = vec![0usize; count];
    let mut remaining_budget = total_budget;
    let mut remaining_items = count;
    for idx in indices {
        let share = remaining_budget / remaining_items;
        let granted = needs[idx].min(share);
        allocations[idx] = granted;
        remaining_budget -= granted;
        remaining_items -= 1;
    }
    allocations
}

enum SlotKind {
    FinalAnswer(usize),
    Error(usize),
    Question(usize),
}

fn to_delivery_view(state: DeliveryState, sanitize: bool) -> DeliveryView {
    DeliveryView {
        delivery_id: state.delivery_id,
        room_id: state.room_id,
        seat_id: state.seat_id,
        seat_name: state.seat_name,
        agent: state.agent,
        status: state.status,
        final_answer: state.final_answer.map(|text| {
            if sanitize {
                sanitize_terminal_text(&text)
            } else {
                text
            }
        }),
        error: state.error.map(|text| {
            if sanitize {
                sanitize_terminal_text(&text)
            } else {
                text
            }
        }),
        resume_command: state.resume_command,
        answer_incomplete: state.answer_incomplete,
        output_folded: false,
        previous_delivery_uncertain: state.previous_delivery_uncertain,
        question: state.question,
        uncertain_delivery_id: state.uncertain_delivery_id,
    }
}

pub(super) fn serialized_len<T: Serialize>(value: &T) -> usize {
    serde_json::to_string(value).map_or(usize::MAX, |text| text.len())
}

fn skeleton_delivery(item: &DeliveryView) -> DeliveryView {
    let has_variable =
        item.final_answer.is_some() || item.error.is_some() || item.question.is_some();
    DeliveryView {
        delivery_id: item.delivery_id.clone(),
        room_id: item.room_id.clone(),
        seat_id: item.seat_id.clone(),
        seat_name: item.seat_name.clone(),
        agent: item.agent,
        status: item.status,
        final_answer: item.final_answer.as_ref().map(|_| String::new()),
        error: item.error.as_ref().map(|_| String::new()),
        resume_command: item.resume_command.clone(),
        answer_incomplete: item.answer_incomplete,
        output_folded: has_variable,
        previous_delivery_uncertain: item.previous_delivery_uncertain,
        question: item.question.as_ref().map(|_| Value::String(String::new())),
        uncertain_delivery_id: item.uncertain_delivery_id.clone(),
    }
}

pub(super) fn render_wait_output(output: WaitOutput, full_output: bool) -> WaitOutputView {
    if full_output {
        return WaitOutputView {
            room_id: output.room_id,
            completed: output.completed,
            timed_out: output.timed_out,
            deliveries: output
                .deliveries
                .into_iter()
                .map(|item| to_delivery_view(item, false))
                .collect(),
            omitted_deliveries: 0,
        };
    }

    let sanitized: Vec<DeliveryView> = output
        .deliveries
        .into_iter()
        .map(|item| to_delivery_view(item, true))
        .collect();
    let candidate = WaitOutputView {
        room_id: output.room_id.clone(),
        completed: output.completed,
        timed_out: output.timed_out,
        deliveries: sanitized.clone(),
        omitted_deliveries: 0,
    };
    if serialized_len(&candidate) <= INLINE_MAX_BYTES {
        return candidate;
    }

    let total = sanitized.len();
    let mut keep_count = total;
    let skeleton_bytes = loop {
        let skeleton_view = WaitOutputView {
            room_id: output.room_id.clone(),
            completed: output.completed,
            timed_out: output.timed_out,
            deliveries: sanitized[..keep_count]
                .iter()
                .map(skeleton_delivery)
                .collect(),
            omitted_deliveries: total - keep_count,
        };
        let bytes = serialized_len(&skeleton_view);
        if bytes <= INLINE_MAX_BYTES || keep_count == 0 {
            break bytes;
        }
        keep_count -= 1;
    };

    let mut kept: Vec<DeliveryView> = sanitized.into_iter().take(keep_count).collect();
    let available = INLINE_MAX_BYTES.saturating_sub(skeleton_bytes);

    let mut slots = Vec::new();
    let mut needs = Vec::new();
    for (idx, item) in kept.iter().enumerate() {
        if let Some(answer) = &item.final_answer {
            slots.push(SlotKind::FinalAnswer(idx));
            needs.push(json_escaped_str_len(answer));
        }
        if let Some(err) = &item.error {
            slots.push(SlotKind::Error(idx));
            needs.push(json_escaped_str_len(err));
        }
        if let Some(question) = &item.question {
            let raw_len = serialized_len(question);
            slots.push(SlotKind::Question(idx));
            needs.push(raw_len.saturating_sub(2));
        }
    }

    let allocations = water_fill(&needs, available);
    for (slot, (need, budget)) in slots.into_iter().zip(needs.into_iter().zip(allocations)) {
        match slot {
            SlotKind::FinalAnswer(idx) => {
                let id = kept[idx].delivery_id.clone();
                if let Some(text) = kept[idx].final_answer.take() {
                    let (rendered, folded) = fold_text_to_json_budget(&text, budget, Some(&id));
                    kept[idx].final_answer = Some(rendered);
                    if folded {
                        kept[idx].output_folded = true;
                    }
                }
            }
            SlotKind::Error(idx) => {
                let id = kept[idx].delivery_id.clone();
                if let Some(text) = kept[idx].error.take() {
                    let (rendered, folded) = fold_text_to_json_budget(&text, budget, Some(&id));
                    kept[idx].error = Some(rendered);
                    if folded {
                        kept[idx].output_folded = true;
                    }
                }
            }
            SlotKind::Question(idx) => {
                if need > budget {
                    let id = kept[idx].delivery_id.clone();
                    if let Some(question) = kept[idx].question.take() {
                        let raw = sanitize_terminal_text(
                            &serde_json::to_string(&question).unwrap_or_default(),
                        );
                        let (rendered, _) = fold_text_to_json_budget(&raw, budget, Some(&id));
                        kept[idx].question = Some(Value::String(rendered));
                        kept[idx].output_folded = true;
                    }
                }
            }
        }
    }

    WaitOutputView {
        room_id: output.room_id,
        completed: output.completed,
        timed_out: output.timed_out,
        deliveries: kept,
        omitted_deliveries: total - keep_count,
    }
}

pub(super) fn structured_ok<T: Serialize>(value: T) -> CallToolResult {
    CallToolResult::structured(serde_json::to_value(value).unwrap_or(Value::Null))
}

pub(super) fn structured_err(error: anyhow::Error) -> CallToolResult {
    let cleaned = sanitize_terminal_text(&format!("{error:#}"));
    let candidate = ErrorOutput {
        error: cleaned.clone(),
    };
    if serialized_len(&candidate) <= INLINE_MAX_BYTES {
        return CallToolResult::structured_error(
            serde_json::to_value(candidate).unwrap_or(Value::Null),
        );
    }
    let skeleton_cost = serialized_len(&ErrorOutput {
        error: String::new(),
    });
    let budget = INLINE_MAX_BYTES.saturating_sub(skeleton_cost);
    let (folded, _) = fold_text_to_json_budget(&cleaned, budget, None);
    CallToolResult::structured_error(
        serde_json::to_value(ErrorOutput { error: folded }).unwrap_or(Value::Null),
    )
}

#[cfg(test)]
mod tests;
