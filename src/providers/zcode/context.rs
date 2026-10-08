use crate::ir::{Event, EventKind, Role};
use serde_json::Value;
use std::ops::Range;

pub(super) fn resume_events(
    records: &[(Value, Value)],
    ranges: &[Range<usize>],
    events: &[Event],
) -> anyhow::Result<Option<Vec<Event>>> {
    let Some(boundary) = records.iter().rposition(|(_, p)| p["type"] == "compaction") else {
        if records.iter().any(|(m, _)| {
            m.pointer("/semantics/providerVisibility")
                .and_then(Value::as_str)
                == Some("hidden")
        }) {
            return Ok(Some(
                records
                    .iter()
                    .enumerate()
                    .filter(|(_, (m, _))| {
                        m.pointer("/semantics/providerVisibility")
                            .and_then(Value::as_str)
                            != Some("hidden")
                    })
                    .flat_map(|(i, _)| events[ranges[i].clone()].iter().cloned())
                    .collect(),
            ));
        }
        return Ok(None);
    };
    let part = &records[boundary].1;
    let mut context = Vec::new();
    let mut start = boundary + 1;
    if let Some(text) = part["summary"].as_str().filter(|s| !s.trim().is_empty()) {
        context.push(Event::at(
            None,
            EventKind::Message {
                role: Role::User,
                text: text.into(),
                source_kind: Some("compact-summary".into()),
            },
        ));
    } else {
        let summary = records.iter().enumerate().skip(start).find(|(_, (m, p))| m["summary"] == true && p["type"] == "text")
            .map(|(i, _)| i).ok_or_else(|| anyhow::anyhow!("latest ZCode compaction has no persisted summary; retry after source compaction completes"))?;
        context.extend_from_slice(&events[ranges[summary].clone()]);
        start = summary + 1;
    }
    for i in start..records.len() {
        let (message, _) = &records[i];
        if message
            .pointer("/semantics/providerVisibility")
            .and_then(Value::as_str)
            != Some("hidden")
        {
            context.extend_from_slice(&events[ranges[i].clone()]);
        }
    }
    Ok(Some(context))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn native_summary_message_and_inline_summary_both_replace_the_archive() {
        let message = |text: &str| {
            Event::at(
                None,
                EventKind::Message {
                    role: Role::User,
                    text: text.into(),
                    source_kind: None,
                },
            )
        };
        let events = vec![
            message("old"),
            Event::at(
                None,
                EventKind::Compaction {
                    id: None,
                    text: None,
                },
            ),
            message("summary"),
            message("new"),
        ];
        let ranges = vec![0..1, 1..2, 2..3, 3..4];
        let mut records = vec![
            (json!({}), json!({"type":"text"})),
            (json!({}), json!({"type":"compaction"})),
            (json!({"summary":true}), json!({"type":"text"})),
            (json!({}), json!({"type":"text"})),
        ];
        let text =
            serde_json::to_string(&resume_events(&records, &ranges, &events).unwrap()).unwrap();
        assert!(!text.contains("old"));
        assert!(text.contains("summary"));
        assert!(text.contains("new"));
        records[1].1["summary"] = json!("inline checkpoint");
        assert!(
            serde_json::to_string(&resume_events(&records, &ranges, &events).unwrap())
                .unwrap()
                .contains("inline checkpoint")
        );
        records[1].1["summary"] = Value::Null;
        records[2].0["summary"] = json!(false);
        assert!(resume_events(&records, &ranges, &events).is_err());
    }
}
