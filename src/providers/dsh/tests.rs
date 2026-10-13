use super::*;
use crate::providers::Provider;

#[test]
fn native_folder_prefixes_and_parent_ids_preserve_legacy_discovery() {
    let home = std::env::temp_dir().join(format!(
        "hb-dsh-native-{}",
        uuid7(chrono::Utc::now().timestamp_millis(), "native-discovery")
    ));
    let root = "00000000-0000-7000-8000-000000000001";
    let legacy_child = "00000000-0000-7000-8000-000000000002";
    let native_child = "00000000-0000-7000-8000-000000000003";
    let cache = home.join("storages/session_projcache/sessions");
    std::fs::create_dir_all(&cache).unwrap();
    for (id, prefixed, parent, filename) in [
        (root, true, None, "session.v4.jsonl.zstd"),
        (
            legacy_child,
            false,
            Some(format!("session-{root}")),
            "session.v4.jsonl",
        ),
        (
            native_child,
            true,
            Some(root.to_string()),
            "session.jsonl.zstd",
        ),
        ("invalid", true, None, "session.v4.jsonl"),
    ] {
        let native_id = if prefixed {
            format!("session-{id}")
        } else {
            id.to_string()
        };
        let directory = home.join("sessions/project").join(&native_id);
        std::fs::create_dir_all(&directory).unwrap();
        let records = [
            json!({"type":"session","version":4,"id":native_id,"cwd":"project","parentSession":parent}),
            json!({"type":"user/message","time":1000,"data":{"id":"request","content":[{"type":"text","text":"retained task"}]}}),
        ];
        let text = records
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let bytes = if filename.ends_with(".zstd") {
            zstd::stream::encode_all(text.as_bytes(), 1).unwrap()
        } else {
            text.into_bytes()
        };
        std::fs::write(directory.join(filename), bytes).unwrap();
        std::fs::write(
            cache.join(format!("{native_id}.json")),
            json!({"record":{"rows":{"title":{"val":format!("title-{id}")}}}}).to_string(),
        )
        .unwrap();
    }
    let provider = DshProvider::new(home.clone());
    let references = provider.discover().unwrap();
    assert_eq!(references.len(), 3);
    let root_ref = references.iter().find(|r| r.id == root).unwrap();
    assert_eq!(
        root_ref.title.as_deref(),
        Some(format!("title-{root}").as_str())
    );
    assert!(root_ref.locator.contains(&format!("session-{root}")));
    assert_eq!(
        provider
            .read(root_ref)
            .unwrap()
            .first_user_text()
            .as_deref(),
        Some("retained task")
    );
    let children = provider.children(root_ref).unwrap();
    assert_eq!(children.len(), 2);
    for child in children {
        assert!(child.id == legacy_child || child.id == native_child);
        assert_eq!(
            provider.read(&child).unwrap().parent_session.as_deref(),
            Some(root)
        );
        assert!(provider.children(&child).unwrap().is_empty());
    }
    std::fs::remove_dir_all(home).unwrap();
}
