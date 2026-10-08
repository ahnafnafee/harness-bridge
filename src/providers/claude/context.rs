//! Keep Claude's retained context separate from its append-only transcript.

use crate::ir::{Event, EventKind};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::ops::Range;

pub(super) fn resume_events(
    records: &[Value],
    ranges: &[Range<usize>],
    events: &[Event],
    child: bool,
) -> anyhow::Result<Option<Vec<Event>>> {
    let Some(boundary) = records.iter().rposition(|o| {
        o.get("type").and_then(Value::as_str) == Some("system")
            && o.get("subtype").and_then(Value::as_str) == Some("compact_boundary")
            && (child || o.get("isSidechain").and_then(Value::as_bool) != Some(true))
    }) else {
        return Ok(None);
    };
    let metadata = &records[boundary]["compactMetadata"];
    let anchor = metadata
        .pointer("/preservedMessages/anchorUuid")
        .or_else(|| metadata.pointer("/preservedSegment/anchorUuid"))
        .and_then(Value::as_str);
    let summary = records
        .iter()
        .enumerate()
        .skip(boundary + 1)
        .find(|(_, o)| {
            o.get("type").and_then(Value::as_str) == Some("user")
                && (child || o.get("isSidechain").and_then(Value::as_bool) != Some(true))
                && (o.get("isCompactSummary").and_then(Value::as_bool) == Some(true)
                    || anchor.is_some_and(|id| o.get("uuid").and_then(Value::as_str) == Some(id)))
        })
        .map(|(i, _)| i)
        .ok_or_else(|| anyhow::anyhow!("latest Claude compact_boundary has no persisted summary; retry after source compaction completes"))?;
    anyhow::ensure!(
        events[ranges[summary].clone()]
            .iter()
            .any(|e| matches!(&e.kind, EventKind::Message { text, .. } if !text.trim().is_empty())),
        "latest Claude compaction summary is empty"
    );

    let by_uuid: HashMap<&str, usize> = records
        .iter()
        .enumerate()
        .filter_map(|(i, o)| Some((o.get("uuid")?.as_str()?, i)))
        .collect();
    let mut indices = vec![summary];
    let preserved = metadata
        .pointer("/preservedMessages/allUuids")
        .or_else(|| metadata.pointer("/preservedMessages/uuids"))
        .and_then(Value::as_array);
    if let Some(uuids) = preserved {
        // allUuids also names ephemeral rendered attachments that need not be
        // persisted. Only stored records have events to carry across harnesses.
        indices.extend(
            uuids
                .iter()
                .filter_map(Value::as_str)
                .filter_map(|id| by_uuid.get(id).copied()),
        );
    } else if let Some(segment) = metadata.get("preservedSegment") {
        let head = segment
            .get("headUuid")
            .and_then(Value::as_str)
            .and_then(|id| by_uuid.get(id).copied());
        let tail = segment
            .get("tailUuid")
            .and_then(Value::as_str)
            .and_then(|id| by_uuid.get(id).copied());
        let (Some(head), Some(tail)) = (head, tail) else {
            anyhow::bail!("Claude preservedSegment endpoints are missing from the transcript");
        };
        anyhow::ensure!(
            head <= tail,
            "Claude preservedSegment endpoints are reversed"
        );
        indices.extend(head..=tail);
    }
    indices.extend(boundary + 1..records.len());

    let mut seen = HashSet::new();
    let mut context = Vec::new();
    for i in indices {
        if (child || records[i].get("isSidechain").and_then(Value::as_bool) != Some(true))
            && seen.insert(i)
        {
            context.extend_from_slice(&events[ranges[i].clone()]);
        }
    }

    // Some older preserved-message lists name a result but omit its assistant
    // block. Retain that result's original call, never an invented substitute.
    let calls: HashMap<&str, &Event> = events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::ToolCall { call_id, .. } => Some((call_id.as_str(), e)),
            _ => None,
        })
        .collect();
    let mut paired = Vec::new();
    let mut seen_calls = HashSet::new();
    for event in context {
        match &event.kind {
            EventKind::ToolCall { call_id, .. } => {
                seen_calls.insert(call_id.clone());
            }
            EventKind::ToolResult { call_id, .. } if !seen_calls.contains(call_id) => {
                let call = calls.get(call_id.as_str()).ok_or_else(|| {
                    anyhow::anyhow!("retained Claude tool result {call_id:?} has no persisted call")
                })?;
                paired.push((*call).clone());
                seen_calls.insert(call_id.clone());
            }
            _ => {}
        }
        paired.push(event);
    }
    Ok(Some(paired))
}
