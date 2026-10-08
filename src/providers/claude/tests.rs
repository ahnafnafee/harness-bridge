use super::*;
use crate::providers::{codex::CodexProvider, Provider};

struct Fixture {
    dir: std::path::PathBuf,
    source: SessionRef,
}

impl Fixture {
    fn new(rows: &[Value]) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "hb-claude-context-{}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            uuid7(
                chrono::Utc::now().timestamp_millis(),
                &serde_json::to_string(rows).unwrap()
            )
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("source.jsonl");
        std::fs::write(
            &path,
            rows.iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        Self {
            source: SessionRef {
                provider: "claude".into(),
                id: "fixture".into(),
                title: Some("fixture".into()),
                cwd: None,
                created_ms: None,
                updated_ms: None,
                locator: path.to_string_lossy().into(),
                migrated: false,
            },
            dir,
        }
    }

    fn read(&self) -> anyhow::Result<Session> {
        ClaudeProvider::new(self.dir.clone()).read(&self.source)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).unwrap();
    }
}

fn message(id: &str, role: &str, content: Value) -> Value {
    json!({"type":role, "uuid":id, "timestamp":"2026-10-08T00:00:00Z",
        "message":{"content":content}, "cwd":"test"})
}

fn summary(id: &str, text: Value) -> Value {
    let mut row = message(id, "user", text);
    row["isCompactSummary"] = json!(true);
    row
}

fn boundary(metadata: Value) -> Value {
    json!({"type":"system", "subtype":"compact_boundary", "uuid":"boundary",
        "timestamp":"2026-10-08T00:00:01Z", "compactMetadata":metadata})
}

fn context_text(session: &Session) -> String {
    serde_json::to_string(session.resume_events.as_ref().unwrap()).unwrap()
}

#[test]
fn latest_checkpoint_retains_declared_messages_and_native_codex_context() {
    let obsolete = "obsolete output ".repeat(100_000);
    let rows = vec![
        message("old", "user", json!(obsolete)),
        boundary(json!({})),
        summary("previous", json!("superseded summary")),
        message("task", "user", json!("retained unsummarized request")),
        message(
            "call",
            "assistant",
            json!([
                {"type":"thinking", "thinking":"retained reasoning"},
                {"type":"tool_use", "id":"c1", "name":"read", "input":{"path":"keep"}}
            ]),
        ),
        message(
            "result",
            "user",
            json!([
                {"type":"tool_result", "tool_use_id":"c1", "content":"retained result"}
            ]),
        ),
        boundary(json!({"preservedMessages":{
            "anchorUuid":"current", "uuids":["task"],
            "allUuids":["task", "call", "result", "ephemeral-attachment", "task"]
        }})),
        summary(
            "current",
            json!([{"type":"text", "text":"current summary"}]),
        ),
        message("new", "assistant", json!("new answer")),
    ];
    let fixture = Fixture::new(&rows);
    let session = fixture.read().unwrap();
    let context = context_text(&session);
    assert!(!context.contains("obsolete"));
    assert!(!context.contains("superseded summary"));
    assert!(context.contains("retained reasoning"));
    assert!(context.contains("retained result"));
    assert!(context.contains("new answer"));
    assert_eq!(context.matches("retained unsummarized request").count(), 1);
    assert!(context.find("current summary") < context.find("retained unsummarized request"));
    assert!(serde_json::to_string(&session.events)
        .unwrap()
        .contains(&obsolete));
    assert_eq!(
        session
            .events
            .iter()
            .filter(|e| matches!(e.kind, EventKind::Compaction { .. }))
            .count(),
        2
    );

    let home = fixture.dir.join("codex");
    std::fs::create_dir_all(home.join("sessions")).unwrap();
    let template = [
        json!({"type":"session_meta", "payload":{"originator":"Codex Desktop", "thread_source":"user", "base_instructions":{"text":"base"}}}),
        json!({"type":"turn_context", "payload":{"model":"gpt-test", "cwd":"test", "approval_policy":"never", "sandbox_policy":{"type":"danger-full-access"}, "effort":"low", "summary":"auto"}}),
    ];
    std::fs::write(
        home.join("sessions/template.jsonl"),
        template
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    let outcome = CodexProvider::new(home)
        .write(&session, &WriteOpts::default())
        .unwrap();
    let rollout = std::fs::read_to_string(outcome.location).unwrap();
    assert!(rollout.contains(&obsolete));
    let checkpoint: Value = rollout
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .find(|r| r["type"] == "compacted")
        .unwrap();
    let history = checkpoint["payload"]["replacement_history"]
        .as_array()
        .unwrap();
    let native = serde_json::to_string(history).unwrap();
    assert!(native.contains("current summary"));
    assert!(native.contains("retained unsummarized request"));
    assert!(!native.contains("obsolete"));
    assert!(!native.contains("superseded summary"));
    let calls: Vec<_> = history
        .iter()
        .filter(|r| r["type"] == "function_call")
        .collect();
    let results: Vec<_> = history
        .iter()
        .filter(|r| r["type"] == "function_call_output")
        .collect();
    assert_eq!(calls.len(), 1);
    assert_eq!(results.len(), 1);
    assert_eq!(calls[0]["call_id"], results[0]["call_id"]);
}

#[test]
fn older_uuid_list_recovers_the_retained_results_original_call() {
    let fixture = Fixture::new(&[
        message(
            "call",
            "assistant",
            json!([{"type":"tool_use", "id":"c", "name":"read", "input":{}}]),
        ),
        message(
            "result",
            "user",
            json!([{"type":"tool_result", "tool_use_id":"c", "content":[{"type":"text", "text":"kept"}]}]),
        ),
        boundary(json!({"preservedMessages":{"uuids":["result"]}})),
        summary("summary", json!("checkpoint")),
    ]);
    let session = fixture.read().unwrap();
    let events = session.resume_events.unwrap();
    assert!(matches!(&events[1].kind, EventKind::ToolCall { call_id, .. } if call_id == "c"));
    assert!(matches!(&events[2].kind, EventKind::ToolResult { call_id, .. } if call_id == "c"));
}

#[test]
fn segment_only_metadata_keeps_its_messages_after_the_summary() {
    let fixture = Fixture::new(&[
        message("old", "user", json!("obsolete")),
        message("head", "user", json!("keep request")),
        message("tail", "assistant", json!("keep answer")),
        boundary(
            json!({"preservedSegment":{"headUuid":"head", "tailUuid":"tail", "anchorUuid":"summary"}}),
        ),
        message("summary", "user", json!("checkpoint")),
    ]);
    let context = context_text(&fixture.read().unwrap());
    assert!(!context.contains("obsolete"));
    assert!(context.contains("keep request"));
    assert!(context.contains("keep answer"));
    assert!(context.find("checkpoint") < context.find("keep request"));
}

#[test]
fn uncompacted_sessions_keep_the_existing_resume_behavior() {
    let fixture = Fixture::new(&[message("task", "user", json!("task"))]);
    let session = fixture.read().unwrap();
    assert!(session.resume_events.is_none());
    assert_eq!(session.events.len(), 1);
}

#[test]
fn incomplete_or_empty_compactions_fail_without_replaying_obsolete_history() {
    let incomplete = Fixture::new(&[message("old", "user", json!("old")), boundary(json!({}))]);
    assert!(incomplete
        .read()
        .unwrap_err()
        .to_string()
        .contains("no persisted summary"));
    let empty = Fixture::new(&[boundary(json!({})), summary("empty", json!(""))]);
    assert!(empty
        .read()
        .unwrap_err()
        .to_string()
        .contains("summary is empty"));
    let orphan = Fixture::new(&[
        boundary(json!({})),
        summary("s", json!("summary")),
        message(
            "r",
            "user",
            json!([{"type":"tool_result", "tool_use_id":"missing", "content":"output"}]),
        ),
    ]);
    assert!(orphan
        .read()
        .unwrap_err()
        .to_string()
        .contains("no persisted call"));
}

#[test]
fn sidechain_records_do_not_replace_the_main_checkpoint() {
    let mut side_boundary = boundary(json!({}));
    side_boundary["isSidechain"] = json!(true);
    let mut side_message = message("side", "user", json!("sidechain text"));
    side_message["isSidechain"] = json!(true);
    let fixture = Fixture::new(&[
        boundary(json!({})),
        summary("main", json!("main summary")),
        side_boundary,
        side_message,
        message("new", "user", json!("main request")),
    ]);
    let session = fixture.read().unwrap();
    let context = context_text(&session);
    assert!(context.contains("main summary"));
    assert!(context.contains("main request"));
    assert!(!context.contains("sidechain text"));
    assert!(serde_json::to_string(&session.events)
        .unwrap()
        .contains("sidechain text"));
}

#[test]
fn claude_and_dsh_exports_resume_the_checkpoint_without_the_archive() {
    use crate::providers::dsh::DshProvider;
    let fixture = Fixture::new(&[
        message("old", "user", json!("obsolete archived request")),
        boundary(json!({})),
        summary("summary", json!("current checkpoint")),
        message(
            "call",
            "assistant",
            json!([{"type":"tool_use", "id":"c", "name":"read", "input":{}}]),
        ),
        message(
            "result",
            "user",
            json!([{"type":"tool_result", "tool_use_id":"c", "content":"current output"}]),
        ),
        message("new", "assistant", json!("current answer")),
    ]);
    let session = fixture.read().unwrap();
    for provider in [
        Box::new(ClaudeProvider::new(fixture.dir.join("claude"))) as Box<dyn Provider>,
        Box::new(DshProvider::new(fixture.dir.join("dsh"))) as Box<dyn Provider>,
    ] {
        let outcome = provider.write(&session, &WriteOpts::default()).unwrap();
        let raw = std::fs::read_to_string(&outcome.location).unwrap();
        assert!(raw.contains("obsolete archived request"));
        let mut source = fixture.source.clone();
        source.id = outcome.native_id;
        source.locator = outcome.location;
        let read = provider.read(&source).unwrap();
        let context = context_text(&read);
        assert!(
            !context.contains("obsolete archived request"),
            "{}",
            provider.name()
        );
        assert!(context.contains("current checkpoint"));
        assert!(context.contains("current output"));
        assert!(context.contains("current answer"));
        assert!(context.contains("ToolCall"));
    }
}

#[test]
fn zcode_export_keeps_archive_visible_and_context_current_and_rolls_back_failures() {
    use crate::providers::zcode::ZcodeProvider;
    let fixture = Fixture::new(&[
        message("old", "user", json!("obsolete archive")),
        boundary(json!({})),
        summary("s", json!("current summary")),
        message(
            "call",
            "assistant",
            json!([{"type":"tool_use", "id":"c_1", "name":"read", "input":{}}]),
        ),
        message(
            "result",
            "user",
            json!([{"type":"tool_result", "tool_use_id":"c_1", "content":"current output"}]),
        ),
    ]);
    let mut session = fixture.read().unwrap();
    let path = fixture.dir.join("zcode.sqlite");
    let con = rusqlite::Connection::open(&path).unwrap();
    con.execute_batch("create table session(id text primary key,project_id text,slug text,directory text,path text,title text,version text,permission text,time_created integer,time_updated integer,task_type text,title_source text);
        create table message(id text primary key,session_id text,time_created integer,time_updated integer,data text,sequence integer);
        create table part(id text primary key,message_id text,session_id text,time_created integer,time_updated integer,data text,sequence integer);
        create table session_entry(id text primary key,session_id text,type text,time_created integer,time_updated integer,data text);").unwrap();
    let provider = ZcodeProvider::new(path);
    let outcome = provider.write(&session, &WriteOpts::default()).unwrap();
    let mut reference = fixture.source.clone();
    reference.id = outcome.native_id.clone();
    let read = provider.read(&reference).unwrap();
    assert!(serde_json::to_string(&read.events)
        .unwrap()
        .contains("obsolete archive"));
    let context = context_text(&read);
    assert!(!context.contains("obsolete archive"));
    assert!(context.contains("current summary"));
    assert!(context.contains("current output"));
    let hidden:i64=con.query_row("select count(*) from message where json_extract(data,'$.semantics.providerVisibility')='hidden'",[],|r|r.get(0)).unwrap();
    assert!(hidden > 0);
    let before: i64 = con
        .query_row("select count(*) from part", [], |r| r.get(0))
        .unwrap();
    session.events.push(Event::at(
        None,
        EventKind::ToolResult {
            call_id: "missing".into(),
            text: "orphan".into(),
        },
    ));
    assert!(provider
        .write(
            &session,
            &WriteOpts {
                dry_run: true,
                ..WriteOpts::default()
            }
        )
        .is_err());
    assert!(provider.write(&session, &WriteOpts::default()).is_err());
    let after: i64 = con
        .query_row("select count(*) from part", [], |r| r.get(0))
        .unwrap();
    assert_eq!(before, after);
}

#[test]
fn family_import_preserves_native_children_and_is_idempotent() {
    use crate::providers::dsh::DshProvider;
    use crate::providers::zcode::ZcodeProvider;
    let protected_text = "parent task ".repeat(200);
    let fixture = Fixture::new(&[message("root", "user", json!(protected_text))]);
    let children_dir = Path::new(&fixture.source.locator)
        .with_extension("")
        .join("subagents");
    std::fs::create_dir_all(&children_dir).unwrap();
    let mut child = message("child", "user", json!("child task"));
    child["sessionId"] = json!(fixture.source.id);
    child["agentId"] = json!("child-agent");
    child["isSidechain"] = json!(true);
    std::fs::write(children_dir.join("agent-child.jsonl"), child.to_string()).unwrap();
    let nested = children_dir.join("agent-child/subagents");
    std::fs::create_dir_all(&nested).unwrap();
    let mut grandchild = child.clone();
    grandchild["message"]["content"] = json!("grandchild task");
    std::fs::write(
        nested.join("agent-grandchild.jsonl"),
        grandchild.to_string(),
    )
    .unwrap();
    let source = ClaudeProvider::new(fixture.dir.clone());
    let max_chars = serde_json::to_string(&fixture.read().unwrap().events)
        .unwrap()
        .chars()
        .count();
    assert_eq!(source.children(&fixture.source).unwrap().len(), 1);
    let codex_home = fixture.dir.join("target-codex");
    std::fs::create_dir_all(codex_home.join("sessions")).unwrap();
    std::fs::write(codex_home.join("sessions/template.jsonl"), [
        json!({"type":"session_meta", "payload":{"originator":"Codex Desktop", "thread_source":"user", "base_instructions":{"text":"base"}}}),
        json!({"type":"turn_context", "payload":{"model":"gpt-test", "cwd":"test", "approval_policy":"never", "sandbox_policy":{"type":"danger-full-access"}, "effort":"low", "summary":"auto"}}),
    ].iter().map(Value::to_string).collect::<Vec<_>>().join("\n")).unwrap();
    let codex_db = rusqlite::Connection::open(codex_home.join("state_5.sqlite")).unwrap();
    codex_db.execute_batch("create table threads(id text primary key, rollout_path,created_at,created_at_ms,updated_at,updated_at_ms,recency_at,recency_at_ms,source,model_provider,originator,cwd,title,first_user_message,preview,name,sandbox_policy,approval_mode,tokens_used,has_user_event,archived,is_pinned,memory_mode,history_mode,cli_version,model,reasoning_effort,thread_source,git_sha,git_branch,git_origin_url,creator_user_id,creator_account_id);
        create table thread_spawn_edges(parent_thread_id text,child_thread_id text primary key,status text);").unwrap();
    let zcode_targets:Vec<_> = [false,true].iter().map(|native| {
        let path = fixture.dir.join(format!("target-zcode-{native}.sqlite"));
        let con = rusqlite::Connection::open(&path).unwrap();
        con.execute_batch("create table session(id text primary key,project_id text,slug text,directory text,path text,title text,version text,permission text,time_created integer,time_updated integer,task_type text,title_source text);
            create table message(id text primary key,session_id text,time_created integer,time_updated integer,data text,sequence integer);
            create table part(id text primary key,message_id text,session_id text,time_created integer,time_updated integer,data text,sequence integer);
            create table session_entry(id text primary key,session_id text,type text,time_created integer,time_updated integer,data text);").unwrap();
        if *native {con.execute_batch("alter table session add column parent_id text").unwrap();}
        Box::new(ZcodeProvider::new(path)) as Box<dyn Provider>
    }).collect();
    for target in [
        Box::new(ClaudeProvider::new(fixture.dir.join("target-claude"))) as Box<dyn Provider>,
        Box::new(DshProvider::new(fixture.dir.join("target-dsh"))) as Box<dyn Provider>,
        Box::new(CodexProvider::new(codex_home)) as Box<dyn Provider>,
    ]
    .into_iter()
    .chain(zcode_targets)
    {
        let outcome = crate::family::migrate(
            &source,
            target.as_ref(),
            fixture.source.clone(),
            &WriteOpts::default(),
            true,
            max_chars,
            false,
        )
        .unwrap();
        let refs = target.discover().unwrap();
        let root = refs
            .into_iter()
            .find(|r| r.id == outcome.native_id)
            .unwrap();
        let children = target.children(&root).unwrap();
        let parent = target.read(&root).unwrap();
        assert!(
            serde_json::to_string(&parent.events)
                .unwrap()
                .contains("Imported session family"),
            "{}",
            target.name()
        );
        let context = context_text(&parent);
        assert!(context.contains(&protected_text), "{}", target.name());
        assert!(
            !context.contains("Imported session family"),
            "{}",
            target.name()
        );
        assert_eq!(
            outcome.extra["resume_context_policy"]["input_chars"],
            max_chars
        );
        assert_eq!(outcome.extra["resume_context_policy"]["pruned"], false);
        assert_eq!(children.len(), 1, "{}", target.name());
        let child = target.read(&children[0]).unwrap();
        assert_eq!(child.parent_session, Some(root.id.clone()));
        assert!(serde_json::to_string(&child.events)
            .unwrap()
            .contains("child task"));
        let nested = target.children(&children[0]).unwrap();
        assert_eq!(nested.len(), 1, "{}", target.name());
        let grandchild = target.read(&nested[0]).unwrap();
        assert_eq!(grandchild.parent_session, Some(children[0].id.clone()));
        if target.name() == "codex" {
            let raw = std::fs::read_to_string(&nested[0].locator).unwrap();
            let meta: Value = serde_json::from_str(raw.lines().next().unwrap()).unwrap();
            assert_eq!(
                meta.pointer("/payload/source/subagent/thread_spawn/depth"),
                Some(&json!(2))
            );
            let parent: String = codex_db
                .query_row(
                    "select parent_thread_id from thread_spawn_edges where child_thread_id=?1",
                    [&nested[0].id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(parent, children[0].id);
        }
        let second = crate::family::migrate(
            &source,
            target.as_ref(),
            fixture.source.clone(),
            &WriteOpts::default(),
            true,
            max_chars,
            false,
        )
        .unwrap();
        assert_eq!(second.native_id, outcome.native_id);
        assert_eq!(
            second.extra["child_sessions"][0]["native_id"],
            outcome.extra["child_sessions"][0]["native_id"]
        );
    }
}

#[test]
fn family_preflight_failure_leaves_destination_unwritten() {
    let fixture = Fixture::new(&[message("root", "user", json!("parent task"))]);
    let dir = Path::new(&fixture.source.locator)
        .with_extension("")
        .join("subagents");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("agent-large.jsonl"),
        message(
            "large",
            "user",
            json!("protected child prompt ".repeat(10_000)),
        )
        .to_string(),
    )
    .unwrap();
    let source = ClaudeProvider::new(fixture.dir.clone());
    let path = fixture.dir.join("unwritten");
    let target = ClaudeProvider::new(path.clone());
    assert!(crate::family::migrate(
        &source,
        &target,
        fixture.source.clone(),
        &WriteOpts::default(),
        true,
        2000,
        true
    )
    .is_err());
    assert!(!path.exists());
}

#[test]
fn uncompacted_foreign_reasoning_uses_plain_text_in_claude_context() {
    let fixture = Fixture::new(&[message(
        "root",
        "assistant",
        json!([
            {"type":"thinking", "thinking":"retained reasoning", "signature":"foreign"},
            {"type":"text", "text":"answer"}
        ]),
    )]);
    let session = fixture.read().unwrap();
    let provider = ClaudeProvider::new(fixture.dir.join("target"));
    let outcome = provider.write(&session, &WriteOpts::default()).unwrap();
    let raw = std::fs::read_to_string(outcome.location).unwrap();
    assert!(raw.contains("[Imported reasoning]"));
    assert!(!raw.contains("\"thinking\""));
    assert!(!raw.contains("\"signature\""));
}
