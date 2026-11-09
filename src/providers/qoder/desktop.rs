use super::*;
use rusqlite::{Connection, OpenFlags};

fn open(path: &Path) -> anyhow::Result<Connection> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("cannot read Qoder desktop history {}", path.display()))?;
    connection.busy_timeout(std::time::Duration::from_secs(3))?;
    connection.execute_batch("BEGIN")?;
    Ok(connection)
}

pub(super) fn discover(path: &Path) -> anyhow::Result<Vec<SessionRef>> {
    let connection = open(path)?;
    let mut statement=connection.prepare("SELECT session_id,title,cwd,created_at,updated_at FROM chat_sessions WHERE deleted_at IS NULL ORDER BY updated_at DESC")
        .context("unsupported Qoder desktop schema; use IDE hook transcripts under --qoder-home instead")?;
    let rows = statement.query_map([], |row| {
        let id: String = row.get(0)?;
        fn date(row: &rusqlite::Row<'_>, index: usize) -> Option<i64> {
            row.get::<_, i64>(index).ok().or_else(|| {
                row.get::<_, String>(index)
                    .ok()
                    .and_then(|s| timestamp(&json!(s)))
            })
        }
        Ok(SessionRef {
            provider: "qoder-ide".into(),
            locator: format!("qoder-db:{id}"),
            id,
            title: row.get(1)?,
            cwd: row.get(2)?,
            created_ms: date(row, 3),
            updated_ms: date(row, 4),
            migrated: false,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

pub(super) fn read(path: &Path, reference: &SessionRef) -> anyhow::Result<Session> {
    let connection = open(path)?;
    let mut statement=connection.prepare("SELECT role,payload_json,created_at FROM chat_session_messages WHERE session_id=?1 ORDER BY sequence")?;
    let rows = statement.query_map([&reference.id], |row| {
        Ok((
            row.get::<_, Option<String>>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    let mut events = Vec::new();
    for row in rows {
        let (role, raw, time) = row?;
        let payload: Value =
            serde_json::from_str(&raw).context("invalid Qoder desktop message payload")?;
        let role = match role.as_deref() {
            Some("user") => Role::User,
            Some("assistant") => Role::Assistant,
            _ => Role::Developer,
        };
        let parts = payload["parts"]
            .as_array()
            .context("unsupported Qoder desktop payload: expected parts")?;
        for part in parts {
            match string(part,"type") {
                "text" => events.push(Event::at(Some(time),EventKind::Message{role,text:part["text"].as_str().context("Qoder desktop text has no text")?.into(),source_kind:Some("qoder-desktop".into())})),
                "thinking" => events.push(Event::at(Some(time),EventKind::Reasoning{text:part["text"].as_str().context("Qoder desktop thinking has no text")?.into()})),
                "tool"=>{
                    let tool=&part["tool"];
                    let call_id=tool["id"].as_str().context("Qoder desktop tool has no id")?.to_owned();
                    events.push(Event::at(Some(time),EventKind::ToolCall{call_id:call_id.clone(),name:tool["name"].as_str().context("Qoder desktop tool has no name")?.into(),arguments:serde_json::to_string(&tool["input"])?}));
                    let text=if !tool["response"].is_null() {
                        tool["response"].as_str().map(str::to_owned).unwrap_or_else(||tool["response"].to_string())
                    } else {
                        let status=string(tool,"status");
                        anyhow::ensure!(matches!(status,"failed"|"canceled"|"cancelled"|"interrupted"),
                            "Qoder desktop tool {call_id:?} has no result; finish the source turn first");
                        format!("[Qoder recorded tool status: {status}; no output was saved]")
                    };
                    events.push(Event::at(Some(time),EventKind::ToolResult{call_id,text}));
                }
                "hook" => {}
                "image"|"file"|"attachment"=>events.push(Event::at(Some(time),EventKind::Message{role,text:format!("[Qoder desktop {} attachment; files are not transferred]",string(part,"type")),source_kind:Some("qoder-attachment".into())})),
                other=>anyhow::bail!("unsupported Qoder desktop message part {other:?}; refusing to discard conversation data"),
            }
        }
        events.push(Event::at(
            Some(time),
            EventKind::Meta {
                kind: "qoder-desktop-message".into(),
                data: payload,
            },
        ));
    }
    anyhow::ensure!(
        !events.is_empty(),
        "the Qoder desktop session has no local messages"
    );
    Ok(Session{source:"qoder-ide".into(),id:reference.id.clone(),title:reference.title.clone(),cwd:reference.cwd.clone(),agent_preset:None,
        parent_session:None,origin:Some("Qoder desktop visible history".into()),created_ms:reference.created_ms.unwrap_or(0),
        updated_ms:reference.updated_ms.unwrap_or(0),events,resume_events:None,
        resume_context_unavailable:Some("Qoder desktop stores visible history separately from its model context; only the readable archive is available".into())})
}
