//! claude.ai export: `conversations.json`, one entry per chat with its
//! `chat_messages`. Messages link to their parent through
//! `parent_message_uuid`, so edits and retries form a tree; older exports are
//! flat lists already in order.

use super::{
    parse_time, session_ref, strip_citations, title_from, Builder, Chat, ChatData, Files, Format,
};
use crate::ir::Event;
use anyhow::Context;
use serde::Deserialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

#[derive(Deserialize)]
pub(super) struct Conversation {
    uuid: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
    #[serde(default)]
    current_leaf_message_uuid: Option<String>,
    #[serde(default)]
    chat_messages: Vec<Message>,
}

#[derive(Deserialize)]
struct Message {
    uuid: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    content: Option<Vec<Value>>,
    #[serde(default)]
    sender: String,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    parent_message_uuid: Option<String>,
    #[serde(default)]
    attachments: Vec<Upload>,
    #[serde(default)]
    files: Vec<FileRef>,
}

#[derive(Deserialize)]
struct Upload {
    #[serde(default)]
    file_name: Option<String>,
    #[serde(default)]
    extracted_content: Option<String>,
}

#[derive(Deserialize)]
struct FileRef {
    #[serde(default)]
    file_name: Option<String>,
}

pub(super) fn load(entry: &str, text: String) -> anyhow::Result<Vec<Chat>> {
    let list: Vec<Conversation> = serde_json::from_str(&text)
        .with_context(|| format!("{entry} is not valid claude.ai export JSON"))?;
    Ok(list
        .into_iter()
        .map(|c| {
            let title = c.name.clone().filter(|n| !n.trim().is_empty()).or_else(|| {
                c.chat_messages
                    .iter()
                    .find(|m| m.sender == "human")
                    .and_then(|m| title_from(&prompt(m)))
            });
            let created = c.created_at.as_deref().and_then(parse_time);
            let updated = c.updated_at.as_deref().and_then(parse_time).or(created);
            Chat {
                reference: session_ref(Format::ClaudeWeb, c.uuid.clone(), title, created, updated),
                data: ChatData::ClaudeWeb(c),
            }
        })
        .collect())
}

/// Messages from the root to the visible leaf.
fn branch<'a>(c: &'a Conversation) -> anyhow::Result<Vec<&'a Message>> {
    if c.chat_messages
        .iter()
        .all(|m| m.parent_message_uuid.is_none())
    {
        return Ok(c.chat_messages.iter().collect());
    }
    let by_id: HashMap<&str, &Message> = c
        .chat_messages
        .iter()
        .map(|m| (m.uuid.as_str(), m))
        .collect();
    let parents: HashSet<&str> = c
        .chat_messages
        .iter()
        .filter_map(|m| m.parent_message_uuid.as_deref())
        .collect();
    let leaf = c
        .current_leaf_message_uuid
        .as_deref()
        .and_then(|id| by_id.get(id).copied())
        .or_else(|| {
            c.chat_messages
                .iter()
                .filter(|m| !parents.contains(m.uuid.as_str()))
                .max_by_key(|m| {
                    m.created_at
                        .as_deref()
                        .and_then(parse_time)
                        .unwrap_or(i64::MIN)
                })
        })
        .with_context(|| format!("conversation {} has no messages", c.uuid))?;
    let mut path = Vec::new();
    let mut seen = HashSet::new();
    let mut cursor = Some(leaf);
    while let Some(m) = cursor {
        anyhow::ensure!(
            seen.insert(m.uuid.as_str()),
            "conversation {} has a cycle at message {}",
            c.uuid,
            m.uuid
        );
        path.push(m);
        cursor = m
            .parent_message_uuid
            .as_deref()
            .and_then(|p| by_id.get(p).copied());
    }
    path.reverse();
    Ok(path)
}

/// A human message's typed text: its text blocks, or the flat `text` field.
fn prompt(m: &Message) -> String {
    let blocks: Vec<&str> = m
        .content
        .iter()
        .flatten()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect();
    if blocks.is_empty() {
        m.text.clone().unwrap_or_default()
    } else {
        blocks.join("\n")
    }
}

pub(super) fn convert(
    c: &Conversation,
    _conversation: &str,
    files: &mut Files,
) -> anyhow::Result<Vec<Event>> {
    let mut b = Builder::default();
    for m in branch(c)? {
        let time = m.created_at.as_deref().and_then(parse_time);
        if m.sender == "human" {
            let mut parts = vec![prompt(m)];
            for upload in &m.attachments {
                if let Some(content) = upload
                    .extracted_content
                    .as_deref()
                    .filter(|t| !t.is_empty())
                {
                    parts.push(format!(
                        "[Attached file: {}]\n{content}",
                        upload.file_name.as_deref().unwrap_or("untitled")
                    ));
                }
            }
            for file in m.files.iter().filter_map(|f| f.file_name.as_deref()) {
                parts.push(files.by_name("file", file));
            }
            b.user(
                time,
                parts
                    .into_iter()
                    .filter(|p| !p.trim().is_empty())
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
            continue;
        }
        let blocks = m.content.as_deref().unwrap_or_default();
        if blocks.is_empty() {
            b.assistant(time, strip_citations(m.text.as_deref().unwrap_or("")));
        }
        for (index, block) in blocks.iter().enumerate() {
            let name = block["name"].as_str().unwrap_or("tool");
            match block["type"].as_str() {
                Some("text") => {
                    b.assistant(time, strip_citations(block["text"].as_str().unwrap_or("")))
                }
                Some("thinking") => {
                    b.reasoning(time, block["thinking"].as_str().unwrap_or("").into())
                }
                Some("tool_use") => {
                    let id = block["id"]
                        .as_str()
                        .map_or_else(|| format!("{}-{index}", m.uuid), String::from);
                    b.call(time, &id, name, block["input"].to_string());
                }
                Some("tool_result") => b.result(
                    time,
                    block["tool_use_id"].as_str(),
                    name,
                    result_text(&block["content"]),
                ),
                _ => {}
            }
        }
    }
    Ok(b.finish())
}

fn result_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|i| i["text"].as_str().or_else(|| i.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}
