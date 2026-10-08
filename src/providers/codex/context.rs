use super::{content_texts, join_text, parse_iso_ms};
use crate::ir::{Event, EventKind, Role};
use serde_json::Value;

fn decode(item: &Value, ts: Option<i64>) -> anyhow::Result<Option<Event>> {
    let kind = match item.get("type").and_then(Value::as_str) {
        Some("message") => EventKind::Message {
            role:match item["role"].as_str() {Some("user")=>Role::User,Some("assistant")=>Role::Assistant,_=>Role::Developer},
            text:join_text(&content_texts(item.get("content"))), source_kind:None,
        },
        Some("reasoning") => EventKind::Reasoning {text:join_text(&content_texts(item.get("summary")))},
        Some("function_call" | "custom_tool_call") => EventKind::ToolCall {
            call_id:item["call_id"].as_str().ok_or_else(|| anyhow::anyhow!("Codex context tool call has no call_id"))?.into(),
            name:item["name"].as_str().unwrap_or("unknown").into(),
            arguments:item.get("arguments").or_else(|| item.get("input")).and_then(Value::as_str).unwrap_or("{}").into(),
        },
        Some("function_call_output" | "custom_tool_call_output") => EventKind::ToolResult {
            call_id:item["call_id"].as_str().ok_or_else(|| anyhow::anyhow!("Codex context tool result has no call_id"))?.into(),
            text:item["output"].as_str().map(str::to_string).unwrap_or_else(|| join_text(&content_texts(item.get("output")))),
        },
        Some("compaction") => anyhow::bail!("encrypted Codex compaction cannot be migrated without a persisted plaintext replacement_history"),
        Some(other) => anyhow::bail!("unsupported Codex resume-context item {other:?}; refusing to discard active context"),
        None => anyhow::bail!("Codex resume-context item has no type"),
    };
    Ok(Some(Event::at(ts, kind)))
}

pub(super) fn resume_events(text: &str) -> anyhow::Result<Option<Vec<Event>>> {
    let records: Vec<Value> = text
        .lines()
        .filter_map(|ln| serde_json::from_str(ln).ok())
        .collect();
    let checkpoint = |record: &Value| {
        record["type"] == "compacted"
            || (record["type"] == "response_item"
                && matches!(
                    record["payload"]["type"].as_str(),
                    Some("compacted" | "compaction")
                ))
    };
    let Some(latest) = records.iter().rposition(checkpoint) else {
        return Ok(None);
    };
    let mut context = None;
    for record in &records[latest..] {
        let ts = record["timestamp"].as_str().and_then(parse_iso_ms);
        let payload = &record["payload"];
        if checkpoint(record) {
            let history = payload.get("replacement_history").and_then(Value::as_array)
                .ok_or_else(|| anyhow::anyhow!("Codex checkpoint has no plaintext replacement_history; refusing to replay the entire archive"))?;
            context = Some(
                history
                    .iter()
                    .map(|item| decode(item, ts))
                    .collect::<anyhow::Result<Vec<_>>>()?
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>(),
            );
        } else if record["type"] == "response_item" {
            if let Some(active) = &mut context {
                if let Some(event) = decode(payload, ts)? {
                    active.push(event)
                }
            }
        }
    }
    Ok(context)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn last_native_checkpoint_replaces_previous_history_and_keeps_later_items() {
        let msg = |text| json!({"type":"message","role":"user","content":[{"type":"input_text","text":text}]});
        let rows = [
            json!({"type":"response_item","payload":msg("obsolete")}),
            json!({"type":"compacted","payload":{"message":"superseded legacy checkpoint"}}),
            json!({"type":"compacted","payload":{"replacement_history":[msg("old summary")]}}),
            json!({"type":"compacted","payload":{"replacement_history":[msg("current summary")]}}),
            json!({"type":"response_item","payload":msg("new request")}),
        ];
        let context = resume_events(
            &rows
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap()
        .unwrap();
        let text = serde_json::to_string(&context).unwrap();
        assert!(text.contains("current summary"));
        assert!(text.contains("new request"));
        assert!(!text.contains("obsolete"));
        assert!(!text.contains("old summary"));
    }

    #[test]
    fn missing_or_opaque_checkpoints_fail_explicitly() {
        for payload in [
            json!({"message":"legacy summary"}),
            json!({"replacement_history":[{"type":"compaction","encrypted_content":"opaque"}]}),
        ] {
            assert!(
                resume_events(&json!({"type":"compacted","payload":payload}).to_string()).is_err()
            );
        }
    }
}
