use super::*;
use crate::providers::{dsh::DshProvider, Provider};

#[test]
fn desktop_names_override_prompt_titles_with_legacy_schema_fallback() {
    let dir = std::env::temp_dir().join(format!(
        "hb-desktop-names-{}",
        uuid7(chrono::Utc::now().timestamp_millis(), "desktop-names")
    ));
    for modern in [true, false] {
        let home = dir.join(modern.to_string());
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join("session_index.jsonl"),
            [
                json!({"id":"named","thread_name":"stale index name"}),
                json!({"id":"empty","thread_name":"saved index name"}),
            ]
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
        )
        .unwrap();
        let connection = rusqlite::Connection::open(home.join("state_5.sqlite")).unwrap();
        if modern {
            connection
                .execute_batch(
                    "create table threads(id text,title text,name text);
                insert into threads values('named','initial prompt','chosen desktop name');
                insert into threads values('fallback','legacy prompt','');
                insert into threads values('empty','','');",
                )
                .unwrap();
        } else {
            connection
                .execute_batch(
                    "create table threads(id text,title text);
                insert into threads values('named','legacy title');
                insert into threads values('empty','');",
                )
                .unwrap();
        }
        connection.close().unwrap();
        let titles = CodexProvider::new(home).session_titles();
        assert_eq!(
            titles["named"],
            if modern {
                "chosen desktop name"
            } else {
                "legacy title"
            }
        );
        assert_eq!(titles["empty"], "saved index name");
        if modern {
            assert_eq!(titles["fallback"], "legacy prompt");
        }
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn native_agent_mail_exports_archive_without_bypassing_encrypted_input_guard() {
    let dir = std::env::temp_dir().join(format!(
        "hb-agent-mail-{}",
        uuid7(chrono::Utc::now().timestamp_millis(), "agent-mail")
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rollout.jsonl");
    let provider = CodexProvider::new(dir.clone());
    let reference = SessionRef {
        provider: "codex".into(),
        id: "fixture".into(),
        title: None,
        cwd: None,
        created_ms: None,
        updated_ms: None,
        locator: path.to_string_lossy().into(),
        migrated: false,
    };
    for encrypted in [false, true] {
        let mut content = vec![json!({"type":"input_text","text":"readable worker report"})];
        if encrypted {
            content.push(json!({"type":"encrypted_content","encrypted_content":"opaque-secret"}));
        }
        let rows = [
            json!({"type":"session_meta","payload":{"id":"fixture","cwd":"test"}}),
            json!({"type":"response_item","payload":{"type":"agent_message","author":"/root/worker","recipient":"/root","content":content}}),
            json!({"type":"event_msg","payload":{"type":"agent_message","message":"UI assistant notification"}}),
        ];
        let original = rows
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&path, &original).unwrap();
        let session = provider.read_for_export(&reference).unwrap();
        assert_eq!(session.events.len(), 1);
        assert_eq!(session.resume_context_unavailable.is_some(), encrypted);
        let archive = serde_json::to_string(&session.events).unwrap();
        assert!(archive.contains("readable worker report"));
        assert!(archive.contains("from /root/worker to /root"));
        assert!(!archive.contains("opaque-secret"));
        assert!(!archive.contains("UI assistant notification"));
        assert_eq!(provider.read(&reference).is_err(), encrypted);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

fn reasoning_template(dir: &std::path::Path) -> CodexProvider {
    let home = dir.join("codex");
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
    CodexProvider::new(home)
}

#[test]
fn imported_reasoning_uses_text_context_without_losing_archive() {
    let dir = std::env::temp_dir().join(format!(
        "hb-reasoning-{}",
        uuid7(chrono::Utc::now().timestamp_millis(), "portable-reasoning")
    ));
    let provider = reasoning_template(&dir);
    let events = vec![
        Event::at(
            Some(1001),
            EventKind::Message {
                role: Role::User,
                text: "task".into(),
                source_kind: None,
            },
        ),
        Event::at(
            Some(1002),
            EventKind::Reasoning {
                text: "older reasoning".into(),
            },
        ),
        Event::at(
            Some(1003),
            EventKind::Reasoning {
                text: "retained reasoning Ω".into(),
            },
        ),
        Event::at(
            Some(1004),
            EventKind::ToolCall {
                call_id: "call-source".into(),
                name: "read".into(),
                arguments: r#"{"path":"test"}"#.into(),
            },
        ),
        Event::at(
            Some(1005),
            EventKind::ToolResult {
                call_id: "call-source".into(),
                text: "result".into(),
            },
        ),
        Event::at(
            Some(1006),
            EventKind::Message {
                role: Role::Assistant,
                text: "answer".into(),
                source_kind: None,
            },
        ),
        Event::at(
            Some(1007),
            EventKind::Meta {
                kind: "goal".into(),
                data: json!({"objective":"preserve the active goal"}),
            },
        ),
        Event::at(
            Some(1008),
            EventKind::Meta {
                kind: "tool-registry".into(),
                data: json!({"added":["read"]}),
            },
        ),
    ];
    // All readers normalize reasoning to text, including native Codex reads.
    // No provider's hidden reasoning state survives the normalized model.
    for source in ["zcode", "claude", "dsh", "agy", "codex"] {
        for checkpointed in [false, true] {
            let session = Session {
                source: source.into(),
                id: format!("{source}-{checkpointed}"),
                title: None,
                cwd: Some("test".into()),
                agent_preset: None,
                parent_session: None,
                origin: None,
                created_ms: 1000,
                updated_ms: 1007,
                events: events.clone(),
                resume_context_unavailable: None,
                resume_events: checkpointed.then(|| {
                    events
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| *i != 1)
                        .map(|(_, event)| event.clone())
                        .collect()
                }),
            };
            let outcome = provider.write(&session, &WriteOpts::default()).unwrap();
            let records: Vec<Value> = std::fs::read_to_string(&outcome.location)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(
                records
                    .iter()
                    .filter(|r| r["type"] == "response_item" && r["payload"]["type"] == "reasoning")
                    .count(),
                2
            );
            assert_eq!(
                records
                    .iter()
                    .filter(|r| r["payload"]["item"]["type"] == "Reasoning")
                    .count(),
                2
            );
            let checkpoint = records
                .iter()
                .rev()
                .find(|r| r["type"] == "compacted")
                .unwrap();
            let history = checkpoint["payload"]["replacement_history"]
                .as_array()
                .unwrap();
            assert!(history.iter().all(|item| item["type"] != "reasoning"));
            assert!(history.iter().any(|item| item["role"] == "developer"
                && item.to_string().contains("preserve the active goal")));
            assert!(history.iter().any(|item| item["role"] == "developer"
                && item.to_string().contains("Tools added: read")));
            assert!(history.iter().any(|item| item["type"] == "message"
                && item["role"] == "assistant"
                && item["content"][0]["text"] == "[Imported reasoning]\nretained reasoning Ω"));
            assert_eq!(
                history
                    .iter()
                    .any(|item| item.to_string().contains("older reasoning")),
                !checkpointed
            );
            assert!(history.iter().any(|item| item["type"] == "function_call"
                && item["call_id"] == "call-source"
                && item["arguments"] == r#"{"path":"test"}"#));
            assert!(history
                .iter()
                .any(|item| item["type"] == "function_call_output"
                    && item["call_id"] == "call-source"
                    && item["output"] == "result"));
            let reference = SessionRef {
                provider: "codex".into(),
                id: outcome.native_id,
                title: None,
                cwd: None,
                created_ms: None,
                updated_ms: None,
                locator: outcome.location,
                migrated: true,
            };
            let reread = provider.read(&reference).unwrap();
            assert_eq!(
                reread
                    .events
                    .iter()
                    .filter(|event| matches!(event.kind, EventKind::Reasoning { .. }))
                    .count(),
                2
            );
            assert!(reread
                .resume_events
                .as_ref()
                .unwrap()
                .iter()
                .all(|event| !matches!(event.kind, EventKind::Reasoning { .. })));
        }
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn zcode_reasoning_import_does_not_invent_native_hidden_state() {
    use crate::providers::zcode::ZcodeProvider;
    let dir = std::env::temp_dir().join(format!(
        "hb-zcode-reasoning-{}",
        uuid7(chrono::Utc::now().timestamp_millis(), "zcode-replay")
    ));
    let codex = reasoning_template(&dir);
    let db = dir.join("source.sqlite");
    let con = rusqlite::Connection::open(&db).unwrap();
    con.execute_batch("create table session(id text primary key,title text,directory text,time_created integer,time_updated integer,parent_id text);
        create table message(id text primary key,session_id text,sequence integer,data text);
        create table part(id text primary key,message_id text,session_id text,sequence integer,data text);
        insert into session values('z-source','fixture','test',1000,1005,null);").unwrap();
    for (i, role, part) in [
        (0, "user", json!({"type":"text","text":"task"})),
        (
            1,
            "assistant",
            json!({"type":"reasoning","text":"source reasoning","providerMetadata":{"opaque":"source-only-state"}}),
        ),
        (
            2,
            "assistant",
            json!({"type":"tool","callID":"original-call","tool":"read","state":{"status":"completed","input":{"path":"test"},"output":"output"}}),
        ),
        (3, "assistant", json!({"type":"text","text":"answer"})),
    ] {
        let id = format!("m{i}");
        con.execute(
            "insert into message values(?1,'z-source',?2,?3)",
            rusqlite::params![id, i, json!({"role":role}).to_string()],
        )
        .unwrap();
        con.execute(
            "insert into part values(?1,?1,'z-source',?2,?3)",
            rusqlite::params![id, i, part.to_string()],
        )
        .unwrap();
    }
    drop(con);
    let zcode = ZcodeProvider::new(db);
    let reference = zcode.discover().unwrap().remove(0);
    let session = zcode.read(&reference).unwrap();
    assert!(session.resume_events.is_none());
    assert!(session.events.iter().any(
        |event| matches!(&event.kind,EventKind::Reasoning {text} if text=="source reasoning")
    ));
    let outcome = codex.write(&session, &WriteOpts::default()).unwrap();
    let text = std::fs::read_to_string(outcome.location).unwrap();
    let checkpoint: Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
    let history = checkpoint["payload"]["replacement_history"]
        .as_array()
        .unwrap();
    assert!(history.iter().all(|item| item["type"] != "reasoning"));
    assert!(history
        .iter()
        .any(|item| item["content"][0]["text"] == "[Imported reasoning]\nsource reasoning"));
    assert!(history
        .iter()
        .any(|item| item["type"] == "function_call_output" && item["call_id"] == "original-call"));
    assert!(!text.contains("source-only-state"));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn dsh_replacements_become_a_native_resume_checkpoint() {
    let dir = std::env::temp_dir().join(format!(
        "hb-context-{}",
        uuid7(chrono::Utc::now().timestamp_millis(), "resume-test")
    ));
    std::fs::create_dir_all(dir.join("codex/sessions")).unwrap();
    let provider = CodexProvider::new(dir.join("codex"));
    let template = [
        json!({"type":"session_meta", "payload":{"originator":"Codex Desktop", "thread_source":"user", "base_instructions":{"text":"base"}}}),
        json!({"type":"turn_context", "payload":{"model":"gpt-test", "cwd":"test", "approval_policy":"never", "sandbox_policy":{"type":"danger-full-access"}, "effort":"low", "summary":"auto"}}),
    ];
    std::fs::write(
        dir.join("codex/sessions/template.jsonl"),
        template
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    let source = dir.join("source.jsonl");
    let old_output = "large obsolete output ".repeat(100_000);
    let rows = vec![
        json!({"type":"session", "id":"source", "cwd":"test"}),
        json!({"type":"turn/start", "time":1000, "seq":0, "data":{}}),
        json!({"type":"user/message", "time":1001, "seq":1, "surfaceOp":"append", "data":{"id":"u1", "source":{"kind":"user"}, "content":[{"type":"text", "text":"original task"}]}}),
        json!({"type":"assistant/message", "time":1002, "seq":2, "surfaceOp":"append", "data":{"message":{"content":[{"type":"text", "text":"old answer"}, {"type":"tool-call", "id":"c1", "name":"read", "arguments":"{}"}]}}}),
        json!({"type":"tool/call", "time":1003, "seq":3, "data":{"callId":"c1", "name":"read", "arguments":"{}"}}),
        json!({"type":"tool/result", "time":1004, "seq":4, "surfaceOp":"append", "data":{"message":{"toolCallId":"c1", "content":[{"type":"text", "text":old_output}]}}}),
        json!({"type":"user/message", "time":1005, "seq":5, "surfaceOp":"append", "data":{"id":"u2", "source":{"kind":"user"}, "content":[{"type":"text", "text":"keep this unsummarized request"}]}}),
        json!({"type":"compaction/summary", "time":1006, "seq":6, "data":{"compactionId":"cmp", "summary":[{"type":"text", "text":"task checkpoint"}], "shadowedSeqs":[1,2,4]}}),
        json!({"type":"user/message", "time":1007, "seq":7, "surfaceOp":{"op":"replace", "startSeq":1, "endSeq":4}, "data":{"id":"checkpoint", "source":{"kind":"compact-checkpoint", "compactionId":"cmp"}, "content":[{"type":"text", "text":"task checkpoint"}]}}),
        json!({"type":"assistant/message", "time":1008, "seq":8, "surfaceOp":"append", "data":{"message":{"content":[{"type":"text", "text":"recent answer"}, {"type":"tool-call", "id":"c2", "name":"read", "arguments":"{}"}]}}}),
        json!({"type":"tool/call", "time":1009, "seq":9, "data":{"callId":"c2", "name":"read", "arguments":"{}"}}),
        json!({"type":"tool/result", "time":1010, "seq":10, "surfaceOp":"append", "data":{"message":{"toolCallId":"c2", "content":[{"type":"text", "text":"obsolete tool output"}]}}}),
        json!({"type":"tool/result", "time":1011, "seq":11, "surfaceOp":{"op":"replace", "startSeq":10, "endSeq":10}, "data":{"message":{"toolCallId":"c2", "content":[{"type":"text", "text":"pruned tool output"}]}}}),
        json!({"type":"turn/end", "time":1012, "seq":12, "data":{}}),
    ];
    std::fs::write(
        &source,
        rows.iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    let reference = SessionRef {
        provider: "dsh".into(),
        id: "source".into(),
        title: None,
        cwd: None,
        created_ms: None,
        updated_ms: None,
        locator: source.to_string_lossy().into(),
        migrated: false,
    };
    let session = DshProvider::new(dir.join("dsh")).read(&reference).unwrap();
    let outcome = provider.write(&session, &WriteOpts::default()).unwrap();
    let text = std::fs::read_to_string(&outcome.location).unwrap();
    assert!(
        text.contains(&old_output),
        "visible transcript must retain the original tool output"
    );
    let records: Vec<Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let checkpoint = records
        .iter()
        .rev()
        .find(|r| r["type"] == "compacted")
        .expect("Codex must receive a native compaction checkpoint");
    let history = checkpoint["payload"]["replacement_history"]
        .as_array()
        .unwrap();
    let context = serde_json::to_string(history).unwrap();
    assert!(context.contains("task checkpoint"));
    assert!(context.contains("keep this unsummarized request"));
    assert!(context.contains("recent answer"));
    assert!(context.contains("pruned tool output"));
    assert!(!context.contains("obsolete"));
    assert!(!context.contains("old answer"));
    assert!(!context.contains("large obsolete output"));
    let calls: Vec<_> = history
        .iter()
        .filter(|r| r["type"] == "function_call")
        .collect();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["call_id"], "c2");
    let outputs: Vec<_> = history
        .iter()
        .filter(|r| r["type"] == "function_call_output")
        .collect();
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0]["call_id"], "c2");
    // The replacement belongs before the lower-sequence request that it did not summarize.
    let summary_pos = history
        .iter()
        .position(|r| r.to_string().contains("task checkpoint"))
        .unwrap();
    let request_pos = history
        .iter()
        .position(|r| r.to_string().contains("keep this unsummarized request"))
        .unwrap();
    assert!(summary_pos < request_pos);
    std::fs::remove_dir_all(&dir).unwrap();
}
