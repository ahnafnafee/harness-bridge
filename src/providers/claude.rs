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

mod context;
#[cfg(test)]
mod tests;

pub struct ClaudeProvider {
    pub claude_home: std::path::PathBuf,
}

impl ClaudeProvider {
    pub fn new(claude_home: std::path::PathBuf) -> Self {
        ClaudeProvider { claude_home }
    }

    fn custom_title(&self, slug: &str, id: &str) -> Option<String> {
        let p = self
            .claude_home
            .join("projects")
            .join(slug)
            .join(id)
            .join("custom-title.json");
        let txt = std::fs::read_to_string(p).ok()?;
        let v: Value = serde_json::from_str(&txt).ok()?;
        v.get("customTitle")
            .and_then(|t| t.as_str())
            .map(str::to_string)
    }

    fn parse_ts(ts: &str) -> Option<i64> {
        chrono::DateTime::parse_from_rfc3339(ts)
            .ok()
            .map(|d| d.timestamp_millis())
    }
}

impl super::Provider for ClaudeProvider {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

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
                    let id = p
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or_default()
                        .to_string();
                    let slug_name = slug_dir.file_name().to_string_lossy().to_string();
                    let title = self.custom_title(&slug_name, &id);
                    let md = f.metadata().ok();
                    refs.push(SessionRef {
                        provider: "claude".into(),
                        id: id.clone(),
                        title,
                        cwd: None,
                        created_ms: None,
                        updated_ms: md.as_ref().and_then(|m| m.modified().ok()).map(|t| {
                            t.duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as i64)
                                .unwrap_or(0)
                        }),
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
        let records: Vec<Value> = text
            .lines()
            .filter_map(|ln| serde_json::from_str(ln).ok())
            .collect();
        let mut event_ranges = Vec::with_capacity(records.len());

        for o in &records {
            let first_event = evs.len();
            let typ = o
                .get("type")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string();
            let ts = o
                .get("timestamp")
                .and_then(|t| t.as_str())
                .and_then(Self::parse_ts);
            if let Some(t) = ts {
                created_ms = created_ms.min(t);
                updated_ms = updated_ms.max(t);
            }
            match typ.as_str() {
                "system"
                    if o.get("subtype").and_then(Value::as_str) == Some("compact_boundary") =>
                {
                    evs.push(Event::at(
                        ts,
                        EventKind::Compaction {
                            id: o.get("uuid").and_then(Value::as_str).map(str::to_string),
                            text: None,
                        },
                    ));
                }
                "ai-title" => {
                    title = o
                        .get("aiTitle")
                        .and_then(|t| t.as_str())
                        .map(str::to_string)
                        .or(title);
                }
                "user" | "assistant" => {
                    if cwd.is_none() {
                        cwd = o.get("cwd").and_then(|c| c.as_str()).map(str::to_string);
                    }
                    let msg = o.get("message").cloned().unwrap_or(Value::Null);
                    let content = msg.get("content").cloned().unwrap_or(Value::Null);
                    match content {
                        Value::String(s) if !s.trim().is_empty() => {
                            evs.push(Event::at(
                                ts,
                                EventKind::Message {
                                    role: if typ == "user" {
                                        Role::User
                                    } else {
                                        Role::Assistant
                                    },
                                    text: s,
                                    source_kind: None,
                                },
                            ));
                        }
                        Value::Array(blocks) => {
                            for b in blocks {
                                let btype = b.get("type").and_then(|t| t.as_str()).unwrap_or("");
                                match (typ.as_str(), btype) {
                                    (_, "text") => {
                                        let s = b
                                            .get("text")
                                            .and_then(|t| t.as_str())
                                            .unwrap_or_default();
                                        if !s.trim().is_empty() {
                                            evs.push(Event::at(
                                                ts,
                                                EventKind::Message {
                                                    role: if typ == "user" {
                                                        Role::User
                                                    } else {
                                                        Role::Assistant
                                                    },
                                                    text: s.to_string(),
                                                    source_kind: None,
                                                },
                                            ));
                                        }
                                    }
                                    ("assistant", "thinking") => {
                                        let s = b
                                            .get("thinking")
                                            .and_then(|t| t.as_str())
                                            .unwrap_or_default();
                                        if !s.trim().is_empty() {
                                            evs.push(Event::at(
                                                ts,
                                                EventKind::Reasoning {
                                                    text: s.to_string(),
                                                },
                                            ));
                                        }
                                    }
                                    ("assistant", "tool_use") => {
                                        evs.push(Event::at(
                                            ts,
                                            EventKind::ToolCall {
                                                call_id: b
                                                    .get("id")
                                                    .and_then(|i| i.as_str())
                                                    .unwrap_or_default()
                                                    .to_string(),
                                                name: b
                                                    .get("name")
                                                    .and_then(|n| n.as_str())
                                                    .unwrap_or("unknown")
                                                    .to_string(),
                                                arguments: serde_json::to_string(
                                                    b.get("input").unwrap_or(&json!({})),
                                                )?,
                                            },
                                        ));
                                    }
                                    ("user", "tool_result") => {
                                        let out = match b.get("content") {
                                            Some(Value::String(s)) => s.clone(),
                                            Some(Value::Array(blocks)) => {
                                                let parts: Vec<String> = blocks
                                                    .iter()
                                                    .filter_map(|bb| {
                                                        bb.get("text")
                                                            .and_then(|t| t.as_str())
                                                            .map(str::to_string)
                                                    })
                                                    .collect();
                                                parts.join("\n\n")
                                            }
                                            _ => String::new(),
                                        };
                                        evs.push(Event::at(
                                            ts,
                                            EventKind::ToolResult {
                                                call_id: b
                                                    .get("tool_use_id")
                                                    .and_then(|i| i.as_str())
                                                    .unwrap_or_default()
                                                    .to_string(),
                                                text: out,
                                            },
                                        ));
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
            event_ranges.push(first_event..evs.len());
        }
        if created_ms == i64::MAX {
            created_ms = updated_ms;
        }

        let child = Path::new(&r.locator)
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == "subagents");
        let parent_session = if child {
            records.iter().find_map(|o| {
                o.get("sessionId")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
        } else {
            None
        };
        let resume_events = context::resume_events(&records, &event_ranges, &evs, child)?;
        Ok(Session {
            source: "claude".into(),
            id: r.id.clone(),
            title,
            cwd,
            agent_preset: None,
            parent_session,
            origin: None,
            created_ms,
            updated_ms,
            events: evs,
            resume_events,
        })
    }

    fn children(&self, parent: &SessionRef) -> anyhow::Result<Vec<SessionRef>> {
        let path = Path::new(&parent.locator);
        let dir = path.with_extension("").join("subagents");
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut children = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "jsonl") {
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default();
                children.push(SessionRef {
                    provider: "claude".into(),
                    id: std::fs::File::open(&path)
                        .ok()
                        .and_then(|file| {
                            std::io::BufRead::lines(std::io::BufReader::new(file))
                                .next()?
                                .ok()
                        })
                        .and_then(|line| serde_json::from_str::<Value>(&line).ok())
                        .and_then(|record| {
                            record["harnessBridgeSessionId"]
                                .as_str()
                                .map(str::to_string)
                        })
                        .unwrap_or_else(|| format!("{}/{name}", parent.id)),
                    title: Some(name.into()),
                    cwd: parent.cwd.clone(),
                    created_ms: None,
                    updated_ms: None,
                    locator: path.to_string_lossy().into(),
                    migrated: false,
                });
            }
        }
        children.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(children)
    }

    fn write(&self, s: &Session, opts: &WriteOpts) -> anyhow::Result<WriteOutcome> {
        let cwd = opts
            .cwd
            .clone()
            .or_else(|| s.cwd.clone())
            .unwrap_or_default();
        let session_id = uuid7(
            s.created_ms,
            &format!("harness-bridge/v1|{}|{}", s.source, s.id),
        );
        let slug = claude_slug(&cwd);
        let project = self.claude_home.join("projects").join(&slug);
        let path = match &s.parent_session {
            Some(parent) => {
                let parent_path = s
                    .events
                    .iter()
                    .rev()
                    .find_map(|event| match &event.kind {
                        EventKind::Meta { kind, data } if kind == "migration-parent-depth" => {
                            data["location"].as_str()
                        }
                        _ => None,
                    })
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| project.join(format!("{parent}.jsonl")));
                parent_path
                    .with_extension("")
                    .join("subagents")
                    .join(format!("agent-{session_id}.jsonl"))
            }
            None => project.join(format!("{session_id}.jsonl")),
        };

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
                "isSidechain": s.parent_session.is_some(),
                "type": typ,
                "message": message,
                "uuid": uuid,
                "timestamp": crate::util::iso_ms(ts_ms),
                "cwd": cwd,
                "sessionId": s.parent_session.as_ref().unwrap_or(&session_id),
                "agentId": if s.parent_session.is_some() {Some(&session_id)} else {None},
                "harnessBridgeSessionId": session_id,
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

        let mut tool_calls: std::collections::HashMap<String, (String, i64)> =
            std::collections::HashMap::new();
        let write_events: Vec<&Event> = s
            .events
            .iter()
            .chain(s.resume_events.iter().flatten())
            .collect();
        for (index, e) in write_events.into_iter().enumerate() {
            if index == s.events.len() && s.resume_events.is_some() {
                push(
                    &mut records,
                    &mut parent_uuid,
                    "system",
                    s.updated_ms,
                    Value::Null,
                    json!({"subtype":"compact_boundary", "parentUuid":null,
                        "content":"Migration resume checkpoint", "compactMetadata":{"trigger":"manual"}}),
                );
                push(
                    &mut records,
                    &mut parent_uuid,
                    "user",
                    s.updated_ms,
                    json!({"role":"user", "content":"The retained migration context follows; earlier transcript entries are archival."}),
                    json!({"isCompactSummary":true}),
                );
            }
            let ts = s.event_ms(e, offset);
            offset += 1;
            match &e.kind {
                EventKind::Message {
                    role,
                    text,
                    source_kind,
                } => match role {
                    Role::User => {
                        push(
                            &mut records,
                            &mut parent_uuid,
                            "user",
                            ts,
                            json!({"role": "user", "content": text}),
                            json!({"userType": "external", "sourceKind": source_kind}),
                        );
                    }
                    Role::Assistant => {
                        push(
                            &mut records,
                            &mut parent_uuid,
                            "assistant",
                            ts,
                            json!({"role": "assistant", "content": [{"type": "text", "text": text}], "model": "imported"}),
                            json!({}),
                        );
                    }
                    // Claude Code has no developer channel; context notes stay in the IR
                    // and are dropped here (documented lossy conversion).
                    Role::Developer => {}
                },
                EventKind::Reasoning { text } => {
                    let content = if s.resume_events.is_none() || index >= s.events.len() {
                        json!([{"type":"text", "text":format!("[Imported reasoning]\n{text}")}])
                    } else {
                        json!([{"type":"thinking", "thinking":text, "signature":""}])
                    };
                    push(
                        &mut records,
                        &mut parent_uuid,
                        "assistant",
                        ts,
                        json!({"role": "assistant", "content":content, "model": "imported"}),
                        json!({}),
                    );
                }
                EventKind::ToolCall {
                    call_id,
                    name,
                    arguments,
                } => {
                    let input: Value = serde_json::from_str(arguments)
                        .unwrap_or_else(|_| json!({"raw": arguments}));
                    tool_calls.insert(call_id.clone(), (name.clone(), ts));
                    push(
                        &mut records,
                        &mut parent_uuid,
                        "assistant",
                        ts,
                        json!({"role": "assistant", "content": [{"type": "tool_use", "id": call_id, "name": name, "input": input}], "model": "imported"}),
                        json!({}),
                    );
                }
                EventKind::ToolResult { call_id, text } => {
                    let (name, _t) = tool_calls
                        .get(call_id)
                        .cloned()
                        .unwrap_or_else(|| ("imported".into(), ts));
                    let _ = name;
                    push(
                        &mut records,
                        &mut parent_uuid,
                        "user",
                        ts,
                        json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": call_id, "content": text}]}),
                        json!({}),
                    );
                }
                EventKind::Compaction { id: _, text } => {
                    if let Some(t) = text {
                        push(
                            &mut records,
                            &mut parent_uuid,
                            "user",
                            ts,
                            json!({"role": "user", "content": format!("[compacted context]\n\n{t}")}),
                            json!({}),
                        );
                    }
                }
                EventKind::TurnStart | EventKind::TurnEnd { .. } => {}
                EventKind::Meta { .. } => {}
            }
        }
        let title = opts
            .name
            .clone()
            .or_else(|| s.title.clone())
            .unwrap_or_else(|| {
                s.first_user_text()
                    .map(|t| t.lines().next().unwrap_or_default().to_string())
                    .unwrap_or_default()
            });
        records.push(json!({
            "type": "ai-title", "aiTitle": title, "sessionId": session_id,
        }));

        if !opts.dry_run {
            std::fs::create_dir_all(path.parent().unwrap())?;
            let body: Vec<String> = records.iter().map(|r| r.to_string()).collect();
            std::fs::write(&path, body.join("\n") + "\n")?;
            // CLI title sidecar (the resume picker reads this too)
            let sidecar = path.parent().unwrap().join(&session_id);
            std::fs::create_dir_all(&sidecar).ok();
            std::fs::write(
                sidecar.join("custom-title.json"),
                json!({"customTitle": title}).to_string(),
            )
            .ok();
        }
        let registered_desktop = if opts.dry_run
            || s.parent_session.is_some()
            || self.claude_home != crate::util::default_claude_home()
        {
            false
        } else {
            self.register_desktop_entry(&path, &session_id, &title, &cwd, s)
                .unwrap_or(false)
        };

        Ok(WriteOutcome {
            provider: "claude".into(),
            location: path.to_string_lossy().to_string(),
            native_id: session_id,
            extra: json!({"records": records.len(), "slug": slug, "dry_run": opts.dry_run,
                          "registered_in_desktop": registered_desktop}),
        })
    }
}

fn turns_count(s: &Session) -> i64 {
    s.events
        .iter()
        .filter(|e| {
            matches!(
                e.kind,
                EventKind::Message {
                    role: Role::User,
                    ..
                }
            )
        })
        .count() as i64
}

impl ClaudeProvider {
    /// The Claude Desktop app does not scan `~/.claude/projects` — it keeps its own
    /// registry under
    /// `%LOCALAPPDATA%/Packages/Claude_*/LocalCache/Roaming/Claude/claude-code-sessions/
    /// <workspace>/<instance>/local_<uuid>.json`, binding UI sessions to CLI session
    /// files via `cliSessionId`. Register the new session there so it shows up.
    fn register_desktop_entry(
        &self,
        _cli_path: &Path,
        session_id: &str,
        title: &str,
        cwd: &str,
        s: &Session,
    ) -> anyhow::Result<bool> {
        let mut registry_roots: Vec<std::path::PathBuf> = Vec::new();
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            let local = std::path::PathBuf::from(local);
            if let Ok(pkgs) = std::fs::read_dir(local.join("Packages")) {
                for p in pkgs.filter_map(|e| e.ok()) {
                    let name = p.file_name().to_string_lossy().to_string();
                    if name.starts_with("Claude") {
                        let root = p
                            .path()
                            .join("LocalCache")
                            .join("Roaming")
                            .join("Claude")
                            .join("claude-code-sessions");
                        if root.is_dir() {
                            registry_roots.push(root);
                        }
                    }
                }
            }
        }
        if let Some(appdata) = std::env::var_os("APPDATA") {
            let root = std::path::PathBuf::from(appdata)
                .join("Claude")
                .join("claude-code-sessions");
            if root.is_dir() {
                registry_roots.push(root);
            }
        }
        if registry_roots.is_empty() {
            return Ok(false);
        }

        // pick the instance dir holding the most registry entries; prefer one that
        // already contains sessions for this cwd
        let mut best: Option<(std::path::PathBuf, usize, bool)> = None;
        for root in &registry_roots {
            for ws in std::fs::read_dir(root)?.filter_map(|e| e.ok()) {
                let ws_path = ws.path();
                if !ws_path.is_dir() {
                    continue;
                }
                for inst in std::fs::read_dir(&ws_path)?.filter_map(|e| e.ok()) {
                    let inst_path = inst.path();
                    if !inst_path.is_dir() {
                        continue;
                    }
                    let mut count = 0usize;
                    let mut cwd_match = false;
                    for f in std::fs::read_dir(&inst_path)?.filter_map(|e| e.ok()) {
                        let fname = f.file_name().to_string_lossy().to_string();
                        if fname.starts_with("local_") && fname.ends_with(".json") {
                            count += 1;
                            if !cwd_match {
                                if let Ok(o) = serde_json::from_str::<Value>(
                                    &std::fs::read_to_string(f.path()).unwrap_or_default(),
                                ) {
                                    if o.get("cwd").and_then(|c| c.as_str()) == Some(cwd) {
                                        cwd_match = true;
                                    }
                                }
                            }
                        }
                    }
                    if count > 0 {
                        let better = match &best {
                            None => true,
                            Some((_, bc, bm)) => {
                                (cwd_match && !(*bm)) || (cwd_match == *bm && count > *bc)
                            }
                        };
                        if better {
                            best = Some((inst_path, count, cwd_match));
                        }
                    }
                }
            }
        }
        let Some(inst_dir) = best.map(|(p, _, _)| p) else {
            return Ok(false);
        };

        let local_uuid = crate::util::uuid7(s.created_ms, &format!("claude-desktop|{}", s.id));
        let entry = json!({
            "sessionId": format!("local_{local_uuid}"),
            "cliSessionId": session_id,
            "cwd": cwd,
            "originCwd": cwd,
            "lastFocusedAt": s.updated_ms,
            "createdAt": s.created_ms,
            "lastActivityAt": s.updated_ms,
            "model": "claude-opus-5-5",
            "effort": "xhigh",
            "effortInherited": true,
            "isArchived": false,
            "title": title,
            "titleSource": "auto",
            "permissionMode": "auto",
            "chromePermissionMode": "skip_all_permission_checks",
            "completedTurns": turns_count(s),
            "alwaysAllowedReasons": [],
            "sessionPermissionUpdates": [],
            "remoteMcpServersConfig": [],
        });
        std::fs::create_dir_all(&inst_dir)?;
        std::fs::write(
            inst_dir.join(format!("local_{local_uuid}.json")),
            entry.to_string(),
        )?;
        Ok(true)
    }
}
