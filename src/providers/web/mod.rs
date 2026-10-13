//! Read-only providers for web chat exports: ChatGPT, claude.ai and Gemini.
//!
//! Each format parses its export into typed records and converts one
//! conversation at a time; this module holds what they share: the export
//! loader, format detection, attachment references and the event builder that
//! keeps tool calls and results paired the way every writer requires.

mod chatgpt;
mod claude_web;
mod export;
mod gemini_web;
#[cfg(test)]
mod tests;

pub use export::Export;

use crate::{
    ir::{Event, EventKind, Role, Session, SessionRef, WriteOpts, WriteOutcome},
    providers::Provider,
};
use std::{cell::OnceCell, path::Path, path::PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    ChatGpt,
    ClaudeWeb,
    GeminiWeb,
}

impl Format {
    pub const ALL: [Format; 3] = [Format::ChatGpt, Format::ClaudeWeb, Format::GeminiWeb];

    pub fn name(self) -> &'static str {
        match self {
            Format::ChatGpt => "chatgpt",
            Format::ClaudeWeb => "claude-web",
            Format::GeminiWeb => "gemini-web",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.name() == name)
    }

    fn app(self) -> &'static str {
        match self {
            Format::ChatGpt => "ChatGPT",
            Format::ClaudeWeb => "claude.ai",
            Format::GeminiWeb => "Gemini",
        }
    }

    /// The format of an export, from its file layout and conversation records.
    pub fn detect(export: &Export) -> anyhow::Result<Self> {
        Ok(locate(export)?.0)
    }
}

/// The detected format, the conversations entry, and its text when already read.
fn locate(export: &Export) -> anyhow::Result<(Format, String, Option<String>)> {
    let lower = |f: &str| f.to_lowercase();
    let chosen = export.chosen().map(lower);
    let gemini = |f: &String| {
        let f = lower(f);
        f.ends_with("myactivity.json")
            && (f.contains("gemini") || chosen.as_deref() == Some("myactivity.json"))
    };
    if let Some(entry) = export.files().iter().find(|f| gemini(f)) {
        return Ok((Format::GeminiWeb, entry.clone(), None));
    }
    if export.files().iter().any(|f| {
        let f = lower(f);
        f.ends_with("myactivity.html") && f.contains("gemini")
    }) {
        anyhow::bail!(
            "this Takeout has Gemini activity as HTML; export it again with My Activity > Multiple formats > Activity records set to JSON"
        );
    }
    let entry = match export.chosen().filter(|c| lower(c).ends_with(".json")) {
        Some(chosen) => chosen.to_string(),
        None => export
            .files()
            .iter()
            .filter(|f| lower(f.rsplit('/').next().unwrap_or(f)) == "conversations.json")
            .min_by_key(|f| f.matches('/').count())
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!("no conversations.json or Gemini MyActivity.json in the export")
            })?,
    };
    let text = export.read_text(&entry)?;
    let first = |key: &str| text.find(&format!("\"{key}\""));
    let format = match (first("mapping"), first("chat_messages")) {
        (Some(a), Some(b)) if a < b => Format::ChatGpt,
        (Some(_), None) => Format::ChatGpt,
        (_, Some(_)) => Format::ClaudeWeb,
        (None, None) => anyhow::bail!("{entry} is not a ChatGPT or claude.ai export"),
    };
    Ok((format, entry, Some(text)))
}

/// A file from the export that a converted conversation refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    /// Path inside the export.
    pub entry: String,
    /// Path relative to the destination base directory.
    pub target: String,
}

/// One parsed conversation, converted to a session only when read.
struct Chat {
    reference: SessionRef,
    data: ChatData,
}

enum ChatData {
    ChatGpt(chatgpt::Conversation),
    ClaudeWeb(claude_web::Conversation),
    GeminiWeb(Vec<gemini_web::Turn>),
}

struct Loaded {
    export: Export,
    chats: Vec<Chat>,
}

pub struct WebProvider {
    format: Format,
    path: Option<PathBuf>,
    loaded: OnceCell<Loaded>,
}

impl WebProvider {
    pub fn new(format: Format, path: Option<PathBuf>) -> Self {
        Self {
            format,
            path,
            loaded: OnceCell::new(),
        }
    }

    pub fn format(&self) -> Format {
        self.format
    }

    /// Parses the export once per run; discovery and reads share the result.
    fn loaded(&self) -> anyhow::Result<&Loaded> {
        if let Some(loaded) = self.loaded.get() {
            return Ok(loaded);
        }
        let path = self.path.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "pass --export <zip, folder or file> to read {} chats",
                self.format.app()
            )
        })?;
        let export = Export::open(path)?;
        let (found, entry, text) = locate(&export)?;
        anyhow::ensure!(
            found == self.format,
            "{} is a {} export; use --from {}",
            path.display(),
            found.app(),
            found.name()
        );
        let mut chats = match self.format {
            Format::ChatGpt => chatgpt::load(&export, &entry, text.unwrap_or_default())?,
            Format::ClaudeWeb => claude_web::load(&entry, text.unwrap_or_default())?,
            Format::GeminiWeb => gemini_web::load(&export, &entry)?,
        };
        chats.sort_by(|a, b| {
            b.reference
                .updated_ms
                .cmp(&a.reference.updated_ms)
                .then_with(|| a.reference.id.cmp(&b.reference.id))
        });
        let _ = self.loaded.set(Loaded { export, chats });
        Ok(self.loaded.get().expect("just set"))
    }

    fn convert(&self, reference: &SessionRef) -> anyhow::Result<(Session, Vec<Attachment>)> {
        let loaded = self.loaded()?;
        let chat = loaded
            .chats
            .iter()
            .find(|c| c.reference.id == reference.id)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "{} export has no conversation {}",
                    self.format.app(),
                    reference.id
                )
            })?;
        let mut files = Files::new(&loaded.export, self.format, &chat.reference.id);
        let events = match &chat.data {
            ChatData::ChatGpt(c) => chatgpt::convert(c, &chat.reference.id, &mut files)?,
            ChatData::ClaudeWeb(c) => claude_web::convert(c, &chat.reference.id, &mut files)?,
            ChatData::GeminiWeb(turns) => gemini_web::convert(turns, &mut files),
        };
        let first = events.iter().find_map(|e| e.time_ms).unwrap_or(0);
        let last = events.iter().rev().find_map(|e| e.time_ms).unwrap_or(first);
        let created_ms = chat.reference.created_ms.unwrap_or(first);
        let session = Session {
            source: self.format.name().into(),
            id: chat.reference.id.clone(),
            title: chat.reference.title.clone(),
            cwd: None,
            agent_preset: None,
            parent_session: None,
            origin: None,
            created_ms,
            updated_ms: chat.reference.updated_ms.unwrap_or(last).max(created_ms),
            events,
            resume_events: None,
            resume_context_unavailable: None,
        };
        Ok((session, files.found))
    }

    /// Files the converted conversation refers to, in first-reference order.
    pub fn attachments(&self, reference: &SessionRef) -> anyhow::Result<Vec<Attachment>> {
        Ok(self.convert(reference)?.1)
    }

    /// Copies a conversation's attachments under `base`, or only reports the plan.
    pub fn copy_attachments(
        &self,
        reference: &SessionRef,
        base: &Path,
        dry_run: bool,
    ) -> anyhow::Result<serde_json::Value> {
        let plan = self.attachments(reference)?;
        let export = &self.loaded()?.export;
        let (mut copied, mut kept) = (0, 0);
        // Targets use `/`; joining part by part keeps paths native on Windows.
        let under = |relative: &str| {
            relative
                .split('/')
                .fold(base.to_path_buf(), |p, part| p.join(part))
        };
        if !dry_run {
            for attachment in &plan {
                if export.copy_to(&attachment.entry, &under(&attachment.target))? {
                    copied += 1;
                } else {
                    kept += 1;
                }
            }
        }
        Ok(serde_json::json!({
            "folder": under(&Files::folder(self.format, &reference.id)),
            "files": plan.len(), "copied": copied, "already_present": kept, "dry_run": dry_run,
        }))
    }
}

impl Provider for WebProvider {
    fn name(&self) -> &'static str {
        self.format.name()
    }

    fn discover(&self) -> anyhow::Result<Vec<SessionRef>> {
        Ok(self
            .loaded()?
            .chats
            .iter()
            .map(|c| c.reference.clone())
            .collect())
    }

    fn read(&self, reference: &SessionRef) -> anyhow::Result<Session> {
        Ok(self.convert(reference)?.0)
    }

    fn write(&self, _session: &Session, _opts: &WriteOpts) -> anyhow::Result<WriteOutcome> {
        let app = self.format.app();
        anyhow::bail!(
            "{app} has no import; harness-bridge reads {app} exports but cannot write to {app}. Choose another --to"
        )
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn session_ref(
    format: Format,
    id: String,
    title: Option<String>,
    created_ms: Option<i64>,
    updated_ms: Option<i64>,
) -> SessionRef {
    SessionRef {
        provider: format.name().into(),
        locator: id.clone(),
        id,
        title,
        cwd: None,
        created_ms,
        updated_ms,
        migrated: false,
    }
}

/// Resolves attachment references for one conversation and records the
/// files that will be copied.
struct Files<'a> {
    export: &'a Export,
    folder: String,
    found: Vec<Attachment>,
}

impl<'a> Files<'a> {
    fn new(export: &'a Export, format: Format, conversation: &str) -> Self {
        Self {
            export,
            folder: Self::folder(format, conversation),
            found: Vec::new(),
        }
    }

    fn folder(format: Format, conversation: &str) -> String {
        let id: String = conversation
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        format!("{}-attachments/{id}", format.name())
    }

    /// A reference by asset id, such as ChatGPT's `file-ABC123`.
    fn by_prefix(&mut self, kind: &str, id: &str, label: &str) -> String {
        match self.export.find_prefix(id) {
            Some(entry) => self.reference(kind, entry.to_string()),
            None => format!("[{kind} {label} was not included in the export]"),
        }
    }

    /// A reference by file name.
    fn by_name(&mut self, kind: &str, name: &str) -> String {
        match self.export.find_name(name) {
            Some(entry) => self.reference(kind, entry.to_string()),
            None => format!("[{kind} {name} was not included in the export]"),
        }
    }

    fn reference(&mut self, kind: &str, entry: String) -> String {
        let name: String = entry
            .rsplit('/')
            .next()
            .unwrap_or(&entry)
            .chars()
            .map(|c| {
                if c.is_control() || "<>:\"/\\|?*".contains(c) {
                    '_'
                } else {
                    c
                }
            })
            .collect();
        let target = format!("{}/{name}", self.folder);
        if !self.found.iter().any(|a| a.entry == entry) {
            self.found.push(Attachment {
                entry,
                target: target.clone(),
            });
        }
        format!("[{kind}: {target}]")
    }
}

const NO_OUTPUT: &str = "[no output recorded in the export]";

/// Assembles turns and keeps every tool result directly behind its call.
#[derive(Default)]
struct Builder {
    events: Vec<Event>,
    in_turn: bool,
    open: Vec<String>,
    last_result: Option<usize>,
}

impl Builder {
    fn push(&mut self, time: Option<i64>, kind: EventKind) {
        self.events.push(Event::at(time, kind));
    }

    /// Calls still open when the conversation moves on get an explicit empty result.
    fn settle(&mut self, time: Option<i64>) {
        for call_id in std::mem::take(&mut self.open) {
            self.push(
                time,
                EventKind::ToolResult {
                    call_id,
                    text: NO_OUTPUT.into(),
                },
            );
        }
        self.last_result = None;
    }

    fn open_turn(&mut self, time: Option<i64>) {
        if !self.in_turn {
            self.push(time, EventKind::TurnStart);
            self.in_turn = true;
        }
    }

    fn user(&mut self, time: Option<i64>, text: String) {
        self.settle(time);
        if self.in_turn {
            self.push(time, EventKind::TurnEnd { reason: None });
            self.in_turn = false;
        }
        self.open_turn(time);
        let text = if text.trim().is_empty() {
            "[empty message]".into()
        } else {
            text
        };
        self.push(
            time,
            EventKind::Message {
                role: Role::User,
                text,
                source_kind: None,
            },
        );
    }

    fn assistant(&mut self, time: Option<i64>, text: String) {
        if text.trim().is_empty() {
            return;
        }
        self.settle(time);
        self.open_turn(time);
        self.push(
            time,
            EventKind::Message {
                role: Role::Assistant,
                text,
                source_kind: None,
            },
        );
    }

    fn reasoning(&mut self, time: Option<i64>, text: String) {
        if text.trim().is_empty() {
            return;
        }
        self.settle(time);
        self.open_turn(time);
        self.push(time, EventKind::Reasoning { text });
    }

    fn call(&mut self, time: Option<i64>, call_id: &str, name: &str, arguments: String) {
        self.open_turn(time);
        self.push(
            time,
            EventKind::ToolCall {
                call_id: call_id.into(),
                name: safe_tool_name(name),
                arguments,
            },
        );
        self.open.push(call_id.into());
        self.last_result = None;
    }

    /// Output for `call_id`, for the open call, appended to the result just
    /// written, or, with no call to answer, kept as text labeled with the tool.
    fn result(&mut self, time: Option<i64>, call_id: Option<&str>, tool: &str, text: String) {
        let explicit = call_id.and_then(|id| self.open.iter().position(|o| o == id));
        if explicit.is_none() {
            if let Some(index) = self.last_result.filter(|i| *i + 1 == self.events.len()) {
                if let EventKind::ToolResult { text: previous, .. } = &mut self.events[index].kind {
                    previous.push_str("\n\n");
                    previous.push_str(&text);
                }
                return;
            }
        }
        let Some(position) = explicit.or_else(|| self.open.len().checked_sub(1)) else {
            self.assistant(time, format!("[{tool}] {text}"));
            return;
        };
        let call_id = self.open.remove(position);
        self.push(time, EventKind::ToolResult { call_id, text });
        self.last_result = Some(self.events.len() - 1);
    }

    fn finish(mut self) -> Vec<Event> {
        let time = self.events.last().and_then(|e| e.time_ms);
        self.settle(time);
        if self.in_turn {
            self.push(time, EventKind::TurnEnd { reason: None });
        }
        self.events
    }
}

/// Tool names writers and the Claude API accept: `[A-Za-z0-9_-]`, 64 at most.
fn safe_tool_name(name: &str) -> String {
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    if safe.is_empty() {
        "tool".into()
    } else {
        safe
    }
}

/// Removes private-use citation markers (U+E200 through U+E201) that web
/// exports embed in answer text.
fn strip_citations(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut inside = false;
    for c in text.chars() {
        match c {
            '\u{e200}' => inside = true,
            '\u{e201}' => inside = false,
            _ if inside || ('\u{e200}'..='\u{e2ff}').contains(&c) => {}
            _ => out.push(c),
        }
    }
    out
}

fn parse_time(text: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|t| t.timestamp_millis())
}

fn secs_ms(secs: f64) -> i64 {
    (secs * 1000.0).round() as i64
}

/// The first line of a prompt, shortened for a title.
fn title_from(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    let title: String = line.chars().take(60).collect();
    Some(if line.chars().count() > 60 {
        format!("{}...", title.trim_end())
    } else {
        title
    })
}

/// Plain text from the small HTML subset web exports use for answers.
fn html_to_text(html: &str) -> String {
    let mut out = String::new();
    let mut pre = 0usize;
    let mut rest = html;
    while let Some(start) = rest.find('<') {
        push_text(&mut out, &rest[..start], pre > 0);
        let Some(len) = rest[start..].find('>') else {
            rest = &rest[start..];
            break;
        };
        let tag = &rest[start + 1..start + len];
        rest = &rest[start + len + 1..];
        let closing = tag.starts_with('/');
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .collect::<String>()
            .to_ascii_lowercase();
        match name.as_str() {
            "br" => out.push('\n'),
            "li" if !closing => {
                breaks(&mut out, 1);
                out.push_str("- ");
            }
            "tr" | "div" => breaks(&mut out, 1),
            "pre" => {
                breaks(&mut out, 2);
                pre = if closing {
                    pre.saturating_sub(1)
                } else {
                    pre + 1
                };
            }
            "p" | "ul" | "ol" | "table" | "blockquote" | "h1" | "h2" | "h3" | "h4" | "h5"
            | "h6" => breaks(&mut out, 2),
            "td" | "th" if closing => out.push(' '),
            "script" | "style" if !closing => {
                let end = format!("</{name}");
                rest = rest.find(&end).map_or("", |i| &rest[i..]);
            }
            _ => {}
        }
    }
    push_text(&mut out, rest, pre > 0);
    let mut lines: Vec<&str> = Vec::new();
    for line in out.lines().map(str::trim_end) {
        if !(line.is_empty() && lines.last().is_none_or(|l| l.is_empty())) {
            lines.push(line);
        }
    }
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

fn push_text(out: &mut String, raw: &str, pre: bool) {
    let text = decode_entities(raw);
    if pre {
        out.push_str(&text);
        return;
    }
    let mut collapsed = String::new();
    let mut space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            space = true;
            continue;
        }
        if space && !(collapsed.is_empty() && (out.is_empty() || out.ends_with(['\n', ' ']))) {
            collapsed.push(' ');
        }
        space = false;
        collapsed.push(c);
    }
    if space && !collapsed.is_empty() {
        collapsed.push(' ');
    }
    out.push_str(&collapsed);
}

/// Ends the output with at least `n` newlines, unless nothing was written yet.
fn breaks(out: &mut String, n: usize) {
    while out.ends_with(' ') {
        out.pop();
    }
    if out.is_empty() {
        return;
    }
    let have = out.chars().rev().take_while(|c| *c == '\n').count();
    for _ in have..n {
        out.push('\n');
    }
}

fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let decoded = rest[1..]
            .find(';')
            .filter(|end| *end <= 10)
            .and_then(|end| {
                let entity = &rest[1..1 + end];
                let c = match entity {
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    "nbsp" => Some(' '),
                    _ => entity
                        .strip_prefix("#x")
                        .or_else(|| entity.strip_prefix("#X"))
                        .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                        .or_else(|| entity.strip_prefix('#').and_then(|dec| dec.parse().ok()))
                        .and_then(char::from_u32),
                }?;
                Some((c, end + 2))
            });
        match decoded {
            Some((c, len)) => {
                out.push(c);
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}
