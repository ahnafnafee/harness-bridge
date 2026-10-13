use super::*;

const ACTIVITY: &str = "Takeout/My Activity/Gemini Apps/MyActivity.json";

fn prompted(time: &str, url: Option<&str>, prompt: &str, html: &str) -> serde_json::Value {
    let mut r = json!({"header":"Gemini Apps", "title":format!("Prompted {prompt}"), "time":time,
        "products":["Gemini Apps"], "safeHtmlItem":[{"html":html}]});
    if let Some(url) = url {
        r["titleUrl"] = json!(url);
    }
    r
}

fn provider(dir: &Dir) -> WebProvider {
    WebProvider::new(Format::GeminiWeb, Some(dir.root()))
}

#[test]
fn records_group_by_conversation_url_in_time_order() {
    let dir = Dir::new("gem-url");
    // Takeout lists activity newest first.
    dir.json(
        ACTIVITY,
        &json!([
            prompted(
                "2026-03-01T10:05:00Z",
                Some("https://gemini.google.com/app/abc123"),
                "and in metric?",
                "<p>About 4 cm.</p>"
            ),
            prompted(
                "2026-03-02T09:00:00Z",
                Some("https://gemini.google.com/app/c/def456"),
                "capital of France?",
                "<p>Paris.</p>"
            ),
            prompted(
                "2026-03-01T10:00:00Z",
                Some("https://gemini.google.com/app/abc123"),
                "how long is a paperclip?",
                "<p>About <b>1.5 inches</b>.</p>"
            ),
        ]),
    );
    let p = provider(&dir);
    let refs = p.discover().unwrap();
    assert_eq!(
        refs.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        ["def456", "abc123"]
    );
    assert_eq!(refs[1].title.as_deref(), Some("how long is a paperclip?"));
    let session = read(&p, "abc123");
    assert_eq!(session.source, "gemini-web");
    assert_eq!(
        kinds(&session.events),
        [
            "turn",
            "user: how long is a paperclip?",
            "assistant: About 1.5 inches.",
            "end",
            "turn",
            "user: and in metric?",
            "assistant: About 4 cm.",
            "end",
        ]
    );
}

#[test]
fn records_without_a_url_group_by_time_gaps() {
    let dir = Dir::new("gem-gaps");
    dir.json(
        ACTIVITY,
        &json!([
            prompted("2026-03-01T12:00:00Z", None, "unrelated later", "<p>c</p>"),
            prompted("2026-03-01T10:20:00Z", None, "follow up", "<p>b</p>"),
            prompted("2026-03-01T10:00:00Z", None, "start", "<p>a</p>"),
        ]),
    );
    let p = provider(&dir);
    let refs = p.discover().unwrap();
    assert_eq!(refs.len(), 2);
    let first = refs
        .iter()
        .find(|r| r.title.as_deref() == Some("start"))
        .unwrap();
    let k = kinds(&p.read(first).unwrap().events);
    assert!(k.contains(&"user: follow up".to_string()), "{k:?}");
    assert!(!k.contains(&"user: unrelated later".to_string()), "{k:?}");
}

#[test]
fn all_three_record_shapes_parse_and_empty_records_are_skipped() {
    let dir = Dir::new("gem-shapes");
    let url = "https://gemini.google.com/app/c/zz9";
    dir.json(
        ACTIVITY,
        &json!([
            {"header":"Gemini","title":"Used Gemini Apps","titleUrl":url,"time":"2026-03-01T10:02:00Z",
             "userInteractions":[{"userInteraction":{"endpoint":2,
                "request":"[[\"third prompt\"]]","response":"[[[\"third answer\"]]]"}}]},
            {"header":"Gemini","title":"Used Gemini Apps","titleUrl":url,"time":"2026-03-01T10:01:00Z",
             "details":[{"name":"Request","value":"second prompt"},{"name":"Response","value":"second answer"}]},
            {"header":"Gemini Apps","title":"Gave feedback","titleUrl":url,"time":"2026-03-01T10:00:30Z"},
            prompted("2026-03-01T10:00:00Z", Some(url), "first prompt", "<p>first &amp; answer</p>"),
        ]),
    );
    assert_eq!(
        kinds(&read(&provider(&dir), "zz9").events)
            .into_iter()
            .filter(|k| k.starts_with("user") || k.starts_with("assistant"))
            .collect::<Vec<_>>(),
        [
            "user: first prompt",
            "assistant: first & answer",
            "user: second prompt",
            "assistant: second answer",
            "user: third prompt",
            "assistant: third answer",
        ]
    );
}

#[test]
fn uploads_and_generated_images_are_referenced() {
    let dir = Dir::new("gem-files");
    let mut record = prompted(
        "2026-03-01T10:00:00Z",
        Some("https://gemini.google.com/app/img1"),
        "draw a cat",
        "<p>Here it is.</p>",
    );
    record["attachedFiles"] = json!(["sketch.jpg", "gone.png"]);
    record["imageFile"] = json!("generated-cat.png");
    dir.json(ACTIVITY, &json!([record]))
        .write("Takeout/My Activity/Gemini Apps/sketch.jpg", b"jpg")
        .write("Takeout/My Activity/Gemini Apps/generated-cat.png", b"png");
    let k = kinds(&read(&provider(&dir), "img1").events);
    assert_eq!(
        k[1],
        "user: draw a cat\n[file: gemini-web-attachments/img1/sketch.jpg]\n[file gone.png was not included in the export]"
    );
    assert_eq!(
        k[2],
        "assistant: Here it is.\n[image: gemini-web-attachments/img1/generated-cat.png]"
    );
}
