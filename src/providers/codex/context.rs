use super::{content_texts, join_text, parse_iso_ms};
use crate::ir::{Event, EventKind, Role};
use serde_json::Value;

#[derive(Debug)]
pub(super) struct UnavailableContext {
    pub reason: &'static str,
    pub readable_events: Option<Vec<Event>>,
}

impl UnavailableContext {
    fn new(reason: &'static str) -> Self {
        Self {
            reason,
            readable_events: None,
        }
    }
}

impl std::fmt::Display for UnavailableContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.reason)
    }
}

impl std::error::Error for UnavailableContext {}

/// Model-input agent mail is distinct from the similarly named UI event.
/// Replay it as labeled external input: destination harnesses cannot verify or
/// deliver the original native agent envelope, but can retain its readable text.
pub(super) fn agent_message(
    item: &Value,
    ts: Option<i64>,
) -> anyhow::Result<(Option<Event>, bool)> {
    let author = item["author"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Codex agent message has no author"))?;
    let recipient = item["recipient"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Codex agent message has no recipient"))?;
    let content = item["content"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Codex agent message has no content array"))?;
    let mut parts = Vec::new();
    let mut encrypted = false;
    for part in content {
        match part["type"].as_str() {
            Some("input_text") => parts.push(
                part["text"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Codex agent message text is not a string"))?
                    .to_string(),
            ),
            Some("encrypted_content") => {
                anyhow::ensure!(
                    part["encrypted_content"].is_string(),
                    "Codex agent message encrypted content is not a string"
                );
                encrypted = true;
                parts.push("[Encrypted agent-message payload unavailable]".into());
            }
            Some(other) => anyhow::bail!("unsupported Codex agent-message content {other:?}"),
            None => anyhow::bail!("Codex agent-message content has no type"),
        }
    }
    let text = parts.join("\n");
    let event = (!text.trim().is_empty()).then(|| {
        Event::at(
            ts,
            EventKind::Message {
                role: Role::User,
                text: format!("[Imported agent message from {author} to {recipient}]\n{text}"),
                source_kind: Some("codex-agent-message".into()),
            },
        )
    });
    Ok((event, encrypted))
}

fn decode(item: &Value, ts: Option<i64>) -> anyhow::Result<Option<Event>> {
    let kind = match item.get("type").and_then(Value::as_str) {
        Some("agent_message") => {
            let (event, encrypted) = agent_message(item, ts)?;
            if encrypted {
                let mut issue = UnavailableContext::new(
                    "encrypted Codex agent message has no portable plaintext payload",
                );
                issue.readable_events = event.map(|event| vec![event]);
                return Err(issue.into());
            }
            return Ok(event);
        }
        Some("message") => EventKind::Message {
            role: match item["role"].as_str() {
                Some("user") => Role::User,
                Some("assistant") => Role::Assistant,
                _ => Role::Developer,
            },
            text: join_text(&content_texts(item.get("content"))),
            source_kind: None,
        },
        Some("reasoning") => EventKind::Reasoning {
            text: join_text(&content_texts(item.get("summary"))),
        },
        Some("function_call" | "custom_tool_call") => EventKind::ToolCall {
            call_id: item["call_id"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Codex context tool call has no call_id"))?
                .into(),
            name: item["name"].as_str().unwrap_or("unknown").into(),
            arguments: item
                .get("arguments")
                .or_else(|| item.get("input"))
                .and_then(Value::as_str)
                .unwrap_or("{}")
                .into(),
        },
        Some("function_call_output" | "custom_tool_call_output") => EventKind::ToolResult {
            call_id: item["call_id"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Codex context tool result has no call_id"))?
                .into(),
            text: item["output"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| join_text(&content_texts(item.get("output")))),
        },
        Some("compaction") => {
            return Err(UnavailableContext::new(
                "encrypted Codex compaction has no portable plaintext replacement_history",
            )
            .into())
        }
        Some(other) => anyhow::bail!(
            "unsupported Codex resume-context item {other:?}; refusing to discard active context"
        ),
        None => anyhow::bail!("Codex resume-context item has no type"),
    };
    Ok(Some(Event::at(ts, kind)))
}

fn append_context(
    item: &Value,
    ts: Option<i64>,
    context: &mut Vec<Event>,
    unavailable: &mut Option<UnavailableContext>,
) -> anyhow::Result<()> {
    match decode(item, ts) {
        Ok(Some(event)) => context.push(event),
        Ok(None) => {}
        Err(error) if error.is::<UnavailableContext>() => {
            let mut issue = error.downcast::<UnavailableContext>()?;
            context.extend(issue.readable_events.take().unwrap_or_default());
            if unavailable
                .as_ref()
                .is_some_and(|previous| previous.reason != issue.reason)
            {
                issue.reason = "Codex retained context includes unavailable encrypted compaction or agent-message payloads";
            }
            *unavailable = Some(issue);
        }
        Err(error) => return Err(error),
    }
    Ok(())
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
    let latest = records.iter().rposition(checkpoint);
    let mut context = Vec::new();
    let mut unavailable = None;
    // Uncompacted agent mail can also contain encrypted active input. Inspect
    // it before allowing an archive-only import to bypass the recovery guard.
    let mut readable_checkpoint = false;
    for record in &records[latest.unwrap_or(0)..] {
        let ts = record["timestamp"].as_str().and_then(parse_iso_ms);
        let payload = &record["payload"];
        if checkpoint(record) {
            if let Some(history) = payload.get("replacement_history").and_then(Value::as_array) {
                readable_checkpoint = true;
                for item in history {
                    append_context(item, ts, &mut context, &mut unavailable)?;
                }
            } else {
                unavailable = Some(UnavailableContext::new(
                    "Codex checkpoint has no plaintext replacement_history",
                ));
            }
        } else if record["type"] == "response_item" {
            // An opaque checkpoint must not hide later unsupported/malformed
            // items by short-circuiting decoding before the tail is inspected.
            // Without a checkpoint, preserve the existing archive reader's
            // support for unrelated native records; only agent mail needs this
            // additional completeness check.
            if latest.is_some() || payload["type"] == "agent_message" {
                append_context(payload, ts, &mut context, &mut unavailable)?;
            }
        }
    }
    if let Some(mut issue) = unavailable {
        if readable_checkpoint && !context.is_empty() {
            issue.readable_events = Some(context);
        }
        return Err(issue.into());
    }
    Ok(latest.map(|_| context))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn mail(content: Value) -> Value {
        json!({"type":"agent_message","author":"/root/worker","recipient":"/root","content":content})
    }

    fn lines(rows: &[Value]) -> String {
        rows.iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn plaintext_agent_mail_preserves_routes_and_text_at_checkpoint_and_tail() {
        let text = lines(&[
            json!({"type":"compacted","payload":{"replacement_history":[mail(json!([
                {"type":"input_text","text":"checkpoint report"},
                {"type":"input_text","text":"second paragraph"}
            ]))]}}),
            json!({"type":"response_item","payload":mail(json!([{"type":"input_text","text":"later report"}]))}),
            // UI assistant notifications are not model-input agent mail.
            json!({"type":"event_msg","payload":{"type":"agent_message","message":"mirrored UI output"}}),
        ]);
        let events = resume_events(&text).unwrap().unwrap();
        assert_eq!(events.len(), 2);
        for event in &events {
            assert!(matches!(&event.kind, EventKind::Message {
                role: Role::User, text, source_kind
            } if text.contains("from /root/worker to /root")
                && source_kind.as_deref() == Some("codex-agent-message")));
        }
        let encoded = serde_json::to_string(&events).unwrap();
        assert!(encoded.contains("checkpoint report\\nsecond paragraph"));
        assert!(encoded.contains("later report"));
        assert!(!encoded.contains("mirrored UI output"));
    }

    #[test]
    fn encrypted_agent_mail_retains_readable_checkpoint_and_tail_for_explicit_recovery() {
        let encrypted = mail(json!([
            {"type":"input_text","text":"readable routing header"},
            {"type":"encrypted_content","encrypted_content":"opaque-secret"}
        ]));
        for at_checkpoint in [true, false] {
            let checkpoint = if at_checkpoint {
                encrypted.clone()
            } else {
                mail(json!([{"type":"input_text","text":"readable checkpoint"}]))
            };
            let text = lines(&[
                json!({"type":"compacted","payload":{"replacement_history":[checkpoint]}}),
                json!({"type":"response_item","payload":encrypted.clone()}),
                json!({"type":"response_item","payload":mail(json!([{"type":"input_text","text":"later report"}]))}),
            ]);
            let issue = resume_events(&text)
                .unwrap_err()
                .downcast::<UnavailableContext>()
                .unwrap();
            assert!(issue.reason.contains("encrypted Codex agent message"));
            let readable = serde_json::to_string(&issue.readable_events.unwrap()).unwrap();
            assert!(readable.contains("readable routing header"));
            assert!(readable.contains("Encrypted agent-message payload unavailable"));
            assert!(readable.contains("later report"));
            assert!(!readable.contains("opaque-secret"));
        }
    }

    #[test]
    fn uncompacted_encrypted_mail_requires_archive_recovery() {
        let text = json!({"type":"response_item","payload":mail(json!([
            {"type":"encrypted_content","encrypted_content":"opaque-secret"}
        ]))})
        .to_string();
        let issue = resume_events(&text)
            .unwrap_err()
            .downcast::<UnavailableContext>()
            .unwrap();
        assert!(issue.readable_events.is_none());
        let plain = json!({"type":"response_item","payload":mail(json!([
            {"type":"input_text","text":"plain report"}
        ]))})
        .to_string();
        assert!(resume_events(&plain).unwrap().is_none());
    }

    #[test]
    fn superseded_encrypted_mail_does_not_taint_a_readable_checkpoint() {
        let text = lines(&[
            json!({"type":"response_item","payload":mail(json!([{"type":"encrypted_content","encrypted_content":"old secret"}]))}),
            json!({"type":"compacted","payload":{"replacement_history":[mail(json!([{"type":"input_text","text":"current report"}]))]}}),
        ]);
        assert_eq!(resume_events(&text).unwrap().unwrap().len(), 1);
    }

    #[test]
    fn malformed_agent_mail_cannot_be_hidden_by_encryption() {
        for item in [
            json!({"type":"agent_message","recipient":"/root","content":[]}),
            json!({"type":"agent_message","author":"/root/worker","content":[]}),
            mail(json!("invalid content")),
            mail(json!([{"type":"input_text","text":17}])),
            mail(json!([{"type":"encrypted_content","encrypted_content":null}])),
            mail(
                json!([{"type":"encrypted_content","encrypted_content":"opaque"},{"type":"unsupported_part"}]),
            ),
        ] {
            for at_checkpoint in [true, false] {
                let mut rows = vec![
                    json!({"type":"compacted","payload":{"replacement_history":[{"type":"compaction","encrypted_content":"opaque"}]}}),
                ];
                if at_checkpoint {
                    rows[0]["payload"]["replacement_history"]
                        .as_array_mut()
                        .unwrap()
                        .push(item.clone());
                } else {
                    rows.push(json!({"type":"response_item","payload":item.clone()}));
                }
                let error = resume_events(&lines(&rows)).unwrap_err();
                assert!(!error.is::<UnavailableContext>());
            }
        }
    }

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

    #[test]
    fn opaque_checkpoint_does_not_mask_unsupported_or_malformed_items() {
        for item in [
            json!({"type":"hosted_tool"}),
            json!({"type":"function_call_output","output":"missing id"}),
        ] {
            let text = [
                json!({"type":"compacted","payload":{"replacement_history":[{"type":"compaction","encrypted_content":"opaque"}, item.clone()]}}),
                json!({"type":"response_item","payload":item}),
            ].iter().map(Value::to_string).collect::<Vec<_>>().join("\n");
            let error = resume_events(&text).unwrap_err();
            assert!(!error.is::<UnavailableContext>());
        }
    }
}
