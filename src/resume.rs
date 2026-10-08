//! Destination-independent preflight; character budgets are not token limits.

use crate::ir::{Event, EventKind, Role, Session};
use serde_json::{json, Value};

pub const DEFAULT_MAX_CHARS: usize = 750_000;

fn size(events: &[Event]) -> anyhow::Result<usize> {
    Ok(serde_json::to_string(events)?.chars().count())
}

pub fn prepare(session: &mut Session, max_chars: usize, prune: bool) -> anyhow::Result<Value> {
    anyhow::ensure!(
        max_chars >= 1024,
        "--resume-max-chars must be at least 1024"
    );
    let source = session.resume_events.as_ref().unwrap_or(&session.events);
    let before = size(source)?;
    if before <= max_chars {
        return Ok(json!({"input_chars":before, "max_chars":max_chars, "pruned":false}));
    }
    anyhow::ensure!(prune,
        "retained resume context is {before} characters, above the {max_chars} character safety budget; compact the source, use --prune-resume-context to shorten reasoning/tool outputs while retaining the archive, or explicitly raise --resume-max-chars for a suitable destination model");
    let mut context: Vec<Event> = source
        .iter()
        .filter(|e| !matches!(e.kind, EventKind::Reasoning { .. }))
        .cloned()
        .collect();
    let reasoning_removed = source.len() - context.len();
    context.insert(0, Event::at(Some(session.updated_ms), EventKind::Message {
        role:Role::User,
        text:"Migration context notice: imported reasoning and tool outputs may be shortened to fit the requested character budget. User requests, assistant text, tool arguments and call IDs are retained. Consult the complete preserved migration transcript for omitted output details before acting.".into(),
        source_kind:Some("migration-context-notice".into()),
    }));
    let mut shortened = 0;
    for cap in [16_384, 8_192, 4_096, 2_048, 1_024, 512, 256, 128] {
        if size(&context)? <= max_chars {
            break;
        }
        shortened = 0;
        for (original, event) in source
            .iter()
            .filter(|e| !matches!(e.kind, EventKind::Reasoning { .. }))
            .zip(context.iter_mut().skip(1))
        {
            if let EventKind::ToolResult { text, .. } = &original.kind {
                if text.chars().count() > cap {
                    let head: String = text.chars().take(cap / 2).collect();
                    let tail: String = text
                        .chars()
                        .rev()
                        .take(cap / 2)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect();
                    if let EventKind::ToolResult { text: output, .. } = &mut event.kind {
                        *output = format!("{head}\n[output shortened for migration; complete output retained in transcript]\n{tail}");
                        shortened += 1;
                    }
                }
            }
        }
    }
    let after = size(&context)?;
    anyhow::ensure!(after <= max_chars,
        "resume context still needs {after} characters after shortening outputs; protected messages/tool arguments exceed the {max_chars} budget. Compact the source or explicitly raise --resume-max-chars; no destination was written");
    session.resume_events = Some(context);
    Ok(
        json!({"input_chars":before, "retained_chars":after, "max_chars":max_chars,
        "pruned":true, "reasoning_events_removed":reasoning_removed, "tool_outputs_shortened":shortened}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(events: Vec<Event>) -> Session {
        Session {
            source: "test".into(),
            id: "test".into(),
            title: None,
            cwd: None,
            agent_preset: None,
            parent_session: None,
            origin: None,
            created_ms: 0,
            updated_ms: 0,
            events,
            resume_events: None,
        }
    }

    #[test]
    fn overflow_fails_before_writing_unless_pruning_is_explicit() {
        let mut s = session(vec![Event::at(
            None,
            EventKind::ToolResult {
                call_id: "c".into(),
                text: "large output ".repeat(10000),
            },
        )]);
        assert!(prepare(&mut s, 2000, false).is_err());
        assert!(s.resume_events.is_none());
        let report = prepare(&mut s, 2000, true).unwrap();
        assert_eq!(report["pruned"], true);
        assert!(size(s.resume_events.as_ref().unwrap()).unwrap() <= 2000);
        assert!(serde_json::to_string(&s.events)
            .unwrap()
            .contains("large output large output"));
    }

    #[test]
    fn pruning_preserves_messages_arguments_ids_and_unicode() {
        let mut s = session(vec![
            Event::at(
                None,
                EventKind::Message {
                    role: Role::User,
                    text: "protected request".into(),
                    source_kind: None,
                },
            ),
            Event::at(
                None,
                EventKind::ToolCall {
                    call_id: "c".into(),
                    name: "read".into(),
                    arguments: "{\"path\":\"keep\"}".into(),
                },
            ),
            Event::at(
                None,
                EventKind::ToolResult {
                    call_id: "c".into(),
                    text: "股票结果".repeat(10000),
                },
            ),
        ]);
        prepare(&mut s, 2000, true).unwrap();
        let context = serde_json::to_string(s.resume_events.as_ref().unwrap()).unwrap();
        assert!(context.contains("protected request"));
        assert!(context.contains("keep"));
        assert!(context.contains("股票"));
        assert!(context.contains("shortened for migration"));
    }

    #[test]
    fn oversized_protected_text_is_never_silently_discarded() {
        let mut s = session(vec![Event::at(
            None,
            EventKind::Message {
                role: Role::User,
                text: "critical ".repeat(10000),
                source_kind: None,
            },
        )]);
        assert!(prepare(&mut s, 2000, true).is_err());
        assert!(s.resume_events.is_none());
    }
}
