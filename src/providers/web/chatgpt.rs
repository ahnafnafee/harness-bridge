//! ChatGPT export: `conversations.json` (with `conversations-*.json` shards when
//! present). Each conversation is a node tree; the visible chat is the branch
//! that ends at `current_node`, and every edit or regeneration is a sibling.

use super::{
    secs_ms, session_ref, strip_citations, Builder, Chat, ChatData, Export, Files, Format,
};
use crate::ir::Event;
use anyhow::Context;
use serde::Deserialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

#[derive(Deserialize)]
pub(super) struct Conversation {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    create_time: Option<f64>,
    #[serde(default)]
    update_time: Option<f64>,
    #[serde(default)]
    conversation_id: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    current_node: Option<String>,
    #[serde(default)]
    mapping: HashMap<String, Node>,
}

#[derive(Deserialize)]
struct Node {
    #[serde(default)]
    message: Option<Message>,
    #[serde(default)]
    parent: Option<String>,
    #[serde(default)]
    children: Vec<String>,
}

#[derive(Deserialize)]
struct Message {
    id: String,
    author: Author,
    #[serde(default)]
    create_time: Option<f64>,
    #[serde(default)]
    content: Value,
    #[serde(default)]
    recipient: Option<String>,
    #[serde(default)]
    metadata: Option<Metadata>,
}

#[derive(Deserialize)]
struct Author {
    role: String,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize, Default)]
struct Metadata {
    #[serde(default)]
    is_visually_hidden_from_conversation: Option<bool>,
    #[serde(default)]
    attachments: Vec<Upload>,
}

#[derive(Deserialize)]
struct Upload {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

/// Records that hold settings or UI state rather than conversation.
const SKIPPED: [&str; 3] = [
    "user_editable_context",
    "model_editable_context",
    "reasoning_recap",
];

pub(super) fn load(export: &Export, entry: &str, text: String) -> anyhow::Result<Vec<Chat>> {
    let folder = entry.rsplit_once('/').map_or("", |(dir, _)| dir);
    let mut texts = vec![(entry.to_string(), text)];
    for shard in export.files().iter().filter(|f| {
        let (dir, name) = f.rsplit_once('/').unwrap_or(("", f));
        dir == folder && name.starts_with("conversations-") && name.ends_with(".json")
    }) {
        texts.push((shard.clone(), export.read_text(shard)?));
    }
    let mut chats = Vec::new();
    for (name, text) in texts {
        let list: Vec<Conversation> = serde_json::from_str(&text)
            .with_context(|| format!("{name} is not valid ChatGPT export JSON"))?;
        for c in list {
            let id = c
                .conversation_id
                .clone()
                .or_else(|| c.id.clone())
                .with_context(|| format!("a conversation in {name} has no id"))?;
            chats.push(Chat {
                reference: session_ref(
                    Format::ChatGpt,
                    id,
                    c.title.clone().filter(|t| !t.trim().is_empty()),
                    c.create_time.map(secs_ms),
                    c.update_time.or(c.create_time).map(secs_ms),
                ),
                data: ChatData::ChatGpt(c),
            });
        }
    }
    Ok(chats)
}

/// Node ids from the root to the visible leaf.
fn branch(c: &Conversation, conversation: &str) -> anyhow::Result<Vec<String>> {
    let leaf = match &c.current_node {
        Some(id) => id.clone(),
        None => {
            let root = c
                .mapping
                .iter()
                .find(|(_, n)| n.parent.as_ref().is_none_or(|p| !c.mapping.contains_key(p)))
                .map(|(id, _)| id.clone())
                .with_context(|| format!("conversation {conversation} has no root node"))?;
            let mut id = root;
            let mut seen = HashSet::new();
            while let Some(next) = c.mapping.get(&id).and_then(|n| n.children.last()) {
                anyhow::ensure!(
                    seen.insert(id.clone()),
                    "conversation {conversation} has a cycle at node {id}"
                );
                id = next.clone();
            }
            id
        }
    };
    let mut path = Vec::new();
    let mut seen = HashSet::new();
    let mut cursor = Some(leaf);
    while let Some(id) = cursor {
        anyhow::ensure!(
            seen.insert(id.clone()),
            "conversation {conversation} has a cycle at node {id}"
        );
        let node = c
            .mapping
            .get(&id)
            .with_context(|| format!("conversation {conversation} refers to missing node {id}"))?;
        cursor = node.parent.clone().filter(|p| c.mapping.contains_key(p));
        path.push(id);
    }
    path.reverse();
    Ok(path)
}

pub(super) fn convert(
    c: &Conversation,
    conversation: &str,
    files: &mut Files,
) -> anyhow::Result<Vec<Event>> {
    let mut b = Builder::default();
    for id in branch(c, conversation)? {
        let node = &c.mapping[&id];
        let Some(m) = &node.message else { continue };
        let metadata = m.metadata.as_ref();
        let content_type = m.content["content_type"].as_str().unwrap_or("");
        if m.author.role == "system"
            || SKIPPED.contains(&content_type)
            || metadata.and_then(|m| m.is_visually_hidden_from_conversation) == Some(true)
        {
            continue;
        }
        let time = m.create_time.map(secs_ms);
        let mut referenced = HashSet::new();
        let text = render(&m.content, files, &mut referenced);
        match m.author.role.as_str() {
            "user" => {
                let mut parts = vec![text];
                for upload in metadata.map_or(&[][..], |m| &m.attachments) {
                    let Some(id) = upload.id.as_deref().filter(|id| !referenced.contains(*id))
                    else {
                        continue;
                    };
                    parts.push(files.by_prefix("file", id, upload.name.as_deref().unwrap_or(id)));
                }
                b.user(time, join(parts));
            }
            "assistant" if content_type == "thoughts" => b.reasoning(time, text),
            "assistant" if m.recipient.as_deref().is_some_and(|r| r != "all") => {
                let arguments = match serde_json::from_str::<Value>(&text) {
                    Ok(Value::Object(_)) => text,
                    _ => serde_json::json!({ "input": text }).to_string(),
                };
                b.call(
                    time,
                    &m.id,
                    m.recipient.as_deref().unwrap_or("tool"),
                    arguments,
                );
            }
            "tool" => {
                let parent = node
                    .parent
                    .as_ref()
                    .and_then(|p| c.mapping.get(p))
                    .and_then(|n| n.message.as_ref())
                    .map(|p| p.id.as_str());
                b.result(
                    time,
                    parent,
                    m.author.name.as_deref().unwrap_or("tool"),
                    text,
                );
            }
            _ => b.assistant(time, text),
        }
    }
    Ok(b.finish())
}

fn render(content: &Value, files: &mut Files, referenced: &mut HashSet<String>) -> String {
    let field = |key: &str| content[key].as_str().unwrap_or("").to_string();
    let content_type = content["content_type"].as_str().unwrap_or("");
    let text = match content_type {
        "text" | "multimodal_text" => content["parts"]
            .as_array()
            .map(|parts| join(parts.iter().map(|p| part(p, files, referenced)).collect()))
            .unwrap_or_default(),
        "code" | "execution_output" => field("text"),
        "tether_browsing_display" => join(vec![field("result"), field("summary")]),
        "tether_quote" | "sonic_webpage" => join(vec![field("title"), field("url"), field("text")]),
        "thoughts" => content["thoughts"]
            .as_array()
            .map(|thoughts| {
                thoughts
                    .iter()
                    .map(|t| {
                        join(vec![
                            t["summary"].as_str().unwrap_or("").into(),
                            t["content"].as_str().unwrap_or("").into(),
                        ])
                    })
                    .filter(|t| !t.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n\n")
            })
            .unwrap_or_default(),
        "system_error" => format!("[{}] {}", field("name"), field("text")),
        other => format!("[{other} content was not converted]"),
    };
    strip_citations(&text)
}

fn part(part: &Value, files: &mut Files, referenced: &mut HashSet<String>) -> String {
    match part {
        Value::String(text) => text.clone(),
        Value::Object(o) => match o.get("content_type").and_then(Value::as_str) {
            Some("image_asset_pointer") => {
                let pointer = o.get("asset_pointer").and_then(Value::as_str).unwrap_or("");
                let id = pointer.split_once("://").map_or(pointer, |(_, id)| id);
                referenced.insert(id.to_string());
                files.by_prefix("image", id, id)
            }
            Some("audio_transcription") => {
                o.get("text").and_then(Value::as_str).unwrap_or("").into()
            }
            Some(other) => format!("[{other} not transferred]"),
            None => String::new(),
        },
        _ => String::new(),
    }
}

fn join(parts: Vec<String>) -> String {
    parts
        .into_iter()
        .filter(|p| !p.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}
