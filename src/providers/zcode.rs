//! ZCode provider.
//!
//! Everything lives in one SQLite db (`~/.zcode/cli/db/db.sqlite`): a `session`
//! row plus `message` rows (role/time/model JSON) with `part` rows (text /
//! reasoning / tool state JSON) ordered by sequence.

use crate::ir::{Event, EventKind, Role, Session, SessionRef, WriteOpts, WriteOutcome};
use crate::util::{uuid7, zcode_project_id};
use rusqlite::Connection;
use serde_json::{json, Value};
mod context;

pub struct ZcodeProvider {
    pub db_path: std::path::PathBuf,
}

fn data_json(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or(Value::Null)
}

impl ZcodeProvider {
    fn has_parent_column(con: &Connection) -> anyhow::Result<bool> {
        let mut stmt = con.prepare("pragma table_info(session)")?;
        let names = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(names.iter().any(|name| name == "parent_id"))
    }
    pub fn new(db_path: std::path::PathBuf) -> Self {
        ZcodeProvider { db_path }
    }

    fn open_ro(&self) -> anyhow::Result<Connection> {
        Ok(Connection::open_with_flags(
            &self.db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?)
    }
}

impl super::Provider for ZcodeProvider {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn name(&self) -> &'static str {
        "zcode"
    }

    fn discover(&self) -> anyhow::Result<Vec<SessionRef>> {
        let con = self.open_ro()?;
        let mut stmt = con.prepare(
            "select id, title, directory, time_created, time_updated from session order by time_updated desc",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, title, directory, created, updated) = row?;
            out.push(SessionRef {
                provider: "zcode".into(),
                id: id.clone(),
                title: Some(title),
                cwd: Some(directory),
                created_ms: Some(created),
                updated_ms: Some(updated),
                locator: id,
                migrated: false,
            });
        }
        Ok(out)
    }

    fn read(&self, r: &SessionRef) -> anyhow::Result<Session> {
        let con = self.open_ro()?;
        let (directory, created, updated, title): (String, i64, i64, String) = con.query_row(
            "select directory, time_created, time_updated, title from session where id = ?1",
            [&r.id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;

        let mut evs: Vec<Event> = Vec::new();
        let mut records = Vec::new();
        let mut event_ranges = Vec::new();
        let mut stmt = con.prepare(
            "select m.data as mdata, p.data as pdata
             from part p
             join message m on m.id = p.message_id
             where p.session_id = ?1
             order by m.sequence, p.sequence",
        )?;
        let rows = stmt.query_map([&r.id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (mraw, praw) = row?;
            let m = data_json(&mraw);
            let p = data_json(&praw);
            let first_event = evs.len();
            let msg_role = m
                .get("role")
                .and_then(|r| r.as_str())
                .unwrap_or("assistant")
                .to_string();
            match p.get("type").and_then(|t| t.as_str()) {
                Some("text") => {
                    let text = p
                        .get("text")
                        .and_then(|t| t.as_str())
                        .unwrap_or_default()
                        .to_string();
                    if !text.trim().is_empty() {
                        let ts = p.pointer("/time/start").and_then(|t| t.as_i64());
                        evs.push(Event::at(
                            ts,
                            EventKind::Message {
                                role: if msg_role == "user" {
                                    Role::User
                                } else {
                                    Role::Assistant
                                },
                                text,
                                source_kind: m
                                    .pointer("/semantics/kind")
                                    .and_then(|k| k.as_str())
                                    .map(str::to_string),
                            },
                        ));
                    }
                }
                Some("reasoning") => {
                    let text = p
                        .get("text")
                        .and_then(|t| t.as_str())
                        .unwrap_or_default()
                        .to_string();
                    if !text.trim().is_empty() {
                        evs.push(Event::at(
                            p.pointer("/time/start").and_then(|t| t.as_i64()),
                            EventKind::Reasoning { text },
                        ));
                    }
                }
                Some("tool") => {
                    let call_id = p
                        .get("callID")
                        .and_then(|c| c.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let name = p
                        .get("tool")
                        .and_then(|t| t.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    let input = p.pointer("/state/input").cloned().unwrap_or(json!({}));
                    let ts = p.pointer("/state/time/start").and_then(|t| t.as_i64());
                    evs.push(Event::at(
                        ts,
                        EventKind::ToolCall {
                            call_id: call_id.clone(),
                            name,
                            arguments: serde_json::to_string(&input)?,
                        },
                    ));
                    let status = p
                        .pointer("/state/status")
                        .and_then(|s| s.as_str())
                        .unwrap_or("");
                    if matches!(status, "completed" | "error") {
                        let out = p.pointer("/state/output").cloned().unwrap_or(json!(""));
                        let text = match out {
                            Value::String(s) => s,
                            other => serde_json::to_string(&other)?,
                        };
                        evs.push(Event::at(
                            p.pointer("/state/time/end").and_then(|t| t.as_i64()),
                            EventKind::ToolResult { call_id, text },
                        ));
                    }
                }
                Some("compaction") => {
                    evs.push(Event::at(
                        None,
                        EventKind::Compaction {
                            id: p.get("id").and_then(|i| i.as_str()).map(str::to_string),
                            text: p
                                .get("summary")
                                .and_then(|s| s.as_str())
                                .map(str::to_string),
                        },
                    ));
                }
                _ => {} // step-start / step-finish / file / timeline
            }
            event_ranges.push(first_event..evs.len());
            records.push((m, p));
        }

        let resume_events = context::resume_events(&records, &event_ranges, &evs)?;
        let parent_session = if Self::has_parent_column(&con)? {
            con.query_row(
                "select parent_id from session where id=?1",
                [&r.id],
                |row| row.get::<_, Option<String>>(0),
            )?
        } else {
            con.query_row("select json_extract(data,'$.parent_session') from session_entry where session_id=?1 and type='migration/parent'",[&r.id],|row|row.get::<_,Option<String>>(0)).ok().flatten()
        };
        Ok(Session {
            source: "zcode".into(),
            id: r.id.clone(),
            title: Some(title),
            cwd: Some(directory),
            agent_preset: None,
            parent_session,
            origin: None,
            created_ms: created,
            updated_ms: updated,
            events: evs,
            resume_events,
            resume_context_unavailable: None,
        })
    }

    fn children(&self, parent: &SessionRef) -> anyhow::Result<Vec<SessionRef>> {
        let con = self.open_ro()?;
        let sql = if Self::has_parent_column(&con)? {
            "select id from session where parent_id=?1"
        } else {
            "select session_id from session_entry where type='migration/parent' and json_extract(data,'$.parent_session')=?1"
        };
        let mut stmt = con.prepare(sql)?;
        let ids = stmt
            .query_map([&parent.id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<std::collections::HashSet<_>>>()?;
        Ok(self
            .discover()?
            .into_iter()
            .filter(|r| ids.contains(&r.id))
            .collect())
    }

    fn write(&self, s: &Session, opts: &WriteOpts) -> anyhow::Result<WriteOutcome> {
        // Do this in dry runs too so a malformed child cannot fail only after
        // the preceding members of its family have already been written.
        for events in std::iter::once(&s.events).chain(s.resume_events.iter()) {
            let mut calls = std::collections::HashSet::new();
            for event in events {
                match &event.kind {
                    EventKind::ToolCall { call_id, .. } => { calls.insert(call_id); },
                    EventKind::ToolResult { call_id, .. } => anyhow::ensure!(calls.contains(call_id),
                        "ZCode tool result {call_id:?} has no matching tool call; no destination was written"),
                    _ => {},
                }
            }
        }
        if opts.dry_run {
            let con = self.open_ro()?;
            for table in ["session", "message", "part", "session_entry"] {
                con.prepare(&format!("select * from {table} limit 0"))?;
            }
        }
        let cwd = opts
            .cwd
            .clone()
            .or_else(|| s.cwd.clone())
            .unwrap_or_default();
        let session_id = format!(
            "sess_{}",
            uuid7(
                s.created_ms,
                &format!("harness-bridge/v1|{}|{}", s.source, s.id)
            )
        );
        let title = opts
            .name
            .clone()
            .or_else(|| s.title.clone())
            .unwrap_or_else(|| {
                s.first_user_text()
                    .map(|t| t.lines().next().unwrap_or_default().to_string())
                    .unwrap_or_default()
            });

        if !opts.dry_run {
            let mut database = Connection::open(&self.db_path)?;
            database.busy_timeout(std::time::Duration::from_secs(10))?;
            let con = database.transaction()?;
            // clear any previous import of this session so re-runs never leave stale rows
            con.execute(
                "delete from part where session_id = ?1",
                rusqlite::params![session_id],
            )?;
            con.execute(
                "delete from message where session_id = ?1",
                rusqlite::params![session_id],
            )?;
            con.execute(
                "insert or replace into session (id, project_id, slug, directory, path, title, version, permission,
                    time_created, time_updated, task_type, title_source)
                 values (?1, ?2, ?1, ?3, ?3, ?4, '0.16.9', '{\"mode\":\"yolo\"}', ?5, ?6, 'interactive', 'first_input')",
                rusqlite::params![
                    session_id,
                    zcode_project_id(&cwd),
                    cwd,
                    title,
                    s.created_ms,
                    s.updated_ms,
                ],
            )?;
            // runtime entries native sessions carry (execution state + model selection)
            con.execute(
                "insert or replace into session_entry (id, session_id, type, time_created, time_updated, data)
                 values (?1, ?2, 'runtime/execution_state', ?3, ?3, ?4)",
                rusqlite::params![
                    format!("{session_id}:runtime-execution-state"),
                    session_id,
                    s.updated_ms,
                    json!({"mode": "yolo", "planEnabled": false}).to_string()
                ],
            )?;

            // semantics native rows carry; the desktop renderer filters on uiVisibility
            if Self::has_parent_column(&con)? {
                con.execute(
                    "update session set parent_id=?1 where id=?2",
                    rusqlite::params![s.parent_session, session_id],
                )?;
            }
            con.execute(
                "delete from session_entry where session_id=?1 and type='migration/parent'",
                [&session_id],
            )?;
            if let Some(parent) = &s.parent_session {
                con.execute("insert into session_entry(id,session_id,type,time_created,time_updated,data) values(?1,?2,'migration/parent',?3,?3,?4)",
                    rusqlite::params![format!("{session_id}:migration-parent"),session_id,s.updated_ms,json!({"parent_session":parent}).to_string()])?;
            }
            let sem_user = json!({"origin": "real_user", "kind": "user_prompt",
                                  "uiVisibility": "visible", "providerVisibility": "visible",
                                  "transcriptVisibility": "visible"});
            let sem_asst = json!({"origin": "agent_runtime", "kind": "assistant_response",
                                  "uiVisibility": "visible", "providerVisibility": "visible",
                                  "transcriptVisibility": "visible"});
            let sem_timeline = json!({"origin": "system", "kind": "timeline_event",
                                      "uiVisibility": "visible", "providerVisibility": "hidden",
                                      "transcriptVisibility": "visible"});

            let mut msg_seq: i64 = 0;
            let mut part_seq: i64 = 0;
            let mut offset: i64 = 0;
            let insert_message = |con: &Connection,
                                  msg_seq: &mut i64,
                                  part_seq: &mut i64,
                                  role: &str,
                                  semantics: Value,
                                  ts: i64,
                                  parts: Vec<Value>|
             -> anyhow::Result<()> {
                let msg_id = format!(
                    "msg_{}_{}",
                    ts,
                    uuid7(ts, &format!("zcode-msg|{}|{}", s.id, *msg_seq))
                );
                let mut data = json!({
                    "role": role,
                    "time": {"created": ts, "completed": ts},
                    "agent": "zcode-agent",
                    "semantics": semantics,
                    "path": {"cwd": cwd, "root": cwd},
                    "mode": "yolo",
                    "planEnabled": false,
                });
                if role == "assistant" {
                    data["cost"] = json!(0);
                    data["tokens"] = json!({"input": 0, "output": 0, "reasoning": 0,
                                            "cache": {"read": 0, "write": 0}});
                }
                con.execute(
                    "insert or replace into message (id, session_id, time_created, time_updated, data, sequence)
                     values (?1, ?2, ?3, ?3, ?4, ?5)",
                    rusqlite::params![msg_id, session_id, ts, data.to_string(), *msg_seq],
                )?;
                *msg_seq += 1;
                for part in parts {
                    let part_id = format!(
                        "part_{}_{}",
                        ts,
                        uuid7(ts, &format!("zcode-part|{}|{}", s.id, *part_seq))
                    );
                    con.execute(
                        "insert or replace into part (id, message_id, session_id, time_created, time_updated, data, sequence)
                         values (?1, ?2, ?3, ?4, ?4, ?5, ?6)",
                        rusqlite::params![part_id, msg_id, session_id, ts, part.to_string(), *part_seq],
                    )?;
                    *part_seq += 1;
                }
                Ok(())
            };

            let write_events: Vec<&Event> = s
                .events
                .iter()
                .chain(s.resume_events.iter().flatten())
                .collect();
            for (index, e) in write_events.into_iter().enumerate() {
                let archival = s.resume_events.is_some() && index < s.events.len();
                let mut sem_user = sem_user.clone();
                let mut sem_asst = sem_asst.clone();
                if archival {
                    sem_user["providerVisibility"] = json!("hidden");
                    sem_asst["providerVisibility"] = json!("hidden");
                }
                if index == s.events.len() && s.resume_events.is_some() {
                    insert_message(
                        &con,
                        &mut msg_seq,
                        &mut part_seq,
                        "assistant",
                        sem_timeline.clone(),
                        s.updated_ms,
                        vec![json!({"type":"compaction", "harnessBridgeResume":true,
                            "summary":"The retained migration context follows; earlier entries are archival."})],
                    )?;
                }
                let ts = s.event_ms(e, offset);
                offset += 1;
                match &e.kind {
                    EventKind::Message { role, text, .. } => {
                        let (zrole, semantics) = match role {
                            Role::User => ("user", sem_user.clone()),
                            Role::Assistant => ("assistant", sem_asst.clone()),
                            // harness-context notes render as system timeline events
                            Role::Developer => ("assistant", sem_timeline.clone()),
                        };
                        let part =
                            json!({"type": "text", "text": text, "time": {"start": ts, "end": ts}});
                        insert_message(
                            &con,
                            &mut msg_seq,
                            &mut part_seq,
                            zrole,
                            semantics,
                            ts,
                            vec![part],
                        )?;
                    }
                    EventKind::Reasoning { text } => {
                        insert_message(
                            &con,
                            &mut msg_seq,
                            &mut part_seq,
                            "assistant",
                            sem_asst.clone(),
                            ts,
                            vec![json!({
                                "type": "reasoning", "text": text, "time": {"start": ts, "end": ts}
                            })],
                        )?;
                    }
                    EventKind::ToolCall {
                        call_id,
                        name,
                        arguments,
                    } => {
                        let input: Value = serde_json::from_str(arguments)
                            .unwrap_or_else(|_| json!({"raw": arguments}));
                        insert_message(
                            &con,
                            &mut msg_seq,
                            &mut part_seq,
                            "assistant",
                            sem_asst.clone(),
                            ts,
                            vec![json!({
                                "type": "tool", "callID": call_id, "tool": name,
                                "state": {"status": "running", "input": input, "time": {"start": ts}}
                            })],
                        )?;
                    }
                    EventKind::ToolResult { call_id, text } => {
                        // attach the result to the pending tool part
                        let pending = con.query_row(
                            "select id, data from part where session_id = ?1 and json_extract(data, '$.type') = 'tool' and json_extract(data, '$.callID') = ?2 order by sequence desc limit 1",
                            rusqlite::params![session_id, call_id],
                            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                        );
                        if let Ok((part_id, raw)) = pending {
                            let mut p = data_json(&raw);
                            p["state"]["status"] = json!("completed");
                            p["state"]["output"] = json!(text);
                            p["state"]["time"]["end"] = json!(ts);
                            con.execute(
                                "update part set data = ?1, time_updated = ?2 where id = ?3",
                                rusqlite::params![p.to_string(), ts, part_id],
                            )?;
                        } else {
                            anyhow::bail!("ZCode tool result {call_id:?} has no matching tool part; import rolled back");
                        }
                    }
                    EventKind::Compaction { id, text } => {
                        insert_message(
                            &con,
                            &mut msg_seq,
                            &mut part_seq,
                            "assistant",
                            sem_timeline.clone(),
                            ts,
                            vec![json!({
                                "type": "compaction", "id": id, "summary": text
                            })],
                        )?;
                    }
                    EventKind::TurnStart | EventKind::TurnEnd { .. } | EventKind::Meta { .. } => {}
                }
            }
            con.commit()?;
        }

        Ok(WriteOutcome {
            provider: "zcode".into(),
            location: format!("{}#{}", self.db_path.display(), session_id),
            native_id: session_id,
            extra: json!({"title": title, "cwd": cwd, "dry_run": opts.dry_run}),
        })
    }
}
