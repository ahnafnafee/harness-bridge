//! Codex Desktop provider.
//!
//! Reads rollout JSONL from `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` and
//! writes desktop-shaped rollouts (session_meta + response_item + event_msg +
//! turn_context) plus the registrations the desktop app needs to list the
//! thread: `session_index.jsonl`, the `threads` table in `state_5.sqlite`
//! (its backfill is one-shot, so new files must be registered), and the
//! `dsh-imports.json` migration ledger.

use crate::ir::{Event, EventKind, Role, Session, SessionRef, WriteOpts, WriteOutcome};
use crate::util::{extended_path, iso_ms, read_maybe_zstd, uuid7};
use serde_json::{json, Map, Value};
use std::path::Path;

pub struct CodexProvider {
    pub codex_home: std::path::PathBuf,
}

fn parse_iso_ms(ts: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(ts).ok().map(|d| d.timestamp_millis())
}

fn content_texts(content: Option<&Value>) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(arr) = content.and_then(|c| c.as_array()) {
        for b in arr {
            if let Some(s) = b.get("text").and_then(|t| t.as_str()) {
                out.push(s.to_string());
            }
        }
    }
    out
}

fn join_text(parts: &[String]) -> String {
    parts.iter().filter(|p| !p.trim().is_empty()).cloned().collect::<Vec<_>>().join("\n\n")
}

impl CodexProvider {
    pub fn new(codex_home: std::path::PathBuf) -> Self {
        CodexProvider { codex_home }
    }

    /// Newest top-level Codex Desktop rollout: template for meta + turn_context.
    fn find_desktop_reference(&self) -> anyhow::Result<(Value, Value, i64)> {
        let mut files: Vec<_> = glob::walk(&self.codex_home)?;
        files.sort();
        files.reverse();
        for f in &files {
            let Ok(text) = read_maybe_zstd(f) else { continue };
            let Some(first) = text.lines().next() else { continue };
            let Ok(meta_line) = serde_json::from_str::<Value>(first) else { continue };
            let meta = meta_line.get("payload").cloned().unwrap_or(Value::Null);
            if meta.get("originator").and_then(|o| o.as_str()) == Some("Codex Desktop")
                && meta.get("thread_source").and_then(|o| o.as_str()) == Some("user")
                && meta.get("base_instructions").map(|b| b.is_object()).unwrap_or(false)
            {
                let mut turn_ctx = None;
                let mut ctx_window = 258400i64;
                for ln in text.lines().skip(1) {
                    let Ok(o) = serde_json::from_str::<Value>(ln) else { continue };
                    match o.get("type").and_then(|t| t.as_str()) {
                        Some("turn_context") if turn_ctx.is_none() => {
                            turn_ctx = Some(o.get("payload").cloned().unwrap_or(Value::Null));
                        }
                        Some("event_msg")
                            if o.pointer("/payload/type").and_then(|t| t.as_str()) == Some("task_started") =>
                        {
                            if let Some(w) = o.pointer("/payload/model_context_window").and_then(|w| w.as_i64()) {
                                ctx_window = w;
                            }
                        }
                        _ => {}
                    }
                    if turn_ctx.is_some() {
                        break;
                    }
                }
                let Some(turn_ctx) = turn_ctx else { continue };
                return Ok((meta, turn_ctx, ctx_window));
            }
        }
        anyhow::bail!("no existing Codex Desktop rollout found to use as a template; is the codex home populated?")
    }

    /// Register the thread so the desktop app lists it: session_index.jsonl,
    /// state_5.sqlite `threads`, and the import ledger.
    fn register(&self, rollout: &Path, thread_id: &str, name: &str, ledger_key: &str) -> anyhow::Result<()> {
        // --- session_index.jsonl ---
        let idx = self.codex_home.join("session_index.jsonl");
        if idx.exists() {
            let bak = idx.with_file_name("session_index.jsonl.bak-harness-bridge");
            if !bak.exists() {
                let _ = std::fs::copy(&idx, &bak);
            }
        }
        let lines_raw = std::fs::read_to_string(&idx).unwrap_or_default();
        let mut entries: Vec<String> = lines_raw
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter(|l| serde_json::from_str::<Value>(l).map(|e| e.get("id").and_then(|i| i.as_str()) != Some(thread_id)).unwrap_or(true))
            .map(str::to_string)
            .collect();
        let last_ts = {
            let text = read_maybe_zstd(rollout)?;
            text.lines().last().and_then(|l| serde_json::from_str::<Value>(l).ok())
                .and_then(|o| o.get("timestamp").and_then(|t| t.as_str()).map(str::to_string))
                .unwrap_or_else(|| iso_ms(chrono::Utc::now().timestamp_millis()))
        };
        entries.push(json!({
            "id": thread_id,
            "thread_name": name,
            "updated_at": last_ts,
        })
        .to_string());
        std::fs::write(&idx, entries.join("\n") + "\n")?;

        // --- state_5.sqlite threads ---
        let db = self.codex_home.join("state_5.sqlite");
        if db.exists() {
            let text = read_maybe_zstd(rollout)?;
            let mut lines = text.lines();
            let meta: Value = serde_json::from_str(lines.next().unwrap_or("{}"))?;
            let meta = meta.get("payload").cloned().unwrap_or(Value::Null);
            let first_ms = meta.get("timestamp").and_then(|t| t.as_str()).and_then(parse_iso_ms).unwrap_or(0);
            let last_ms = text
                .lines()
                .last()
                .and_then(|l| serde_json::from_str::<Value>(l).ok())
                .and_then(|o| o.get("timestamp").and_then(|t| t.as_str()).and_then(parse_iso_ms))
                .unwrap_or(first_ms);
            let mut first_user = String::new();
            let mut model = None;
            let mut effort = None;
            for ln in text.lines().skip(1) {
                let Ok(o) = serde_json::from_str::<Value>(ln) else { continue };
                if o.get("type").and_then(|t| t.as_str()) == Some("response_item") {
                    let p = o.get("payload").cloned().unwrap_or(Value::Null);
                    if p.get("type").and_then(|t| t.as_str()) == Some("message")
                        && p.get("role").and_then(|r| r.as_str()) == Some("user")
                        && first_user.is_empty()
                    {
                        first_user = join_text(&content_texts(p.get("content")));
                    }
                }
                if o.get("type").and_then(|t| t.as_str()) == Some("turn_context") && model.is_none() {
                    model = o.pointer("/payload/model").and_then(|m| m.as_str()).map(str::to_string);
                    effort = o.pointer("/payload/effort").and_then(|m| m.as_str()).map(str::to_string);
                }
            }
            let git = meta.get("git").cloned().unwrap_or(Value::Null);
            let creator = |k: &str| meta.get(k).and_then(|v| v.as_str()).map(str::to_string);
            let row: Map<String, Value> = [
                ("id", json!(thread_id)),
                ("rollout_path", json!(extended_path(&rollout.to_string_lossy()))),
                ("created_at", json!(first_ms / 1000)),
                ("created_at_ms", json!(first_ms)),
                ("updated_at", json!(last_ms / 1000)),
                ("updated_at_ms", json!(last_ms)),
                ("recency_at", json!(last_ms / 1000)),
                ("recency_at_ms", json!(last_ms)),
                ("source", json!("vscode")),
                ("model_provider", meta.get("model_provider").cloned().unwrap_or(json!("openai"))),
                ("originator", meta.get("originator").cloned().unwrap_or(json!("Codex Desktop"))),
                ("cwd", json!(extended_path(meta.get("cwd").and_then(|c| c.as_str()).unwrap_or_default()))),
                ("title", json!(first_user.chars().take(2000).collect::<String>())),
                ("first_user_message", json!(first_user.chars().take(2000).collect::<String>())),
                ("preview", json!(first_user.chars().take(500).collect::<String>())),
                ("name", json!(name)),
                ("sandbox_policy", json!("{\"type\":\"danger-full-access\"}")),
                ("approval_mode", json!("never")),
                ("tokens_used", json!(0)),
                ("has_user_event", json!(1)),
                ("archived", json!(0)),
                ("is_pinned", json!(0)),
                ("memory_mode", json!("enabled")),
                ("history_mode", json!("paginated")),
                ("cli_version", meta.get("cli_version").cloned().unwrap_or(json!(""))),
                ("model", model.map(Value::String).unwrap_or(Value::Null)),
                ("reasoning_effort", effort.map(Value::String).unwrap_or(Value::Null)),
                ("thread_source", json!("user")),
                ("git_sha", git.get("commit_hash").cloned().unwrap_or(Value::Null)),
                ("git_branch", git.get("branch").cloned().unwrap_or(Value::Null)),
                ("git_origin_url", git.get("repository_url").cloned().unwrap_or(Value::Null)),
                ("creator_user_id", creator("creator_user_id").map(Value::String).unwrap_or(Value::Null)),
                ("creator_account_id", creator("creator_account_id").map(Value::String).unwrap_or(Value::Null)),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
            let cols: Vec<&str> = row.keys().map(|s| s.as_str()).collect();
            let q = format!(
                "INSERT OR REPLACE INTO threads ({}) VALUES ({})",
                cols.join(","),
                vec!["?"; cols.len()].join(",")
            );
            fn to_sql(v: &serde_json::Value) -> rusqlite::types::Value {
                match v {
                    Value::Null => rusqlite::types::Value::Null,
                    Value::Bool(b) => rusqlite::types::Value::Integer(*b as i64),
                    Value::Number(n) => n.as_i64().map(rusqlite::types::Value::Integer).unwrap_or_else(|| {
                        rusqlite::types::Value::Real(n.as_f64().unwrap_or(0.0))
                    }),
                    Value::String(s) => rusqlite::types::Value::Text(s.clone()),
                    other => rusqlite::types::Value::Text(other.to_string()),
                }
            }
            let params: Vec<rusqlite::types::Value> = row.values().map(to_sql).collect();
            let con = rusqlite::Connection::open(&db)?;
            con.execute(&q, rusqlite::params_from_iter(params))?;
            con.close().ok();
        }

        // --- import ledger ---
        let mut ledger = super::load_codex_import_ledger(&self.codex_home);
        if let Some(obj) = ledger.as_object_mut() {
            obj.insert(
                ledger_key.to_string(),
                json!({
                    "thread_id": thread_id,
                    "name": name,
                    "rollout": rollout.to_string_lossy(),
                    "imported_at": iso_ms(chrono::Utc::now().timestamp_millis()),
                }),
            );
        }
        super::save_codex_import_ledger(&self.codex_home, &ledger)?;
        Ok(())
    }
}

/// Tiny recursive directory walk (glob crates not pulled in).
mod glob {
    use std::path::Path;

    pub fn walk(root: &Path) -> anyhow::Result<Vec<std::path::PathBuf>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(root)? {
            let p = entry?.path();
            if p.is_dir() {
                out.extend(walk(&p)?);
            } else if p.extension().map(|e| e == "jsonl").unwrap_or(false) {
                out.push(p);
            }
        }
        Ok(out)
    }

    /// Read only the first line (session_meta) without loading multi-MB files.
    /// Meta lines carry base_instructions (~25 KB), so read a generous prefix.
    pub fn read_first_line(path: &Path) -> Option<String> {
        use std::io::Read;
        let mut f = std::fs::File::open(path).ok()?;
        let mut buf = vec![0u8; 128 * 1024];
        let n = f.read(&mut buf).ok()?;
        String::from_utf8_lossy(&buf[..n]).lines().next().map(str::to_string)
    }
}

impl super::Provider for CodexProvider {
    fn name(&self) -> &'static str {
        "codex"
    }

    fn discover(&self) -> anyhow::Result<Vec<SessionRef>> {
        let mut refs = Vec::new();
        for f in glob::walk(&self.codex_home.join("sessions"))? {
            let name = f.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
            let id = name
                .trim_start_matches("rollout-")
                .trim_end_matches(".jsonl")
                .rsplit('-')
                .take(5)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("-");
            if id.len() != 36 || id.chars().filter(|c| *c == '-').count() != 4 {
                continue;
            }
            let meta_line = match glob::read_first_line(&f) {
                Some(l) => l,
                None => continue,
            };
            let Ok(meta_line) = serde_json::from_str::<Value>(&meta_line) else { continue };
            let meta = meta_line.get("payload").cloned().unwrap_or(Value::Null);
            if meta.get("cwd").is_none() {
                continue;
            }
            let updated = std::fs::metadata(&f).and_then(|m| m.modified()).ok();
            let updated_ms = updated
                .map(|t| t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0));
            refs.push(SessionRef {
                provider: "codex".into(),
                id: id.clone(),
                title: None,
                cwd: meta.get("cwd").and_then(|c| c.as_str()).map(str::to_string),
                created_ms: meta.get("timestamp").and_then(|t| t.as_str()).and_then(parse_iso_ms),
                updated_ms,
                locator: f.to_string_lossy().to_string(),
                migrated: false,
            });
        }
        Ok(refs)
    }

    fn read(&self, r: &SessionRef) -> anyhow::Result<Session> {
        let text = read_maybe_zstd(Path::new(&r.locator))?;
        let mut lines = text.lines();
        let meta_line: Value = serde_json::from_str(lines.next().ok_or_else(|| anyhow::anyhow!("empty rollout"))?)?;
        let meta = meta_line.get("payload").cloned().unwrap_or(Value::Null);
        let created_ms = meta.get("timestamp").and_then(|t| t.as_str()).and_then(parse_iso_ms).unwrap_or(0);
        let updated_ms = text
            .lines()
            .last()
            .and_then(|l| serde_json::from_str::<Value>(l).ok())
            .and_then(|o| o.get("timestamp").and_then(|t| t.as_str()).and_then(parse_iso_ms))
            .unwrap_or(created_ms);

        let mut evs: Vec<Event> = Vec::new();
        let mut result_idx: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        let mut saw_task_started = false;
        let mut turn_ctx_positions: Vec<i64> = Vec::new();

        for ln in lines {
            let Ok(o) = serde_json::from_str::<Value>(ln) else { continue };
            let ts = o.get("timestamp").and_then(|t| t.as_str()).and_then(parse_iso_ms);
            let typ = o.get("type").and_then(|t| t.as_str()).unwrap_or("").to_string();
            let p = o.get("payload").cloned().unwrap_or(Value::Null);
            match typ.as_str() {
                "response_item" => match p.get("type").and_then(|t| t.as_str()) {
                    Some("message") => {
                        let role = match p.get("role").and_then(|r| r.as_str()) {
                            Some("user") => Role::User,
                            Some("assistant") => Role::Assistant,
                            _ => Role::Developer,
                        };
                        let text = join_text(&content_texts(p.get("content")));
                        if !text.is_empty() {
                            evs.push(Event::at(ts, EventKind::Message {
                                role,
                                text,
                                source_kind: p.get("role").and_then(|r| r.as_str()).map(str::to_string),
                            }));
                        }
                    }
                    Some("reasoning") => {
                        let parts: Vec<String> = p
                            .get("summary")
                            .and_then(|s| s.as_array())
                            .into_iter()
                            .flatten()
                            .filter_map(|b| b.get("text").and_then(|t| t.as_str()).map(str::to_string))
                            .collect();
                        let text = join_text(&parts);
                        if !text.is_empty() {
                            evs.push(Event::at(ts, EventKind::Reasoning { text }));
                        }
                    }
                    Some("function_call") => {
                        if let Some(cid) = p.get("call_id").and_then(|c| c.as_str()) {
                            evs.push(Event::at(ts, EventKind::ToolCall {
                                call_id: cid.to_string(),
                                name: p.get("name").and_then(|n| n.as_str()).unwrap_or("unknown").to_string(),
                                arguments: p.get("arguments").and_then(|a| a.as_str()).unwrap_or("{}").to_string(),
                            }));
                        }
                    }
                    Some("function_call_output") => {
                        if let Some(cid) = p.get("call_id").and_then(|c| c.as_str()) {
                            let text = p.get("output").and_then(|o| o.as_str()).unwrap_or_default().to_string();
                            match result_idx.get(cid) {
                                Some(&i) => {
                                    if let EventKind::ToolResult { text: prev, .. } = &mut evs[i].kind {
                                        *prev = join_text(&[prev.clone(), text]);
                                    }
                                }
                                None => {
                                    evs.push(Event::at(ts, EventKind::ToolResult { call_id: cid.to_string(), text }));
                                    result_idx.insert(cid.to_string(), evs.len() - 1);
                                }
                            }
                        }
                    }
                    Some("compacted") => {
                        evs.push(Event::at(ts, EventKind::Compaction {
                            id: None,
                            text: p.get("message").and_then(|m| m.as_str()).map(str::to_string),
                        }));
                    }
                    _ => {}
                },
                "event_msg" => match p.get("type").and_then(|t| t.as_str()) {
                    Some("task_started") => {
                        saw_task_started = true;
                        evs.push(Event::at(ts, EventKind::TurnStart));
                    }
                    Some("task_complete") => evs.push(Event::at(ts, EventKind::TurnEnd { reason: None })),
                    _ => {}
                },
                "turn_context" => {
                    if let Some(t) = ts {
                        turn_ctx_positions.push(t);
                    }
                }
                _ => {}
            }
        }
        if !saw_task_started && !turn_ctx_positions.is_empty() {
            // old-format rollouts: derive turns from turn_context lines
            evs = turn_ctx_positions
                .into_iter()
                .map(|t| Event::at(Some(t), EventKind::TurnStart))
                .chain(evs)
                .collect();
            evs.sort_by_key(|e| e.time_ms.unwrap_or(0));
        }

        Ok(Session {
            source: "codex".into(),
            id: meta.get("id").and_then(|i| i.as_str()).unwrap_or(&r.id).to_string(),
            title: r.title.clone(),
            cwd: meta.get("cwd").and_then(|c| c.as_str()).map(str::to_string),
            agent_preset: None,
            parent_session: meta.get("parent_thread_id")
                .filter(|p| meta.get("id").and_then(|i| i.as_str()) != Some(p.as_str().unwrap_or("")))
                .and_then(|p| p.as_str())
                .map(str::to_string),
            origin: meta.get("originator").and_then(|o| o.as_str()).map(str::to_string),
            created_ms,
            updated_ms,
            events: evs,
        })
    }

    fn write(&self, s: &Session, opts: &WriteOpts) -> anyhow::Result<WriteOutcome> {
        let (ref_meta, ref_turn_ctx, ctx_window) = self.find_desktop_reference()?;
        let new_cwd = opts.cwd.clone().or_else(|| s.cwd.clone()).unwrap_or_default();

        // thread id: reuse a previous import of the same source session, else derive deterministically
        let ledger_key = if s.source == "dsh" { s.id.clone() } else { format!("{}:{}", s.source, s.id) };
        let ledger = super::load_codex_import_ledger(&self.codex_home);
        let prior = ledger.get(&ledger_key).and_then(|e| e.get("thread_id")).and_then(|t| t.as_str()).map(str::to_string);
        let prior_rollout = ledger.get(&ledger_key).and_then(|e| e.get("rollout")).and_then(|t| t.as_str()).map(str::to_string);
        let thread_id = match (&prior, &prior_rollout) {
            (Some(id), Some(p)) if Path::new(p).exists() => id.clone(),
            _ => uuid7(s.created_ms, &format!("harness-bridge/v1|{}|{}", s.source, s.id)),
        };
        let window_id = uuid7(s.created_ms + 1, &format!("harness-bridge-window|{thread_id}"));

        let name = opts.name.clone().or_else(|| s.title.clone()).unwrap_or_else(|| {
            s.first_user_text().map(|t| t.lines().next().unwrap_or_default().to_string()).unwrap_or_default()
        });

        let meta_payload = json!({
            "creator_user_id": ref_meta.get("creator_user_id").cloned().unwrap_or(Value::Null),
            "creator_account_id": ref_meta.get("creator_account_id").cloned().unwrap_or(Value::Null),
            "session_id": thread_id,
            "id": thread_id,
            "timestamp": iso_ms(s.created_ms),
            "cwd": new_cwd,
            "runtime_workspace_roots": [new_cwd],
            "originator": "Codex Desktop",
            "cli_version": ref_meta.get("cli_version").cloned().unwrap_or(json!("0.160.0")),
            "source": "vscode",
            "thread_source": "user",
            "model_provider": ref_meta.get("model_provider").cloned().unwrap_or(json!("openai")),
            "base_instructions": ref_meta.get("base_instructions").cloned().unwrap_or(Value::Null),
            "history_mode": "paginated",
            "context_window": {"window_id": window_id},
            "git": crate::providers::git_info(&new_cwd),
        });

        let mut out: Vec<Map<String, Value>> = Vec::new();
        macro_rules! emit {
            ($ms:expr, $typ:expr, $payload:expr) => {{
                let mut line = Map::new();
                line.insert("timestamp".into(), json!(iso_ms($ms)));
                line.insert("ordinal".into(), json!(out.len()));
                line.insert("type".into(), json!($typ));
                line.insert("payload".into(), $payload);
                out.push(line);
                out.len() - 1
            }};
        }

        emit!(s.created_ms, "session_meta", meta_payload.clone());

        // import note (dsh wording matches the original migration for diff parity)
        let note = if s.source == "dsh" {
            let parent_clause = s
                .parent_session
                .as_ref()
                .map(|p| format!(", seeded from parent session {p}"))
                .unwrap_or_default();
            format!(
                "<session-import>\n\
                 This thread was imported from a DeepSeek Harness desktop session.\n\
                 - Source session: {id} (\"{title}\"), agent preset {preset}, origin {origin}{parent_clause}.\n\
                 - Every message below originally ran with working directory {og}; file paths in the history refer to that directory and its subdirectories.\n\
                 - This thread's base directory is {nc}. Do not assume files exist relative to the base directory; the history operates in the original directories.\n\
                 </session-import>",
                id = s.id,
                title = s.title.as_deref().unwrap_or(""),
                preset = s.agent_preset.as_deref().unwrap_or("unknown"),
                origin = s.origin.as_deref().unwrap_or("unknown"),
                parent_clause = parent_clause,
                og = s.cwd.as_deref().unwrap_or("unknown"),
                nc = new_cwd,
            )
        } else {
            format!(
                "<session-import>\n\
                 This thread was imported from a {src} session (source session {id}).\n\
                 - Every message below originally ran with working directory {og}; file paths in the history refer to that directory and its subdirectories.\n\
                 - This thread's base directory is {nc}. Do not assume files exist relative to the base directory; the history operates in the original directories.\n\
                 </session-import>",
                src = s.source,
                id = s.id,
                og = s.cwd.as_deref().unwrap_or("unknown"),
                nc = new_cwd,
            )
        };
        emit!(s.created_ms, "response_item", json!({
            "type": "message",
            "id": format!("msg_{}", uuid7(s.created_ms + 2, &format!("import-note|{}", s.id))),
            "role": "developer",
            "content": [{"type": "input_text", "text": note}]
        }));

        // sessions without turn boundaries get one synthetic turn
        let has_turns = s.events.iter().any(|e| matches!(e.kind, EventKind::TurnStart));
        let mut events: Vec<Event> = Vec::new();
        if !has_turns && !s.events.is_empty() {
            events.push(Event::at(Some(s.created_ms), EventKind::TurnStart));
            events.extend(s.events.iter().cloned());
            events.push(Event::at(Some(s.updated_ms), EventKind::TurnEnd { reason: None }));
        } else {
            events = s.events.clone();
        }

        let mut cur_turn: Option<(String, Option<String>)> = None; // (turn_id, last assistant text)
        let mut turn_no: i64 = 0;
        let mut calls: std::collections::HashMap<String, (String, String, i64, Option<usize>)> =
            std::collections::HashMap::new(); // call_id -> (name, arguments, started_ms, ui_idx)
        let mut offset: i64 = 0;

        macro_rules! ui {
            ($ms:expr, $item:expr, $started:expr) => {{
                if let Some((tid, _)) = &cur_turn {
                    emit!($ms, "event_msg", json!({
                        "type": "item_completed",
                        "thread_id": thread_id,
                        "turn_id": tid,
                        "item": $item,
                        "started_at_ms": $started,
                        "completed_at_ms": $ms,
                    }))
                } else {
                    usize::MAX
                }
            }};
        }

        let close_turn = |out: &mut Vec<Map<String, Value>>, cur: &mut Option<(String, Option<String>)>, ms: i64| {
            if let Some((tid, last)) = cur.take() {
                let mut line = Map::new();
                line.insert("timestamp".into(), json!(iso_ms(ms)));
                line.insert("ordinal".into(), json!(out.len()));
                line.insert("type".into(), json!("event_msg"));
                line.insert(
                    "payload".into(),
                    json!({"type": "task_complete", "turn_id": tid, "last_agent_message": last}),
                );
                out.push(line);
            }
        };

        for e in &events {
            let ms = s.event_ms(e, offset);
            offset += 1;
            match &e.kind {
                EventKind::TurnStart => {
                    close_turn(&mut out, &mut cur_turn, ms);
                    turn_no += 1;
                    let tid = uuid7(ms, &format!("turn|{}|{}|{}", s.id, turn_no, ms));
                    cur_turn = Some((tid.clone(), None));
                    emit!(ms, "event_msg", json!({
                        "type": "task_started", "turn_id": tid, "root_turn_id": tid,
                        "started_at": ms / 1000, "model_context_window": ctx_window,
                        "collaboration_mode_kind": "default"
                    }));
                    let mut tc = ref_turn_ctx.clone();
                    tc["turn_id"] = json!(tid);
                    tc["root_turn_id"] = json!(tid);
                    tc["cwd"] = json!(new_cwd);
                    tc["current_date"] = json!(iso_ms(ms)[..10].to_string());
                    tc["timezone"] = json!(ref_turn_ctx.get("timezone").cloned().unwrap_or(json!("UTC")));
                    emit!(ms, "turn_context", tc);
                }
                EventKind::TurnEnd { reason: _ } => close_turn(&mut out, &mut cur_turn, ms),
                EventKind::Message { role, text, source_kind: _ } => {
                    let mid = format!("msg_{}", uuid7(ms, &format!("msg|{}|{}", s.id, offset)));
                    let codex_role = match role {
                        Role::User => "user",
                        Role::Assistant => "assistant",
                        Role::Developer => "developer",
                    };
                    let content_type = if *role == Role::Assistant { "output_text" } else { "input_text" };
                    emit!(ms, "response_item", json!({
                        "type": "message", "id": mid, "role": codex_role,
                        "content": [{"type": content_type, "text": text}]
                    }));
                    match role {
                        Role::User => {
                            let uid = uuid7(ms, &format!("ui|{}|{}", s.id, offset));
                            ui!(ms, json!({"type": "UserMessage", "id": uid, "client_id": uid,
                                           "content": [{"type": "text", "text": text}]}), ms);
                        }
                        Role::Assistant => {
                            if let Some((_, last)) = cur_turn.as_mut() {
                                *last = Some(text.clone());
                            }
                            ui!(ms, json!({"type": "AgentMessage", "id": mid,
                                           "content": [{"type": "Text", "text": text}], "phase": "final_answer"}), ms);
                        }
                        Role::Developer => {}
                    }
                }
                EventKind::Reasoning { text } => {
                    let rid = format!("rs_{}", uuid7(ms, &format!("rs|{}|{}", s.id, offset)));
                    emit!(ms, "response_item", json!({
                        "type": "reasoning", "id": rid,
                        "summary": [{"type": "summary_text", "text": text}]
                    }));
                    ui!(ms, json!({"type": "Reasoning", "id": rid, "summary_text": [text], "raw_content": []}), ms);
                }
                EventKind::ToolCall { call_id, name: tool, arguments } => {
                    emit!(ms, "response_item", json!({
                        "type": "function_call", "id": format!("fc_{}", uuid7(ms, &format!("fc|{}|{}", s.id, offset))),
                        "name": tool, "arguments": arguments, "call_id": call_id
                    }));
                    calls.insert(call_id.clone(), (tool.clone(), arguments.clone(), ms, None));
                }
                EventKind::ToolResult { call_id, text } => {
                    emit!(ms, "response_item", json!({
                        "type": "function_call_output", "id": format!("fco_{}", uuid7(ms, &format!("fco|{}|{}", s.id, offset))),
                        "call_id": call_id, "output": text
                    }));
                    if let Some((tool, arguments, started, ui_idx)) = calls.get_mut(call_id) {
                        let parsed: Option<Value> = serde_json::from_str(arguments).ok();
                        let args = match parsed {
                            Some(v) if v.is_object() => v,
                            Some(v) => json!({"arguments": v}),
                            None => json!({"raw": arguments}),
                        };
                        let item = json!({
                            "type": "McpToolCall", "id": call_id, "server": format!("{}-imported", s.source),
                            "tool": tool, "arguments": args, "status": "completed",
                            "result": {"content": [{"type": "text",
                                                    "text": if text.is_empty() { "(no output)".to_string() } else { text.clone() }}]}
                        });
                        if ui_idx.is_none() {
                            *ui_idx = Some(ui!(ms, item, *started));
                        } else if let Some(idx) = *ui_idx {
                            if idx != usize::MAX {
                                if let Some(line) = out.get_mut(idx) {
                                    if let Some(payload) = line.get_mut("payload") {
                                        payload["item"] = item;
                                        payload["completed_at_ms"] = json!(ms);
                                    }
                                }
                            }
                        }
                    }
                }
                EventKind::Compaction { id, text: _ } => {
                    let cid = id.clone().unwrap_or_else(|| uuid7(ms, &format!("cmp|{}|{}", s.id, offset)));
                    ui!(ms, json!({"type": "ContextCompaction", "id": cid}), ms);
                }
                EventKind::Meta { kind, data } => match kind.as_str() {
                    "tool-registry" => {
                        let mut parts: Vec<String> = Vec::new();
                        let added: Vec<String> = data.get("added").and_then(|a| a.as_array()).map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
                        let removed: Vec<String> = data.get("removed").and_then(|a| a.as_array()).map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
                        if !added.is_empty() {
                            parts.push(format!("Tools added: {}", added.join(", ")));
                        }
                        if !removed.is_empty() {
                            parts.push(format!("Tools removed: {}", removed.join(", ")));
                        }
                        if !parts.is_empty() {
                            emit!(ms, "response_item", json!({
                                "type": "message", "id": format!("msg_{}", uuid7(ms, &format!("reg|{}|{}", s.id, offset))),
                                "role": "developer",
                                "content": [{"type": "input_text", "text": format!("<tool-registry> {}</tool-registry>", parts.join("; "))}]
                            }));
                        }
                    }
                    "goal" => {
                        if let Some(obj) = data.get("objective").and_then(|o| o.as_str()) {
                            emit!(ms, "response_item", json!({
                                "type": "message", "id": format!("msg_{}", uuid7(ms, &format!("goal|{}|{}", s.id, offset))),
                                "role": "developer",
                                "content": [{"type": "input_text", "text": format!("<session-goal> {obj}</session-goal>")}]
                            }));
                        }
                    }
                    _ => {}
                },
            }
        }
        close_turn(&mut out, &mut cur_turn, s.updated_ms);

        let target_dir = self
            .codex_home
            .join("sessions")
            .join(&iso_ms(s.created_ms)[..4])
            .join(&iso_ms(s.created_ms)[5..7])
            .join(&iso_ms(s.created_ms)[8..10]);
        // codex file names carry second precision: 2026-10-04T06-14-39
        let stamp = iso_ms(s.created_ms)[..19].replace(':', "-");
        let rollout = target_dir.join(format!("rollout-{stamp}-{thread_id}.jsonl"));

        if !opts.dry_run {
            std::fs::create_dir_all(&target_dir)?;
            let body: Vec<String> = out
                .iter()
                .map(|m| Value::Object(m.clone()).to_string())
                .collect();
            std::fs::write(&rollout, body.join("\n") + "\n")?;
            self.register(&rollout, &thread_id, &name, &ledger_key)?;
        }

        Ok(WriteOutcome {
            provider: "codex".into(),
            location: rollout.to_string_lossy().to_string(),
            native_id: thread_id,
            extra: json!({"lines": out.len(), "dry_run": opts.dry_run}),
        })
    }
}
