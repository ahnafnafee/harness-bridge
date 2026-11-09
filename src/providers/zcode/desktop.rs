//! Desktop 3.17 task registration is separate from the CLI transcript store.

use crate::ir::{EventKind, Session};
use crate::util::uuid7;
use anyhow::Context;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Derive only from the selected store. An arbitrary --zcode-db copy must never
/// register tasks in the user's real desktop index.
pub(super) fn index_path(database: &Path) -> Option<PathBuf> {
    let db_dir = database.parent()?;
    let cli_dir = db_dir.parent()?;
    if database.file_name()? != "db.sqlite"
        || db_dir.file_name()? != "db"
        || cli_dir.file_name()? != "cli"
    {
        return None;
    }
    Some(cli_dir.parent()?.join("v2/tasks-index.sqlite"))
}

pub(super) fn attach(con: &Connection, path: &Path, read_only: bool) -> anyhow::Result<()> {
    // URI modes prevent ATTACH from creating an empty index if it disappears.
    let absolute = std::path::absolute(path)?;
    let mut uri = String::from("file:");
    for byte in absolute.to_string_lossy().replace('\\', "/").bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~:".contains(&byte) {
            uri.push(byte as char);
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri.push_str(if read_only { "?mode=ro" } else { "?mode=rw" });
    con.execute("ATTACH DATABASE ?1 AS desktop", [&uri])
        .with_context(|| format!("cannot open ZCode desktop index {}", path.display()))?;
    Ok(())
}

pub(super) struct Registration {
    pub project_id: String,
    new_project_name: Option<String>,
}

impl Registration {
    pub fn prepare(
        con: &Connection,
        session: &Session,
        session_id: &str,
        cwd: &str,
    ) -> anyhow::Result<Option<Self>> {
        // Saved children belong to their root task, not the top-level sidebar.
        if session.parent_session.is_some() {
            return Ok(None);
        }
        for sql in [
            "SELECT workspace_key,workspace_path,task_id,title,provider,mode,created_at,updated_at,
                meta_json,host_id,root_path,project_id,cli_title,cli_title_source,cli_created_at,
                cli_updated_at,cli_deleted,handoff_pending,searchable_text,title_overridden
             FROM desktop.tasks LIMIT 0",
            "SELECT project_id,host_id,name,name_overridden,position,deleted,created_at,updated_at
             FROM desktop.projects LIMIT 0",
            "SELECT project_id,root_path,position,is_primary FROM desktop.project_roots LIMIT 0",
            "SELECT host_id,kind,position FROM desktop.hosts LIMIT 0",
        ] {
            con.prepare(sql)
                .context("unsupported ZCode desktop index schema; no destination was written")?;
        }
        anyhow::ensure!(
            !cwd.trim().is_empty(),
            "ZCode Desktop needs a working directory; pass --cwd"
        );
        let owner: Option<Option<String>> = con
            .query_row(
                "SELECT host_id FROM desktop.tasks WHERE workspace_key=?1 AND task_id=?2",
                params![cwd, session_id],
                |row| row.get(0),
            )
            .optional()?;
        anyhow::ensure!(
            owner.flatten().is_none_or(|host| host == "local"),
            "the ZCode desktop task belongs to another host; no destination was written"
        );

        // Preserve an explicit desktop project assignment on a repeated import.
        let assigned: Option<String> = con
            .query_row(
                "SELECT t.project_id FROM desktop.tasks t
                 JOIN desktop.projects p ON p.project_id=t.project_id
                 WHERE t.workspace_key=?1 AND t.task_id=?2 AND t.host_id='local'
                    AND p.host_id='local' AND p.deleted=0",
                params![cwd, session_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(project_id) = assigned {
            return Ok(Some(Self {
                project_id,
                new_project_name: None,
            }));
        }
        let mut statement = con.prepare(
            "SELECT p.project_id FROM desktop.projects p
             JOIN desktop.project_roots r ON r.project_id=p.project_id
             WHERE p.host_id='local' AND p.deleted=0 AND r.root_path=?1 LIMIT 2",
        )?;
        let projects = statement
            .query_map([cwd], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        anyhow::ensure!(projects.len() <= 1,
            "multiple active ZCode projects use {cwd:?}; resolve the duplicate projects in ZCode or choose another --cwd");
        if let Some(project_id) = projects.into_iter().next() {
            return Ok(Some(Self {
                project_id,
                new_project_name: None,
            }));
        }

        // Use the same root/name rules as Desktop without reviving deleted projects.
        let base_name = cwd
            .trim_end_matches(['/', '\\'])
            .rsplit(['/', '\\'])
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or("Imported sessions");
        let mut name = base_name.to_owned();
        let mut ordinal = 2;
        while con.query_row(
            "SELECT EXISTS(SELECT 1 FROM desktop.projects WHERE host_id='local' AND deleted=0 AND name=?1)",
            [&name], |row| row.get::<_, bool>(0),
        )? {
            name = format!("{base_name} ({ordinal})");
            ordinal += 1;
        }
        let mut ordinal = 0;
        let project_id = loop {
            let candidate = uuid7(
                session.created_ms,
                &format!("zcode-project|{cwd}|{ordinal}"),
            );
            if !con.query_row(
                "SELECT EXISTS(SELECT 1 FROM desktop.projects WHERE project_id=?1)",
                [&candidate],
                |row| row.get::<_, bool>(0),
            )? {
                break candidate;
            }
            ordinal += 1;
        };
        Ok(Some(Self {
            project_id,
            new_project_name: Some(name),
        }))
    }

    pub fn write(
        &self,
        con: &Connection,
        session: &Session,
        session_id: &str,
        cwd: &str,
        title: &str,
    ) -> anyhow::Result<()> {
        con.execute(
            "INSERT OR IGNORE INTO desktop.hosts(host_id,kind,position)
             VALUES('local','local',(SELECT count(*) FROM desktop.hosts))",
            [],
        )?;
        if let Some(name) = &self.new_project_name {
            con.execute(
                "INSERT INTO desktop.projects(project_id,host_id,name,name_overridden,position,created_at,updated_at)
                 VALUES(?1,'local',?2,0,(SELECT count(*) FROM desktop.projects WHERE host_id='local'),?3,?3)",
                params![self.project_id, name, session.updated_ms],
            )?;
            con.execute(
                "INSERT INTO desktop.project_roots(project_id,root_path,position,is_primary) VALUES(?1,?2,0,1)",
                params![self.project_id, cwd],
            )?;
        }
        let existing: Option<String> = con
            .query_row(
                "SELECT meta_json FROM desktop.tasks WHERE workspace_key=?1 AND task_id=?2",
                params![cwd, session_id],
                |row| row.get(0),
            )
            .optional()?;
        let mut meta = existing
            .map(|raw| serde_json::from_str::<Value>(&raw))
            .transpose()?
            .unwrap_or_else(|| json!({}));
        anyhow::ensure!(
            meta.is_object(),
            "ZCode task metadata must be a JSON object"
        );
        for (key, value) in json!({"taskId":session_id,"title":title,"workspacePath":cwd,
            "createdAt":session.created_ms,"updatedAt":session.updated_ms})
        .as_object()
        .unwrap()
        {
            meta[key] = value.clone();
        }
        let object = meta.as_object_mut().unwrap();
        object
            .entry("traceId")
            .or_insert_with(|| json!(uuid7(session.created_ms, session_id)));
        object.entry("provider").or_insert(json!("glm"));
        object.entry("mode").or_insert(json!("default"));
        let searchable = session
            .events
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::Message { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        con.execute(
            "INSERT INTO desktop.tasks(workspace_key,workspace_path,task_id,title,provider,mode,
                created_at,updated_at,meta_json,host_id,root_path,project_id,cli_title,
                cli_title_source,cli_created_at,cli_updated_at,cli_deleted,handoff_pending,searchable_text)
             VALUES(?1,?1,?2,?3,'glm','default',?4,?5,?6,'local',?1,?7,?3,'first_input',?4,?5,0,8,?8)
             ON CONFLICT(workspace_key,task_id) DO UPDATE SET
                title=CASE WHEN title_overridden=1 THEN title ELSE excluded.title END,
                created_at=excluded.created_at,updated_at=excluded.updated_at,
                meta_json=excluded.meta_json,host_id='local',root_path=excluded.root_path,
                project_id=excluded.project_id,cli_title=excluded.cli_title,
                cli_title_source=excluded.cli_title_source,cli_created_at=excluded.cli_created_at,
                cli_updated_at=excluded.cli_updated_at,cli_deleted=0,
                handoff_pending=tasks.handoff_pending | 8,searchable_text=excluded.searchable_text",
            params![cwd,session_id,title,session.created_ms,session.updated_ms,meta.to_string(),self.project_id,searchable],
        )?;
        Ok(())
    }
}
