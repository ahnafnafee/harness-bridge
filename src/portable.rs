//! Versioned, destination-independent session files for machine-to-machine moves.

use crate::{
    ir::{Session, SessionRef, WriteOpts, WriteOutcome},
    providers::Provider,
};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::{BufReader, Write},
    path::Path,
};

const FORMAT: &str = "harness-bridge-session";
const VERSION: u32 = 2;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    root_session_id: String,
    sessions: Vec<Session>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bundle {
    format: String,
    version: u32,
    exported_at: String,
    sha256: String,
    payload: Payload,
}

fn checksum(payload: &impl Serialize) -> anyhow::Result<String> {
    fn sort_objects(value: &mut Value) {
        match value {
            Value::Object(object) => {
                object.values_mut().for_each(sort_objects);
                object.sort_keys();
            }
            Value::Array(array) => array.iter_mut().for_each(sort_objects),
            _ => {}
        }
    }
    let mut value = serde_json::to_value(payload)?;
    sort_objects(&mut value);
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(&value)?)))
}

impl Bundle {
    /// Read source stores only. Destination budgeting belongs to import, where
    /// the chosen harness/model and machine are known.
    pub fn collect(
        source: &dyn Provider,
        root: SessionRef,
        include_children: bool,
    ) -> anyhow::Result<Self> {
        let root_session_id = root.id.clone();
        let mut queue = VecDeque::from([(root, None::<String>)]);
        let mut seen = HashSet::new();
        let mut sessions = Vec::new();
        while let Some((reference, parent)) = queue.pop_front() {
            anyhow::ensure!(
                seen.insert(reference.id.clone()),
                "duplicate or cyclic child session {:?}",
                reference.id
            );
            anyhow::ensure!(
                seen.len() <= crate::family::MAX_SESSIONS,
                "session family exceeds the 1024-session safety bound"
            );
            if include_children {
                for child in source.children(&reference)? {
                    queue.push_back((child, Some(reference.id.clone())));
                }
            }
            let mut session = source
                .read_for_export(&reference)
                .with_context(|| format!("cannot export family member {}", reference.id))?;
            anyhow::ensure!(
                session.id == reference.id,
                "source returned a different session id for {}",
                reference.id
            );
            // Native child readers may report a shared root id. Store canonical
            // discovery links so the receiver can translate every generation.
            if parent.is_some() {
                session.parent_session = parent;
            }
            sessions.push(session);
        }
        Self::new(Payload {
            root_session_id,
            sessions,
        })
    }

    fn new(payload: Payload) -> anyhow::Result<Self> {
        // Old readers cannot enforce the unavailable-context guard. Keep normal
        // files at v1; require v2 only when context reconstruction is necessary.
        let version = if payload
            .sessions
            .iter()
            .any(|s| s.resume_context_unavailable.is_some())
        {
            VERSION
        } else {
            1
        };
        let bundle = Self {
            format: FORMAT.into(),
            version,
            exported_at: crate::util::iso_ms(chrono::Utc::now().timestamp_millis()),
            sha256: checksum(&payload)?,
            payload,
        };
        bundle.validate()?;
        Ok(bundle)
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let file = std::fs::File::open(path)
            .with_context(|| format!("cannot open portable session {}", path.display()))?;
        let value: Value = serde_json::from_reader(BufReader::new(file))
            .with_context(|| format!("invalid portable session JSON in {}", path.display()))?;
        anyhow::ensure!(
            value["format"] == FORMAT,
            "not a harness-bridge portable session (expected format {FORMAT:?})"
        );
        anyhow::ensure!(
            matches!(value["version"].as_u64(), Some(1..=2)),
            "unsupported portable session version {}; this build supports versions 1 through {VERSION}",
            value["version"]
        );
        anyhow::ensure!(
            value["sha256"] == checksum(&value["payload"])?,
            "portable session checksum mismatch; the payload is damaged or changed"
        );
        let bundle: Self =
            serde_json::from_value(value).context("invalid portable session schema")?;
        bundle.validate()?;
        Ok(bundle)
    }

    fn validate(&self) -> anyhow::Result<()> {
        let sessions = &self.payload.sessions;
        anyhow::ensure!(
            !sessions.is_empty(),
            "portable session contains no sessions"
        );
        anyhow::ensure!(
            sessions.len() <= crate::family::MAX_SESSIONS,
            "session family exceeds the 1024-session safety bound"
        );
        chrono::DateTime::parse_from_rfc3339(&self.exported_at)
            .context("invalid export timestamp")?;
        let mut by_id = HashMap::new();
        for session in sessions {
            if let Some(reason) = &session.resume_context_unavailable {
                anyhow::ensure!(
                    self.version >= 2,
                    "unavailable resume context requires portable format version 2"
                );
                anyhow::ensure!(
                    !reason.trim().is_empty(),
                    "unavailable resume context has no reason"
                );
            }
            anyhow::ensure!(
                matches!(
                    session.source.as_str(),
                    "dsh" | "codex" | "claude" | "zcode" | "agy"
                ),
                "unsupported source provider {:?}",
                session.source
            );
            anyhow::ensure!(
                session.source == sessions[0].source,
                "portable family mixes source providers"
            );
            anyhow::ensure!(
                !session.id.trim().is_empty(),
                "portable session has an empty id"
            );
            anyhow::ensure!(
                by_id.insert(session.id.as_str(), session).is_none(),
                "duplicate portable session id {:?}",
                session.id
            );
            for ms in [Some(session.created_ms), Some(session.updated_ms)]
                .into_iter()
                .chain(
                    session
                        .events
                        .iter()
                        .chain(session.resume_events.iter().flatten())
                        .map(|event| event.time_ms),
                )
                .flatten()
            {
                anyhow::ensure!(
                    chrono::DateTime::from_timestamp_millis(ms).is_some(),
                    "invalid timestamp in portable session {}",
                    session.id
                );
            }
        }
        let root = by_id
            .get(self.payload.root_session_id.as_str())
            .context("portable root session is missing")?;
        anyhow::ensure!(
            !root
                .parent_session
                .as_deref()
                .is_some_and(|id| by_id.contains_key(id)),
            "portable root has a parent inside its own family"
        );
        for session in sessions.iter().filter(|s| s.id != root.id) {
            let parent = session
                .parent_session
                .as_deref()
                .context("portable child session has no parent")?;
            anyhow::ensure!(
                by_id.contains_key(parent),
                "missing parent {parent:?} for portable child {}",
                session.id
            );
        }
        let mut reachable = HashSet::new();
        let mut queue = VecDeque::from([root.id.as_str()]);
        while let Some(id) = queue.pop_front() {
            anyhow::ensure!(reachable.insert(id), "cyclic portable session family");
            queue.extend(
                sessions
                    .iter()
                    .filter(|s| s.parent_session.as_deref() == Some(id))
                    .map(|s| s.id.as_str()),
            );
        }
        anyhow::ensure!(
            reachable.len() == sessions.len(),
            "portable session contains disconnected or cyclic child sessions"
        );
        Ok(())
    }

    pub fn root(&self) -> SessionRef {
        // Constructors validate the root and family before exposing a provider.
        Self::reference(
            self.payload
                .sessions
                .iter()
                .find(|s| s.id == self.payload.root_session_id)
                .unwrap(),
        )
    }

    fn reference(session: &Session) -> SessionRef {
        SessionRef {
            provider: session.source.clone(),
            id: session.id.clone(),
            title: session.title.clone(),
            cwd: session.cwd.clone(),
            created_ms: Some(session.created_ms),
            updated_ms: Some(session.updated_ms),
            locator: session.id.clone(),
            migrated: false,
        }
    }

    pub fn save(&self, path: &Path, dry_run: bool) -> anyhow::Result<Value> {
        self.validate()?;
        let mut bytes = serde_json::to_vec_pretty(self)?;
        bytes.push(b'\n');
        if !dry_run {
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent)?;
            }
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .with_context(|| {
                    format!(
                        "cannot create export {}; choose a new path if it already exists",
                        path.display()
                    )
                })?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        Ok(
            json!({"format":self.format, "version":self.version, "location":path,
            "source_provider":self.payload.sessions[0].source, "root_session_id":self.payload.root_session_id,
            "sessions":self.payload.sessions.len(), "events":self.payload.sessions.iter().map(|s|s.events.len()).sum::<usize>(),
            "bytes":bytes.len(), "sha256":self.sha256, "dry_run":dry_run,
            "resume_context_unavailable":self.payload.sessions.iter().filter_map(|s| s.resume_context_unavailable.as_ref().map(|reason|json!({"session_id":s.id,"reason":reason}))).collect::<Vec<_>>()}),
        )
    }
}

// The existing family pipeline can import entirely from this in-memory provider.
// No source homes, template files or databases from the exporting PC are opened.
impl Provider for Bundle {
    fn name(&self) -> &'static str {
        "portable"
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn discover(&self) -> anyhow::Result<Vec<SessionRef>> {
        Ok(self.payload.sessions.iter().map(Self::reference).collect())
    }
    fn read(&self, reference: &SessionRef) -> anyhow::Result<Session> {
        self.payload
            .sessions
            .iter()
            .find(|s| s.id == reference.id)
            .cloned()
            .with_context(|| format!("portable session {:?} is missing", reference.id))
    }
    fn children(&self, parent: &SessionRef) -> anyhow::Result<Vec<SessionRef>> {
        Ok(self
            .payload
            .sessions
            .iter()
            .filter(|s| s.parent_session.as_deref() == Some(&parent.id))
            .map(Self::reference)
            .collect())
    }
    fn write(&self, _: &Session, _: &WriteOpts) -> anyhow::Result<WriteOutcome> {
        anyhow::bail!("portable session provider is read-only; use export to create a file")
    }
}

#[cfg(test)]
mod tests;
