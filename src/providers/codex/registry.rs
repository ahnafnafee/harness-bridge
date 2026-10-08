//! Desktop sidebar registration and the deterministic import ledger.

use super::{content_texts, join_text, parse_iso_ms, CodexProvider};
use crate::util::{extended_path, iso_ms, read_maybe_zstd};
use serde_json::{json, Map, Value};
use std::path::Path;

impl CodexProvider {
    /// Register the thread so the desktop app lists it: session_index.jsonl,
    /// state_5.sqlite `threads`, and the import ledger.
    pub(super) fn register(
        &self,
        rollout: &Path,
        thread_id: &str,
        name: &str,
        ledger_key: &str,
    ) -> anyhow::Result<()> {
        // --- session_index.jsonl ---
        let idx = self.codex_home.join("session_index.jsonl");
        if idx.exists() {
            let bak = idx.with_file_name("session_index.jsonl.bak-harness-bridge");
            if !bak.exists() {
                let _ = std::fs::copy(&idx, &bak);
            }
        }
        let lines_raw = std::fs::read_to_string(&idx).unwrap_or_default();
        let mut entries: Vec<String> = lines_raw
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter(|l| {
                serde_json::from_str::<Value>(l)
                    .map(|e| e.get("id").and_then(|i| i.as_str()) != Some(thread_id))
                    .unwrap_or(true)
            })
            .map(str::to_string)
            .collect();
        let last_ts = {
            let text = read_maybe_zstd(rollout)?;
            text.lines()
                .last()
                .and_then(|l| serde_json::from_str::<Value>(l).ok())
                .and_then(|o| {
                    o.get("timestamp")
                        .and_then(|t| t.as_str())
                        .map(str::to_string)
                })
                .unwrap_or_else(|| iso_ms(chrono::Utc::now().timestamp_millis()))
        };
        entries.push(
            json!({
                "id": thread_id,
                "thread_name": name,
                "updated_at": last_ts,
            })
            .to_string(),
        );
        std::fs::write(&idx, entries.join("\n") + "\n")?;

        // --- state_5.sqlite threads ---
        let db = self.codex_home.join("state_5.sqlite");
        if db.exists() {
            let text = read_maybe_zstd(rollout)?;
            let mut lines = text.lines();
            let meta: Value = serde_json::from_str(lines.next().unwrap_or("{}"))?;
            let meta = meta.get("payload").cloned().unwrap_or(Value::Null);
            let first_ms = meta
                .get("timestamp")
                .and_then(|t| t.as_str())
                .and_then(parse_iso_ms)
                .unwrap_or(0);
            let last_ms = text
                .lines()
                .last()
                .and_then(|l| serde_json::from_str::<Value>(l).ok())
                .and_then(|o| {
                    o.get("timestamp")
                        .and_then(|t| t.as_str())
                        .and_then(parse_iso_ms)
                })
                .unwrap_or(first_ms);
            let mut first_user = String::new();
            let mut model = None;
            let mut effort = None;
            for ln in text.lines().skip(1) {
                let Ok(o) = serde_json::from_str::<Value>(ln) else {
                    continue;
                };
                if o.get("type").and_then(|t| t.as_str()) == Some("response_item") {
                    let p = o.get("payload").cloned().unwrap_or(Value::Null);
                    if p.get("type").and_then(|t| t.as_str()) == Some("message")
                        && p.get("role").and_then(|r| r.as_str()) == Some("user")
                        && first_user.is_empty()
                    {
                        first_user = join_text(&content_texts(p.get("content")));
                    }
                }
                if o.get("type").and_then(|t| t.as_str()) == Some("turn_context") && model.is_none()
                {
                    model = o
                        .pointer("/payload/model")
                        .and_then(|m| m.as_str())
                        .map(str::to_string);
                    effort = o
                        .pointer("/payload/effort")
                        .and_then(|m| m.as_str())
                        .map(str::to_string);
                }
            }
            let git = meta.get("git").cloned().unwrap_or(Value::Null);
            let creator = |k: &str| meta.get(k).and_then(|v| v.as_str()).map(str::to_string);
            let row: Map<String, Value> = [
                ("id", json!(thread_id)),
                (
                    "rollout_path",
                    json!(extended_path(&rollout.to_string_lossy())),
                ),
                ("created_at", json!(first_ms / 1000)),
                ("created_at_ms", json!(first_ms)),
                ("updated_at", json!(last_ms / 1000)),
                ("updated_at_ms", json!(last_ms)),
                ("recency_at", json!(last_ms / 1000)),
                ("recency_at_ms", json!(last_ms)),
                (
                    "source",
                    meta.get("source").cloned().unwrap_or(json!("vscode")),
                ),
                (
                    "model_provider",
                    meta.get("model_provider")
                        .cloned()
                        .unwrap_or(json!("openai")),
                ),
                (
                    "originator",
                    meta.get("originator")
                        .cloned()
                        .unwrap_or(json!("Codex Desktop")),
                ),
                (
                    "cwd",
                    json!(extended_path(
                        meta.get("cwd").and_then(|c| c.as_str()).unwrap_or_default()
                    )),
                ),
                (
                    "title",
                    json!(first_user.chars().take(2000).collect::<String>()),
                ),
                (
                    "first_user_message",
                    json!(first_user.chars().take(2000).collect::<String>()),
                ),
                (
                    "preview",
                    json!(first_user.chars().take(500).collect::<String>()),
                ),
                ("name", json!(name)),
                ("sandbox_policy", json!("{\"type\":\"danger-full-access\"}")),
                ("approval_mode", json!("never")),
                ("tokens_used", json!(0)),
                ("has_user_event", json!(1)),
                ("archived", json!(0)),
                ("is_pinned", json!(0)),
                ("memory_mode", json!("enabled")),
                ("history_mode", json!("paginated")),
                (
                    "cli_version",
                    meta.get("cli_version").cloned().unwrap_or(json!("")),
                ),
                ("model", model.map(Value::String).unwrap_or(Value::Null)),
                (
                    "reasoning_effort",
                    effort.map(Value::String).unwrap_or(Value::Null),
                ),
                (
                    "thread_source",
                    meta.get("thread_source").cloned().unwrap_or(json!("user")),
                ),
                (
                    "git_sha",
                    git.get("commit_hash").cloned().unwrap_or(Value::Null),
                ),
                (
                    "git_branch",
                    git.get("branch").cloned().unwrap_or(Value::Null),
                ),
                (
                    "git_origin_url",
                    git.get("repository_url").cloned().unwrap_or(Value::Null),
                ),
                (
                    "creator_user_id",
                    creator("creator_user_id")
                        .map(Value::String)
                        .unwrap_or(Value::Null),
                ),
                (
                    "creator_account_id",
                    creator("creator_account_id")
                        .map(Value::String)
                        .unwrap_or(Value::Null),
                ),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
            let cols: Vec<&str> = row.keys().map(|s| s.as_str()).collect();
            let q = format!(
                "INSERT OR REPLACE INTO threads ({}) VALUES ({})",
                cols.join(","),
                vec!["?"; cols.len()].join(",")
            );
            fn to_sql(v: &serde_json::Value) -> rusqlite::types::Value {
                match v {
                    Value::Null => rusqlite::types::Value::Null,
                    Value::Bool(b) => rusqlite::types::Value::Integer(*b as i64),
                    Value::Number(n) => n
                        .as_i64()
                        .map(rusqlite::types::Value::Integer)
                        .unwrap_or_else(|| rusqlite::types::Value::Real(n.as_f64().unwrap_or(0.0))),
                    Value::String(s) => rusqlite::types::Value::Text(s.clone()),
                    other => rusqlite::types::Value::Text(other.to_string()),
                }
            }
            let params: Vec<rusqlite::types::Value> = row.values().map(to_sql).collect();
            let con = rusqlite::Connection::open(&db)?;
            con.busy_timeout(std::time::Duration::from_secs(10))?;
            con.execute(&q, rusqlite::params_from_iter(params))?;
            if let Some(parent) = meta.get("parent_thread_id").and_then(Value::as_str) {
                let has_edges:bool = con.query_row("select exists(select 1 from sqlite_master where type='table' and name='thread_spawn_edges')",[],|r|r.get(0))?;
                if has_edges {
                    con.execute("insert or replace into thread_spawn_edges(parent_thread_id,child_thread_id,status) values(?1,?2,'closed')",rusqlite::params![parent,thread_id])?;
                }
            }
            con.close().ok();
        }

        // --- import ledger ---
        let mut ledger = crate::providers::load_codex_import_ledger(&self.codex_home);
        if let Some(obj) = ledger.as_object_mut() {
            obj.insert(
                ledger_key.to_string(),
                json!({
                    "thread_id": thread_id,
                    "name": name,
                    "rollout": rollout.to_string_lossy(),
                    "imported_at": iso_ms(chrono::Utc::now().timestamp_millis()),
                }),
            );
        }
        crate::providers::save_codex_import_ledger(&self.codex_home, &ledger)?;
        Ok(())
    }
}
