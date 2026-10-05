//! agy (Google Antigravity CLI) provider — experimental.
//!
//! Sessions live in `~/.gemini/antigravity-cli`. Reading prefers the plain
//! JSONL transcripts the CLI keeps at
//! `brain/<conversation-id>/.system_generated/logs/transcript(_full)?.jsonl`;
//! when those are missing (older conversations), it falls back to a heuristic
//! decode of the protobuf `steps.step_payload` blobs in the per-conversation
//! sqlite db (step_type 14 = user prompt, 132 = tool call/result). Writing is
//! not implemented: agy's native format is protobuf and the summaries index is
//! managed by the CLI itself.

use crate::ir::{Event, EventKind, Role, Session, SessionRef, WriteOpts, WriteOutcome};
use serde_json::{json, Value};

pub struct AgyProvider {
    pub agy_dir: std::path::PathBuf,
}

impl AgyProvider {
    pub fn new(agy_dir: std::path::PathBuf) -> Self {
        AgyProvider { agy_dir }
    }

    fn summaries_db(&self) -> std::path::PathBuf {
        self.agy_dir.join("conversation_summaries.db")
    }
}

fn parse_agy_ts(ts: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(ts).ok().map(|d| d.timestamp_millis())
}

/// Generic protobuf wire-format walker (no schema): returns (field, wire, value).
fn wire_fields(buf: &[u8]) -> Vec<(u64, u64, Vec<u8>)> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < buf.len() {
        let (tag, ni) = match read_varint(buf, i) {
            Some(v) => v,
            None => break,
        };
        i = ni;
        let field = tag >> 3;
        let wire = tag & 7;
        match wire {
            0 => match read_varint(buf, i) {
                Some((_, ni)) => i = ni,
                None => break,
            },
            1 => i += 8,
            5 => i += 4,
            2 => {
                let (len, ni) = match read_varint(buf, i) {
                    Some(v) => v,
                    None => break,
                };
                i = ni;
                let end = (i + len as usize).min(buf.len());
                out.push((field, wire, buf[i..end].to_vec()));
                i = end;
            }
            _ => break,
        }
        if field == 0 {
            break;
        }
    }
    out
}

fn read_varint(buf: &[u8], mut i: usize) -> Option<(u64, usize)> {
    let mut result = 0u64;
    let mut shift = 0;
    loop {
        let b = *buf.get(i)?;
        i += 1;
        result |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Some((result, i));
        }
        shift += 7;
        if shift > 63 {
            return None;
        }
    }
}

fn sub(buf: &[u8], field: u64) -> Vec<Vec<u8>> {
    wire_fields(buf)
        .into_iter()
        .filter(|(f, w, _)| *f == field && *w == 2)
        .map(|(_, _, v)| v)
        .collect()
}

fn str_field(buf: &[u8], field: u64) -> Option<String> {
    sub(buf, field)
        .into_iter()
        .find_map(|v| String::from_utf8(v).ok())
}

/// Decode a `steps.step_payload` blob into IR events.
///
/// Step-type map reverse-engineered by the community (ArcticWinterSturm's gist,
/// cross-checked against the embedded Go protobuf descriptors):
/// 14=user prompt (text at field 19/2), 15=assistant/planner response (text at
/// 20/3), 5/7/8/9/17/21/38/132=tool calls with `{1: call_id, 2: name, 3: args JSON}`
/// under field 5/4, run_command output under 28/21/1, 98=history injection,
/// 23=task/plan, 101/28=status plumbing.
fn decode_step(step_type: i64, payload: &[u8], _ts: Option<i64>) -> Vec<EventKind> {
    let mut out = Vec::new();

    fn tool_records(payload: &[u8]) -> Vec<EventKind> {
        let mut out = Vec::new();
        for five in sub(payload, 5) {
            for four in sub(&five, 4) {
                let call_id = str_field(&four, 1).unwrap_or_default();
                let name = str_field(&four, 2);
                let args_json = str_field(&four, 3).unwrap_or_default();
                match name {
                    Some(name) if !call_id.is_empty() => out.push(EventKind::ToolCall {
                        call_id,
                        name,
                        arguments: args_json,
                    }),
                    _ => {
                        if !args_json.is_empty() {
                            out.push(EventKind::ToolResult { call_id, text: args_json });
                        }
                    }
                }
            }
        }
        out
    }

    match step_type {
        14 => {
            let text = str_field(payload, 19)
                .or_else(|| sub(payload, 19).into_iter().find_map(|n| str_field(&n, 2)))
                .unwrap_or_default();
            if !text.trim().is_empty() {
                out.push(EventKind::Message { role: Role::User, text, source_kind: None });
            }
        }
        15 => {
            // assistant/planner response: text at field 20/3; may also carry tool records
            let text = sub(payload, 20)
                .first()
                .and_then(|n| str_field(n, 3))
                .unwrap_or_default();
            if !text.trim().is_empty() {
                out.push(EventKind::Message { role: Role::Assistant, text, source_kind: None });
            }
            out.extend(tool_records(payload));
        }
        5 | 7 | 8 | 9 | 17 | 21 | 38 | 132 => {
            let calls = tool_records(payload);
            if calls.is_empty() {
                // some builds put the whole tool record directly at field 4/3
                let call_id = str_field(payload, 1).unwrap_or_default();
                let name = str_field(payload, 2);
                let args_json = str_field(payload, 3).unwrap_or_default();
                if let (Some(name), false) = (name, call_id.is_empty()) {
                    out.push(EventKind::ToolCall { call_id, name, arguments: args_json });
                }
            } else {
                out.extend(calls);
            }
            // run_command results land under field 28/21/1
            if step_type == 21 {
                for twenty_eight in sub(payload, 28) {
                    for twenty_one in sub(&twenty_eight, 21) {
                        if let Some(text) = str_field(&twenty_one, 1) {
                            if !text.trim().is_empty() {
                                out.push(EventKind::ToolResult { call_id: String::new(), text });
                            }
                        }
                    }
                }
            }
        }
        98 => {
            if let Some(text) = sub(payload, 111).first().and_then(|n| str_field(n, 1)) {
                if !text.trim().is_empty() {
                    out.push(EventKind::Compaction { id: None, text: Some(text) });
                }
            }
        }
        _ => {} // 23 task/plan, 101 stop hook, 28 command status, ... plumbing
    }
    out
}

impl super::Provider for AgyProvider {
    fn name(&self) -> &'static str {
        "agy"
    }

    fn discover(&self) -> anyhow::Result<Vec<SessionRef>> {
        let db = self.summaries_db();
        if !db.exists() {
            anyhow::bail!("no agy conversation index at {}", db.display());
        }
        let con = rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut stmt = con.prepare(
            "select conversation_id, title, last_modified_time, workspace_uris, step_count
             from conversation_summaries order by last_modified_time desc",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, title, modified, workspaces, steps) = row?;
            let cwd = serde_json::from_str::<Value>(&workspaces)
                .ok()
                .and_then(|v| v.as_array().and_then(|a| a.first()).and_then(|w| w.as_str()).map(str::to_string))
                .map(|w| w.trim_start_matches("file://").to_string())
                // file:///C:/... -> C:/... (drop the leading slash before a drive letter)
                .map(|w| {
                    if w.len() > 2 && w.starts_with('/') && w.as_bytes()[2] == b':' {
                        w[1..].to_string()
                    } else {
                        w
                    }
                });
            let created_ms = parse_agy_ts(&modified); // only modified time is indexed
            out.push(SessionRef {
                provider: "agy".into(),
                id: id.clone(),
                title: Some(title),
                cwd,
                created_ms,
                updated_ms: created_ms,
                locator: format!("steps={steps}"),
                migrated: false,
            });
        }
        Ok(out)
    }

    fn read(&self, r: &SessionRef) -> anyhow::Result<Session> {
        let brain = self
            .agy_dir
            .join("brain")
            .join(&r.id)
            .join(".system_generated")
            .join("logs");
        let mut events: Vec<Event> = Vec::new();
        let mut created_ms = r.created_ms.unwrap_or(0);
        let mut updated_ms = r.updated_ms.unwrap_or(0);
        let mut used_jsonl = false;

        for name in ["transcript_full.jsonl", "transcript.jsonl"] {
            let p = brain.join(name);
            if !p.exists() {
                continue;
            }
            let text = std::fs::read_to_string(&p)?;
            for ln in text.lines() {
                let Ok(o) = serde_json::from_str::<Value>(ln) else { continue };
                let ts = o.get("created_at").and_then(|t| t.as_str()).and_then(parse_agy_ts);
                if let Some(t) = ts {
                    created_ms = created_ms.min(t);
                    updated_ms = updated_ms.max(t);
                }
                let typ = o.get("type").and_then(|t| t.as_str()).unwrap_or("");
                let source = o.get("source").and_then(|t| t.as_str()).unwrap_or("");
                let content = o.get("content").and_then(|t| t.as_str()).map(str::to_string);
                match typ {
                    "USER_INPUT" => {
                        let text = content.unwrap_or_default();
                        let inner = text
                            .strip_prefix("<USER_REQUEST>")
                            .and_then(|t| t.strip_suffix("</USER_REQUEST>"))
                            .map(str::to_string)
                            .unwrap_or(text);
                        if !inner.trim().is_empty() {
                            events.push(Event::at(ts, EventKind::Message {
                                role: Role::User,
                                text: inner,
                                source_kind: Some("agy-user".into()),
                            }));
                        }
                    }
                    "PLANNER_RESPONSE" => {
                        if let Some(calls) = o.get("tool_calls").and_then(|c| c.as_array()) {
                            for (i, c) in calls.iter().enumerate() {
                                events.push(Event::at(ts, EventKind::ToolCall {
                                    call_id: format!("agy-{}-{i}", r.id),
                                    name: c.get("name").and_then(|n| n.as_str()).unwrap_or("unknown").to_string(),
                                    arguments: serde_json::to_string(c.get("args").unwrap_or(&json!({})))?,
                                }));
                            }
                        } else if let Some(text) = content {
                            if !text.trim().is_empty() {
                                events.push(Event::at(ts, EventKind::Message {
                                    role: Role::Assistant,
                                    text,
                                    source_kind: None,
                                }));
                            }
                        }
                    }
                    "GENERIC" if source == "MODEL" => {
                        if let Some(text) = content {
                            if !text.trim().is_empty() {
                                events.push(Event::at(ts, EventKind::Message {
                                    role: Role::Assistant,
                                    text,
                                    source_kind: Some("agy-generic".into()),
                                }));
                            }
                        }
                    }
                    _ => {}
                }
            }
            used_jsonl = !events.is_empty();
            if used_jsonl {
                break;
            }
        }

        if !used_jsonl {
            // fallback: heuristic protobuf decode from the per-conversation sqlite
            let db = self.agy_dir.join("conversations").join(format!("{}.db", r.id));
            if !db.exists() {
                anyhow::bail!("no agy transcript or conversation db found for {}", r.id);
            }
            let con = rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            let mut stmt = con.prepare("select step_type, step_payload from steps order by idx")?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, Option<Vec<u8>>>(1)?))
            })?;
            for row in rows {
                let (step_type, payload) = row?;
                if let Some(payload) = payload {
                    for kind in decode_step(step_type, &payload, None) {
                        events.push(Event::at(None, kind));
                    }
                }
            }
            if events.iter().all(|e| e.time_ms.is_none()) {
                for e in events.iter_mut() {
                    e.time_ms = Some(created_ms);
                }
            }
        }

        Ok(Session {
            source: "agy".into(),
            id: r.id.clone(),
            title: r.title.clone(),
            cwd: r.cwd.clone(),
            agent_preset: None,
            parent_session: None,
            origin: Some("agy".into()),
            created_ms,
            updated_ms: updated_ms.max(created_ms),
            events,
        })
    }

    fn write(&self, _s: &Session, _opts: &WriteOpts) -> anyhow::Result<WriteOutcome> {
        anyhow::bail!(
            "writing agy sessions is not supported: its native transcript is protobuf managed by the CLI. \
             Export agy -> codex/claude/zcode/dsh instead."
        )
    }
}
