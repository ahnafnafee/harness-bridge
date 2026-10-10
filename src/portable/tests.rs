use super::*;
use crate::{
    ir::{Event, EventKind, Role},
    providers::{codex::CodexProvider, dsh::DshProvider},
};

struct Temp(std::path::PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "hb-portable-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        assert!(self.0.starts_with(std::env::temp_dir()));
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

fn message(text: &str) -> Event {
    Event::at(
        Some(1791516000000),
        EventKind::Message {
            role: Role::User,
            text: text.into(),
            source_kind: Some("user".into()),
        },
    )
}

fn session(source: &str, id: &str, parent: Option<&str>) -> Session {
    Session {
        source: source.into(),
        id: id.into(),
        parent_session: parent.map(str::to_string),
        title: Some("Portable session Ω".into()),
        cwd: Some(r"F:\source-pc\project".into()),
        agent_preset: Some("original-preset".into()),
        origin: Some("desktop".into()),
        created_ms: 1791516000000,
        updated_ms: 1791516001000,
        events: vec![
            message("obsolete archive"),
            message("current task Ω"),
            Event::at(
                None,
                EventKind::Reasoning {
                    text: "reasoning".into(),
                },
            ),
            Event::at(
                None,
                EventKind::ToolCall {
                    call_id: "source-call".into(),
                    name: "read".into(),
                    arguments: r#"{"path":"F:\\source-pc\\file"}"#.into(),
                },
            ),
            Event::at(
                None,
                EventKind::ToolResult {
                    call_id: "source-call".into(),
                    text: "result".into(),
                },
            ),
            Event::at(
                None,
                EventKind::Compaction {
                    id: Some("checkpoint".into()),
                    text: Some("summary".into()),
                },
            ),
            Event::at(
                None,
                EventKind::Meta {
                    kind: "source-record".into(),
                    data: json!({"key":"value"}),
                },
            ),
        ],
        resume_events: Some(vec![message("current task Ω")]),
    }
}

fn bundle(sessions: Vec<Session>) -> anyhow::Result<Bundle> {
    Bundle::new(Payload {
        root_session_id: "root".into(),
        sessions,
    })
}

#[test]
fn portable_roundtrip_preserves_every_provider_archive_context_and_links() {
    let temp = Temp::new();
    for source in ["claude", "dsh", "codex", "zcode", "agy"] {
        let original = bundle(vec![
            session(source, "root", Some("external-parent")),
            session(source, "child", Some("root")),
            session(source, "grandchild", Some("child")),
        ])
        .unwrap();
        let path = temp.0.join(source).join("session.hbridge.json");
        let preview = original.save(&path, true).unwrap();
        assert_eq!(preview["sessions"], 3);
        assert!(!path.parent().unwrap().exists());
        original.save(&path, false).unwrap();
        let before = std::fs::read(&path).unwrap();
        assert!(original.save(&path, false).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let loaded = Bundle::load(&path).unwrap();
        assert_eq!(
            serde_json::to_value(&loaded.payload).unwrap(),
            serde_json::to_value(&original.payload).unwrap()
        );
        assert_eq!(loaded.root().id, "root");
        let children = loaded.children(&loaded.root()).unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].id, "child");
        assert_eq!(loaded.children(&children[0]).unwrap()[0].id, "grandchild");
        assert_eq!(
            loaded
                .read(&loaded.root())
                .unwrap()
                .parent_session
                .as_deref(),
            Some("external-parent")
        );
    }
    let mut empty_context = session("claude", "root", None);
    empty_context.resume_events = Some(vec![]);
    let empty = bundle(vec![empty_context]).unwrap();
    let path = temp.0.join("empty-context.hbridge.json");
    empty.save(&path, false).unwrap();
    assert!(Bundle::load(&path).unwrap().payload.sessions[0]
        .resume_events
        .as_ref()
        .unwrap()
        .is_empty());
}

#[test]
fn portable_import_rejects_corruption_versions_and_invalid_families() {
    let temp = Temp::new();
    let original = bundle(vec![session("claude", "root", None)]).unwrap();
    let path = temp.0.join("session.hbridge.json");
    let value = serde_json::to_value(&original).unwrap();
    for (mutated, expected) in [
        (
            {
                let mut v = value.clone();
                v["format"] = json!("other");
                v
            },
            "not a harness-bridge",
        ),
        (
            {
                let mut v = value.clone();
                v["version"] = json!(999);
                v["payload"] = json!("unknown schema");
                v
            },
            "unsupported portable session version",
        ),
        (
            {
                let mut v = value.clone();
                v["payload"]["sessions"][0]["title"] = json!("damaged");
                v
            },
            "checksum mismatch",
        ),
        (
            {
                let mut v = value.clone();
                v["payload"]["sessions"][0]["events"][0]["kind"] = json!("unsupported-kind");
                v["sha256"] = json!(checksum(&v["payload"]).unwrap());
                v
            },
            "invalid portable session schema",
        ),
    ] {
        std::fs::write(&path, serde_json::to_vec(&mutated).unwrap()).unwrap();
        let error = Bundle::load(&path).err().unwrap().to_string();
        assert!(error.contains(expected), "{error}");
    }
    std::fs::write(&path, b"{truncated").unwrap();
    assert!(Bundle::load(&path).is_err());
    for sessions in [
        vec![],
        vec![session("claude", "other", None)],
        vec![
            session("claude", "root", None),
            session("claude", "root", None),
        ],
        vec![
            session("claude", "root", None),
            session("claude", "child", None),
        ],
        vec![
            session("claude", "root", None),
            session("claude", "child", Some("missing")),
        ],
        vec![
            session("claude", "root", Some("child")),
            session("claude", "child", Some("root")),
        ],
        vec![
            session("claude", "root", None),
            session("claude", "child", Some("grandchild")),
            session("claude", "grandchild", Some("child")),
        ],
        vec![
            session("claude", "root", None),
            session("zcode", "child", Some("root")),
        ],
        vec![session("unknown", "root", None)],
        {
            let mut s = session("claude", "root", None);
            s.created_ms = i64::MAX;
            vec![s]
        },
    ] {
        assert!(bundle(sessions).is_err());
    }
}

#[test]
fn portable_family_preflight_and_dry_run_do_not_write_or_change_bundle() {
    let temp = Temp::new();
    let root = session("claude", "root", None);
    let mut child = session("claude", "child", Some("root"));
    child.resume_events = Some(vec![message(&"protected child prompt ".repeat(2000))]);
    let original = bundle(vec![root, child]).unwrap();
    let file = temp.0.join("family.hbridge.json");
    original.save(&file, false).unwrap();
    let before = std::fs::read(&file).unwrap();
    let loaded = Bundle::load(&file).unwrap();
    let target_path = temp.0.join("target-pc");
    let target = DshProvider::new(target_path.clone());
    assert!(crate::family::migrate(
        &loaded,
        &target,
        loaded.root(),
        &WriteOpts::default(),
        true,
        2000,
        true
    )
    .is_err());
    assert!(!target_path.exists());
    let preview = crate::family::migrate(
        &loaded,
        &target,
        loaded.root(),
        &WriteOpts {
            dry_run: true,
            ..WriteOpts::default()
        },
        true,
        100000,
        false,
    )
    .unwrap();
    assert_eq!(preview.extra["child_sessions"].as_array().unwrap().len(), 1);
    assert!(!target_path.exists());
    assert_eq!(std::fs::read(&file).unwrap(), before);
}

#[test]
fn portable_import_prunes_only_active_context_and_keeps_source_paths() {
    let temp = Temp::new();
    let mut root = session("zcode", "root", None);
    for event in &mut root.events {
        if let EventKind::ToolResult { text, .. } = &mut event.kind {
            *text = "complete large source output ".repeat(2000);
        }
    }
    root.resume_events = Some(
        root.events
            .iter()
            .filter(|e| {
                !matches!(
                    &e.kind,
                    EventKind::Compaction { .. } | EventKind::Meta { .. }
                ) && !matches!(&e.kind,EventKind::Message {text,..} if text=="obsolete archive")
            })
            .cloned()
            .collect(),
    );
    let original = bundle(vec![root]).unwrap();
    let file = temp.0.join("session.hbridge.json");
    original.save(&file, false).unwrap();
    let before = std::fs::read(&file).unwrap();
    let loaded = Bundle::load(&file).unwrap();
    let target_home = temp.0.join("target-codex");
    std::fs::create_dir_all(target_home.join("sessions")).unwrap();
    std::fs::write(target_home.join("sessions/template.jsonl"),[
        json!({"type":"session_meta","payload":{"originator":"Codex Desktop","thread_source":"user","base_instructions":{"text":"local template"}}}),
        json!({"type":"turn_context","payload":{"model":"gpt-test","cwd":"test","approval_policy":"never","sandbox_policy":{"type":"danger-full-access"}}}),
    ].iter().map(Value::to_string).collect::<Vec<_>>().join("\n")).unwrap();
    let target = CodexProvider::new(target_home);
    let outcome = crate::family::migrate(
        &loaded,
        &target,
        loaded.root(),
        &WriteOpts {
            cwd: Some("target-pc/project".into()),
            name: Some("Moved chat".into()),
            dry_run: false,
        },
        true,
        2048,
        true,
    )
    .unwrap();
    assert_eq!(outcome.extra["resume_context_policy"]["pruned"], true);
    let raw = std::fs::read_to_string(&outcome.location).unwrap();
    assert!(raw.contains("complete large source output complete large source output"));
    let rows: Vec<Value> = raw
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows[0]["payload"]["cwd"], "target-pc/project");
    let history = rows.last().unwrap()["payload"]["replacement_history"]
        .as_array()
        .unwrap();
    assert!(history.iter().all(|item| item["type"] != "reasoning"));
    let active = serde_json::to_string(history).unwrap();
    assert!(!active.contains("obsolete archive"));
    assert!(active.contains("output shortened"));
    assert!(active.contains("source-pc"));
    assert_eq!(std::fs::read(&file).unwrap(), before);
}

#[test]
fn portable_checksum_covers_all_payload_fields_and_ignores_json_layout() {
    let temp = Temp::new();
    let original = bundle(vec![session("claude", "root", None)]).unwrap();
    let mut value = serde_json::to_value(original).unwrap();
    fn reverse_objects(value: &mut Value) {
        match value {
            Value::Object(object) => {
                object.values_mut().for_each(reverse_objects);
                let reversed = object
                    .iter()
                    .rev()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                *object = reversed;
            }
            Value::Array(array) => array.iter_mut().for_each(reverse_objects),
            _ => {}
        }
    }
    reverse_objects(&mut value);
    let path = temp.0.join("reformatted.hbridge.json");
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    Bundle::load(&path).unwrap();
    value["payload"]["sessions"][0]["unexpected_field"] = json!("not part of the original payload");
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(Bundle::load(&path)
        .err()
        .unwrap()
        .to_string()
        .contains("checksum mismatch"));
}

#[test]
fn portable_nested_children_use_receiver_paths_and_depth() {
    use crate::providers::claude::ClaudeProvider;
    let temp = Temp::new();
    let foreign_dir = temp.0.join("exporting-pc-unavailable");
    let mut sessions = vec![
        session("claude", "root", None),
        session("claude", "child", Some("root")),
        session("claude", "grandchild", Some("child")),
    ];
    for child in sessions.iter_mut().skip(1) {
        child.events.push(Event::at(
            None,
            EventKind::Meta {
                kind: "migration-parent-depth".into(),
                data: json!({"depth":99,"location":foreign_dir.join("old-parent.jsonl")}),
            },
        ));
    }
    let file = temp.0.join("family.hbridge.json");
    bundle(sessions).unwrap().save(&file, false).unwrap();
    let loaded = Bundle::load(&file).unwrap();
    let codex_home = temp.0.join("receiving-codex");
    std::fs::create_dir_all(codex_home.join("sessions")).unwrap();
    std::fs::write(codex_home.join("sessions/template.jsonl"),[
        json!({"type":"session_meta","payload":{"originator":"Codex Desktop","thread_source":"user","base_instructions":{"text":"local template"}}}),
        json!({"type":"turn_context","payload":{"model":"gpt-test","cwd":"test","approval_policy":"never","sandbox_policy":{"type":"danger-full-access"}}}),
    ].iter().map(Value::to_string).collect::<Vec<_>>().join("\n")).unwrap();
    let claude_home = temp.0.join("receiving-claude");
    for (home, target) in [
        (
            claude_home.clone(),
            Box::new(ClaudeProvider::new(claude_home)) as Box<dyn Provider>,
        ),
        (
            codex_home.clone(),
            Box::new(CodexProvider::new(codex_home)) as Box<dyn Provider>,
        ),
    ] {
        let outcome = crate::family::migrate(
            &loaded,
            target.as_ref(),
            loaded.root(),
            &WriteOpts::default(),
            true,
            750000,
            false,
        )
        .unwrap();
        let children = outcome.extra["child_sessions"].as_array().unwrap();
        assert_eq!(children.len(), 2);
        for child in children {
            assert!(Path::new(child["location"].as_str().unwrap()).starts_with(&home));
        }
        if target.name() == "codex" {
            let raw = std::fs::read_to_string(children[1]["location"].as_str().unwrap()).unwrap();
            let meta: Value = serde_json::from_str(raw.lines().next().unwrap()).unwrap();
            assert_eq!(
                meta.pointer("/payload/source/subagent/thread_spawn/depth"),
                Some(&json!(2))
            );
        }
        assert!(!foreign_dir.exists());
    }
}
