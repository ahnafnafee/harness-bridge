use super::*;
use crate::providers::Provider;
use std::path::PathBuf;

struct Fixture {
    _root: tempfile::TempDir,
    provider: ZcodeProvider,
    index_path: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("store #1/cli/db/db.sqlite");
        let index_path = root.path().join("store #1/v2/tasks-index.sqlite");
        std::fs::create_dir_all(database.parent().unwrap()).unwrap();
        std::fs::create_dir_all(index_path.parent().unwrap()).unwrap();
        Connection::open(&database).unwrap().execute_batch(
            "CREATE TABLE session(id TEXT PRIMARY KEY,project_id TEXT,slug TEXT,directory TEXT,path TEXT,
                title TEXT,version TEXT,permission TEXT,time_created INTEGER,time_updated INTEGER,
                task_type TEXT,title_source TEXT,parent_id TEXT);
             CREATE TABLE message(id TEXT PRIMARY KEY,session_id TEXT,time_created INTEGER,
                time_updated INTEGER,data TEXT,sequence INTEGER);
             CREATE TABLE part(id TEXT PRIMARY KEY,message_id TEXT,session_id TEXT,time_created INTEGER,
                time_updated INTEGER,data TEXT,sequence INTEGER);
             CREATE TABLE session_entry(id TEXT PRIMARY KEY,session_id TEXT,type TEXT,
                time_created INTEGER,time_updated INTEGER,data TEXT);"
        ).unwrap();
        Connection::open(&index_path).unwrap().execute_batch(
            "CREATE TABLE hosts(host_id TEXT PRIMARY KEY,kind TEXT NOT NULL,position INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE projects(project_id TEXT PRIMARY KEY,host_id TEXT NOT NULL,name TEXT NOT NULL,
                name_overridden INTEGER NOT NULL DEFAULT 0,position INTEGER NOT NULL,
                deleted INTEGER NOT NULL DEFAULT 0,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL);
             CREATE UNIQUE INDEX project_name ON projects(host_id,name) WHERE deleted=0;
             CREATE TABLE project_roots(project_id TEXT,root_path TEXT,position INTEGER,is_primary INTEGER,
                PRIMARY KEY(project_id,root_path));
             CREATE TABLE tasks(workspace_key TEXT NOT NULL,workspace_path TEXT NOT NULL,
                task_id TEXT NOT NULL,title TEXT NOT NULL DEFAULT '',provider TEXT,mode TEXT NOT NULL DEFAULT 'build',
                created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL,meta_json TEXT NOT NULL DEFAULT '{}',
                host_id TEXT,root_path TEXT,project_id TEXT,cli_title TEXT,cli_title_source TEXT,
                cli_created_at INTEGER,cli_updated_at INTEGER,cli_deleted INTEGER,
                handoff_pending INTEGER NOT NULL DEFAULT 0,searchable_text TEXT NOT NULL DEFAULT '',
                title_overridden INTEGER NOT NULL DEFAULT 0,pinned INTEGER NOT NULL DEFAULT 0,
                archived INTEGER NOT NULL DEFAULT 0,deleted INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY(workspace_key,task_id));"
        ).unwrap();
        Self {
            _root: root,
            provider: ZcodeProvider::new(database),
            index_path,
        }
    }

    fn index(&self) -> Connection {
        Connection::open(&self.index_path).unwrap()
    }

    fn project(&self, id: &str, name: &str, cwd: &str, deleted: bool) {
        let con = self.index();
        con.execute(
            "INSERT INTO projects VALUES(?1,'local',?2,0,0,?3,1,1)",
            rusqlite::params![id, name, deleted],
        )
        .unwrap();
        con.execute(
            "INSERT INTO project_roots VALUES(?1,?2,0,1)",
            rusqlite::params![id, cwd],
        )
        .unwrap();
    }
}

fn session() -> Session {
    Session {
        source: "qoder-ide".into(),
        id: "source-conversation".into(),
        title: Some("harness-debug".into()),
        cwd: Some("C:\\work\\example".into()),
        agent_preset: None,
        parent_session: None,
        origin: None,
        created_ms: 1_794_201_576_423,
        updated_ms: 1_794_235_881_408,
        events: vec![Event::at(
            None,
            EventKind::Message {
                role: Role::User,
                text: "original prompt".into(),
                source_kind: None,
            },
        )],
        resume_events: None,
        resume_context_unavailable: None,
    }
}

#[test]
fn desktop_import_repairs_unassigned_task_and_preserves_desktop_preferences() {
    let fixture = Fixture::new();
    let source = session();
    let cwd = source.cwd.as_ref().unwrap();
    fixture.project("active-project", "example", cwd, false);
    fixture.project("deleted-project", "example", cwd, true);
    let before_native = std::fs::read(&fixture.provider.db_path).unwrap();
    let before_index = std::fs::read(&fixture.index_path).unwrap();
    let preview = fixture
        .provider
        .write(
            &source,
            &WriteOpts {
                dry_run: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(preview.extra["desktop_project_id"], "active-project");
    assert_eq!(
        std::fs::read(&fixture.provider.db_path).unwrap(),
        before_native
    );
    assert_eq!(std::fs::read(&fixture.index_path).unwrap(), before_index);

    let con = fixture.index();
    con.execute(
        "INSERT INTO tasks(workspace_key,workspace_path,task_id,title,created_at,updated_at,
        host_id,root_path,pinned,archived,title_overridden,meta_json)
        VALUES(?1,?1,?2,'Personal title',1,1,'local',?1,1,1,1,'{\"model\":\"chosen-model\"}')",
        rusqlite::params![cwd, preview.native_id],
    )
    .unwrap();
    let outcome = fixture
        .provider
        .write(&source, &WriteOpts::default())
        .unwrap();
    assert_eq!(outcome.native_id, preview.native_id);
    let row:(String,String,i64,i64,i64,String) = con.query_row(
        "SELECT project_id,title,pinned,archived,handoff_pending,meta_json FROM tasks WHERE task_id=?1",
        [&outcome.native_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)),
    ).unwrap();
    assert_eq!(
        (&row.0, &row.1, row.2, row.3, row.4),
        (&"active-project".into(), &"Personal title".into(), 1, 1, 8)
    );
    assert_eq!(
        serde_json::from_str::<Value>(&row.5).unwrap()["model"],
        "chosen-model"
    );
    fixture
        .provider
        .write(&source, &WriteOpts::default())
        .unwrap();
    assert_eq!(
        con.query_row("SELECT count(*) FROM tasks", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    let reference = fixture.provider.discover().unwrap().remove(0);
    let read = fixture.provider.read(&reference).unwrap();
    assert_eq!(read.first_user_text().unwrap(), "original prompt");
}

#[test]
fn desktop_import_creates_project_and_does_not_list_saved_children() {
    let fixture = Fixture::new();
    // A same-named project elsewhere must not steal the imported chat.
    fixture.project("elsewhere", "example", "D:\\different", false);
    let mut source = session();
    let outcome = fixture
        .provider
        .write(&source, &WriteOpts::default())
        .unwrap();
    let con = fixture.index();
    let (name, root): (String, String) = con
        .query_row(
            "SELECT p.name,r.root_path FROM tasks t JOIN projects p USING(project_id)
         JOIN project_roots r USING(project_id) WHERE t.task_id=?1",
            [&outcome.native_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(name, "example (2)");
    assert_eq!(root, source.cwd.as_ref().unwrap().as_str());
    source.id = "saved-child".into();
    source.parent_session = Some(outcome.native_id);
    fixture
        .provider
        .write(&source, &WriteOpts::default())
        .unwrap();
    assert_eq!(
        con.query_row("SELECT count(*) FROM tasks", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(fixture.provider.discover().unwrap().len(), 2);
}

#[test]
fn desktop_failure_rolls_back_transcript_and_index_together() {
    let fixture = Fixture::new();
    let mut source = session();
    let outcome = fixture
        .provider
        .write(&source, &WriteOpts::default())
        .unwrap();
    let con = fixture.index();
    con.execute_batch("PRAGMA journal_mode=WAL;
        CREATE TRIGGER reject_import BEFORE UPDATE ON tasks BEGIN SELECT RAISE(ABORT,'index rejected write'); END;").unwrap();
    source.events.push(Event::at(
        None,
        EventKind::Message {
            role: Role::Assistant,
            text: "must be rolled back".into(),
            source_kind: None,
        },
    ));
    assert!(fixture
        .provider
        .write(&source, &WriteOpts::default())
        .unwrap_err()
        .to_string()
        .contains("index rejected write"));
    let reference = fixture.provider.discover().unwrap().remove(0);
    let read = fixture.provider.read(&reference).unwrap();
    assert!(!serde_json::to_string(&read)
        .unwrap()
        .contains("must be rolled back"));
    assert_eq!(reference.id, outcome.native_id);
    assert!(!con
        .query_row("SELECT searchable_text FROM tasks", [], |r| r
            .get::<_, String>(0))
        .unwrap()
        .contains("must be rolled back"));
}

#[test]
fn desktop_ambiguity_and_unknown_schema_fail_during_preflight() {
    let fixture = Fixture::new();
    let source = session();
    let cwd = source.cwd.as_ref().unwrap();
    fixture.project("first", "first", cwd, false);
    fixture.project("second", "second", cwd, false);
    for dry_run in [true, false] {
        let error = fixture
            .provider
            .write(
                &source,
                &WriteOpts {
                    dry_run,
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert!(error.to_string().contains("multiple active ZCode projects"));
        assert!(fixture.provider.discover().unwrap().is_empty());
    }
    fixture.index().execute_batch("DROP TABLE tasks").unwrap();
    assert!(fixture
        .provider
        .write(
            &source,
            &WriteOpts {
                dry_run: true,
                ..Default::default()
            }
        )
        .unwrap_err()
        .to_string()
        .contains("unsupported ZCode desktop index schema"));
}

#[test]
fn database_override_never_falls_back_to_the_real_desktop_index() {
    let root = tempfile::tempdir().unwrap();
    assert!(desktop::index_path(&root.path().join("zcode.sqlite")).is_none());
    assert_eq!(
        desktop::index_path(&root.path().join("cli/db/db.sqlite")),
        Some(root.path().join("v2/tasks-index.sqlite"))
    );
    let fixture = Fixture::new();
    std::fs::remove_file(&fixture.index_path).unwrap();
    fixture
        .provider
        .write(&session(), &WriteOpts::default())
        .unwrap();
    assert!(!fixture.index_path.exists());
}

#[test]
fn desktop_index_wal_change_invalidates_a_preview_before_writing() {
    use crate::app::{TransferPlan, TransferRequest};
    use crate::providers::Homes;

    let fixture = Fixture::new();
    fixture
        .provider
        .write(&session(), &WriteOpts::default())
        .unwrap();
    let reference = fixture.provider.discover().unwrap().remove(0);
    let homes = Homes {
        zcode_db: Some(fixture.provider.db_path.clone()),
        ..Default::default()
    };
    let file = fixture._root.path().join("session.hbridge.json");
    crate::app::export(&reference, &homes, &file, false, false).unwrap();
    let con = fixture.index();
    con.execute_batch("PRAGMA journal_mode=WAL").unwrap();
    let mut request = TransferRequest::new(&reference, "zcode");
    request.from.clear();
    request.id.clear();
    request.file = Some(file);
    let before = std::fs::read(&fixture.provider.db_path).unwrap();
    let plan = TransferPlan::prepare(request, &homes).unwrap();
    con.execute(
        "UPDATE tasks SET searchable_text=?1",
        ["desktop changed during preview".repeat(1000)],
    )
    .unwrap();
    assert!(plan
        .execute()
        .unwrap_err()
        .to_string()
        .contains("destination changed"));
    assert_eq!(std::fs::read(&fixture.provider.db_path).unwrap(), before);
}

#[test]
fn desktop_import_does_not_take_over_another_hosts_task() {
    let fixture = Fixture::new();
    let source = session();
    let preview = fixture
        .provider
        .write(
            &source,
            &WriteOpts {
                dry_run: true,
                ..Default::default()
            },
        )
        .unwrap();
    fixture
        .index()
        .execute(
            "INSERT INTO tasks(workspace_key,workspace_path,task_id,created_at,updated_at,host_id)
        VALUES(?1,?1,?2,1,1,'remote-host')",
            rusqlite::params![source.cwd, preview.native_id],
        )
        .unwrap();
    for dry_run in [true, false] {
        assert!(fixture
            .provider
            .write(
                &source,
                &WriteOpts {
                    dry_run,
                    ..Default::default()
                }
            )
            .unwrap_err()
            .to_string()
            .contains("another host"));
    }
    assert!(fixture.provider.discover().unwrap().is_empty());
}

#[test]
fn archival_checkpoints_do_not_replace_the_imported_resume_context() {
    let fixture = Fixture::new();
    let mut source = session();
    source.events.push(Event::at(
        None,
        EventKind::Compaction {
            id: None,
            text: None,
        },
    ));
    source.resume_events = Some(vec![
        Event::at(
            None,
            EventKind::Compaction {
                id: None,
                text: None,
            },
        ),
        Event::at(
            None,
            EventKind::Message {
                role: Role::User,
                text: "retained context".into(),
                source_kind: Some("compact-summary".into()),
            },
        ),
    ]);
    fixture
        .provider
        .write(&source, &WriteOpts::default())
        .unwrap();
    let reference = fixture.provider.discover().unwrap().remove(0);
    let read = fixture.provider.read(&reference).unwrap();
    let archive = serde_json::to_string(&read.events).unwrap();
    let context = serde_json::to_string(read.resume_events.as_ref().unwrap()).unwrap();
    assert!(archive.contains("original prompt"));
    assert!(archive.contains("Compaction"));
    assert!(context.contains("retained context"));
    assert!(!context.contains("original prompt"));
}
