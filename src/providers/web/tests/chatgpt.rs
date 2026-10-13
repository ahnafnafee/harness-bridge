use super::*;

/// Builds a ChatGPT `mapping` from (id, parent, message) triples.
fn mapping(nodes: &[(&str, Option<&str>, serde_json::Value)]) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (id, parent, message) in nodes {
        let children: Vec<&str> = nodes
            .iter()
            .filter(|(_, p, _)| p == &Some(*id))
            .map(|(c, _, _)| *c)
            .collect();
        map.insert(
            id.to_string(),
            json!({"id": id, "parent": parent, "children": children,
                   "message": if message.is_null() { serde_json::Value::Null } else {
                       let mut m = message.clone(); m["id"] = json!(id); m }}),
        );
    }
    serde_json::Value::Object(map)
}

fn msg(role: &str, t: f64, content: serde_json::Value) -> serde_json::Value {
    json!({"author":{"role":role,"name":null}, "create_time":t, "content":content, "recipient":"all", "metadata":{}})
}

fn text(s: &str) -> serde_json::Value {
    json!({"content_type":"text","parts":[s]})
}

fn conversation(
    id: &str,
    title: &str,
    current: Option<&str>,
    map: serde_json::Value,
) -> serde_json::Value {
    let mut c = json!({"title":title,"create_time":100.0,"update_time":200.0,"conversation_id":id,"mapping":map});
    if let Some(current) = current {
        c["current_node"] = json!(current);
    }
    c
}

fn provider(dir: &Dir) -> WebProvider {
    WebProvider::new(Format::ChatGpt, Some(dir.root()))
}

#[test]
fn the_visible_branch_follows_current_node() {
    let dir = Dir::new("gpt-branch");
    let map = mapping(&[
        ("r", None, json!(null)),
        ("q", Some("r"), msg("user", 101.0, text("question"))),
        (
            "old",
            Some("q"),
            msg("assistant", 102.0, text("first draft")),
        ),
        (
            "new",
            Some("q"),
            msg("assistant", 103.0, text("regenerated")),
        ),
    ]);
    dir.json(
        "conversations.json",
        &json!([conversation("c1", "Branchy", Some("new"), map)]),
    );
    let session = read(&provider(&dir), "c1");
    assert_eq!(session.source, "chatgpt");
    assert_eq!(session.title.as_deref(), Some("Branchy"));
    assert_eq!(
        kinds(&session.events),
        ["turn", "user: question", "assistant: regenerated", "end"]
    );
    assert_eq!(session.events[1].time_ms, Some(101000));
}

#[test]
fn without_current_node_the_newest_child_is_followed() {
    let dir = Dir::new("gpt-nocurrent");
    let map = mapping(&[
        ("r", None, json!(null)),
        ("q", Some("r"), msg("user", 101.0, text("question"))),
        (
            "old",
            Some("q"),
            msg("assistant", 102.0, text("first draft")),
        ),
        (
            "new",
            Some("q"),
            msg("assistant", 103.0, text("regenerated")),
        ),
    ]);
    dir.json(
        "conversations.json",
        &json!([conversation("c1", "t", None, map)]),
    );
    let session = read(&provider(&dir), "c1");
    assert!(kinds(&session.events).contains(&"assistant: regenerated".to_string()));
}

#[test]
fn content_types_render_and_hidden_records_are_skipped() {
    let dir = Dir::new("gpt-content");
    let mut hidden = msg("system", 100.5, text("system prompt"));
    hidden["metadata"]["is_visually_hidden_from_conversation"] = json!(true);
    let mut python = msg(
        "assistant",
        103.0,
        json!({"content_type":"code","language":"python","text":"2+2"}),
    );
    python["recipient"] = json!("python");
    let mut browse = msg(
        "assistant",
        106.0,
        json!({"content_type":"code","text":"search(\"fridges\")"}),
    );
    browse["recipient"] = json!("web.run");
    let mut tool_out = msg(
        "tool",
        104.0,
        json!({"content_type":"execution_output","text":"4"}),
    );
    tool_out["author"]["name"] = json!("python");
    let mut display = msg(
        "tool",
        107.0,
        json!({"content_type":"tether_browsing_display","result":"Energy Star says 450 kWh."}),
    );
    display["author"]["name"] = json!("web");
    let mut quote = msg(
        "tool",
        108.0,
        json!({"content_type":"tether_quote","title":"Energy Star","url":"https://example.org","text":"450 kWh"}),
    );
    quote["author"]["name"] = json!("web");
    let map = mapping(&[
        ("r", None, json!(null)),
        ("s", Some("r"), hidden),
        (
            "ctx",
            Some("s"),
            msg(
                "user",
                100.7,
                json!({"content_type":"user_editable_context","user_instructions":"Be brief."}),
            ),
        ),
        (
            "q",
            Some("ctx"),
            msg(
                "user",
                101.0,
                json!({"content_type":"multimodal_text","parts":[
            {"content_type":"audio_transcription","text":"spoken question"}, "typed question"]}),
            ),
        ),
        (
            "th",
            Some("q"),
            msg(
                "assistant",
                102.0,
                json!({"content_type":"thoughts","thoughts":[{"summary":"Plan","content":"Add the numbers."}]}),
            ),
        ),
        (
            "recap",
            Some("th"),
            msg(
                "assistant",
                102.5,
                json!({"content_type":"reasoning_recap","content":"Thought for 2s"}),
            ),
        ),
        ("py", Some("recap"), python),
        ("out", Some("py"), tool_out),
        (
            "ans",
            Some("out"),
            msg(
                "assistant",
                105.0,
                text("It is 4.\u{e200}cite\u{e202}turn0\u{e201}"),
            ),
        ),
        ("web", Some("ans"), browse),
        ("disp", Some("web"), display),
        ("quote", Some("disp"), quote),
        (
            "err",
            Some("quote"),
            msg(
                "assistant",
                109.0,
                json!({"content_type":"system_error","name":"ToolError","text":"timed out"}),
            ),
        ),
        (
            "odd",
            Some("err"),
            msg("assistant", 110.0, json!({"content_type":"brand_new_type"})),
        ),
    ]);
    dir.json(
        "conversations.json",
        &json!([conversation("c1", "t", Some("odd"), map)]),
    );
    let k = kinds(&read(&provider(&dir), "c1").events);
    assert_eq!(
        k,
        [
            "turn",
            "user: spoken question\ntyped question",
            "reasoning: Plan\nAdd the numbers.",
            "call py python {\"input\":\"2+2\"}",
            "result py: 4",
            "assistant: It is 4.",
            "call web web_run {\"input\":\"search(\\\"fridges\\\")\"}",
            "result web: Energy Star says 450 kWh.\n\nEnergy Star\nhttps://example.org\n450 kWh",
            "assistant: [ToolError] timed out",
            "assistant: [brand_new_type content was not converted]",
            "end",
        ]
    );
}

#[test]
fn json_tool_arguments_pass_through() {
    let dir = Dir::new("gpt-json");
    let mut dalle = msg(
        "assistant",
        102.0,
        json!({"content_type":"code","text":"{\"prompt\":\"a fridge\"}"}),
    );
    dalle["recipient"] = json!("dalle.text2im");
    let map = mapping(&[
        ("r", None, json!(null)),
        ("q", Some("r"), msg("user", 101.0, text("draw"))),
        ("d", Some("q"), dalle),
    ]);
    dir.json(
        "conversations.json",
        &json!([conversation("c1", "t", Some("d"), map)]),
    );
    let k = kinds(&read(&provider(&dir), "c1").events);
    assert!(
        k.contains(&"call d dalle_text2im {\"prompt\":\"a fridge\"}".to_string()),
        "{k:?}"
    );
    assert!(
        k.contains(&"result d: [no output recorded in the export]".to_string()),
        "{k:?}"
    );
}

#[test]
fn attachments_resolve_to_destination_paths() {
    let dir = Dir::new("gpt-files");
    let mut upload = msg(
        "user",
        101.0,
        json!({"content_type":"multimodal_text","parts":[
        {"content_type":"image_asset_pointer","asset_pointer":"file-service://file-ABC123","width":10,"height":10},
        {"content_type":"image_asset_pointer","asset_pointer":"sediment://file_00ff","width":10,"height":10},
        "what is this?"]}),
    );
    upload["metadata"]["attachments"] = json!([{"id":"file-DOC9","name":"report.pdf"}]);
    let map = mapping(&[("r", None, json!(null)), ("q", Some("r"), upload)]);
    dir.json(
        "conversations.json",
        &json!([conversation("c1", "t", Some("q"), map)]),
    )
    .write("file-ABC123-label.png", b"png")
    .write("user-x/file-DOC9-report.pdf", b"pdf");
    let p = provider(&dir);
    let k = kinds(&read(&p, "c1").events);
    assert_eq!(
        k[1],
        "user: [image: chatgpt-attachments/c1/file-ABC123-label.png]\n[image file_00ff was not included in the export]\nwhat is this?\n[file: chatgpt-attachments/c1/file-DOC9-report.pdf]"
    );
    let reference = p.discover().unwrap().remove(0);
    let plan = p.attachments(&reference).unwrap();
    assert_eq!(
        plan.iter()
            .map(|a| (a.entry.as_str(), a.target.as_str()))
            .collect::<Vec<_>>(),
        [
            (
                "file-ABC123-label.png",
                "chatgpt-attachments/c1/file-ABC123-label.png"
            ),
            (
                "user-x/file-DOC9-report.pdf",
                "chatgpt-attachments/c1/file-DOC9-report.pdf"
            ),
        ]
    );
}

#[test]
fn broken_trees_fail_with_the_node_id() {
    let dir = Dir::new("gpt-broken");
    let missing = mapping(&[
        ("r", None, json!(null)),
        ("q", Some("r"), msg("user", 1.0, text("x"))),
    ]);
    let mut cycle = mapping(&[
        ("a", Some("b"), msg("user", 1.0, text("x"))),
        ("b", Some("a"), msg("user", 2.0, text("y"))),
    ]);
    cycle["a"]["parent"] = json!("b");
    dir.json(
        "conversations.json",
        &json!([
            conversation("c1", "missing", Some("ghost"), missing),
            conversation("c2", "cycle", Some("a"), cycle)
        ]),
    );
    let p = provider(&dir);
    for (query, expected) in [("c1", "ghost"), ("c2", "cycle")] {
        let reference = p
            .discover()
            .unwrap()
            .into_iter()
            .find(|r| r.id == query)
            .unwrap();
        let error = p.read(&reference).unwrap_err().to_string();
        assert!(error.contains(expected), "{error}");
    }
}

#[test]
fn discovery_lists_newest_first_across_shards() {
    let dir = Dir::new("gpt-shards");
    let one = |id: &str, t: f64| {
        let mut c = conversation(
            id,
            id,
            Some("q"),
            mapping(&[("q", None, msg("user", t, text("x")))]),
        );
        c["create_time"] = json!(t);
        c["update_time"] = json!(t + 1.0);
        c
    };
    dir.json("conversations.json", &json!([one("old", 10.0)]))
        .json("conversations-001.json", &json!([one("new", 20.0)]));
    let refs = provider(&dir).discover().unwrap();
    assert_eq!(
        refs.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        ["new", "old"]
    );
    assert_eq!(refs[0].created_ms, Some(20000));
    assert_eq!(refs[0].updated_ms, Some(21000));
    assert_eq!(refs[0].provider, "chatgpt");
}
