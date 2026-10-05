//! Claude Code provider.
//!
//! Transcripts: `~/.claude/projects/<slug>/<session-uuid>.jsonl` where the slug
//! maps every char outside [A-Za-z0-9] to '-'. Records chain via
//! parentUuid/uuid; user records carry tool_result blocks, assistant records
//! carry thinking/text/tool_use blocks. No index — the directory is the list.

use crate::ir::{Event, EventKind, Role, Session, SessionRef, WriteOpts, WriteOutcome};
use crate::util::{claude_slug, uuid7};
use serde_json::{json, Value};
use std::path::Path;

pub struct ClaudeProvider {
    pub claude_home: std::path::PathBuf,
}

impl ClaudeProvider {
    pub fn new(claude_home: std::path::PathBuf) -> Self {
        ClaudeProvider { claude_home }
    }

    fn custom_title(&self, slug: &str, id: &str) -> Option<String> {
        let p = self.claude_home.join("projects").join(slug).join(id).join("custom-title.json");
        let txt = std::fs::read_to_string(p).ok()?;
        let v: Value = serde_json::from_str(&txt).ok()?;
        v.get("customTitle").and_then(|t| t.as_str()).map(str::to_string)
    }

    fn parse_ts(ts: &str) -> Option<i64> {
        chrono::DateTime::parse_from_rfc3339(ts).ok().map(|d| d.timestamp_millis())
    }
}

impl super::Provider for ClaudeProvider {
    fn name(&self) -> &'static str {
        "claude"
    }

    fn discover(&self) -> anyhow::Result<Vec<SessionRef>> {
        let mut refs = Vec::new();
        let projects = self.claude_home.join("projects");
        for slug_dir in std::fs::read_dir(&projects)?.filter_map(|e| e.ok()) {
            if !slug_dir.path().is_dir() {
                continue;
            }
            for f in std::fs::read_dir(slug_dir.path())?.filter_map(|e| e.ok()) {
                let p = f.path();
                if p.extension().map(|e| e == "jsonl").unwrap_or(false) {
                    let id = p.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_string();
                    let slug_name = slug_dir.file_name().to_string_lossy().to_string();
                    let title = self.custom_title(&slug_name, &id);
                    let md = f.metadata().ok();
                    refs.push(SessionRef {
                        provider: "claude".into(),
                        id: id.clone(),
                        title,
                        cwd: None,
                        created_ms: None,
                        updated_ms: md
                            .as_ref()
                            .and_then(|m| m.modified().ok())
                            .map(|t| t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)),
                        locator: p.to_string_lossy().to_string(),
                        migrated: false,
                    });
                }
            }
        }
        Ok(refs)
    }

    fn read(&self, r: &SessionRef) -> anyhow::Result<Session> {
        let text = crate::util::read_maybe_zstd(Path::new(&r.locator))?;
        let mut evs: Vec<Event> = Vec::new();
        let mut title = r.title.clone();
        let mut cwd: Option<String> = None;
        let mut created_ms = i64::MAX;
        let mut updated_ms = 0i64;

        for ln in text.lines() {
            let Ok(o) = serde_json::from_str::<Value>(ln) else { continue };
            let typ = o.get("type").and_then(|t| t.as_str()).unwrap_or("").to_string();
            let ts = o.get("timestamp").and_then(|t| t.as_str()).and_then(Self::parse_ts);
            if let Some(t) = ts {
                created_ms = created_ms.min(t);
                updated_ms = updated_ms.max(t);
            }
            match typ.as_str() {
                "ai-title" => {
                    title = o.get("aiTitle").and_then(|t| t.as_str()).map(str::to_string).or(title);
                }
                "user" | "assistant" => {
                    if cwd.is_none() {
                        cwd = o.get("cwd").and_then(|c| c.as_str()).map(str::to_string);
                    }
                    let msg = o.get("message").cloned().unwrap_or(Value::Null);
                    let content = msg.get("content").cloned().unwrap_or(Value::Null);
                    match content {
                        Value::String(s) => {
                            if !s.trim().is_empty() {
                                evs.push(Event::at(ts, EventKind::Message {
                                    role: if typ == "user" { Role::User } else { Role::Assistant },
                                    text: s,
                                    source_kind: None,
                                }));
                            }
                        }
                        Value::Array(blocks) => {
                            for b in blocks {
                                let btype = b.get("type").and_then(|t| t.as_str()).unwrap_or("");
                                match (typ.as_str(), btype) {
                                    (_, "text") => {
                                        let s = b.get("text").and_then(|t| t.as_str()).unwrap_or_default();
                                        if !s.trim().is_empty() {
                                            evs.push(Event::at(ts, EventKind::Message {
                                                role: if typ == "user" { Role::User } else { Role::Assistant },
                                                text: s.to_string(),
                                                source_kind: None,
                                            }));
                                        }
                                    }
                                    ("assistant", "thinking") => {
                                        let s = b.get("thinking").and_then(|t| t.as_str()).unwrap_or_default();
                                        if !s.trim().is_empty() {
                                            evs.push(Event::at(ts, EventKind::Reasoning { text: s.to_string() }));
                                        }
                                    }
                                    ("assistant", "tool_use") => {
                                        evs.push(Event::at(ts, EventKind::ToolCall {
                                            call_id: b.get("id").and_then(|i| i.as_str()).unwrap_or_default().to_string(),
                                            name: b.get("name").and_then(|n| n.as_str()).unwrap_or("unknown").to_string(),
                                            arguments: serde_json::to_string(b.get("input").unwrap_or(&json!({})))?,
                                        }));
                                    }
                                    ("user", "tool_result") => {
                                        let out = match b.get("content") {
                                            Some(Value::String(s)) => s.clone(),
                                            Some(Value::Array(blocks)) => {
                                                let parts: Vec<String> = blocks
                                                    .iter()
                                                    .filter_map(|bb| bb.get("text").and_then(|t| t.as_str()).map(str::to_string))
                                                    .collect();
                                                parts.join("\n\n")
                                            }
                                            _ => String::new(),
                                        };
                                        evs.push(Event::at(ts, EventKind::ToolResult {
                                            call_id: b.get("tool_use_id").and_then(|i| i.as_str()).unwrap_or_default().to_string(),
                                            text: out,
                                        }));
                                    }
                                    _ => {}
                                }
                            }
                        }
                        _ => {}
                    }
                }
                _ => {} // attachments, queue-operations, last-prompt
            }
        }
        if created_ms == i64::MAX {
            created_ms = updated_ms;
        }

        Ok(Session {
            source: "claude".into(),
            id: r.id.clone(),
            title,
            cwd,
            agent_preset: None,
            parent_session: None,
            origin: None,
            created_ms,
            updated_ms,
            events: evs,
        })
    }

    fn write(&self, s: &Session, opts: &WriteOpts) -> anyhow::Result<WriteOutcome> {
        let cwd = opts.cwd.clone().or_else(|| s.cwd.clone()).unwrap_or_default();
        let session_id = uuid7(s.created_ms, &format!("harness-bridge/v1|{}|{}", s.source, s.id));
        let slug = claude_slug(&cwd);
        let path = self
            .claude_home
            .join("projects")
            .join(&slug)
            .join(format!("{session_id}.jsonl"));

        let mut records: Vec<Value> = Vec::new();
        let mut parent_uuid: Option<String> = None;
        let mut offset: i64 = 0;

        let push = |records: &mut Vec<Value>,
                        parent: &mut Option<String>,
                        typ: &str,
                        ts_ms: i64,
                        message: Value,
                        extra: Value| {
            let uuid = uuid7(ts_ms, &format!("claude|{}|{}", s.id, records.len()));
            let mut rec = json!({
                "parentUuid": parent.clone(),
                "isSidechain": false,
                "type": typ,
                "message": message,
                "uuid": uuid,
                "timestamp": crate::util::iso_ms(ts_ms),
                "cwd": cwd,
                "sessionId": session_id,
                "version": "2.1.150",
            });
            if let Value::Object(map) = &mut rec {
                if let Value::Object(extra) = extra {
                    for (k, v) in extra {
                        map.insert(k, v);
                    }
                }
            }
            *parent = Some(uuid);
            records.push(rec);
        };

        let mut tool_calls: std::collections::HashMap<String, (String, i64)> = std::collections::HashMap::new();
        for e in &s.events {
            let ts = s.event_ms(e, offset);
            offset += 1;
            match &e.kind {
                EventKind::Message { role, text, source_kind } => match role {
                    Role::User => {
                        push(&mut records, &mut parent_uuid, "user", ts,
                            json!({"role": "user", "content": text}),
                            json!({"userType": "external", "sourceKind": source_kind}));
                    }
                    Role::Assistant => {
                        push(&mut records, &mut parent_uuid, "assistant", ts,
                            json!({"role": "assistant", "content": [{"type": "text", "text": text}], "model": "imported"}),
                            json!({}));
                    }
                    // Claude Code has no developer channel; context notes stay in the IR
                    // and are dropped here (documented lossy conversion).
                    Role::Developer => {}
                },
                EventKind::Reasoning { text } => {
                    push(&mut records, &mut parent_uuid, "assistant", ts,
                        json!({"role": "assistant", "content": [{"type": "thinking", "thinking": text, "signature": ""}], "model": "imported"}),
                        json!({}));
                }
                EventKind::ToolCall { call_id, name, arguments } => {
                    let input: Value = serde_json::from_str(arguments)
                        .unwrap_or_else(|_| json!({"raw": arguments}));
                    tool_calls.insert(call_id.clone(), (name.clone(), ts));
                    push(&mut records, &mut parent_uuid, "assistant", ts,
                        json!({"role": "assistant", "content": [{"type": "tool_use", "id": call_id, "name": name, "input": input}], "model": "imported"}),
                        json!({}));
                }
                EventKind::ToolResult { call_id, text } => {
                    let (name, _t) = tool_calls.get(call_id).cloned().unwrap_or_else(|| ("imported".into(), ts));
                    let _ = name;
                    push(&mut records, &mut parent_uuid, "user", ts,
                        json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": call_id, "content": text}]}),
                        json!({}));
                }
                EventKind::Compaction { id: _, text } => {
                    if let Some(t) = text {
                        push(&mut records, &mut parent_uuid, "user", ts,
                            json!({"role": "user", "content": format!("[compacted context]\n\n{t}")}),
                            json!({}));
                    }
                }
                EventKind::TurnStart | EventKind::TurnEnd { .. } => {}
                EventKind::Meta { .. } => {}
            }
        }
        if let Some(title) = opts.name.clone().or_else(|| s.title.clone()) {
            records.push(json!({
                "type": "ai-title", "aiTitle": title, "sessionId": session_id,
            }));
        }

        if !opts.dry_run {
            std::fs::create_dir_all(path.parent().unwrap())?;
            let body: Vec<String> = records.iter().map(|r| r.to_string()).collect();
            std::fs::write(&path, body.join("\n") + "\n")?;
        }
        Ok(WriteOutcome {
            provider: "claude".into(),
            location: path.to_string_lossy().to_string(),
            native_id: session_id,
            extra: json!({"records": records.len(), "slug": slug, "dry_run": opts.dry_run}),
        })
    }
}
