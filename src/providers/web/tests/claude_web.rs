use super::*;

const ROOT: &str = "00000000-0000-4000-8000-000000000000";

fn message(
    uuid: &str,
    parent: Option<&str>,
    sender: &str,
    minute: u32,
    content: serde_json::Value,
) -> serde_json::Value {
    let mut m = json!({"uuid":uuid, "sender":sender, "text":"",
        "created_at":format!("2026-02-01T10:{minute:02}:00.000Z"), "content":content,
        "attachments":[], "files":[]});
    if let Some(parent) = parent {
        m["parent_message_uuid"] = json!(parent);
    }
    m
}

fn text(s: &str) -> serde_json::Value {
    json!([{"type":"text","text":s}])
}

fn conversation(uuid: &str, name: &str, messages: Vec<serde_json::Value>) -> serde_json::Value {
    json!({"uuid":uuid, "name":name, "created_at":"2026-02-01T10:00:00.000Z",
           "updated_at":"2026-02-01T11:00:00.000Z", "chat_messages":messages})
}

fn provider(dir: &Dir) -> WebProvider {
    WebProvider::new(Format::ClaudeWeb, Some(dir.root()))
}

#[test]
fn an_edited_prompt_resolves_to_the_current_leaf() {
    let dir = Dir::new("cw-branch");
    let mut c = conversation(
        "k1",
        "Edits",
        vec![
            message("q1", Some(ROOT), "human", 1, text("first wording")),
            message("a1", Some("q1"), "assistant", 2, text("answer to first")),
            message("q2", Some(ROOT), "human", 3, text("edited wording")),
            message("a2", Some("q2"), "assistant", 4, text("answer to edit")),
            message(
                "a3",
                Some("q1"),
                "assistant",
                5,
                text("late retry of first"),
            ),
        ],
    );
    c["current_leaf_message_uuid"] = json!("a2");
    dir.json("conversations.json", &json!([c]));
    let session = read(&provider(&dir), "k1");
    assert_eq!(session.source, "claude-web");
    assert_eq!(
        kinds(&session.events),
        [
            "turn",
            "user: edited wording",
            "assistant: answer to edit",
            "end"
        ]
    );
    assert_eq!(
        session.events[1].time_ms,
        Some(parse_time("2026-02-01T10:03:00.000Z").unwrap())
    );
}

#[test]
fn without_a_current_leaf_the_latest_leaf_wins_and_flat_exports_keep_order() {
    let dir = Dir::new("cw-leaf");
    let tree = conversation(
        "k1",
        "Tree",
        vec![
            message("q1", Some(ROOT), "human", 1, text("first wording")),
            message("a1", Some("q1"), "assistant", 2, text("answer to first")),
            message("q2", Some(ROOT), "human", 3, text("edited wording")),
            message("a2", Some("q2"), "assistant", 4, text("answer to edit")),
        ],
    );
    let flat = conversation(
        "k2",
        "Flat",
        vec![
            message("x1", None, "human", 1, text("hello")),
            message("x2", None, "assistant", 2, text("hi")),
        ],
    );
    dir.json("conversations.json", &json!([tree, flat]));
    let p = provider(&dir);
    assert_eq!(kinds(&read(&p, "k1").events)[1], "user: edited wording");
    assert_eq!(
        kinds(&read(&p, "k2").events),
        ["turn", "user: hello", "assistant: hi", "end"]
    );
}

#[test]
fn blocks_become_text_reasoning_and_paired_tools() {
    let dir = Dir::new("cw-blocks");
    let assistant = json!([
        {"type":"thinking","thinking":"Check the weather tool."},
        {"type":"tool_use","id":"toolu_1","name":"web_search","input":{"query":"weather"}},
        {"type":"tool_result","tool_use_id":"toolu_1","name":"web_search","content":[{"type":"text","text":"Sunny"}]},
        {"type":"tool_use","name":"artifacts","input":{"command":"create","title":"Plan"}},
        {"type":"tool_result","name":"artifacts","content":[{"type":"text","text":"OK"}]},
        {"type":"text","text":"It is sunny."}
    ]);
    let mut plain = message("q2", Some("a1"), "human", 3, json!(null));
    plain["text"] = json!("text-only follow up");
    plain.as_object_mut().unwrap().remove("content");
    dir.json(
        "conversations.json",
        &json!([conversation(
            "k1",
            "Blocks",
            vec![
                message("q1", Some(ROOT), "human", 1, text("weather?")),
                message("a1", Some("q1"), "assistant", 2, assistant),
                plain
            ]
        )]),
    );
    assert_eq!(
        kinds(&read(&provider(&dir), "k1").events),
        [
            "turn",
            "user: weather?",
            "reasoning: Check the weather tool.",
            "call toolu_1 web_search {\"query\":\"weather\"}",
            "result toolu_1: Sunny",
            "call a1-3 artifacts {\"command\":\"create\",\"title\":\"Plan\"}",
            "result a1-3: OK",
            "assistant: It is sunny.",
            "end",
            "turn",
            "user: text-only follow up",
            "end",
        ]
    );
}

#[test]
fn attachments_inline_extracted_text_and_reference_files() {
    let dir = Dir::new("cw-files");
    let mut q = message("q1", Some(ROOT), "human", 1, text("summarize these"));
    q["attachments"] = json!([{"file_name":"notes.txt","file_type":"txt","extracted_content":"line one\nline two"}]);
    q["files"] = json!([{"file_name":"photo.png"}, {"file_name":"lost.pdf"}]);
    dir.json(
        "conversations.json",
        &json!([conversation("k1", "", vec![q])]),
    )
    .write("photo.png", b"png");
    let p = provider(&dir);
    let session = read(&p, "k1");
    assert_eq!(
        kinds(&session.events)[1],
        "user: summarize these\n[Attached file: notes.txt]\nline one\nline two\n[file: claude-web-attachments/k1/photo.png]\n[file lost.pdf was not included in the export]"
    );
    assert_eq!(
        session.title.as_deref(),
        Some("summarize these"),
        "an empty name falls back to the first prompt"
    );
    let reference = p.discover().unwrap().remove(0);
    assert_eq!(
        p.attachments(&reference).unwrap()[0].target,
        "claude-web-attachments/k1/photo.png"
    );
}
