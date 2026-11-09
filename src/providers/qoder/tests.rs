use super::*;
use crate::providers::Homes;

fn record(id: &str, parent: Option<&str>, role: &str, text: &str) -> Value {
    json!({"type":role,"uuid":id,"parentUuid":parent,"sessionId":"session-one","timestamp":"2026-10-21T10:00:00Z",
        "cwd":"C:\\work\\demo","isSidechain":false,"userType":"external","version":"1.1.85",
        "message":{"role":role,"content":[{"type":"text","text":text}]}})
}
fn fixture(root: &Path, records: &[Value], ide: bool) -> PathBuf {
    let directory = root
        .join("projects/demo")
        .join(if ide { "transcript" } else { "" });
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("session-one.jsonl");
    std::fs::write(
        &path,
        records
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();
    path
}
fn words(events: &[Event]) -> String {
    serde_json::to_string(events).unwrap()
}

#[test]
fn native_active_leaf_preserves_forks_only_in_archive() {
    let root = tempfile::tempdir().unwrap();
    fixture(
        root.path(),
        &[
            record("u", None, "user", "shared prompt"),
            record("a", Some("u"), "assistant", "discarded branch"),
            record("b", Some("u"), "assistant", "active branch"),
            json!({"type":"active-leaf","sessionId":"session-one","leafUuid":"b","explicit":true,"timestamp":1792576800000i64}),
        ],
        false,
    );
    let provider = QoderProvider::cli(root.path().into());
    let reference = provider.discover().unwrap().remove(0);
    let session = provider.read(&reference).unwrap();
    assert_eq!(session.source, "qoder");
    assert!(words(&session.events).contains("discarded branch"));
    let retained = words(session.resume_events.as_ref().unwrap());
    assert!(retained.contains("active branch"));
    assert!(!retained.contains("discarded branch"));
}

#[test]
fn empty_active_leaf_is_an_empty_context_and_broken_chains_fail() {
    let rows = vec![
        record("u", None, "user", "archived"),
        json!({"type":"active-leaf","leafUuid":null}),
    ];
    assert!(active_chain(&rows).unwrap().is_empty());
    let missing = vec![record("a", Some("absent"), "assistant", "bad")];
    assert!(active_chain(&missing)
        .unwrap_err()
        .to_string()
        .contains("missing message"));
    let cycle = vec![record("a", Some("a"), "assistant", "bad")];
    assert!(active_chain(&cycle).is_err());
}

#[test]
fn committed_compaction_keeps_summary_and_incomplete_compaction_fails() {
    let root = tempfile::tempdir().unwrap();
    let mut summary = record("summary", Some("boundary"), "user", "retained summary");
    summary["isCompactSummary"] = json!(true);
    let rows = vec![
        record("old", None, "user", "superseded prompt"),
        json!({"type":"system","subtype":"compact_boundary","uuid":"boundary","parentUuid":null,
            "sessionId":"session-one","requiresActiveLeafCommit":true,"compactMetadata":{}}),
        summary,
        record("answer", Some("summary"), "assistant", "later answer"),
        json!({"type":"active-leaf","leafUuid":"answer","sessionId":"session-one","explicit":false,"compactBoundaryUuid":"boundary"}),
    ];
    let path = fixture(root.path(), &rows, false);
    let provider = QoderProvider::cli(root.path().into());
    let reference = provider.discover().unwrap().remove(0);
    let session = provider.read(&reference).unwrap();
    assert!(words(&session.events).contains("superseded prompt"));
    let retained = words(session.resume_events.as_ref().unwrap());
    assert!(retained.contains("retained summary"));
    assert!(!retained.contains("superseded prompt"));
    let mut incomplete = rows.clone();
    incomplete[2]["isCompactSummary"] = json!(false);
    std::fs::write(
        &path,
        incomplete
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    assert!(provider
        .read(&reference)
        .unwrap_err()
        .to_string()
        .contains("no saved summary"));
    let mut uncommitted = rows;
    uncommitted[4]["explicit"] = json!(true);
    std::fs::write(
        path,
        uncommitted
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    assert!(provider
        .read(&reference)
        .unwrap_err()
        .to_string()
        .contains("no committed active-leaf"));
}

#[test]
fn documented_ide_transcripts_without_parent_links_keep_every_turn() {
    let root = tempfile::tempdir().unwrap();
    fixture(
        root.path(),
        &[
            json!({"type":"session_meta","data":{"meta_type":"session_info","content":{"mode":"agent"}}}),
            json!({"type":"user","uuid":"u","message":{"role":"user","content":"IDE prompt"}}),
            json!({"type":"assistant","uuid":"a","message":{"role":"assistant","content":[{"type":"text","text":"IDE answer"}]}}),
            json!({"type":"progress","data":{"type":"hook_progress","hookEvent":"Stop"}}),
        ],
        true,
    );
    let provider = QoderProvider::ide(root.path().into(), None);
    let reference = provider.discover().unwrap().remove(0);
    let session = provider.read(&reference).unwrap();
    let retained = words(session.resume_events.as_ref().unwrap());
    assert!(retained.contains("IDE prompt"));
    assert!(retained.contains("IDE answer"));
    assert!(words(&session.events).contains("hook_progress"));
}

#[test]
fn malformed_lines_and_unknown_content_are_not_silently_dropped() {
    let root = tempfile::tempdir().unwrap();
    let path = fixture(root.path(), &[record("u", None, "user", "hello")], false);
    std::fs::write(&path, "{\"type\":\"user\"}\n{broken}\n").unwrap();
    assert!(QoderProvider::records(&path)
        .unwrap_err()
        .to_string()
        .contains("line 2"));
    assert!(decode_record(
        &json!({"type":"assistant","message":{"content":[{"type":"future-state"}]}})
    )
    .is_err());
}

#[test]
fn replacements_require_explicit_recovery_but_archives_export() {
    let root = tempfile::tempdir().unwrap();
    fixture(
        root.path(),
        &[
            record("u", None, "user", "protected request"),
            json!({"type":"content-replacement","sessionId":"session-one","replacements":[{"messageUuid":"u","content":"short"}]}),
        ],
        false,
    );
    let provider = QoderProvider::cli(root.path().into());
    let reference = provider.discover().unwrap().remove(0);
    assert!(provider.read(&reference).is_err());
    let bundle = crate::portable::Bundle::collect(&provider, reference, false).unwrap();
    let file = root.path().join("export.hbridge.json");
    bundle.save(&file, false).unwrap();
    let loaded = crate::portable::Bundle::load(&file).unwrap();
    let mut session = loaded.read(&loaded.root()).unwrap();
    assert!(session.resume_context_unavailable.is_some());
    assert!(
        crate::resume::prepare(&mut session, 750_000, crate::resume::Options::default()).is_err()
    );
    crate::resume::prepare(
        &mut session,
        750_000,
        crate::resume::Options {
            rebuild: true,
            prune: false,
        },
    )
    .unwrap();
    assert!(words(session.resume_events.as_ref().unwrap())
        .contains("Migration context recovery notice"));
}

#[test]
fn native_write_roundtrips_context_and_tools_without_source_mutation() {
    let source = tempfile::tempdir().unwrap();
    let destination = tempfile::tempdir().unwrap();
    let mut call = record("a", Some("u"), "assistant", "");
    call["message"]["content"] = json!([{"type":"thinking","thinking":"analysis"},{"type":"tool_use","id":"call-1","name":"Read","input":{"path":"a.rs"}}]);
    let mut result = record("r", Some("a"), "user", "");
    result["message"]["content"] =
        json!([{"type":"tool_result","tool_use_id":"call-1","content":"file content"}]);
    let path = fixture(
        source.path(),
        &[
            record("u", None, "user", "hello"),
            call,
            result,
            record("end", Some("r"), "assistant", "answer"),
        ],
        false,
    );
    let original = std::fs::read(&path).unwrap();
    let reader = QoderProvider::cli(source.path().into());
    let reference = reader.discover().unwrap().remove(0);
    let mut session = reader.read(&reference).unwrap();
    session.source = "claude".into();
    let writer = QoderProvider::cli(destination.path().join("new-home"));
    let preview = writer
        .write(
            &session,
            &WriteOpts {
                dry_run: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert!(!destination.path().join("new-home").exists());
    let outcome = writer.write(&session, &WriteOpts::default()).unwrap();
    assert_eq!(preview.native_id, outcome.native_id);
    let imported = writer.discover().unwrap().remove(0);
    let restored = writer.read(&imported).unwrap();
    let retained = words(restored.resume_events.as_ref().unwrap());
    assert!(retained.contains("file content"));
    assert!(retained.contains("Imported reasoning"));
    assert!(retained.contains("call-1"));
    assert_eq!(original, std::fs::read(path).unwrap());
    assert!(!outcome.extra["ide_registration"].as_bool().unwrap());
}

#[test]
fn ide_transcripts_are_discovered_separately_and_never_written() {
    let root = tempfile::tempdir().unwrap();
    fixture(root.path(), &[record("u", None, "user", "CLI")], false);
    fixture(root.path(), &[record("i", None, "user", "IDE")], true);
    let ide = QoderProvider::ide(root.path().into(), None);
    let cli = QoderProvider::cli(root.path().into());
    assert_eq!(ide.discover().unwrap().len(), 1);
    assert_eq!(cli.discover().unwrap().len(), 1);
    let session = ide.read(&ide.discover().unwrap().remove(0)).unwrap();
    assert_eq!(session.source, "qoder-ide");
    assert!(words(&session.events).contains("IDE"));
    assert!(ide.write(&session, &WriteOpts::default()).is_err());
    let homes = Homes {
        qoder: Some(root.path().into()),
        ..Default::default()
    };
    let configured = super::super::provider_named("qoder-ide", &homes).unwrap();
    assert_eq!(configured.discover().unwrap().len(), 1);
}

#[test]
fn desktop_database_is_read_only_and_explicit_about_missing_context() {
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join("main.sqlite");
    let connection = rusqlite::Connection::open(&db).unwrap();
    connection.execute_batch("CREATE TABLE chat_sessions(session_id TEXT,title TEXT,cwd TEXT,created_at TEXT,updated_at TEXT,deleted_at TEXT);CREATE TABLE chat_session_messages(session_id TEXT,sequence INTEGER,role TEXT,payload_json TEXT,created_at INTEGER);
        INSERT INTO chat_sessions VALUES('ide-session','IDE task','C:/work','2026-10-21T10:00:00Z','2026-10-21T10:01:00Z',NULL);").unwrap();
    connection
        .execute(
            "INSERT INTO chat_session_messages VALUES('ide-session',1,'user',?1,1792576800000)",
            [json!({"parts":[{"type":"text","text":"keep this request"}]}).to_string()],
        )
        .unwrap();
    drop(connection);
    let before = std::fs::read(&db).unwrap();
    let provider = QoderProvider::ide(root.path().join("absent"), Some(db.clone()));
    let reference = provider.discover().unwrap().remove(0);
    assert_eq!(reference.title.as_deref(), Some("IDE task"));
    assert!(provider.read(&reference).is_err());
    let session = provider.read_for_export(&reference).unwrap();
    assert!(session.resume_context_unavailable.is_some());
    assert!(words(&session.events).contains("keep this request"));
    assert_eq!(before, std::fs::read(db).unwrap());
}
