//! Destination-independent preflight; character budgets are not token limits.

use crate::ir::{Event, EventKind, Role, Session};
use serde_json::{json, Value};

pub const DEFAULT_MAX_CHARS: usize = 750_000;

#[derive(Clone, Copy, Default)]
pub struct Options {
    pub prune: bool,
    pub rebuild: bool,
}

fn size(events: &[Event]) -> anyhow::Result<usize> {
    Ok(serde_json::to_string(events)?.chars().count())
}

pub fn prepare(session: &mut Session, max_chars: usize, options: Options) -> anyhow::Result<Value> {
    anyhow::ensure!(
        max_chars >= 1024,
        "--resume-max-chars must be at least 1024"
    );
    let mut candidate = session.clone();
    let unavailable = candidate.resume_context_unavailable.take();
    if let Some(reason) = &unavailable {
        anyhow::ensure!(options.rebuild,
            "retained resume context is unavailable: {reason}. Use --rebuild-resume-context to reconstruct context from readable checkpoint items or the transcript. Encrypted hidden state cannot be recovered; no destination was written");
        let reconstruction_source = if candidate.resume_events.is_some() {
            "readable-checkpoint"
        } else {
            "archive"
        };
        let notice = Event::at(Some(session.updated_ms), EventKind::Message {
            role: Role::User,
            text: format!("Migration context recovery notice: the source's complete retained model context was unavailable ({reason}). This context was rebuilt from {reconstruction_source}; archive reconstruction includes history superseded by compaction. Encrypted hidden state was not recovered. The full readable transcript is preserved. Verify the current task, decisions and tool results before acting."),
            source_kind: Some("migration-context-recovery".into()),
        });
        let mut context = candidate
            .resume_events
            .take()
            .unwrap_or_else(|| candidate.events.clone());
        context.insert(0, notice);
        candidate.resume_events = Some(context);
    }
    let mut report = prepare_context(&mut candidate, max_chars, options.prune)?;
    if let Some(reason) = unavailable {
        report["reconstruction_source"] = json!(if session.resume_events.is_some() {
            "readable-checkpoint"
        } else {
            "archive"
        });
        report["rebuilt"] = json!(true);
        report["source_context_unavailable"] = json!(reason);
    }
    *session = candidate;
    Ok(report)
}

fn prepare_context(session: &mut Session, max_chars: usize, prune: bool) -> anyhow::Result<Value> {
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
            resume_context_unavailable: None,
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
        assert!(prepare(&mut s, 2000, Options::default()).is_err());
        assert!(s.resume_events.is_none());
        let report = prepare(
            &mut s,
            2000,
            Options {
                prune: true,
                ..Default::default()
            },
        )
        .unwrap();
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
        prepare(
            &mut s,
            2000,
            Options {
                prune: true,
                ..Default::default()
            },
        )
        .unwrap();
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
        assert!(prepare(
            &mut s,
            2000,
            Options {
                prune: true,
                ..Default::default()
            }
        )
        .is_err());
        assert!(s.resume_events.is_none());
    }

    #[test]
    fn unavailable_context_requires_recovery_and_failed_preflight_is_atomic() {
        let mut s = session(vec![Event::at(
            None,
            EventKind::Message {
                role: Role::User,
                text: "protected ".repeat(3000),
                source_kind: None,
            },
        )]);
        s.resume_context_unavailable = Some("encrypted checkpoint".into());
        let before = serde_json::to_value(&s).unwrap();
        assert!(prepare(&mut s, DEFAULT_MAX_CHARS, Options::default())
            .unwrap_err()
            .to_string()
            .contains("--rebuild-resume-context"));
        assert!(prepare(
            &mut s,
            2000,
            Options {
                prune: true,
                rebuild: true
            }
        )
        .is_err());
        assert_eq!(serde_json::to_value(&s).unwrap(), before);
        let report = prepare(
            &mut s,
            DEFAULT_MAX_CHARS,
            Options {
                rebuild: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(report["reconstruction_source"], "archive");
        assert!(s.resume_context_unavailable.is_none());
        assert_eq!(serde_json::to_value(&s.events).unwrap(), before["events"]);
        assert!(serde_json::to_string(&s.resume_events)
            .unwrap()
            .contains("recovery notice"));
    }

    #[test]
    fn explicit_recovery_prefers_checkpoint_and_does_not_replace_known_context() {
        let mut s = session(vec![Event::at(
            None,
            EventKind::Message {
                role: Role::User,
                text: "superseded ".repeat(10000),
                source_kind: None,
            },
        )]);
        s.resume_events = Some(vec![Event::at(
            None,
            EventKind::Message {
                role: Role::User,
                text: "current task".into(),
                source_kind: None,
            },
        )]);
        let before = serde_json::to_value(&s).unwrap();
        let options = Options {
            rebuild: true,
            ..Default::default()
        };
        assert!(prepare(&mut s, 2000, options)
            .unwrap()
            .get("rebuilt")
            .is_none());
        assert_eq!(serde_json::to_value(&s).unwrap(), before);
        s.resume_context_unavailable = Some("encrypted checkpoint".into());
        let report = prepare(&mut s, 2000, options).unwrap();
        assert_eq!(report["reconstruction_source"], "readable-checkpoint");
        assert!(!serde_json::to_string(&s.resume_events)
            .unwrap()
            .contains("superseded superseded"));
        assert_eq!(serde_json::to_value(&s.events).unwrap(), before["events"]);
    }
}
