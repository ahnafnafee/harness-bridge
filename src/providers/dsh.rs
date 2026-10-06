//! DeepSeek Harness (dsh) provider.
//!
//! Reads `~/.dsh/sessions/<project>/<uuid>/session.v4.jsonl[.zstd]` event
//! transcripts and writes the same format back (plain JSONL; dsh stores them
//! zstd-compressed but the event schema is identical).

use crate::ir::{EventKind, Role, Session, SessionRef, WriteOpts, WriteOutcome};
use crate::util::{read_maybe_zstd, uuid7};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

mod context;

use context::{decode_events, replay_surface};

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
            let p = self
                .dsh_home
                .join("storages")
                .join("session_projcache")
                .join("sessions")
                .join(&name);
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

    /// Port a dsh conversation to a different agent preset: copy the raw
    /// transcript (full event fidelity — telemetry, seeds, everything), rewrite
    /// the header's id + agentPreset, and clone the session's projcache entry
    /// with the preset row patched so the desktop list shows it under the
    /// target preset. Idempotent: same source + preset -> same new session id.
    pub fn port_preset(
        &self,
        src: &SessionRef,
        target_preset: &str,
        opts: &WriteOpts,
    ) -> anyhow::Result<WriteOutcome> {
        if !self
            .dsh_home
            .join(".agent-presets")
            .join(target_preset)
            .is_dir()
        {
            anyhow::bail!(
                "preset '{target_preset}' is not installed as a directory preset under {}. \
                 Directory presets are what the dsh CLI reads; install it (or run its sync.mjs for the desktop app) first.",
                self.dsh_home.join(".agent-presets").display()
            );
        }
        let text = read_maybe_zstd(Path::new(&src.locator))?;
        let mut lines = text.lines();
        let mut header: Value = serde_json::from_str(
            lines
                .next()
                .ok_or_else(|| anyhow::anyhow!("empty dsh transcript"))?,
        )?;
        if header.get("type").and_then(|t| t.as_str()) != Some("session") {
            anyhow::bail!("unexpected dsh transcript header");
        }
        let created_ms = header
            .get("createdAt")
            .and_then(|t| t.as_i64())
            .unwrap_or(0);
        let body = lines.collect::<Vec<_>>();

        // deterministic per (source session, target preset)
        let new_id = uuid7(
            created_ms,
            &format!("dsh-port|{}|{}", src.id, target_preset),
        );

        header["id"] = json!(new_id);
        header["agentPreset"] = json!(target_preset);
        let mut out = vec![header.to_string()];
        out.extend(body.iter().map(|s| s.to_string()));

        if !opts.dry_run {
            // transcript: same project dir as the source, compressed like dsh writes
            // locator = <home>/sessions/<project-slug>/<session-id>/session.v4.jsonl[.zstd]
            let project_dir = Path::new(&src.locator)
                .parent()
                .and_then(|p| p.parent())
                .ok_or_else(|| anyhow::anyhow!("bad locator"))?;
            let dst_dir = project_dir.join(&new_id);
            std::fs::create_dir_all(&dst_dir)?;
            let dst = dst_dir.join("session.v4.jsonl.zstd");
            let compressed = zstd::stream::encode_all(out.join("\n").as_bytes(), 3)?;
            std::fs::write(&dst, &compressed)?;

            // projcache: clone the source's entry with the preset row patched
            let proj_dir = self
                .dsh_home
                .join("storages")
                .join("session_projcache")
                .join("sessions");
            for name in [
                format!("{}.json", src.id),
                format!("session-{}.json", src.id),
            ] {
                let p = proj_dir.join(&name);
                if let Ok(txt) = std::fs::read_to_string(&p) {
                    if let Ok(mut v) = serde_json::from_str::<Value>(&txt) {
                        if let Some(val) = v.pointer_mut("/record/rows/agentPreset/val") {
                            *val = json!(target_preset);
                        }
                        if let Some(name) = &opts.name {
                            if let Some(val) = v.pointer_mut("/record/rows/title/val") {
                                *val = json!(name);
                            }
                        }
                        let dst_proj = proj_dir.join(format!("{new_id}.json"));
                        std::fs::write(&dst_proj, serde_json::to_string_pretty(&v)?)?;
                        break;
                    }
                }
            }

            // desktop wrapper: the desktop app's sidebar lists only `session-<uuid>`
            // registry sessions (workspace.json tables.workspaces[].sessionIds + a
            // prefixed projcache + a transcript dir of the same name). Create all
            // three so the port shows up without hand-editing. The wrapper's
            // transcript header is rewritten to native desktop shape (top-level,
            // no parent linkage).
            let desktop_uuid = uuid7(created_ms, &format!("dsh-port-desktop|{}|{}", src.id, target_preset));
            let desktop_id = format!("session-{desktop_uuid}");
            let mut desktop_header = header.clone();
            desktop_header["id"] = json!(desktop_id);
            desktop_header["isSeeded"] = json!(false);
            desktop_header["delegationDepth"] = json!(0);
            if let Some(obj) = desktop_header.as_object_mut() {
                obj.remove("parentSession");
                obj.remove("origin");
            }
            let mut desktop_out = vec![desktop_header.to_string()];
            desktop_out.extend(out.iter().skip(1).cloned());
            let dst_desktop_dir = project_dir.join(&desktop_id);
            std::fs::create_dir_all(&dst_desktop_dir)?;
            let desktop_compressed = zstd::stream::encode_all(desktop_out.join("\n").as_bytes(), 3)?;
            std::fs::write(dst_desktop_dir.join("session.v4.jsonl.zstd"), &desktop_compressed)?;
            for name in [
                format!("{}.json", src.id),
                format!("session-{}.json", src.id),
            ] {
                let p = proj_dir.join(&name);
                if let Ok(txt) = std::fs::read_to_string(&p) {
                    if let Ok(mut v) = serde_json::from_str::<Value>(&txt) {
                        if let Some(val) = v.pointer_mut("/record/rows/agentPreset/val") {
                            *val = json!(target_preset);
                        }
                        if let Some(name) = &opts.name {
                            if let Some(val) = v.pointer_mut("/record/rows/title/val") {
                                *val = json!(name);
                            }
                        }
                        // surface the port at the top of the recency-sorted sidebar:
                        // its original lastPromptAt is days old and buries it
                        if let Some(val) = v.pointer_mut("/record/rows/sessionListMetadata/val") {
                            val["lastPromptAt"] = json!(std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as i64)
                                .unwrap_or(0));
                        }
                        // subagent-born transcripts carry isSeeded/inheritedEventCount;
                        // the desktop hides subagent-classified sessions from the sidebar,
                        // so the wrapper must present as top-level
                        if let Some(identity) = v.pointer_mut("/record/identity") {
                            identity["isSeeded"] = json!(false);
                            identity["inheritedEventCount"] = json!(0);
                        }
                        let dst_proj = proj_dir.join(format!("{desktop_id}.json"));
                        std::fs::write(&dst_proj, serde_json::to_string_pretty(&v)?)?;
                        break;
                    }
                }
            }
            let ws_path = self.dsh_home.join("storages").join("workspace.json");
            let mut registered_ws = false;
            if let Ok(txt) = std::fs::read_to_string(&ws_path) {
                if let Ok(mut ws) = serde_json::from_str::<Value>(&txt) {
                    let cwd_str = header.get("cwd").and_then(|c| c.as_str()).unwrap_or_default();
                    let ws_list = ws.pointer_mut("/tables/workspaces").and_then(|w| w.as_object_mut());
                    if let Some(workspaces) = ws_list {
                        let entry = workspaces
                            .values_mut()
                            .find(|v| v.get("path").and_then(|p| p.as_str()) == Some(cwd_str));
                        if let Some(entry) = entry {
                            if let Some(ids) = entry.get_mut("sessionIds").and_then(|s| s.as_array_mut()) {
                                if !ids.iter().any(|i| i.as_str() == Some(desktop_id.as_str())) {
                                    ids.push(json!(desktop_id));
                                }
                                registered_ws = true;
                            }
                        }
                    }
                    if registered_ws {
                        let bak = ws_path.with_extension("json.bak-harness-bridge");
                        if !bak.exists() {
                            let _ = std::fs::copy(&ws_path, &bak);
                        }
                        let _ = std::fs::write(&ws_path, serde_json::to_string(&ws)?);
                    }
                }
            }

            Ok(WriteOutcome {
                provider: "dsh".into(),
                location: dst.to_string_lossy().to_string(),
                native_id: new_id,
                extra: json!({
                    "preset": target_preset,
                    "events": out.len(),
                    "desktop_session_id": desktop_id,
                    "registered_in_desktop": registered_ws,
                    "note": "raw transcript copy with header agentPreset rewritten; projcache cloned; desktop wrapper registered in workspace.json (restart the dsh desktop to see it)",
                }),
            })
        } else {
            Ok(WriteOutcome {
                provider: "dsh".into(),
                location: "(dry run)".into(),
                native_id: new_id,
                extra: json!({"preset": target_preset, "events": out.len(), "dry_run": true}),
            })
        }
    }
}

impl super::Provider for DshProvider {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

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
                let transcript = [
                    "session.v4.jsonl.zstd",
                    "session.v4.jsonl",
                    "session.v3.jsonl.zstd",
                    "session.v3.jsonl",
                    "session.jsonl",
                ]
                .iter()
                .map(|f| s.path().join(f))
                .find(|p| p.exists());
                let Some(transcript) = transcript else {
                    continue;
                };
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
            lines
                .next()
                .ok_or_else(|| anyhow::anyhow!("empty dsh transcript"))?,
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

        let (evs, title) = decode_events(&events);
        let resume_events = replay_surface(&events)?.map(|surface| decode_events(&surface).0);

        Ok(Session {
            source: "dsh".into(),
            id: r.id.clone(),
            title: title.or_else(|| r.title.clone()),
            cwd: header
                .get("cwd")
                .and_then(|c| c.as_str())
                .map(str::to_string),
            agent_preset: header
                .get("agentPreset")
                .and_then(|c| c.as_str())
                .map(str::to_string),
            parent_session: header
                .get("parentSession")
                .and_then(|c| c.as_str())
                .map(str::to_string),
            origin: header
                .get("origin")
                .and_then(|c| c.as_str())
                .map(str::to_string),
            created_ms,
            updated_ms,
            events: evs,
            resume_events,
        })
    }

    fn write(&self, s: &Session, opts: &WriteOpts) -> anyhow::Result<WriteOutcome> {
        let cwd = opts
            .cwd
            .clone()
            .or_else(|| s.cwd.clone())
            .unwrap_or_default();
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
                    push!(
                        time,
                        json!({"type": "turn/end", "data": {"turn": turn, "reason": {"kind": reason.clone().unwrap_or_else(|| "end".into())}}})
                    );
                }
                EventKind::Message {
                    role,
                    text,
                    source_kind,
                } => match role {
                    Role::User => {
                        push!(
                            time,
                            json!({
                                "type": "user/message",
                                "data": {
                                    "content": [{"type": "text", "text": text}],
                                    "source": {"kind": source_kind.clone().unwrap_or_else(|| "user".into())},
                                    "role": "user",
                                    "id": uuid7(time, &format!("dsh-user|{seq}")),
                                },
                                "surfaceOp": "append"
                            })
                        );
                    }
                    Role::Developer => {
                        if source_kind.as_deref() == Some("system-prompt") {
                            push!(
                                time,
                                json!({
                                    "type": "system/message",
                                    "data": {"turn": turn, "step": 1,
                                             "message": {"role": "system", "content": [{"type": "text", "text": text}]}}
                                })
                            );
                        } else {
                            push!(
                                time,
                                json!({
                                    "type": "developer/message",
                                    "data": {"turn": turn, "step": 1,
                                             "message": {"role": "developer", "source": {"kind": source_kind.clone().unwrap_or_else(|| "import".into())},
                                                         "content": [{"type": "text", "text": text}]}}
                                })
                            );
                        }
                    }
                    Role::Assistant => {
                        let mut content = Vec::new();
                        if let Some((_, r)) = pending_reasoning.take() {
                            content.push(json!({"type": "reasoning", "text": r}));
                        }
                        content.push(json!({"type": "text", "text": text}));
                        push!(
                            time,
                            json!({
                                "type": "assistant/message",
                                "data": {"turn": turn, "step": 1,
                                         "message": {"role": "assistant", "source": {"kind": "model"}, "content": content}}
                            })
                        );
                    }
                },
                EventKind::Reasoning { text } => {
                    pending_reasoning = Some((time, text.clone()));
                }
                EventKind::ToolCall {
                    call_id,
                    name,
                    arguments,
                } => {
                    push!(
                        time,
                        json!({
                            "type": "tool/call",
                            "data": {"turn": turn, "step": 1, "callId": call_id, "name": name, "arguments": arguments}
                        })
                    );
                }
                EventKind::ToolResult { call_id, text } => {
                    push!(
                        time,
                        json!({
                            "type": "tool/result",
                            "data": {"turn": turn, "step": 1,
                                     "message": {"role": "tool", "source": {"kind": "tool", "callId": call_id},
                                                 "toolCallId": call_id, "content": [{"type": "text", "text": text}]}}
                        })
                    );
                }
                EventKind::Compaction { id, text } => {
                    let cid = id
                        .clone()
                        .unwrap_or_else(|| uuid7(time, &format!("dsh-cmp|{seq}")));
                    push!(
                        time,
                        json!({"type": "compaction/start", "data": {"compactionId": cid, "turn": turn}})
                    );
                    push!(
                        time,
                        json!({"type": "compaction/summary", "data": {"compactionId": cid, "summary": [{"type": "text", "text": text.clone().unwrap_or_default()}]}})
                    );
                    push!(
                        time,
                        json!({"type": "compaction/end", "data": {"compactionId": cid, "turn": turn}})
                    );
                }
                EventKind::Meta { kind, data } => match kind.as_str() {
                    "tool-registry" => {
                        let mut content = Vec::new();
                        for n in data
                            .get("added")
                            .and_then(|a| a.as_array())
                            .into_iter()
                            .flatten()
                        {
                            content.push(json!({"type": "tool-addition", "toolName": n}));
                        }
                        for n in data
                            .get("removed")
                            .and_then(|a| a.as_array())
                            .into_iter()
                            .flatten()
                        {
                            content.push(json!({"type": "tool-removal", "toolName": n}));
                        }
                        push!(
                            time,
                            json!({
                                "type": "developer/message",
                                "data": {"turn": turn, "step": 1, "headerSeq": seq,
                                         "message": {"role": "developer", "source": {"kind": "tool-registry"}, "content": content}}
                            })
                        );
                    }
                    "goal" => {
                        push!(
                            time,
                            json!({
                                "type": "goal/change",
                                "data": {"kind": "goal/change", "version": 1, "operation": "create",
                                         "goal": {"id": uuid7(time, "dsh-goal"), "revision": 1,
                                                  "objective": data.get("objective").cloned().unwrap_or(Value::Null),
                                                  "phase": "active", "maxGoalRounds": 40}}
                            })
                        );
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
