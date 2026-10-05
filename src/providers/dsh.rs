//! DeepSeek Harness (dsh) provider.
//!
//! Reads `~/.dsh/sessions/<project>/<uuid>/session.v4.jsonl[.zstd]` event
//! transcripts and writes the same format back (plain JSONL; dsh stores them
//! zstd-compressed but the event schema is identical).

use crate::ir::{Event, EventKind, Role, Session, SessionRef, WriteOpts, WriteOutcome};
use crate::util::{read_maybe_zstd, uuid7};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub struct DshProvider {
    pub dsh_home: PathBuf,
}

/// dsh source kinds that represent genuine user-channel input.
const REAL_USER_SOURCES: &[&str] = &["user", "user-approval"];

fn is_real_user(kind: Option<&str>) -> bool {
    match kind {
        None => true,
        Some(k) => REAL_USER_SOURCES.contains(&k),
    }
}

/// Best-effort reconstruction of dsh's project-dir slug (`--F-Miscellaneous-GitHub-mod-smali--`).
fn project_slug(cwd: &str) -> String {
    let mut out = String::new();
    let mut last_sep = false;
    for c in cwd.chars() {
        if c.is_ascii_alphanumeric() || c == '.' {
            out.push(c);
            last_sep = false;
        } else if c == ' ' {
            out.push_str("~0020");
            last_sep = false;
        } else if !last_sep {
            out.push('-');
            last_sep = true;
        }
    }
    format!("--{}--", out.trim_matches('-'))
}

impl DshProvider {
    pub fn new(dsh_home: PathBuf) -> Self {
        DshProvider { dsh_home }
    }

    fn projcache_title(&self, dsh_id: &str) -> Option<String> {
        for name in [format!("{dsh_id}.json"), format!("session-{dsh_id}.json")] {
            let p = self.dsh_home.join("storages").join("session_projcache").join("sessions").join(&name);
            if let Ok(txt) = std::fs::read_to_string(&p) {
                if let Ok(v) = serde_json::from_str::<Value>(&txt) {
                    if let Some(t) = v.pointer("/record/rows/title/val").and_then(|t| t.as_str()) {
                        if !t.trim().is_empty() {
                            return Some(t.trim().to_string());
                        }
                    }
                }
            }
        }
        None
    }
}

impl super::Provider for DshProvider {
    fn name(&self) -> &'static str {
        "dsh"
    }

    fn discover(&self) -> anyhow::Result<Vec<SessionRef>> {
        let mut out = Vec::new();
        let root = self.dsh_home.join("sessions");
        let mut projects: Vec<_> = std::fs::read_dir(&root)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .collect();
        projects.sort_by_key(|e| e.file_name());
        for proj in projects {
            let mut sessions: Vec<_> = std::fs::read_dir(proj.path())?
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .collect();
            sessions.sort_by_key(|e| e.file_name());
            for s in sessions {
                let dsh_id = s.file_name().to_string_lossy().to_string();
                if dsh_id.len() != 36 || dsh_id.chars().filter(|c| *c == '-').count() != 4 {
                    continue;
                }
                let transcript = ["session.v4.jsonl.zstd", "session.v4.jsonl", "session.v3.jsonl.zstd", "session.v3.jsonl", "session.jsonl"]
                    .iter()
                    .map(|f| s.path().join(f))
                    .find(|p| p.exists());
                let Some(transcript) = transcript else { continue };
                let title = self.projcache_title(&dsh_id);
                out.push(SessionRef {
                    provider: "dsh".into(),
                    id: dsh_id.clone(),
                    title,
                    cwd: None,
                    created_ms: None,
                    updated_ms: None,
                    locator: transcript.to_string_lossy().to_string(),
                    migrated: false,
                });
            }
        }
        Ok(out)
    }

    fn read(&self, r: &SessionRef) -> anyhow::Result<Session> {
        let text = read_maybe_zstd(Path::new(&r.locator))?;
        let mut lines = text.lines();
        let header: Value = serde_json::from_str(
            lines.next().ok_or_else(|| anyhow::anyhow!("empty dsh transcript"))?,
        )?;
        if header.get("type").and_then(|t| t.as_str()) != Some("session") {
            anyhow::bail!("unexpected dsh transcript header");
        }

        let mut events: Vec<(i64, Value)> = Vec::new();
        for ln in lines {
            if let Ok(v) = serde_json::from_str::<Value>(ln) {
                let t = v.get("time").and_then(|t| t.as_i64()).unwrap_or(0);
                events.push((t, v));
            }
        }
        let created_ms = events.iter().map(|(t, _)| *t).min().unwrap_or(0);
        let updated_ms = events.iter().map(|(t, _)| *t).max().unwrap_or(0);

        let user_msg_ids: std::collections::HashSet<String> = events
            .iter()
            .filter_map(|(_, v)| {
                if v.get("type").and_then(|t| t.as_str()) == Some("user/message") {
                    v.pointer("/data/id").and_then(|i| i.as_str()).map(str::to_string)
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

        for (time, v) in &events {
            let t = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
            let d = v.get("data").cloned().unwrap_or(Value::Null);
            match t {
                "turn/start" => evs.push(Event::at(Some(*time), EventKind::TurnStart)),
                "turn/end" => evs.push(Event::at(
                    Some(*time),
                    EventKind::TurnEnd {
                        reason: d.pointer("/reason/kind").and_then(|k| k.as_str()).map(str::to_string),
                    },
                )),
                "session/title" => {
                    title = d.get("title").and_then(|t| t.as_str()).map(str::to_string);
                }
                "system/message" => {
                    let text = join_text(&block_texts(d.pointer("/message/content")));
                    if !text.is_empty() {
                        evs.push(Event::at(Some(*time), EventKind::Message {
                            role: Role::Developer,
                            text,
                            source_kind: Some("system-prompt".into()),
                        }));
                    }
                }
                "user/message" => {
                    let text = join_text(&block_texts(d.get("content")));
                    if !text.is_empty() {
                        let kind = d.pointer("/source/kind").and_then(|k| k.as_str()).map(str::to_string);
                        evs.push(Event::at(Some(*time), EventKind::Message {
                            role: if is_real_user(kind.as_deref()) { Role::User } else { Role::Developer },
                            text,
                            source_kind: kind,
                        }));
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
                                let kind = ins.pointer("/source/kind").and_then(|k| k.as_str()).map(str::to_string);
                                evs.push(Event::at(Some(*time), EventKind::Message {
                                    role: if is_real_user(kind.as_deref()) { Role::User } else { Role::Developer },
                                    text,
                                    source_kind: kind,
                                }));
                            }
                        }
                    }
                }
                "assistant/message" => {
                    let msg = d.get("message").cloned().unwrap_or(Value::Null);
                    let mut reasoning = Vec::new();
                    let mut text = Vec::new();
                    for b in msg.get("content").and_then(|c| c.as_array()).into_iter().flatten() {
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
                        evs.push(Event::at(Some(*time), EventKind::Reasoning { text: join_text(&reasoning) }));
                    }
                    let text = join_text(&text);
                    if !text.is_empty() {
                        evs.push(Event::at(Some(*time), EventKind::Message {
                            role: Role::Assistant,
                            text,
                            source_kind: None,
                        }));
                    }
                }
                "tool/call" => {
                    if let Some(cid) = d.get("callId").and_then(|c| c.as_str()) {
                        if seen_calls.insert(cid.to_string()) {
                            evs.push(Event::at(Some(*time), EventKind::ToolCall {
                                call_id: cid.to_string(),
                                name: d.get("name").and_then(|n| n.as_str()).unwrap_or("unknown").to_string(),
                                arguments: d.get("arguments").and_then(|a| a.as_str()).unwrap_or("{}").to_string(),
                            }));
                        }
                    }
                }
                "tool/result" => {
                    let cid = d.pointer("/message/toolCallId").and_then(|c| c.as_str()).map(str::to_string);
                    let Some(cid) = cid else { continue };
                    let text = join_text(&block_texts(d.pointer("/message/content")));
                    match result_idx.get(&cid) {
                        Some(&i) => {
                            if let EventKind::ToolResult { text: prev, .. } = &mut evs[i].kind {
                                *prev = join_text(&[prev.clone(), text]);
                            }
                        }
                        None => {
                            evs.push(Event::at(Some(*time), EventKind::ToolResult { call_id: cid.clone(), text }));
                            result_idx.insert(cid, evs.len() - 1);
                        }
                    }
                }
                "compaction/summary" => {
                    let text = join_text(&block_texts(d.get("summary")));
                    evs.push(Event::at(Some(*time), EventKind::Compaction {
                        id: d.get("compactionId").and_then(|c| c.as_str()).map(str::to_string),
                        text: (!text.is_empty()).then_some(text),
                    }));
                }
                "developer/message" => {
                    let mut added = Vec::new();
                    let mut removed = Vec::new();
                    for b in d.pointer("/message/content").and_then(|c| c.as_array()).into_iter().flatten() {
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
                        evs.push(Event::at(Some(*time), EventKind::Meta {
                            kind: "tool-registry".into(),
                            data: json!({"added": added, "removed": removed}),
                        }));
                    }
                }
                "goal/change" => {
                    if d.get("operation").and_then(|o| o.as_str()) == Some("create") {
                        if let Some(obj) = d.pointer("/goal/objective").and_then(|o| o.as_str()) {
                            evs.push(Event::at(Some(*time), EventKind::Meta {
                                kind: "goal".into(),
                                data: json!({"objective": obj}),
                            }));
                        }
                    }
                }
                _ => {} // telemetry: retries, request headers, sandbox modes, titles, ...
            }
        }

        Ok(Session {
            source: "dsh".into(),
            id: r.id.clone(),
            title: title.or_else(|| r.title.clone()),
            cwd: header.get("cwd").and_then(|c| c.as_str()).map(str::to_string),
            agent_preset: header.get("agentPreset").and_then(|c| c.as_str()).map(str::to_string),
            parent_session: header.get("parentSession").and_then(|c| c.as_str()).map(str::to_string),
            origin: header.get("origin").and_then(|c| c.as_str()).map(str::to_string),
            created_ms,
            updated_ms,
            events: evs,
        })
    }

    fn write(&self, s: &Session, opts: &WriteOpts) -> anyhow::Result<WriteOutcome> {
        let cwd = opts.cwd.clone().or_else(|| s.cwd.clone()).unwrap_or_default();
        // stable per source session so re-imports overwrite instead of duplicating
        let new_id = uuid7(s.created_ms, &format!("dsh-import|{}", s.id));

        let slug = project_slug(&cwd);
        let dir = self.dsh_home.join("sessions").join(slug).join(&new_id);
        let path = dir.join("session.v4.jsonl");

        let mut header = json!({
            "type": "session",
            "version": 4,
            "id": new_id,
            "createdAt": s.created_ms,
            "cwd": cwd,
            "origin": "import",
            "isSeeded": false,
        });
        if let Some(p) = &s.parent_session {
            header["parentSession"] = json!(p);
        }
        if let Some(p) = &s.agent_preset {
            header["agentPreset"] = json!(p);
        }

        let mut out = vec![header.to_string()];
        let mut seq: i64 = 0;
        let mut turn: i64 = 0;
        let mut offset: i64 = 0;
        let mut pending_reasoning: Option<(i64, String)> = None; // (time, text)

        macro_rules! push {
            ($time:expr, $obj:expr) => {{
                let mut o = $obj;
                o["seq"] = json!(seq);
                o["time"] = json!($time);
                seq += 1;
                out.push(o.to_string());
            }};
        }

        for e in &s.events {
            let time = s.event_ms(e, offset);
            offset += 1;
            match &e.kind {
                EventKind::TurnStart => {
                    turn += 1;
                    push!(time, json!({"type": "turn/start", "data": {"turn": turn}}));
                }
                EventKind::TurnEnd { reason } => {
                    push!(time, json!({"type": "turn/end", "data": {"turn": turn, "reason": {"kind": reason.clone().unwrap_or_else(|| "end".into())}}}));
                }
                EventKind::Message { role, text, source_kind } => match role {
                    Role::User => {
                        push!(time, json!({
                            "type": "user/message",
                            "data": {
                                "content": [{"type": "text", "text": text}],
                                "source": {"kind": source_kind.clone().unwrap_or_else(|| "user".into())},
                                "role": "user",
                                "id": uuid7(time, &format!("dsh-user|{seq}")),
                            },
                            "surfaceOp": "append"
                        }));
                    }
                    Role::Developer => {
                        if source_kind.as_deref() == Some("system-prompt") {
                            push!(time, json!({
                                "type": "system/message",
                                "data": {"turn": turn, "step": 1,
                                         "message": {"role": "system", "content": [{"type": "text", "text": text}]}}
                            }));
                        } else {
                            push!(time, json!({
                                "type": "developer/message",
                                "data": {"turn": turn, "step": 1,
                                         "message": {"role": "developer", "source": {"kind": source_kind.clone().unwrap_or_else(|| "import".into())},
                                                     "content": [{"type": "text", "text": text}]}}
                            }));
                        }
                    }
                    Role::Assistant => {
                        let mut content = Vec::new();
                        if let Some((_, r)) = pending_reasoning.take() {
                            content.push(json!({"type": "reasoning", "text": r}));
                        }
                        content.push(json!({"type": "text", "text": text}));
                        push!(time, json!({
                            "type": "assistant/message",
                            "data": {"turn": turn, "step": 1,
                                     "message": {"role": "assistant", "source": {"kind": "model"}, "content": content}}
                        }));
                    }
                },
                EventKind::Reasoning { text } => {
                    pending_reasoning = Some((time, text.clone()));
                }
                EventKind::ToolCall { call_id, name, arguments } => {
                    push!(time, json!({
                        "type": "tool/call",
                        "data": {"turn": turn, "step": 1, "callId": call_id, "name": name, "arguments": arguments}
                    }));
                }
                EventKind::ToolResult { call_id, text } => {
                    push!(time, json!({
                        "type": "tool/result",
                        "data": {"turn": turn, "step": 1,
                                 "message": {"role": "tool", "source": {"kind": "tool", "callId": call_id},
                                             "toolCallId": call_id, "content": [{"type": "text", "text": text}]}}
                    }));
                }
                EventKind::Compaction { id, text } => {
                    let cid = id.clone().unwrap_or_else(|| uuid7(time, &format!("dsh-cmp|{seq}")));
                    push!(time, json!({"type": "compaction/start", "data": {"compactionId": cid, "turn": turn}}));
                    push!(time, json!({"type": "compaction/summary", "data": {"compactionId": cid, "summary": [{"type": "text", "text": text.clone().unwrap_or_default()}]}}));
                    push!(time, json!({"type": "compaction/end", "data": {"compactionId": cid, "turn": turn}}));
                }
                EventKind::Meta { kind, data } => match kind.as_str() {
                    "tool-registry" => {
                        let mut content = Vec::new();
                        for n in data.get("added").and_then(|a| a.as_array()).into_iter().flatten() {
                            content.push(json!({"type": "tool-addition", "toolName": n}));
                        }
                        for n in data.get("removed").and_then(|a| a.as_array()).into_iter().flatten() {
                            content.push(json!({"type": "tool-removal", "toolName": n}));
                        }
                        push!(time, json!({
                            "type": "developer/message",
                            "data": {"turn": turn, "step": 1, "headerSeq": seq,
                                     "message": {"role": "developer", "source": {"kind": "tool-registry"}, "content": content}}
                        }));
                    }
                    "goal" => {
                        push!(time, json!({
                            "type": "goal/change",
                            "data": {"kind": "goal/change", "version": 1, "operation": "create",
                                     "goal": {"id": uuid7(time, "dsh-goal"), "revision": 1,
                                              "objective": data.get("objective").cloned().unwrap_or(Value::Null),
                                              "phase": "active", "maxGoalRounds": 40}}
                        }));
                    }
                    _ => {} // unknown meta: skip
                },
            }
        }

        if !opts.dry_run {
            std::fs::create_dir_all(&dir)?;
            std::fs::write(&path, out.join("\n") + "\n")?;
        }
        Ok(WriteOutcome {
            provider: "dsh".into(),
            location: path.to_string_lossy().to_string(),
            native_id: new_id,
            extra: json!({"note": "written as plain JSONL (dsh stores zstd); the harness reads both", "events": out.len()}),
        })
    }
}

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
    parts.iter().filter(|p| !p.trim().is_empty()).cloned().collect::<Vec<_>>().join("\n\n")
}
