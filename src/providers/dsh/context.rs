//! Decode the archive and replay DSH's current model surface independently.

use super::is_real_user;
use crate::ir::{Event, EventKind, Role};
use serde_json::{json, Value};

fn block_texts(blocks: Option<&Value>) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(arr) = blocks.and_then(|b| b.as_array()) {
        for b in arr {
            if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                if let Some(s) = b.get("text").and_then(|t| t.as_str()) {
                    out.push(s.to_string());
                }
            }
        }
    }
    out
}

fn join_text(parts: &[String]) -> String {
    parts
        .iter()
        .filter(|p| !p.trim().is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Decode records independently of the current model surface.
pub(super) fn decode_events(events: &[(i64, Value)]) -> (Vec<Event>, Option<String>) {
    let user_msg_ids: std::collections::HashSet<String> = events
        .iter()
        .filter_map(|(_, v)| {
            if v.get("type").and_then(|t| t.as_str()) == Some("user/message") {
                v.pointer("/data/id")
                    .and_then(|i| i.as_str())
                    .map(str::to_string)
            } else {
                None
            }
        })
        .collect();

    let mut evs: Vec<Event> = Vec::new();
    let mut seen_calls: std::collections::HashSet<String> = std::collections::HashSet::new();
    // call_id -> index in evs of the ToolResult event (for merging duplicate results)
    let mut result_idx: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut title: Option<String> = None;

    for (time, v) in events {
        let t = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let d = v.get("data").cloned().unwrap_or(Value::Null);
        match t {
            "turn/start" => evs.push(Event::at(Some(*time), EventKind::TurnStart)),
            "turn/end" => evs.push(Event::at(
                Some(*time),
                EventKind::TurnEnd {
                    reason: d
                        .pointer("/reason/kind")
                        .and_then(|k| k.as_str())
                        .map(str::to_string),
                },
            )),
            "session/title" => {
                title = d.get("title").and_then(|t| t.as_str()).map(str::to_string);
            }
            "system/message" => {
                let text = join_text(&block_texts(d.pointer("/message/content")));
                if !text.is_empty() {
                    evs.push(Event::at(
                        Some(*time),
                        EventKind::Message {
                            role: Role::Developer,
                            text,
                            source_kind: Some("system-prompt".into()),
                        },
                    ));
                }
            }
            "user/message" => {
                let text = join_text(&block_texts(d.get("content")));
                if !text.is_empty() {
                    let kind = d
                        .pointer("/source/kind")
                        .and_then(|k| k.as_str())
                        .map(str::to_string);
                    evs.push(Event::at(
                        Some(*time),
                        EventKind::Message {
                            role: if is_real_user(kind.as_deref()) {
                                Role::User
                            } else {
                                Role::Developer
                            },
                            text,
                            source_kind: kind,
                        },
                    ));
                }
            }
            "agent/inbox/spliced" => {
                if let Some(inserted) = d.get("inserted").and_then(|i| i.as_array()) {
                    for ins in inserted {
                        let id = ins.get("id").and_then(|i| i.as_str());
                        if id.map(|i| user_msg_ids.contains(i)).unwrap_or(false) {
                            continue; // surfaced later as user/message
                        }
                        let text = join_text(&block_texts(ins.get("content")));
                        if !text.is_empty() {
                            let kind = ins
                                .pointer("/source/kind")
                                .and_then(|k| k.as_str())
                                .map(str::to_string);
                            evs.push(Event::at(
                                Some(*time),
                                EventKind::Message {
                                    role: if is_real_user(kind.as_deref()) {
                                        Role::User
                                    } else {
                                        Role::Developer
                                    },
                                    text,
                                    source_kind: kind,
                                },
                            ));
                        }
                    }
                }
            }
            "assistant/message" => {
                let msg = d.get("message").cloned().unwrap_or(Value::Null);
                let mut reasoning = Vec::new();
                let mut text = Vec::new();
                for b in msg
                    .get("content")
                    .and_then(|c| c.as_array())
                    .into_iter()
                    .flatten()
                {
                    match b.get("type").and_then(|t| t.as_str()) {
                        Some("reasoning") => {
                            if let Some(s) = b.get("text").and_then(|t| t.as_str()) {
                                reasoning.push(s.to_string());
                            }
                        }
                        Some("text") => {
                            if let Some(s) = b.get("text").and_then(|t| t.as_str()) {
                                text.push(s.to_string());
                            }
                        }
                        _ => {} // tool-call blocks are captured by tool/call events
                    }
                }
                if !reasoning.is_empty() {
                    evs.push(Event::at(
                        Some(*time),
                        EventKind::Reasoning {
                            text: join_text(&reasoning),
                        },
                    ));
                }
                let text = join_text(&text);
                if !text.is_empty() {
                    evs.push(Event::at(
                        Some(*time),
                        EventKind::Message {
                            role: Role::Assistant,
                            text,
                            source_kind: None,
                        },
                    ));
                }
            }
            "tool/call" => {
                if let Some(cid) = d.get("callId").and_then(|c| c.as_str()) {
                    if seen_calls.insert(cid.to_string()) {
                        evs.push(Event::at(
                            Some(*time),
                            EventKind::ToolCall {
                                call_id: cid.to_string(),
                                name: d
                                    .get("name")
                                    .and_then(|n| n.as_str())
                                    .unwrap_or("unknown")
                                    .to_string(),
                                arguments: d
                                    .get("arguments")
                                    .and_then(|a| a.as_str())
                                    .unwrap_or("{}")
                                    .to_string(),
                            },
                        ));
                    }
                }
            }
            "tool/result" => {
                let cid = d
                    .pointer("/message/toolCallId")
                    .and_then(|c| c.as_str())
                    .map(str::to_string);
                let Some(cid) = cid else { continue };
                let text = join_text(&block_texts(d.pointer("/message/content")));
                match result_idx.get(&cid) {
                    Some(&i) => {
                        if let EventKind::ToolResult { text: prev, .. } = &mut evs[i].kind {
                            *prev = join_text(&[prev.clone(), text]);
                        }
                    }
                    None => {
                        evs.push(Event::at(
                            Some(*time),
                            EventKind::ToolResult {
                                call_id: cid.clone(),
                                text,
                            },
                        ));
                        result_idx.insert(cid, evs.len() - 1);
                    }
                }
            }
            "compaction/summary" => {
                let text = join_text(&block_texts(d.get("summary")));
                evs.push(Event::at(
                    Some(*time),
                    EventKind::Compaction {
                        id: d
                            .get("compactionId")
                            .and_then(|c| c.as_str())
                            .map(str::to_string),
                        text: (!text.is_empty()).then_some(text),
                    },
                ));
            }
            "developer/message" => {
                let mut added = Vec::new();
                let mut removed = Vec::new();
                for b in d
                    .pointer("/message/content")
                    .and_then(|c| c.as_array())
                    .into_iter()
                    .flatten()
                {
                    match b.get("type").and_then(|t| t.as_str()) {
                        Some("tool-addition") => {
                            if let Some(n) = b.get("toolName").and_then(|n| n.as_str()) {
                                added.push(n.to_string());
                            }
                        }
                        Some("tool-removal") => {
                            if let Some(n) = b.get("toolName").and_then(|n| n.as_str()) {
                                removed.push(n.to_string());
                            }
                        }
                        _ => {}
                    }
                }
                if !added.is_empty() || !removed.is_empty() {
                    evs.push(Event::at(
                        Some(*time),
                        EventKind::Meta {
                            kind: "tool-registry".into(),
                            data: json!({"added": added, "removed": removed}),
                        },
                    ));
                }
            }
            "goal/change" => {
                if d.get("operation").and_then(|o| o.as_str()) == Some("create") {
                    if let Some(obj) = d.pointer("/goal/objective").and_then(|o| o.as_str()) {
                        evs.push(Event::at(
                            Some(*time),
                            EventKind::Meta {
                                kind: "goal".into(),
                                data: json!({"objective": obj}),
                            },
                        ));
                    }
                }
            }
            _ => {} // telemetry: retries, request headers, sandbox modes, titles, ...
        }
    }

    (evs, title)
}

/// Replacement bounds identify positions in the current surface, rather than
/// numeric sequence ranges. New summaries can precede older retained messages.
pub(super) fn replay_surface(events: &[(i64, Value)]) -> anyhow::Result<Option<Vec<(i64, Value)>>> {
    if !events.iter().any(|(_, v)| v.get("surfaceOp").is_some()) {
        return Ok(None); // older transcripts do not record an authoritative surface
    }
    let mut surface: Vec<&(i64, Value)> = Vec::new();
    let mut tool_calls = std::collections::HashMap::new();
    for event @ (_, record) in events {
        if record["type"] == "tool/call" {
            if let Some(id) = record.pointer("/data/callId").and_then(Value::as_str) {
                tool_calls.insert(id, event);
            }
        }
        match record.get("surfaceOp") {
            Some(Value::String(op)) if op == "append" => surface.push(event),
            Some(op) if op.get("op").and_then(Value::as_str) == Some("replace") => {
                let start = op.get("startSeq").and_then(Value::as_i64);
                let end = op.get("endSeq").and_then(Value::as_i64);
                anyhow::ensure!(
                    start.is_some() && end.is_some(),
                    "DSH surface replacement has no bounds at seq {}",
                    record["seq"]
                );
                let a = surface
                    .iter()
                    .position(|(_, v)| v.get("seq").and_then(Value::as_i64) == start);
                let b = surface
                    .iter()
                    .position(|(_, v)| v.get("seq").and_then(Value::as_i64) == end);
                let (Some(a), Some(b)) = (a, b) else {
                    anyhow::bail!("cannot replay DSH surface replacement at seq {}: missing bounds {start:?}..{end:?}", record["seq"]);
                };
                anyhow::ensure!(
                    a <= b,
                    "reversed DSH surface replacement at seq {}",
                    record["seq"]
                );
                surface.splice(a..=b, [event]);
            }
            Some(op) => anyhow::bail!("unsupported DSH surface operation: {op}"),
            None => {}
        }
    }

    let mut active = Vec::new();
    let mut emitted_calls = std::collections::HashSet::new();
    for (time, record) in surface {
        // Tool calls are telemetry outside the surface. Keep only the calls
        // paired with retained messages/results and preserve their ordering.
        if record["type"] == "tool/result" {
            if let Some(id) = record
                .pointer("/data/message/toolCallId")
                .and_then(Value::as_str)
            {
                if !emitted_calls.contains(id) {
                    let call = tool_calls.get(id).ok_or_else(|| {
                        anyhow::anyhow!("retained DSH tool result has no call: {id}")
                    })?;
                    active.push((*call).clone());
                    emitted_calls.insert(id.to_string());
                }
            }
        }
        active.push((*time, record.clone()));
        if record["type"] == "assistant/message" {
            for block in record
                .pointer("/data/message/content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if block["type"] != "tool-call" {
                    continue;
                }
                if let Some(id) = block.get("id").and_then(Value::as_str) {
                    if emitted_calls.insert(id.to_string()) {
                        active.push((*time, json!({"type":"tool/call", "data":{
                            "callId":id, "name":block.get("name"), "arguments":block.get("arguments")
                        }})));
                    }
                }
            }
        }
    }
    Ok(Some(active))
}

#[cfg(test)]
mod context_tests {
    use super::*;

    fn record(seq: i64, op: Value) -> (i64, Value) {
        (
            seq,
            json!({"seq":seq, "type":"user/message", "surfaceOp":op,
            "data":{"content":[{"type":"text", "text":format!("message {seq}")}]}}),
        )
    }

    #[test]
    fn replacement_bounds_follow_surface_order_instead_of_sequence_order() {
        let events = vec![
            record(1, json!("append")),
            record(2, json!("append")),
            record(3, json!({"op":"replace", "startSeq":1, "endSeq":1})),
            record(4, json!({"op":"replace", "startSeq":3, "endSeq":2})),
        ];
        let surface = replay_surface(&events).unwrap().unwrap();
        assert_eq!(surface.len(), 1);
        assert_eq!(surface[0].1["seq"], 4);
    }

    #[test]
    fn missing_or_reversed_surface_bounds_fail_instead_of_replaying_all_history() {
        for op in [
            json!({"op":"replace", "startSeq":99, "endSeq":2}),
            json!({"op":"replace", "startSeq":2, "endSeq":1}),
            json!({"op":"replace"}),
            json!("unsupported"),
        ] {
            let events = vec![
                record(1, json!("append")),
                record(2, json!("append")),
                record(3, op),
            ];
            assert!(replay_surface(&events).is_err());
        }
    }

    #[test]
    fn retained_result_restores_its_call_when_the_assistant_was_summarized() {
        let events = vec![
            (
                0,
                json!({"seq":0, "type":"tool/call", "data":{"callId":"call", "name":"read", "arguments":"{}"}}),
            ),
            (
                1,
                json!({"seq":1, "type":"tool/result", "surfaceOp":"append", "data":{"message":{"toolCallId":"call", "content":[{"type":"text", "text":"retained result"}]}}}),
            ),
        ];
        let surface = replay_surface(&events).unwrap().unwrap();
        let (decoded, _) = decode_events(&surface);
        assert!(
            matches!(&decoded[0].kind, EventKind::ToolCall { call_id, .. } if call_id == "call")
        );
        assert!(
            matches!(&decoded[1].kind, EventKind::ToolResult { call_id, text } if call_id == "call" && text == "retained result")
        );
    }

    #[test]
    fn legacy_transcripts_without_surface_operations_have_no_separate_context() {
        assert!(
            replay_surface(&[(1, json!({"type":"user/message", "data":{}}))])
                .unwrap()
                .is_none()
        );
    }
}
