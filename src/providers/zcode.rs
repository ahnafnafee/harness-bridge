//! ZCode provider.
//!
//! Everything lives in one SQLite db (`~/.zcode/cli/db/db.sqlite`): a `session`
//! row plus `message` rows (role/time/model JSON) with `part` rows (text /
//! reasoning / tool state JSON) ordered by sequence.

use crate::ir::{Event, EventKind, Role, Session, SessionRef, WriteOpts, WriteOutcome};
use crate::util::{uuid7, zcode_project_id};
use rusqlite::Connection;
use serde_json::{json, Value};

pub struct ZcodeProvider {
    pub db_path: std::path::PathBuf,
}

fn data_json(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or(Value::Null)
}

impl ZcodeProvider {
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
            let msg_role = m.get("role").and_then(|r| r.as_str()).unwrap_or("assistant").to_string();
            match p.get("type").and_then(|t| t.as_str()) {
                Some("text") => {
                    let text = p.get("text").and_then(|t| t.as_str()).unwrap_or_default().to_string();
                    if !text.trim().is_empty() {
                        let ts = p.pointer("/time/start").and_then(|t| t.as_i64());
                        evs.push(Event::at(ts, EventKind::Message {
                            role: if msg_role == "user" { Role::User } else { Role::Assistant },
                            text,
                            source_kind: m.pointer("/semantics/kind").and_then(|k| k.as_str()).map(str::to_string),
                        }));
                    }
                }
                Some("reasoning") => {
                    let text = p.get("text").and_then(|t| t.as_str()).unwrap_or_default().to_string();
                    if !text.trim().is_empty() {
                        evs.push(Event::at(
                            p.pointer("/time/start").and_then(|t| t.as_i64()),
                            EventKind::Reasoning { text },
                        ));
                    }
                }
                Some("tool") => {
                    let call_id = p.get("callID").and_then(|c| c.as_str()).unwrap_or_default().to_string();
                    let name = p.get("tool").and_then(|t| t.as_str()).unwrap_or("unknown").to_string();
                    let input = p.pointer("/state/input").cloned().unwrap_or(json!({}));
                    let ts = p.pointer("/state/time/start").and_then(|t| t.as_i64());
                    evs.push(Event::at(ts, EventKind::ToolCall {
                        call_id: call_id.clone(),
                        name,
                        arguments: serde_json::to_string(&input)?,
                    }));
                    let status = p.pointer("/state/status").and_then(|s| s.as_str()).unwrap_or("");
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
                    evs.push(Event::at(None, EventKind::Compaction {
                        id: p.get("id").and_then(|i| i.as_str()).map(str::to_string),
                        text: p.get("summary").and_then(|s| s.as_str()).map(str::to_string),
                    }));
                }
                _ => {} // step-start / step-finish / file / timeline
            }
        }

        Ok(Session {
            source: "zcode".into(),
            id: r.id.clone(),
            title: Some(title),
            cwd: Some(directory),
            agent_preset: None,
            parent_session: None,
            origin: None,
            created_ms: created,
            updated_ms: updated,
            events: evs,
        })
    }

    fn write(&self, s: &Session, opts: &WriteOpts) -> anyhow::Result<WriteOutcome> {
        let cwd = opts.cwd.clone().or_else(|| s.cwd.clone()).unwrap_or_default();
        let session_id = format!("sess_{}", uuid7(s.created_ms, &format!("harness-bridge/v1|{}|{}", s.source, s.id)));
        let title = opts
            .name
            .clone()
            .or_else(|| s.title.clone())
            .unwrap_or_else(|| s.first_user_text().map(|t| t.lines().next().unwrap_or_default().to_string()).unwrap_or_default());

        if !opts.dry_run {
            let con = Connection::open(&self.db_path)?;
            con.busy_timeout(std::time::Duration::from_secs(10))?;
            con.execute(
                "insert or replace into session (id, project_id, slug, directory, title, version, permission,
                    time_created, time_updated, task_type, title_source)
                 values (?1, ?2, ?1, ?3, ?4, '0.16.9', '{\"mode\":\"yolo\"}', ?5, ?6, 'interactive', 'first_input')",
                rusqlite::params![
                    session_id,
                    zcode_project_id(&cwd),
                    cwd,
                    title,
                    s.created_ms,
                    s.updated_ms,
                ],
            )?;

            let mut msg_seq: i64 = 0;
            let mut part_seq: i64 = 0;
            let mut offset: i64 = 0;
            let insert_message = |con: &Connection,
                                      msg_seq: &mut i64,
                                      part_seq: &mut i64,
                                      role: &str,
                                      ts: i64,
                                      parts: Vec<Value>|
             -> anyhow::Result<()> {
                let msg_id = format!("msg_{}_{}", ts, uuid7(ts, &format!("zcode-msg|{}|{}", s.id, *msg_seq)));
                let data = json!({
                    "role": role,
                    "time": {"created": ts},
                    "agent": "harness-bridge",
                    "semantics": {"origin": "imported", "kind": format!("imported_{}", role)},
                });
                con.execute(
                    "insert or replace into message (id, session_id, time_created, time_updated, data, sequence)
                     values (?1, ?2, ?3, ?3, ?4, ?5)",
                    rusqlite::params![msg_id, session_id, ts, data.to_string(), *msg_seq],
                )?;
                *msg_seq += 1;
                for part in parts {
                    let part_id = format!("part_{}_{}", ts, uuid7(ts, &format!("zcode-part|{}|{}", s.id, *part_seq)));
                    con.execute(
                        "insert or replace into part (id, message_id, session_id, time_created, time_updated, data, sequence)
                         values (?1, ?2, ?3, ?4, ?4, ?5, ?6)",
                        rusqlite::params![part_id, msg_id, session_id, ts, part.to_string(), *part_seq],
                    )?;
                    *part_seq += 1;
                }
                Ok(())
            };

            for e in &s.events {
                let ts = s.event_ms(e, offset);
                offset += 1;
                match &e.kind {
                    EventKind::Message { role, text, .. } => {
                        let zrole = match role {
                            Role::User => "user",
                            Role::Assistant | Role::Developer => "assistant",
                        };
                        let part = if role == &Role::Developer {
                            json!({"type": "text", "text": format!("[context] {text}"), "time": {"start": ts, "end": ts}})
                        } else {
                            json!({"type": "text", "text": text, "time": {"start": ts, "end": ts}})
                        };
                        insert_message(&con, &mut msg_seq, &mut part_seq, zrole, ts, vec![part])?;
                    }
                    EventKind::Reasoning { text } => {
                        insert_message(&con, &mut msg_seq, &mut part_seq, "assistant", ts, vec![json!({
                            "type": "reasoning", "text": text, "time": {"start": ts, "end": ts}
                        })])?;
                    }
                    EventKind::ToolCall { call_id, name, arguments } => {
                        let input: Value = serde_json::from_str(arguments)
                            .unwrap_or_else(|_| json!({"raw": arguments}));
                        insert_message(&con, &mut msg_seq, &mut part_seq, "assistant", ts, vec![json!({
                            "type": "tool", "callID": call_id, "tool": name,
                            "state": {"status": "running", "input": input, "time": {"start": ts}}
                        })])?;
                    }
                    EventKind::ToolResult { call_id, text } => {
                        // attach the result to the pending tool part
                        let pending = con.query_row(
                            "select id, data from part where session_id = ?1 and data like ?2 order by sequence desc limit 1",
                            rusqlite::params![session_id, format!("%{}%", call_id)],
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
                        }
                    }
                    EventKind::Compaction { id, text } => {
                        insert_message(&con, &mut msg_seq, &mut part_seq, "assistant", ts, vec![json!({
                            "type": "compaction", "id": id, "summary": text
                        })])?;
                    }
                    EventKind::TurnStart | EventKind::TurnEnd { .. } | EventKind::Meta { .. } => {}
                }
            }
            con.close().ok();
        }

        Ok(WriteOutcome {
            provider: "zcode".into(),
            location: format!("{}#{}", self.db_path.display(), session_id),
            native_id: session_id,
            extra: json!({"title": title, "cwd": cwd, "dry_run": opts.dry_run}),
        })
    }
}
