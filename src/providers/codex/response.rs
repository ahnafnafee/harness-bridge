//! Native model items shared by visible rollouts and resume checkpoints.
//!
//! Transcript ids retain the existing seeds so repeated imports remain stable.

use crate::ir::{Event, EventKind, Role, Session};
use crate::util::uuid7;
use serde_json::{json, Value};

pub(super) fn transcript_item(s: &Session, event: &Event, ms: i64, ordinal: i64) -> Option<Value> {
    let seed = match &event.kind {
        EventKind::Message { .. } => "msg",
        EventKind::Reasoning { .. } => "rs",
        EventKind::ToolCall { .. } => "fc",
        EventKind::ToolResult { .. } => "fco",
        EventKind::Meta { kind, .. } if kind == "tool-registry" => "reg",
        EventKind::Meta { kind, .. } if kind == "goal" => "goal",
        _ => return None,
    };
    encode(
        &event.kind,
        &uuid7(ms, &format!("{seed}|{}|{ordinal}", s.id)),
    )
}

pub(super) fn resume_item(s: &Session, event: &Event, ms: i64, ordinal: usize) -> Option<Value> {
    // Surface replay contains model messages and tool pairs, not archive telemetry.
    match &event.kind {
        EventKind::Message { .. }
        | EventKind::Reasoning { .. }
        | EventKind::ToolCall { .. }
        | EventKind::ToolResult { .. } => encode(
            &event.kind,
            &uuid7(ms, &format!("resume-item|{}|{ordinal}", s.id)),
        ),
        _ => None,
    }
}

fn message(id: &str, role: Role, text: &str) -> Value {
    let role_name = match role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Developer => "developer",
    };
    let content_type = if role == Role::Assistant {
        "output_text"
    } else {
        "input_text"
    };
    json!({"type":"message", "id":format!("msg_{id}"), "role":role_name,
        "content":[{"type":content_type, "text":text}]})
}

fn encode(kind: &EventKind, id: &str) -> Option<Value> {
    Some(match kind {
        EventKind::Message { role, text, .. } => message(id, *role, text),
        EventKind::Reasoning { text } => json!({"type":"reasoning", "id":format!("rs_{id}"),
            "summary":[{"type":"summary_text", "text":text}]}),
        EventKind::ToolCall {
            call_id,
            name,
            arguments,
        } => json!({"type":"function_call", "id":format!("fc_{id}"),
            "call_id":call_id, "name":name, "arguments":arguments}),
        EventKind::ToolResult { call_id, text } => {
            json!({"type":"function_call_output", "id":format!("fco_{id}"),
            "call_id":call_id, "output":text})
        }
        EventKind::Meta { kind, data } if kind == "tool-registry" => {
            let mut parts = Vec::new();
            for (key, label) in [("added", "Tools added"), ("removed", "Tools removed")] {
                let tools: Vec<_> = data
                    .get(key)
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                if !tools.is_empty() {
                    parts.push(format!("{label}: {}", tools.join(", ")));
                }
            }
            if parts.is_empty() {
                return None;
            }
            message(
                id,
                Role::Developer,
                &format!("<tool-registry> {}</tool-registry>", parts.join("; ")),
            )
        }
        EventKind::Meta { kind, data } if kind == "goal" => {
            let objective = data.get("objective").and_then(Value::as_str)?;
            message(
                id,
                Role::Developer,
                &format!("<session-goal> {objective}</session-goal>"),
            )
        }
        _ => return None,
    })
}
