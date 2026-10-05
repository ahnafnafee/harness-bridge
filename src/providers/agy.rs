//! agy (Google Antigravity CLI) provider.
//!
//! Sessions live in `~/.gemini/antigravity-cli`. Reading prefers the plain
//! JSONL transcripts the CLI keeps at
//! `brain/<conversation-id>/.system_generated/logs/transcript(_full)?.jsonl`;
//! when those are missing (older conversations), it falls back to decoding the
//! protobuf `steps.step_payload` blobs in the per-conversation sqlite db
//! (step_type map: 14=user, 15=assistant, 5/7/8/9/17/21/38/132=tool calls).
//!
//! Writing replicates the local store natively: a small existing conversation
//! is used as a wire-format template, each IR message becomes a cloned step
//! payload with the text swapped at the protobuf level, and the steps table,
//! `trajectory_meta`, the summaries index and the brain transcripts are all
//! (re)built. Tool-call steps are not emitted yet (text turns only).

use crate::ir::{Event, EventKind, Role, Session, SessionRef, WriteOpts, WriteOutcome};
use crate::util::uuid7;
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
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|d| d.timestamp_millis())
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

fn looks_like_uuid(s: &str) -> bool {
    s.len() == 36 && s.chars().filter(|c| *c == '-').count() == 4
}

/// The per-step uuid lives as a 36-char utf8 leaf inside the field-5 envelope.
fn step_uuid(payload: &[u8]) -> Option<String> {
    for five in sub(payload, 5) {
        if let Some(s) = str_field(&five, 12) {
            if looks_like_uuid(&s) {
                return Some(s);
            }
        }
        for (_f, w, v) in wire_fields(&five) {
            if w == 2 {
                if let Ok(s) = String::from_utf8(v) {
                    if looks_like_uuid(&s) {
                        return Some(s);
                    }
                }
            }
        }
    }
    None
}

fn enc_varint(mut v: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return out;
        }
        out.push(b | 0x80);
    }
}

/// Re-encode a protobuf buffer, replacing utf8 string leaves via `repl`.
/// Non-utf8 length-delimited fields are re-parsed as nested protobuf when
/// possible (round-trips identically when nothing is replaced).
fn rewire(buf: &[u8], repl: &mut dyn FnMut(&str) -> Option<String>) -> anyhow::Result<Vec<u8>> {
    let mut out: Vec<u8> = Vec::with_capacity(buf.len() + 64);
    let mut i = 0usize;
    while i < buf.len() {
        let tag_start = i;
        let (tag, ni) = read_varint(buf, i).ok_or_else(|| anyhow::anyhow!("bad tag"))?;
        i = ni;
        let field = tag >> 3;
        let wire = tag & 7;
        if field == 0 {
            anyhow::bail!("invalid field 0");
        }
        match wire {
            0 => {
                let (_, ni) = read_varint(buf, i).ok_or_else(|| anyhow::anyhow!("bad varint"))?;
                out.extend_from_slice(&buf[tag_start..ni]);
                i = ni;
            }
            1 => {
                let end = i + 8;
                let end = end.min(buf.len());
                out.extend_from_slice(&buf[tag_start..end]);
                i = end;
            }
            5 => {
                let end = i + 4;
                let end = end.min(buf.len());
                out.extend_from_slice(&buf[tag_start..end]);
                i = end;
            }
            2 => {
                let (len, ni) = read_varint(buf, i).ok_or_else(|| anyhow::anyhow!("bad len"))?;
                i = ni;
                let end = (i + len as usize).min(buf.len());
                let val = &buf[i..end];
                i = end;
                let as_str = std::str::from_utf8(val).ok();
                let printable = as_str
                    .map(|s| s.chars().filter(|c| !c.is_control()).count())
                    .map(|n| n as f64 / val.len().max(1) as f64)
                    .unwrap_or(-1.0);
                if printable > 0.9 {
                    if let Some(s) = as_str {
                        if let Some(new) = repl(s) {
                            let nb = new.as_bytes();
                            out.extend_from_slice(&enc_varint(tag));
                            out.extend_from_slice(&enc_varint(nb.len() as u64));
                            out.extend_from_slice(nb);
                            continue;
                        }
                    }
                    out.extend_from_slice(&buf[tag_start..i]);
                } else if let Ok(nested) = rewire(val, repl) {
                    out.extend_from_slice(&enc_varint(tag));
                    out.extend_from_slice(&enc_varint(nested.len() as u64));
                    out.extend_from_slice(&nested);
                } else {
                    out.extend_from_slice(&buf[tag_start..i]);
                }
            }
            _ => anyhow::bail!("unsupported wire type {wire}"),
        }
    }
    Ok(out)
}

fn contains_utf8_leaf(buf: &[u8], needle: &str) -> bool {
    // cheap byte-level check is sufficient: protobuf string leaves are plain utf8
    buf.windows(needle.len()).any(|w| w == needle.as_bytes())
}

/// A small real agy conversation used as the structural template for writes.
struct AgyTemplate {
    cascade_id: String,
    trajectory_id: String,
    user_payload: Vec<u8>,
    user_text: String,
    user_step_uuid: Option<String>,
    asst_payload: Vec<u8>,
    asst_text: String,
    asst_step_uuid: Option<String>,
    summaries_row: Option<Vec<(String, rusqlite::types::Value)>>,
    summaries_columns: Vec<String>,
}

impl AgyProvider {
    fn find_template(&self) -> anyhow::Result<AgyTemplate> {
        let db = self.summaries_db();
        if !db.exists() {
            anyhow::bail!(
                "no agy conversation index at {} — cannot pick a write template",
                db.display()
            );
        }
        let con =
            rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        con.busy_timeout(std::time::Duration::from_secs(10))?;
        let mut stmt = con.prepare(
            "select conversation_id from conversation_summaries
             where step_count between 2 and 8 order by last_modified_time desc limit 40",
        )?;
        let ids: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .collect();
        drop(stmt);
        con.close().ok();

        let mut last_err = String::new();
        for id in ids {
            match self.try_template(&id) {
                Ok(t) => return Ok(t),
                Err(e) => last_err = format!("{id}: {e}"),
            }
        }
        anyhow::bail!(
            "no suitable agy template conversation found (need a small conversation with a brain transcript \
             containing both a user and an assistant message); last candidate failed: {last_err}"
        )
    }

    fn try_template(&self, id: &str) -> anyhow::Result<AgyTemplate> {
        let db_path = self.agy_dir.join("conversations").join(format!("{id}.db"));
        if !db_path.exists() {
            anyhow::bail!("no conversation db");
        }
        // brain transcript supplies the exact template texts
        let logs = self
            .agy_dir
            .join("brain")
            .join(id)
            .join(".system_generated")
            .join("logs");
        let mut user_text = None;
        let mut asst_text = None;
        for name in ["transcript_full.jsonl", "transcript.jsonl"] {
            let p = logs.join(name);
            if !p.exists() {
                continue;
            }
            for ln in std::fs::read_to_string(&p)?.lines() {
                let Ok(o) = serde_json::from_str::<Value>(ln) else {
                    continue;
                };
                let typ = o.get("type").and_then(|t| t.as_str()).unwrap_or("");
                let source = o.get("source").and_then(|t| t.as_str()).unwrap_or("");
                match (typ, source) {
                    ("USER_INPUT", _) if user_text.is_none() => {
                        let raw = o
                            .get("content")
                            .and_then(|c| c.as_str())
                            .unwrap_or_default();
                        // brain transcripts wrap the raw prompt in <USER_REQUEST>…</USER_REQUEST>
                        let inner = raw
                            .split_once("<USER_REQUEST>")
                            .and_then(|(_, rest)| rest.split_once("</USER_REQUEST>"))
                            .map(|(inner, _)| inner)
                            .unwrap_or(raw);
                        user_text = Some(inner.trim_matches('\n').to_string());
                    }
                    ("PLANNER_RESPONSE", "MODEL") | ("GENERIC", "MODEL") if asst_text.is_none() => {
                        asst_text = o
                            .get("content")
                            .and_then(|c| c.as_str())
                            .map(str::to_string)
                    }
                    _ => {}
                }
                if user_text.is_some() && asst_text.is_some() {
                    break;
                }
            }
            if user_text.is_some() && asst_text.is_some() {
                break;
            }
        }
        let (Some(user_text), Some(asst_text)) = (user_text, asst_text) else {
            anyhow::bail!("brain transcript lacks a user+assistant pair");
        };
        if user_text.trim().is_empty() || asst_text.trim().is_empty() {
            anyhow::bail!("empty template texts");
        }

        let con = rusqlite::Connection::open_with_flags(
            &db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let mut user_payload = None;
        let mut asst_payload = None;
        let rows: Vec<(i64, Vec<u8>)> = {
            let mut stmt = con.prepare("select step_type, step_payload from steps order by idx")?;
            let rows = stmt.query_map([], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Option<Vec<u8>>>(1)?))
            })?;
            rows.filter_map(|r| r.ok())
                .filter_map(|(t, p)| p.map(|p| (t, p)))
                .collect()
        };
        for (t, p) in &rows {
            if *t == 14 && user_payload.is_none() && contains_utf8_leaf(p, &user_text) {
                user_payload = Some(p.clone());
            }
            if *t == 15 && asst_payload.is_none() && contains_utf8_leaf(p, &asst_text) {
                // reject cross-contamination: assistant payload must not contain the user text
                if !contains_utf8_leaf(p, &user_text) {
                    asst_payload = Some(p.clone());
                }
            }
        }
        con.close().ok();
        let (Some(user_payload), Some(asst_payload)) = (user_payload, asst_payload) else {
            anyhow::bail!("payloads do not contain the transcript texts");
        };

        // summaries row copy
        let sdb = self.summaries_db();
        let scon = rusqlite::Connection::open_with_flags(
            &sdb,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let columns: Vec<String> = {
            let mut stmt = scon.prepare("pragma table_info(conversation_summaries)")?;
            let cols = stmt
                .query_map([], |r| r.get::<_, String>(1))?
                .filter_map(|r| r.ok())
                .collect();
            cols
        };
        let summaries_row: Option<Vec<(String, rusqlite::types::Value)>> = (|| {
            let _ = &scon;
            let mut stmt = scon
                .prepare(&format!(
                    "select {} from conversation_summaries where conversation_id = ?1",
                    columns.join(",")
                ))
                .ok()?;
            let mut rows = stmt.query(rusqlite::params![id]).ok()?;
            let row = rows.next().ok()??;
            let mut vals = Vec::new();
            for i in 0..columns.len() {
                let v = rusqlite::types::Value::from(row.get_ref(i).ok()?);
                vals.push((columns[i].clone(), v));
            }
            Some(vals)
        })();

        let cascade_id = id.to_string();
        let trajectory_id = {
            let tcon = rusqlite::Connection::open_with_flags(
                &db_path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )?;
            let tid = tcon
                .query_row(
                    "select trajectory_id from trajectory_meta limit 1",
                    [],
                    |r| r.get::<_, String>(0),
                )
                .unwrap_or_default();
            tcon.close().ok();
            tid
        };
        Ok(AgyTemplate {
            cascade_id,
            trajectory_id,
            user_step_uuid: step_uuid(&user_payload),
            asst_step_uuid: step_uuid(&asst_payload),
            user_payload,
            user_text,
            asst_payload,
            asst_text,
            summaries_row,
            summaries_columns: columns,
        })
    }

    fn brain_ts(ms: i64) -> String {
        chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
            .unwrap_or_else(chrono::Utc::now)
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    }
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
                            out.push(EventKind::ToolResult {
                                call_id,
                                text: args_json,
                            });
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
                out.push(EventKind::Message {
                    role: Role::User,
                    text,
                    source_kind: None,
                });
            }
        }
        15 => {
            // assistant/planner response: text at field 20/3; may also carry tool records
            let text = sub(payload, 20)
                .first()
                .and_then(|n| str_field(n, 3))
                .unwrap_or_default();
            if !text.trim().is_empty() {
                out.push(EventKind::Message {
                    role: Role::Assistant,
                    text,
                    source_kind: None,
                });
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
                    out.push(EventKind::ToolCall {
                        call_id,
                        name,
                        arguments: args_json,
                    });
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
                                out.push(EventKind::ToolResult {
                                    call_id: String::new(),
                                    text,
                                });
                            }
                        }
                    }
                }
            }
        }
        98 => {
            if let Some(text) = sub(payload, 111).first().and_then(|n| str_field(n, 1)) {
                if !text.trim().is_empty() {
                    out.push(EventKind::Compaction {
                        id: None,
                        text: Some(text),
                    });
                }
            }
        }
        _ => {} // 23 task/plan, 101 stop hook, 28 command status, ... plumbing
    }
    out
}

impl super::Provider for AgyProvider {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn name(&self) -> &'static str {
        "agy"
    }

    fn discover(&self) -> anyhow::Result<Vec<SessionRef>> {
        let db = self.summaries_db();
        if !db.exists() {
            anyhow::bail!("no agy conversation index at {}", db.display());
        }
        let con =
            rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
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
                .and_then(|v| {
                    v.as_array()
                        .and_then(|a| a.first())
                        .and_then(|w| w.as_str())
                        .map(str::to_string)
                })
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
                let Ok(o) = serde_json::from_str::<Value>(ln) else {
                    continue;
                };
                let ts = o
                    .get("created_at")
                    .and_then(|t| t.as_str())
                    .and_then(parse_agy_ts);
                if let Some(t) = ts {
                    created_ms = created_ms.min(t);
                    updated_ms = updated_ms.max(t);
                }
                let typ = o.get("type").and_then(|t| t.as_str()).unwrap_or("");
                let source = o.get("source").and_then(|t| t.as_str()).unwrap_or("");
                let content = o
                    .get("content")
                    .and_then(|t| t.as_str())
                    .map(str::to_string);
                match typ {
                    "USER_INPUT" => {
                        let text = content.unwrap_or_default();
                        let inner = text
                            .strip_prefix("<USER_REQUEST>")
                            .and_then(|t| t.strip_suffix("</USER_REQUEST>"))
                            .map(str::to_string)
                            .unwrap_or(text);
                        if !inner.trim().is_empty() {
                            events.push(Event::at(
                                ts,
                                EventKind::Message {
                                    role: Role::User,
                                    text: inner,
                                    source_kind: Some("agy-user".into()),
                                },
                            ));
                        }
                    }
                    "PLANNER_RESPONSE" => {
                        if let Some(calls) = o.get("tool_calls").and_then(|c| c.as_array()) {
                            for (i, c) in calls.iter().enumerate() {
                                events.push(Event::at(
                                    ts,
                                    EventKind::ToolCall {
                                        call_id: format!("agy-{}-{i}", r.id),
                                        name: c
                                            .get("name")
                                            .and_then(|n| n.as_str())
                                            .unwrap_or("unknown")
                                            .to_string(),
                                        arguments: serde_json::to_string(
                                            c.get("args").unwrap_or(&json!({})),
                                        )?,
                                    },
                                ));
                            }
                        } else if let Some(text) = content {
                            if !text.trim().is_empty() {
                                events.push(Event::at(
                                    ts,
                                    EventKind::Message {
                                        role: Role::Assistant,
                                        text,
                                        source_kind: None,
                                    },
                                ));
                            }
                        }
                    }
                    "GENERIC" if source == "MODEL" => {
                        if let Some(text) = content {
                            if !text.trim().is_empty() {
                                events.push(Event::at(
                                    ts,
                                    EventKind::Message {
                                        role: Role::Assistant,
                                        text,
                                        source_kind: Some("agy-generic".into()),
                                    },
                                ));
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
            let db = self
                .agy_dir
                .join("conversations")
                .join(format!("{}.db", r.id));
            if !db.exists() {
                anyhow::bail!("no agy transcript or conversation db found for {}", r.id);
            }
            let con = rusqlite::Connection::open_with_flags(
                &db,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )?;
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
            resume_events: None,
        })
    }

    fn write(&self, s: &Session, opts: &WriteOpts) -> anyhow::Result<WriteOutcome> {
        let template = self.find_template()?;
        let cwd = opts
            .cwd
            .clone()
            .or_else(|| s.cwd.clone())
            .unwrap_or_default();
        let name = opts
            .name
            .clone()
            .or_else(|| s.title.clone())
            .unwrap_or_else(|| {
                s.first_user_text()
                    .map(|t| t.lines().next().unwrap_or_default().to_string())
                    .unwrap_or_default()
            });

        // collect the text turns; tool steps are not representable yet (documented)
        let mut turns: Vec<(i64, Role, String)> = Vec::new(); // (time_ms, role, text)
        let mut offset: i64 = 0;
        for e in &s.events {
            let ms = s.event_ms(e, offset);
            offset += 1;
            match &e.kind {
                EventKind::Message {
                    role: Role::User,
                    text,
                    ..
                } if !text.trim().is_empty() => {
                    turns.push((ms, Role::User, text.clone()));
                }
                EventKind::Message {
                    role: Role::Assistant,
                    text,
                    ..
                } if !text.trim().is_empty() => {
                    turns.push((ms, Role::Assistant, text.clone()));
                }
                _ => {}
            }
        }
        if turns.is_empty() {
            anyhow::bail!("session has no user/assistant text turns to write natively (tool-only sessions are not supported yet)");
        }

        let new_id = uuid7(
            s.created_ms,
            &format!("harness-bridge/v1|{}|{}", s.source, s.id),
        );
        let new_traj = uuid7(
            s.created_ms + 1,
            &format!("harness-bridge-traj|{}|{}", s.source, s.id),
        );

        // build the per-step payloads by cloning the template envelopes
        let mut steps: Vec<(i64, Vec<u8>)> = Vec::new(); // (step_type, payload)
        for (i, (_, role, text)) in turns.iter().enumerate() {
            let (tpl_payload, tpl_text, tpl_uuid, step_type) = match role {
                Role::User => (
                    &template.user_payload,
                    &template.user_text,
                    &template.user_step_uuid,
                    14i64,
                ),
                _ => (
                    &template.asst_payload,
                    &template.asst_text,
                    &template.asst_step_uuid,
                    15i64,
                ),
            };
            let step_uuid = uuid7(
                s.created_ms + 2 + i as i64,
                &format!("agy-step|{}|{}", s.id, i),
            );
            let mut pairs: Vec<(String, String)> = vec![(tpl_text.clone(), text.clone())];
            if let Some(u) = tpl_uuid {
                pairs.push((u.clone(), step_uuid));
            }
            let mut repl = |leaf: &str| -> Option<String> {
                let mut out = leaf.to_string();
                let mut hit = false;
                for (a, b) in &pairs {
                    if out.contains(a.as_str()) {
                        out = out.replace(a.as_str(), b.as_str());
                        hit = true;
                    }
                }
                // swap the template conversation/trajectory ids everywhere
                if !template.cascade_id.is_empty() && out.contains(&template.cascade_id) {
                    out = out.replace(&template.cascade_id, &new_id);
                    hit = true;
                }
                if !template.trajectory_id.is_empty() && out.contains(&template.trajectory_id) {
                    out = out.replace(&template.trajectory_id, &new_traj);
                    hit = true;
                }
                if hit {
                    Some(out)
                } else {
                    None
                }
            };
            let payload = rewire(tpl_payload, &mut repl)?;
            steps.push((step_type, payload));
        }

        if !opts.dry_run {
            // 1. conversation db (cloned from the template db, steps rebuilt)
            let dst = self
                .agy_dir
                .join("conversations")
                .join(format!("{new_id}.db"));
            std::fs::create_dir_all(dst.parent().unwrap())?;
            let tpl_db = self
                .agy_dir
                .join("conversations")
                .join(format!("{}.db", template.cascade_id));
            std::fs::copy(&tpl_db, &dst)?;
            let con = rusqlite::Connection::open(&dst)?;
            con.busy_timeout(std::time::Duration::from_secs(10))?;
            con.execute("delete from steps", [])?;
            for (idx, (step_type, payload)) in steps.iter().enumerate() {
                con.execute(
                    "insert into steps (idx, step_type, status, has_subtrajectory, step_payload, step_format)
                     values (?1, ?2, 3, 0, ?3, 0)",
                    rusqlite::params![idx as i64, step_type, payload],
                )?;
            }
            con.execute(
                "update trajectory_meta set trajectory_id = ?1, cascade_id = ?2",
                rusqlite::params![new_traj, new_id],
            )?;
            con.close().ok();

            // 2. summaries index row (cloned from the template row)
            let sdb = self.summaries_db();
            let scon = rusqlite::Connection::open(&sdb)?;
            scon.busy_timeout(std::time::Duration::from_secs(10))?;
            let now = chrono::Utc::now()
                .format("%Y-%m-%d %H:%M:%S%.6f+00:00")
                .to_string();
            let uri = format!("file:///{}", cwd.replace('\\', "/").trim_start_matches('/'));
            if let (Some(row), true) = (
                &template.summaries_row,
                !template.summaries_columns.is_empty(),
            ) {
                let cols: Vec<String> = row.iter().map(|(c, _)| c.clone()).collect();
                let vals: Vec<rusqlite::types::Value> = row
                    .iter()
                    .map(|(c, v)| match c.as_str() {
                        "conversation_id" => rusqlite::types::Value::Text(new_id.clone()),
                        "title" => rusqlite::types::Value::Text(name.clone()),
                        "preview" => rusqlite::types::Value::Text(name.chars().take(200).collect()),
                        "step_count" => rusqlite::types::Value::Integer(steps.len() as i64),
                        "last_modified_time" | "last_user_input_time" => {
                            rusqlite::types::Value::Text(now.clone())
                        }
                        "workspace_uris" => rusqlite::types::Value::Text(json!([uri]).to_string()),
                        "status" => rusqlite::types::Value::Text(String::new()),
                        _ => v.clone(),
                    })
                    .collect();
                scon.execute(
                    &format!(
                        "insert or replace into conversation_summaries ({}) values ({})",
                        cols.join(","),
                        vec!["?"; cols.len()].join(",")
                    ),
                    rusqlite::params_from_iter(vals),
                )?;
            }
            scon.close().ok();

            // 3. brain transcripts (agy's own plain-JSONL history)
            let logs = self
                .agy_dir
                .join("brain")
                .join(&new_id)
                .join(".system_generated")
                .join("logs");
            std::fs::create_dir_all(&logs)?;
            let mut lines = String::new();
            for (i, (ms, role, text)) in turns.iter().enumerate() {
                let rec = json!({
                    "step_index": i,
                    "source": if *role == Role::User { "USER_EXPLICIT" } else { "MODEL" },
                    "type": if *role == Role::User { "USER_INPUT" } else { "PLANNER_RESPONSE" },
                    "status": "DONE",
                    "created_at": Self::brain_ts(*ms),
                    "content": text,
                });
                lines.push_str(&rec.to_string());
                lines.push('\n');
            }
            std::fs::write(logs.join("transcript.jsonl"), &lines)?;
            std::fs::write(logs.join("transcript_full.jsonl"), &lines)?;

            // 4. title annotation
            let ann = self.agy_dir.join("annotations");
            std::fs::create_dir_all(&ann)?;
            std::fs::write(
                ann.join(format!("{new_id}.pbtxt")),
                format!("title:\"{}\"\n", name.replace('"', "'")),
            )?;
        }

        Ok(WriteOutcome {
            provider: "agy".into(),
            location: self
                .agy_dir
                .join("conversations")
                .join(format!("{new_id}.db"))
                .to_string_lossy()
                .to_string(),
            native_id: new_id,
            extra: json!({
                "steps": steps.len(),
                "note": "native protobuf write; text turns only (tool steps not representable yet); \
                         resume with: agy --conversation <id>",
                "dry_run": opts.dry_run,
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn varint(v: u64) -> Vec<u8> {
        enc_varint(v)
    }

    fn field_tag(field: u64, wire: u64) -> Vec<u8> {
        enc_varint((field << 3) | wire)
    }

    #[test]
    fn rewire_replaces_and_relengths() {
        // field1 varint(5), field2 "hello", field3 { field1 "world" }
        let mut buf = Vec::new();
        buf.extend(field_tag(1, 0));
        buf.extend(varint(5));
        buf.extend(field_tag(2, 2));
        buf.extend(varint(5));
        buf.extend(b"hello");
        buf.extend(field_tag(3, 2));
        let mut nested = Vec::new();
        nested.extend(field_tag(1, 2));
        nested.extend(varint(5));
        nested.extend(b"world");
        buf.extend(varint(nested.len() as u64));
        buf.extend(&nested);

        let mut repl = |s: &str| -> Option<String> {
            if s == "hello" {
                Some("hello, brave new world".to_string())
            } else {
                None
            }
        };
        let out = rewire(&buf, &mut repl).unwrap();
        assert!(contains_utf8_leaf(&out, "hello, brave new world"));
        assert!(!contains_utf8_leaf(&out, "hello\u{0}")); // no corruption
                                                          // nested untouched, structure preserved: parse back
                                                          // wire_fields only collects length-delimited leaves; the wire-0 field is preserved too
        let fields = wire_fields(&out);
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].0, 2);
        assert_eq!(
            str_field(&out, 2).as_deref(),
            Some("hello, brave new world")
        );
        assert_eq!(fields[1].0, 3);
        let nested2 = sub(&out, 3);
        assert_eq!(nested2.len(), 1);
        assert_eq!(str_field(&nested2[0], 1).as_deref(), Some("world"));
    }

    #[test]
    fn rewire_identity_when_no_match() {
        let buf = b"\x0a\x05hello\x12\x03abc".to_vec();
        let mut repl = |_: &str| -> Option<String> { None };
        let out = rewire(&buf, &mut repl).unwrap();
        assert_eq!(out, buf);
    }
}
