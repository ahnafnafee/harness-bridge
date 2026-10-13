use super::*;
use crate::ir::{EventKind, Role};
use serde_json::json;
use std::io::Write;
use std::path::{Path, PathBuf};

mod chatgpt;
mod claude_web;
mod gemini_web;

/// A throwaway export folder that can also be zipped.
pub(super) struct Dir(pub PathBuf);

impl Dir {
    pub fn new(tag: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "hb-web-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("export")).unwrap();
        Dir(dir)
    }

    /// Root of the extracted export.
    pub fn root(&self) -> PathBuf {
        self.0.join("export")
    }

    pub fn write(&self, rel: &str, bytes: impl AsRef<[u8]>) -> &Self {
        let path = self.root().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
        self
    }

    pub fn json(&self, rel: &str, value: &serde_json::Value) -> &Self {
        self.write(rel, serde_json::to_string(value).unwrap())
    }

    /// Zips the export folder the way the web apps ship it.
    pub fn zip(&self) -> PathBuf {
        let path = self.0.join("export.zip");
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let mut stack = vec![self.root()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let entry = entry.unwrap().path();
                if entry.is_dir() {
                    stack.push(entry);
                    continue;
                }
                let name = entry
                    .strip_prefix(self.root())
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                writer
                    .start_file(name, zip::write::SimpleFileOptions::default())
                    .unwrap();
                writer.write_all(&std::fs::read(&entry).unwrap()).unwrap();
            }
        }
        writer.finish().unwrap();
        path
    }

    pub fn dest(&self) -> PathBuf {
        self.0.join("dest")
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Compact view of a session's events for assertions.
pub(super) fn kinds(events: &[crate::ir::Event]) -> Vec<String> {
    events
        .iter()
        .map(|e| match &e.kind {
            EventKind::TurnStart => "turn".into(),
            EventKind::TurnEnd { .. } => "end".into(),
            EventKind::Message {
                role: Role::User,
                text,
                ..
            } => format!("user: {text}"),
            EventKind::Message { role, text, .. } => {
                format!("{}: {text}", format!("{role:?}").to_lowercase())
            }
            EventKind::Reasoning { text } => format!("reasoning: {text}"),
            EventKind::ToolCall {
                call_id,
                name,
                arguments,
            } => format!("call {call_id} {name} {arguments}"),
            EventKind::ToolResult { call_id, text } => format!("result {call_id}: {text}"),
            EventKind::Compaction { .. } => "compaction".into(),
            EventKind::Meta { kind, .. } => format!("meta {kind}"),
        })
        .collect()
}

pub(super) fn read(provider: &WebProvider, query: &str) -> crate::ir::Session {
    let reference = provider
        .discover()
        .unwrap()
        .into_iter()
        .find(|r| r.id == query || r.title.as_deref() == Some(query))
        .unwrap_or_else(|| panic!("no conversation {query}"));
    provider.read(&reference).unwrap()
}

fn chatgpt_minimal() -> serde_json::Value {
    json!([{"title":"t","create_time":1.0,"update_time":2.0,"conversation_id":"c1","current_node":"a",
        "mapping":{"r":{"id":"r","message":null,"parent":null,"children":["a"]},
                   "a":{"id":"a","parent":"r","children":[],"message":{"id":"a","author":{"role":"user"},
                        "create_time":1.0,"content":{"content_type":"text","parts":["hi"]},"recipient":"all"}}}}])
}

#[test]
fn zip_folder_and_file_inputs_see_the_same_export() {
    let dir = Dir::new("inputs");
    dir.json("conversations.json", &chatgpt_minimal())
        .write("files/file-ABC123-photo one.png", b"png");
    let zip = dir.zip();
    let file = dir.root().join("conversations.json");
    for path in [zip.as_path(), dir.root().as_path(), file.as_path()] {
        let export = Export::open(path).unwrap();
        let mut files = export.files().to_vec();
        files.sort();
        assert_eq!(
            files,
            ["conversations.json", "files/file-ABC123-photo one.png"],
            "{}",
            path.display()
        );
        assert!(export
            .read_text("conversations.json")
            .unwrap()
            .contains("\"mapping\""));
        assert_eq!(
            export.find_prefix("file-ABC123"),
            Some("files/file-ABC123-photo one.png")
        );
        assert_eq!(
            export.find_prefix("file-ABC12"),
            None,
            "a prefix must end at a separator"
        );
        assert_eq!(
            export.find_name("file-ABC123-photo one.png"),
            Some("files/file-ABC123-photo one.png")
        );
    }
}

#[test]
fn unusable_inputs_fail_with_directions() {
    let dir = Dir::new("bad");
    let missing = Export::open(&dir.0.join("nope.zip"))
        .err()
        .unwrap()
        .to_string();
    assert!(missing.contains("cannot open export"), "{missing}");
    dir.write("takeout.tgz", b"x");
    let tgz = Export::open(&dir.root().join("takeout.tgz"))
        .err()
        .unwrap()
        .to_string();
    assert!(tgz.contains("extract"), "{tgz}");
}

#[test]
fn detection_names_each_format_and_explains_html_takeout() {
    let gpt = Dir::new("detect-gpt");
    gpt.json("conversations.json", &chatgpt_minimal());
    let claude = Dir::new("detect-claude");
    claude.json(
        "conversations.json",
        &json!([{"uuid":"u","name":"n","chat_messages":[]}]),
    );
    let gemini = Dir::new("detect-gemini");
    gemini.json(
        "Takeout/My Activity/Gemini Apps/MyActivity.json",
        &json!([]),
    );
    for (dir, expected) in [
        (&gpt, Format::ChatGpt),
        (&claude, Format::ClaudeWeb),
        (&gemini, Format::GeminiWeb),
    ] {
        let export = Export::open(&dir.zip()).unwrap();
        assert_eq!(Format::detect(&export).unwrap(), expected);
    }
    let html = Dir::new("detect-html");
    html.write("Takeout/My Activity/Gemini Apps/MyActivity.html", b"<html>");
    let error = Format::detect(&Export::open(&html.root()).unwrap())
        .unwrap_err()
        .to_string();
    assert!(error.contains("JSON"), "{error}");
    let unknown = Dir::new("detect-unknown");
    unknown.json("conversations.json", &json!([{"something":"else"}]));
    let error = Format::detect(&Export::open(&unknown.root()).unwrap())
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("not a ChatGPT or claude.ai export"),
        "{error}"
    );
    let empty = Dir::new("detect-empty");
    empty.write("readme.txt", b"x");
    let error = Format::detect(&Export::open(&empty.root()).unwrap())
        .unwrap_err()
        .to_string();
    assert!(error.contains("no conversations.json"), "{error}");
}

#[test]
fn a_provider_rejects_an_export_of_another_format() {
    let claude = Dir::new("mismatch");
    claude.json(
        "conversations.json",
        &json!([{"uuid":"u","name":"n","chat_messages":[]}]),
    );
    let provider = WebProvider::new(Format::ChatGpt, Some(claude.root()));
    let error = provider.discover().unwrap_err().to_string();
    assert!(error.contains("claude-web"), "{error}");
    let none = WebProvider::new(Format::ChatGpt, None);
    assert!(none
        .discover()
        .unwrap_err()
        .to_string()
        .contains("--export"));
}

#[test]
fn web_providers_are_read_only() {
    let dir = Dir::new("readonly");
    dir.json("conversations.json", &chatgpt_minimal());
    let provider = WebProvider::new(Format::ChatGpt, Some(dir.root()));
    let session = read(&provider, "c1");
    let error = provider
        .write(&session, &crate::ir::WriteOpts::default())
        .unwrap_err()
        .to_string();
    assert!(error.contains("no import"), "{error}");
}

#[test]
fn builder_pairs_tool_output_and_closes_turns() {
    let mut b = Builder::default();
    b.user(Some(1), "q1".into());
    b.call(Some(2), "c1", "dalle.text2im", "{}".into());
    b.result(Some(3), Some("c1"), "dalle", "img".into());
    b.result(Some(4), Some("other"), "dalle", "more".into());
    b.assistant(Some(5), "a1".into());
    b.call(Some(6), "c2", "python", "{}".into());
    b.user(Some(7), "q2".into());
    b.result(Some(8), None, "bio", "Model set context updated.".into());
    let events = b.finish();
    assert_eq!(
        kinds(&events),
        [
            "turn",
            "user: q1",
            "call c1 dalle_text2im {}",
            "result c1: img\n\nmore",
            "assistant: a1",
            "call c2 python {}",
            "result c2: [no output recorded in the export]",
            "end",
            "turn",
            "user: q2",
            "assistant: [bio] Model set context updated.",
            "end",
        ]
    );
}

#[test]
fn helpers_normalize_names_citations_html_and_times() {
    assert_eq!(
        safe_tool_name("canmore.create_textdoc"),
        "canmore_create_textdoc"
    );
    assert_eq!(safe_tool_name(""), "tool");
    assert_eq!(safe_tool_name(&"x".repeat(80)).len(), 64);
    assert_eq!(
        strip_citations("Paris\u{e200}cite\u{e202}turn0search1\u{e201} is big."),
        "Paris is big."
    );
    assert_eq!(
        html_to_text("<p>Hi &amp; <b>bye</b></p><ul><li>one</li><li>two&#33;</li></ul><pre><code>x &lt; 2</code></pre>"),
        "Hi & bye\n\n- one\n- two!\n\nx < 2"
    );
    assert_eq!(parse_time("2026-01-15T10:30:00.123Z"), Some(1768473000123));
    assert_eq!(parse_time("not a time"), None);
    assert_eq!(secs_ms(1760000000.5), 1760000000500);
}

#[test]
fn attachments_copy_once_from_zip_and_folder() {
    let dir = Dir::new("copy");
    dir.json("conversations.json", &chatgpt_minimal())
        .write("file-ABC123-photo.png", b"png-bytes");
    for (n, path) in [dir.zip(), dir.root()].iter().enumerate() {
        let export = Export::open(path).unwrap();
        let dest = dir.dest().join(n.to_string()).join("a/photo.png");
        assert!(export.copy_to("file-ABC123-photo.png", &dest).unwrap());
        assert_eq!(std::fs::read(&dest).unwrap(), b"png-bytes");
        assert!(
            !export.copy_to("file-ABC123-photo.png", &dest).unwrap(),
            "identical file is kept"
        );
    }
}

#[test]
fn the_registry_builds_web_providers_from_the_export_option() {
    let dir = Dir::new("registry");
    dir.json("conversations.json", &chatgpt_minimal());
    let homes = crate::providers::Homes {
        export: Some(dir.zip()),
        ..Default::default()
    };
    for name in ["chatgpt", "claude-web", "gemini-web"] {
        assert_eq!(
            crate::providers::provider_named(name, &homes)
                .unwrap()
                .name(),
            name
        );
    }
    let chatgpt = crate::providers::provider_named("chatgpt", &homes).unwrap();
    assert_eq!(chatgpt.discover().unwrap()[0].id, "c1");
    let unknown = crate::providers::provider_named("bard", &homes)
        .err()
        .unwrap()
        .to_string();
    assert!(unknown.contains("gemini-web"), "{unknown}");
}

#[test]
fn web_sessions_travel_in_portable_files() {
    let dir = Dir::new("portable");
    dir.json("conversations.json", &chatgpt_minimal());
    let provider = WebProvider::new(Format::ChatGpt, Some(dir.root()));
    let root = provider.discover().unwrap().remove(0);
    let bundle = crate::portable::Bundle::collect(&provider, root, false).unwrap();
    let file = dir.0.join("chat.hbridge.json");
    bundle.save(&file, false).unwrap();
    let loaded = crate::portable::Bundle::load(&file).unwrap();
    assert_eq!(loaded.root().provider, "chatgpt");
}

#[test]
fn a_web_chat_written_to_claude_reads_back_with_turns_and_tools() {
    use crate::providers::claude::ClaudeProvider;
    let dir = Dir::new("to-claude");
    dir.json(
        "conversations.json",
        &json!([{"uuid":"k1","name":"Weather","created_at":"2026-02-01T10:00:00Z","updated_at":"2026-02-01T10:01:00Z",
            "chat_messages":[
                {"uuid":"q","sender":"human","created_at":"2026-02-01T10:00:00Z","content":[{"type":"text","text":"weather?"}]},
                {"uuid":"a","sender":"assistant","created_at":"2026-02-01T10:00:30Z","content":[
                    {"type":"tool_use","id":"toolu_1","name":"web_search","input":{"query":"weather"}},
                    {"type":"tool_result","tool_use_id":"toolu_1","content":[{"type":"text","text":"Sunny"}]},
                    {"type":"text","text":"It is sunny."}]}]}]),
    );
    let session = read(&WebProvider::new(Format::ClaudeWeb, Some(dir.root())), "k1");
    let claude = ClaudeProvider::new(dir.0.join("claude-home"));
    let outcome = claude
        .write(
            &session,
            &crate::ir::WriteOpts {
                cwd: Some(dir.dest().to_string_lossy().into()),
                ..Default::default()
            },
        )
        .unwrap();
    let reference = claude
        .discover()
        .unwrap()
        .into_iter()
        .find(|r| r.id == outcome.native_id)
        .unwrap();
    let back = kinds(&claude.read(&reference).unwrap().events).join("\n");
    for expected in ["weather?", "web_search", "Sunny", "It is sunny."] {
        assert!(back.contains(expected), "{expected} missing from:\n{back}");
    }
}

#[test]
fn attachment_copies_report_the_plan_then_skip_identical_files() {
    let dir = Dir::new("copy-report");
    dir.json(
        "conversations.json",
        &json!([{"uuid":"k1","name":"Files","chat_messages":[
            {"uuid":"q","sender":"human","created_at":"2026-02-01T10:00:00Z","content":[{"type":"text","text":"see"}],
             "files":[{"file_name":"photo.png"}]}]}]),
    )
    .write("photo.png", b"png");
    let provider = WebProvider::new(Format::ClaudeWeb, Some(dir.zip()));
    let reference = provider.discover().unwrap().remove(0);
    let dest = dir.dest();
    let plan = provider.copy_attachments(&reference, &dest, true).unwrap();
    assert_eq!(
        (plan["files"].as_u64(), plan["copied"].as_u64()),
        (Some(1), Some(0))
    );
    assert!(!dest.join("claude-web-attachments/k1/photo.png").exists());
    let first = provider.copy_attachments(&reference, &dest, false).unwrap();
    assert_eq!(first["copied"], 1);
    assert_path_exists(&dest.join("claude-web-attachments/k1/photo.png"));
    let second = provider.copy_attachments(&reference, &dest, false).unwrap();
    assert_eq!(
        (
            second["copied"].as_u64(),
            second["already_present"].as_u64()
        ),
        (Some(0), Some(1))
    );
}

pub(super) fn assert_path_exists(path: &Path) {
    assert!(path.exists(), "missing {}", path.display());
}
