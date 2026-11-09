//! Qoder CLI project transcripts and read-only IDE history.
//!
//! CLI records use parentUuid chains and an explicit active-leaf commit. IDE
//! hook transcripts live in projects/<slug>/transcript; recent desktop builds
//! also keep visible messages in main.sqlite. That database is never written.

use super::Provider;
use crate::{
    ir::{Event, EventKind, Role, Session, SessionRef, WriteOpts, WriteOutcome},
    util::{iso_ms, uuid7},
};
use anyhow::Context;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

mod desktop;
#[cfg(test)]
mod tests;

pub struct QoderProvider {
    home: PathBuf,
    ide: bool,
    desktop_db: Option<PathBuf>,
}

impl QoderProvider {
    pub fn cli(home: PathBuf) -> Self {
        Self {
            home,
            ide: false,
            desktop_db: None,
        }
    }
    pub fn ide(home: PathBuf, desktop_db: Option<PathBuf>) -> Self {
        Self {
            home,
            ide: true,
            desktop_db,
        }
    }

    fn records(path: &Path) -> anyhow::Result<Vec<Value>> {
        use std::io::BufRead;
        let file =
            std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
        let mut records = Vec::new();
        for (line, text) in std::io::BufReader::new(file).lines().enumerate() {
            let text = text?;
            if text.trim().is_empty() {
                continue;
            }
            let record: Value = serde_json::from_str(text.trim_start_matches('\u{feff}'))
                .with_context(|| {
                    format!(
                        "invalid Qoder transcript {} at line {}",
                        path.display(),
                        line + 1
                    )
                })?;
            anyhow::ensure!(
                record.is_object(),
                "Qoder transcript line {} must be an object",
                line + 1
            );
            records.push(record);
        }
        Ok(records)
    }

    fn reference(&self, path: &Path) -> anyhow::Result<SessionRef> {
        let records = Self::records(path)?;
        let mut title = None;
        let mut cwd = None;
        let mut dates = Vec::new();
        for record in &records {
            match string(record, "type") {
                "custom-title" => title = record["customTitle"].as_str().map(str::to_owned),
                "ai-title" if title.is_none() => {
                    title = record["aiTitle"].as_str().map(str::to_owned)
                }
                _ => {}
            }
            cwd = record["cwd"].as_str().map(str::to_owned).or(cwd);
            if let Some(time) = timestamp(&record["timestamp"]) {
                dates.push(time);
            }
        }
        if title.is_none() {
            title = records.iter().find_map(|record| {
                if string(record, "type") != "user"
                    || record["isMeta"] == true
                    || record["isCompactSummary"] == true
                {
                    return None;
                }
                text_content(&record["message"]["content"])
                    .filter(|text| !text.trim().is_empty())
                    .map(|text| {
                        text.lines()
                            .next()
                            .unwrap_or_default()
                            .chars()
                            .take(100)
                            .collect()
                    })
            });
        }
        let modified = std::fs::metadata(path)?
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|time| time.as_millis() as i64);
        Ok(SessionRef {
            provider: self.name().into(),
            id: path
                .file_stem()
                .context("transcript has no id")?
                .to_string_lossy()
                .into(),
            title,
            cwd,
            created_ms: dates.iter().min().copied().or(modified),
            updated_ms: dates.iter().max().copied().or(modified),
            locator: path.to_string_lossy().into(),
            migrated: false,
        })
    }

    fn read_transcript(
        &self,
        reference: &SessionRef,
        archive_only: bool,
    ) -> anyhow::Result<Session> {
        let path = Path::new(&reference.locator);
        let records = Self::records(path)?;
        let mut events = Vec::new();
        let mut ranges = Vec::new();
        for record in &records {
            let start = events.len();
            events.extend(decode_record(record)?);
            ranges.push(start..events.len());
        }
        let mut unavailable = None;
        let chain = if self.ide && !records.iter().any(|r| r.get("parentUuid").is_some()) {
            (0..records.len()).collect()
        } else {
            active_chain(&records)?
        };
        if let Some(boundary) = chain.iter().rposition(|&index| {
            string(&records[index], "type") == "system"
                && string(&records[index], "subtype") == "compact_boundary"
        }) {
            let record = &records[chain[boundary]];
            if record["requiresActiveLeafCommit"] == true {
                anyhow::ensure!(records.iter().any(|marker| string(marker, "type") == "active-leaf" && marker["explicit"] == false && marker["compactBoundaryUuid"] == record["uuid"]),
                    "Qoder compaction has no committed active-leaf; retry after compaction completes");
            }
            anyhow::ensure!(
                chain[boundary + 1..]
                    .iter()
                    .any(|&index| records[index]["isCompactSummary"] == true
                        && text_content(&records[index]["message"]["content"])
                            .is_some_and(|text| !text.trim().is_empty())),
                "Qoder compaction has no saved summary; retry after compaction completes"
            );
        }
        let mut retained = Vec::new();
        for index in chain {
            let record = &records[index];
            if string(record, "type") == "system" && string(record, "subtype") == "compact_boundary"
            {
                if record
                    .pointer("/compactMetadata/preservedSegment")
                    .is_some()
                    || record
                        .pointer("/compactMetadata/preservedMessages")
                        .is_some()
                {
                    unavailable = Some("Qoder compaction has a preserved segment whose context ordering is not supported".into());
                } else {
                    retained.clear();
                }
            }
            if record["isVisibleInTranscriptOnly"] != true {
                retained.extend_from_slice(&events[ranges[index].clone()]);
            }
        }
        if records.iter().any(|r| {
            string(r, "type") == "content-replacement"
                && r["replacements"].as_array().is_some_and(|a| !a.is_empty())
        }) {
            unavailable = Some("Qoder persisted content replacements; their complete retained context is not yet portable".into());
        }
        let compression = path.with_extension("").join("compression-v2/state.json");
        if compression.is_file() {
            let state: Value = serde_json::from_slice(&std::fs::read(&compression)?)
                .context("invalid Qoder compression state")?;
            if state["version"] != 2
                || !state
                    .pointer("/state/replacementDecisions")
                    .is_some_and(|v| v.as_object().is_some_and(|v| v.is_empty()))
            {
                unavailable = Some("Qoder compression state includes unsupported replacements or an unknown version".into());
            }
        }
        if !archive_only {
            if let Some(reason) = &unavailable {
                anyhow::bail!("{reason}; export the readable archive or explicitly use --rebuild-resume-context");
            }
        }
        let created_ms = reference.created_ms.unwrap_or(0);
        Ok(Session {
            source: self.name().into(),
            id: reference.id.clone(),
            title: reference.title.clone(),
            cwd: reference.cwd.clone(),
            agent_preset: None,
            parent_session: None,
            origin: Some(if self.ide { "Qoder IDE" } else { "Qoder CLI" }.into()),
            created_ms,
            updated_ms: reference.updated_ms.unwrap_or(created_ms),
            events,
            resume_events: Some(retained),
            resume_context_unavailable: unavailable,
        })
    }
}

impl Provider for QoderProvider {
    fn name(&self) -> &'static str {
        if self.ide {
            "qoder-ide"
        } else {
            "qoder"
        }
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn discover(&self) -> anyhow::Result<Vec<SessionRef>> {
        let mut references = Vec::new();
        let projects = self.home.join("projects");
        if projects.is_dir() {
            for entry in std::fs::read_dir(&projects)? {
                let project = entry?.path();
                let directory = if self.ide {
                    project.join("transcript")
                } else {
                    project
                };
                if !directory.is_dir() {
                    continue;
                }
                for entry in std::fs::read_dir(&directory)? {
                    let path = entry?.path();
                    if path.extension().is_some_and(|ext| ext == "jsonl") {
                        references.push(self.reference(&path)?);
                    }
                }
            }
        }
        if self.ide {
            if let Some(database) = &self.desktop_db {
                if database.is_file() {
                    for mut reference in desktop::discover(database)? {
                        if !references.iter().any(|r| r.id == reference.id) {
                            // Recent desktop builds use the CLI runtime. Prefer
                            // its committed context when that exact session has
                            // a local transcript, retaining the desktop title.
                            if let Some(cwd) = &reference.cwd {
                                let transcript = self
                                    .home
                                    .join("projects")
                                    .join(crate::util::claude_slug(cwd))
                                    .join(format!("{}.jsonl", reference.id));
                                if transcript.is_file() {
                                    reference.locator = transcript.to_string_lossy().into();
                                }
                            }
                            references.push(reference);
                        }
                    }
                }
            }
        }
        anyhow::ensure!(
            projects.is_dir() || self.desktop_db.as_ref().is_some_and(|p| p.is_file()),
            "no Qoder history at {}; initialize Qoder or pass --qoder-home",
            self.home.display()
        );
        references.sort_by(|a, b| b.updated_ms.cmp(&a.updated_ms).then(a.id.cmp(&b.id)));
        Ok(references)
    }

    fn read(&self, reference: &SessionRef) -> anyhow::Result<Session> {
        if reference.locator.starts_with("qoder-db:") {
            anyhow::bail!("Qoder desktop history has visible messages but no verified resume checkpoint; use --rebuild-resume-context to recover from that archive");
        }
        self.read_transcript(reference, false)
    }

    fn read_for_export(&self, reference: &SessionRef) -> anyhow::Result<Session> {
        if reference.locator.starts_with("qoder-db:") {
            return desktop::read(
                self.desktop_db
                    .as_deref()
                    .context("Qoder desktop database is not configured")?,
                reference,
            );
        }
        self.read_transcript(reference, true)
    }

    fn children(&self, parent: &SessionRef) -> anyhow::Result<Vec<SessionRef>> {
        if parent.locator.starts_with("qoder-db:") {
            return Ok(Vec::new());
        }
        let directory = Path::new(&parent.locator)
            .with_extension("")
            .join("subagents");
        if !directory.is_dir() {
            return Ok(Vec::new());
        }
        let mut children = Vec::new();
        for entry in std::fs::read_dir(directory)? {
            let path = entry?.path();
            if path.extension().is_some_and(|ext| ext == "jsonl") {
                children.push(self.reference(&path)?);
            }
        }
        children.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(children)
    }

    fn write(&self, session: &Session, opts: &WriteOpts) -> anyhow::Result<WriteOutcome> {
        anyhow::ensure!(
            !self.ide,
            "Qoder IDE is a read-only source; import into qoder (CLI) or another harness"
        );
        let cwd = opts
            .cwd
            .as_deref()
            .or(session.cwd.as_deref())
            .filter(|cwd| !cwd.trim().is_empty())
            .context("Qoder CLI needs a destination folder; pass --cwd")?;
        let id = uuid7(
            session.created_ms,
            &format!("qoder|{}|{}", session.source, session.id),
        );
        let directory = self
            .home
            .join("projects")
            .join(crate::util::claude_slug(cwd));
        let path = match &session.parent_session {
            Some(parent) => {
                let parent_path = session
                    .events
                    .iter()
                    .rev()
                    .find_map(|event| match &event.kind {
                        EventKind::Meta { kind, data } if kind == "migration-parent-depth" => {
                            data["location"].as_str()
                        }
                        _ => None,
                    })
                    .map(PathBuf::from)
                    .unwrap_or_else(|| directory.join(format!("{parent}.jsonl")));
                parent_path
                    .with_extension("")
                    .join("subagents")
                    .join(format!("{id}.jsonl"))
            }
            None => directory.join(format!("{id}.jsonl")),
        };
        let title = opts
            .name
            .clone()
            .or_else(|| session.title.clone())
            .unwrap_or_else(|| "Imported session".into());
        let mut records =
            vec![json!({"type":"workspace-directories","sessionId":id,"directories":[cwd]})];
        let mut parent = None;
        encode_events(
            &session.events,
            session,
            &id,
            cwd,
            &mut records,
            &mut parent,
            false,
        )?;
        if let Some(retained) = &session.resume_events {
            let boundary = uuid7(session.updated_ms, &format!("qoder|{id}|boundary"));
            records.push(json!({"type":"system","subtype":"compact_boundary","uuid":boundary,"parentUuid":null,
                "sessionId":id,"timestamp":iso_ms(session.updated_ms),"cwd":cwd,"isSidechain":false,
                "userType":"external","version":"1.1.85","content":"Imported resume checkpoint",
                "compactMetadata":{"trigger":"manual"}}));
            parent = Some(boundary);
            let summary = uuid7(session.updated_ms, &format!("qoder|{id}|summary"));
            records.push(json!({"type":"user","uuid":summary,"parentUuid":parent,"timestamp":iso_ms(session.updated_ms),
                "cwd":cwd,"sessionId":id,"isSidechain":false,"userType":"external","version":"1.1.85","isCompactSummary":true,
                "message":{"role":"user","content":[{"type":"text","text":"The retained migration context follows; earlier transcript entries are archival."}]}}));
            parent = Some(summary);
            encode_events(retained, session, &id, cwd, &mut records, &mut parent, true)?;
        }
        records.push(json!({"type":"active-leaf","sessionId":id,"leafUuid":parent,"explicit":true,"timestamp":session.updated_ms}));
        records.push(json!({"type":"custom-title","sessionId":id,"customTitle":title}));
        let archive = path.with_extension("").join("migration-archive.json");
        if !opts.dry_run {
            std::fs::create_dir_all(path.parent().unwrap())?;
            std::fs::create_dir_all(archive.parent().unwrap())?;
            std::fs::write(&archive, serde_json::to_vec_pretty(session)?)?;
            let mut body = records
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            body.push('\n');
            std::fs::write(&path, body)?;
        }
        Ok(WriteOutcome {
            provider: "qoder".into(),
            location: path.to_string_lossy().into(),
            native_id: id.clone(),
            extra: json!({"records":records.len(),"dry_run":opts.dry_run,"resume_command":format!("qoder --resume {id}"),
                "cwd":cwd,"archive":archive,"ide_registration":false}),
        })
    }
}

fn encode_events(
    events: &[Event],
    session: &Session,
    id: &str,
    cwd: &str,
    records: &mut Vec<Value>,
    parent: &mut Option<String>,
    active: bool,
) -> anyhow::Result<()> {
    let mut calls = HashSet::new();
    for event in events {
        let (kind, content) = match &event.kind {
            EventKind::Message { role, text, .. } => (
                if *role == Role::Assistant {
                    "assistant"
                } else {
                    "user"
                },
                json!([{"type":"text","text":if *role==Role::Developer {format!("[Imported harness context]\n{text}")}else{text.clone()}}]),
            ),
            EventKind::Reasoning { text } if active || session.resume_events.is_none() => (
                "assistant",
                json!([{"type":"text","text":format!("[Imported reasoning]\n{text}")}]),
            ),
            EventKind::Reasoning { text } => (
                "assistant",
                json!([{"type":"thinking","thinking":text,"signature":""}]),
            ),
            EventKind::ToolCall {
                call_id,
                name,
                arguments,
            } => {
                anyhow::ensure!(
                    calls.insert(call_id),
                    "duplicate Qoder tool call {call_id:?}"
                );
                (
                    "assistant",
                    json!([{"type":"tool_use","id":call_id,"name":name,"input":serde_json::from_str::<Value>(arguments).unwrap_or_else(|_|json!({"raw":arguments}))}]),
                )
            }
            EventKind::ToolResult { call_id, text } => {
                anyhow::ensure!(
                    calls.remove(call_id),
                    "Qoder tool result {call_id:?} has no preceding call"
                );
                (
                    "user",
                    json!([{"type":"tool_result","tool_use_id":call_id,"content":text}]),
                )
            }
            EventKind::Compaction {
                text: Some(text), ..
            } => (
                "user",
                json!([{"type":"text","text":format!("[Compacted context]\n{text}")} ]),
            ),
            _ => continue,
        };
        let index = records.len();
        let uuid = uuid7(
            session.created_ms + index as i64,
            &format!("qoder|{id}|{index}"),
        );
        records.push(json!({"type":kind,"uuid":uuid,"parentUuid":parent,"timestamp":iso_ms(session.event_ms(event,index as i64)),
            "cwd":cwd,"sessionId":id,"isSidechain":false,"userType":"external","version":"1.1.85",
            "message":{"role":kind,"content":content,"model":"imported"}}));
        *parent = Some(uuid);
    }
    anyhow::ensure!(
        calls.is_empty(),
        "Qoder conversion has unresolved tool calls; complete the source turn before migrating"
    );
    Ok(())
}

fn active_chain(records: &[Value]) -> anyhow::Result<Vec<usize>> {
    let by_id: HashMap<&str, usize> = records
        .iter()
        .enumerate()
        .filter_map(|(i, r)| r["uuid"].as_str().map(|id| (id, i)))
        .collect();
    let marker = records
        .iter()
        .rev()
        .find(|r| string(r, "type") == "active-leaf");
    let mut leaf = match marker {
        Some(record) => {
            anyhow::ensure!(
                record
                    .get("leafUuid")
                    .is_some_and(|leaf| leaf.is_null() || leaf.is_string()),
                "Qoder active-leaf must have a string or null leafUuid"
            );
            record["leafUuid"].as_str()
        }
        None => records
            .iter()
            .rev()
            .find(|r| matches!(string(r, "type"), "user" | "assistant") && r["isSidechain"] != true)
            .and_then(|r| r["uuid"].as_str()),
    };
    let mut chain = Vec::new();
    let mut seen = HashSet::new();
    while let Some(id) = leaf {
        anyhow::ensure!(seen.insert(id), "cyclic Qoder parent chain at {id:?}");
        let index = *by_id
            .get(id)
            .with_context(|| format!("Qoder active chain refers to missing message {id:?}"))?;
        chain.push(index);
        leaf = records[index]["parentUuid"].as_str();
    }
    chain.reverse();
    Ok(chain)
}

fn decode_record(record: &Value) -> anyhow::Result<Vec<Event>> {
    let time = timestamp(&record["timestamp"]);
    let mut events = Vec::new();
    match string(record, "type") {
        "user" | "assistant" => {
            let role = if string(record, "type") == "user" {
                Role::User
            } else {
                Role::Assistant
            };
            let content = &record["message"]["content"];
            if let Some(text) = content.as_str() {
                events.push(Event::at(
                    time,
                    EventKind::Message {
                        role,
                        text: text.into(),
                        source_kind: None,
                    },
                ));
            } else {
                let blocks = content
                    .as_array()
                    .context("Qoder message content must be text or an array")?;
                for block in blocks {
                    let kind=match string(block,"type") {
                        "text"=>EventKind::Message{role,text:block["text"].as_str().context("Qoder text block has no text")?.into(),source_kind:None},
                        "thinking"=>EventKind::Reasoning{text:block["thinking"].as_str().or_else(||block["text"].as_str()).context("Qoder thinking block has no text")?.into()},
                        "tool_use"=>EventKind::ToolCall{call_id:block["id"].as_str().context("Qoder tool use has no id")?.into(),name:block["name"].as_str().context("Qoder tool use has no name")?.into(),arguments:serde_json::to_string(&block["input"])?},
                        "tool_result"=>EventKind::ToolResult{call_id:block["tool_use_id"].as_str().context("Qoder tool result has no id")?.into(),text:text_content(&block["content"]).unwrap_or_else(||block["content"].to_string())},
                        "image"|"document"=>EventKind::Message{role,text:format!("[Qoder {} attachment; binary content is not transferred]",string(block,"type")),source_kind:Some("qoder-attachment".into())},
                        other=>anyhow::bail!("unsupported Qoder content block {other:?}; refusing to discard active conversation data"),
                    };
                    events.push(Event::at(time, kind));
                }
            }
        }
        "system" if string(record, "subtype") == "compact_boundary" => events.push(Event::at(
            time,
            EventKind::Compaction {
                id: record["uuid"].as_str().map(str::to_owned),
                text: None,
            },
        )),
        "attachment" => {
            if let Some(text) = record["attachment"]["content"].as_str() {
                events.push(Event::at(
                    time,
                    EventKind::Message {
                        role: Role::Developer,
                        text: text.into(),
                        source_kind: Some("qoder-attachment".into()),
                    },
                ));
            }
            events.push(Event::at(
                time,
                EventKind::Meta {
                    kind: "qoder-attachment".into(),
                    data: record.clone(),
                },
            ));
        }
        _ => events.push(Event::at(
            time,
            EventKind::Meta {
                kind: format!("qoder-{}", string(record, "type")),
                data: record.clone(),
            },
        )),
    };
    Ok(events)
}

fn string<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or_default()
}
fn timestamp(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| {
        value
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.timestamp_millis())
    })
}
fn text_content(value: &Value) -> Option<String> {
    value.as_str().map(str::to_owned).or_else(|| {
        value.as_array().map(|blocks| {
            blocks
                .iter()
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
    })
}
