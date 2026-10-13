//! Gemini through Google Takeout: `My Activity/Gemini Apps/MyActivity.json`, an
//! activity log with one prompt and response per record, newest first. There is
//! no message tree; records join a chat through the id in `titleUrl`
//! (`https://gemini.google.com/app/<id>` or `/app/c/<id>`), and records
//! without one are grouped while less than 30 minutes apart.

use super::{
    html_to_text, parse_time, session_ref, title_from, Builder, Chat, ChatData, Export, Files,
    Format,
};
use crate::ir::Event;
use anyhow::Context;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

const GAP_MS: i64 = 30 * 60 * 1000;

#[derive(Deserialize)]
struct Record {
    #[serde(default)]
    title: Option<String>,
    #[serde(default, rename = "titleUrl")]
    title_url: Option<String>,
    #[serde(default)]
    time: Option<String>,
    #[serde(default, rename = "safeHtmlItem")]
    safe_html_item: Vec<Value>,
    #[serde(default)]
    details: Vec<Value>,
    #[serde(default, rename = "userInteractions")]
    user_interactions: Vec<Value>,
    #[serde(default, rename = "attachedFiles")]
    attached_files: Value,
    #[serde(default, rename = "imageFile")]
    image_file: Value,
}

/// One prompt and response from the activity log.
pub(super) struct Turn {
    time: i64,
    prompt: String,
    response: String,
    uploads: Vec<String>,
    images: Vec<String>,
}

pub(super) fn load(export: &Export, entry: &str) -> anyhow::Result<Vec<Chat>> {
    let records: Vec<Record> = serde_json::from_str(&export.read_text(entry)?)
        .with_context(|| format!("{entry} is not valid Takeout activity JSON"))?;
    let mut turns: Vec<(Option<String>, Turn)> = records
        .into_iter()
        .filter_map(|r| {
            let time = r.time.as_deref().and_then(parse_time)?;
            let turn = Turn {
                time,
                prompt: prompt(&r),
                response: response(&r),
                uploads: names(&r.attached_files),
                images: names(&r.image_file),
            };
            (!turn.prompt.is_empty() || !turn.response.is_empty())
                .then(|| (chat_id(r.title_url.as_deref()), turn))
        })
        .collect();
    turns.sort_by_key(|(_, t)| t.time);
    let mut groups: BTreeMap<String, Vec<Turn>> = BTreeMap::new();
    let mut loose: Option<(String, i64)> = None;
    for (id, turn) in turns {
        let key = match id {
            Some(id) => id,
            None => {
                let key = match &loose {
                    Some((key, last)) if turn.time - last < GAP_MS => key.clone(),
                    _ => format!("activity-{}", turn.time),
                };
                loose = Some((key.clone(), turn.time));
                key
            }
        };
        groups.entry(key).or_default().push(turn);
    }
    Ok(groups
        .into_iter()
        .map(|(id, turns)| {
            let title = turns.iter().find_map(|t| title_from(&t.prompt));
            let (first, last) = (turns[0].time, turns[turns.len() - 1].time);
            Chat {
                reference: session_ref(Format::GeminiWeb, id, title, Some(first), Some(last)),
                data: ChatData::GeminiWeb(turns),
            }
        })
        .collect())
}

pub(super) fn convert(turns: &[Turn], files: &mut Files) -> Vec<Event> {
    let mut b = Builder::default();
    for turn in turns {
        let mut prompt = vec![turn.prompt.clone()];
        prompt.extend(turn.uploads.iter().map(|name| files.by_name("file", name)));
        b.user(Some(turn.time), join(prompt));
        let mut answer = vec![turn.response.clone()];
        answer.extend(turn.images.iter().map(|name| files.by_name("image", name)));
        b.assistant(Some(turn.time), join(answer));
    }
    b.finish()
}

/// The chat id: the last path segment after `/app/`, skipping a `c` segment.
fn chat_id(url: Option<&str>) -> Option<String> {
    let path = url?.split(['?', '#']).next()?;
    let (_, after) = path.split_once("/app/")?;
    let id = after.split('/').find(|s| !s.is_empty() && *s != "c")?;
    id.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        .then(|| id.to_string())
}

fn prompt(r: &Record) -> String {
    if let Some(text) = r.title.as_deref().and_then(|t| t.strip_prefix("Prompted ")) {
        return text.trim().to_string();
    }
    if let Some(text) = detail(r, "Request") {
        return text;
    }
    join(
        r.user_interactions
            .iter()
            .map(|i| strings(&i["userInteraction"]["request"]))
            .collect(),
    )
}

fn response(r: &Record) -> String {
    let html: Vec<String> = r
        .safe_html_item
        .iter()
        .filter_map(|i| i["html"].as_str())
        .map(html_to_text)
        .collect();
    if !html.is_empty() {
        return join(html);
    }
    if let Some(text) = detail(r, "Response") {
        return text;
    }
    join(
        r.user_interactions
            .iter()
            .map(|i| strings(&i["userInteraction"]["response"]))
            .collect(),
    )
}

fn detail(r: &Record, name: &str) -> Option<String> {
    r.details
        .iter()
        .find(|d| d["name"] == name)
        .and_then(|d| d["value"].as_str())
        .map(|v| v.trim().to_string())
}

/// Text inside a `userInteractions` payload: a JSON document encoded as a
/// string whose layout is undocumented, so every string leaf is collected.
fn strings(payload: &Value) -> String {
    fn collect(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::String(s) if !s.trim().is_empty() => out.push(s.trim().to_string()),
            Value::Array(items) => items.iter().for_each(|i| collect(i, out)),
            Value::Object(map) => map.values().for_each(|v| collect(v, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    match payload.as_str().map(serde_json::from_str::<Value>) {
        Some(Ok(parsed)) => collect(&parsed, &mut out),
        Some(Err(_)) => collect(payload, &mut out),
        None => collect(payload, &mut out),
    }
    out.join("\n")
}

/// File names from a string, a list of strings, or objects with a name field.
fn names(value: &Value) -> Vec<String> {
    match value {
        Value::String(s) if !s.is_empty() => vec![s.clone()],
        Value::Array(items) => items.iter().flat_map(names).collect(),
        Value::Object(map) => ["name", "fileName", "file_name"]
            .iter()
            .find_map(|k| map.get(*k).and_then(Value::as_str))
            .map(|s| vec![s.to_string()])
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn join(parts: Vec<String>) -> String {
    parts
        .into_iter()
        .filter(|p| !p.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}
