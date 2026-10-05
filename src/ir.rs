//! Normalized intermediate representation shared by all providers.
//!
//! Readers map a harness-native transcript into a [`Session`]; writers map a
//! [`Session`] back into their native format. [`EventKind::Meta`] carries
//! structured source records where supported; fidelity limits live with each
//! provider.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    /// A genuine user prompt (or harness notice delivered on the user channel).
    User,
    /// Assistant output.
    Assistant,
    /// Harness-injected context / instructions (codex `developer`, dsh system/context).
    Developer,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EventKind {
    TurnStart,
    TurnEnd {
        reason: Option<String>,
    },
    Message {
        role: Role,
        text: String,
        /// Provider-specific origin tag kept for lossless round-trips
        /// (e.g. dsh source kinds: `user`, `time-context`, `compact-checkpoint`, ...).
        source_kind: Option<String>,
    },
    /// Assistant thinking text (rendered as reasoning items where supported).
    Reasoning {
        text: String,
    },
    ToolCall {
        call_id: String,
        name: String,
        /// JSON-encoded arguments as emitted by the source harness.
        arguments: String,
    },
    ToolResult {
        call_id: String,
        text: String,
    },
    /// Context compaction marker; `text` when the source kept the summary.
    Compaction {
        id: Option<String>,
        text: Option<String>,
    },
    /// Harness-specific record preserved as structured JSON (goals, tool registry, ...).
    Meta {
        kind: String,
        data: serde_json::Value,
    },
}

/// An IR event with its original wall-clock time when the source kept one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub time_ms: Option<i64>,
    pub kind: EventKind,
}

impl Event {
    pub fn at(time_ms: Option<i64>, kind: EventKind) -> Self {
        Event { time_ms, kind }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Provider the session was read from ("dsh", "codex", "claude", "zcode", "agy").
    pub source: String,
    /// Source-native session id.
    pub id: String,
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub agent_preset: Option<String>,
    pub parent_session: Option<String>,
    pub origin: Option<String>,
    pub created_ms: i64,
    pub updated_ms: i64,
    pub events: Vec<Event>,
    /// Current model context after the source harness's replacements/pruning.
    /// `events` remains the complete transcript for display and archival fidelity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_events: Option<Vec<Event>>,
}

impl Session {
    pub fn first_user_text(&self) -> Option<String> {
        self.events.iter().find_map(|e| match &e.kind {
            EventKind::Message {
                role: Role::User,
                text,
                ..
            } if !text.trim().is_empty() => Some(text.clone()),
            _ => None,
        })
    }

    /// Effective event time, falling back to the session creation time.
    pub fn event_ms(&self, e: &Event, offset_ms: i64) -> i64 {
        e.time_ms.unwrap_or(self.created_ms + offset_ms)
    }
}

/// A session found by `discover`, before reading the full transcript.
#[derive(Debug, Clone, Serialize)]
pub struct SessionRef {
    pub provider: String,
    pub id: String,
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub created_ms: Option<i64>,
    pub updated_ms: Option<i64>,
    /// Provider-specific locator (file path, db key, ...) used by `read`.
    pub locator: String,
    pub migrated: bool,
}

#[derive(Debug, Clone, Default)]
pub struct WriteOpts {
    /// Base directory recorded in the written session (None = keep source cwd).
    pub cwd: Option<String>,
    /// Display name override.
    pub name: Option<String>,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct WriteOutcome {
    pub provider: String,
    /// Primary artifact path (or db key).
    pub location: String,
    /// Native id assigned in the target harness.
    pub native_id: String,
    pub extra: serde_json::Value,
}
