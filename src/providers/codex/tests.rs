use super::*;
use crate::providers::{dsh::DshProvider, Provider};

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
