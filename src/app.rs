//! Application operations shared by all frontends. A plan holds a source
//! snapshot, so confirmation writes exactly the conversation that was previewed.

use crate::{
    ir::{SessionRef, WriteOpts, WriteOutcome},
    portable::Bundle,
    providers::{self, Homes},
};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Serialize)]
pub struct ProviderStatus {
    pub info: providers::ProviderInfo,
    pub sessions: usize,
    pub problem: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Catalog {
    pub providers: Vec<ProviderStatus>,
    pub sessions: Vec<SessionRef>,
}

pub fn discover(homes: &Homes) -> Catalog {
    let mut catalog = Catalog::default();
    let export_format = homes.export.as_deref().map(|path| {
        providers::web::Export::open(path)
            .and_then(|export| providers::web::Format::detect(&export))
    });
    for info in providers::CATALOG {
        let result = if info.export_required {
            match &export_format {
                Some(Ok(format)) if format.name() == info.name => {
                    discover_provider(info.name, homes)
                }
                Some(Err(error)) => Err(anyhow::anyhow!("{error:#}")),
                _ => {
                    continue;
                }
            }
        } else {
            discover_provider(info.name, homes)
        };
        let (sessions, problem) = match result {
            Ok(sessions) => (sessions, None),
            Err(error) => (Vec::new(), Some(format!("{error:#}"))),
        };
        catalog.providers.push(ProviderStatus {
            info: *info,
            sessions: sessions.len(),
            problem,
        });
        catalog.sessions.extend(sessions);
    }
    sort_sessions(&mut catalog.sessions);
    catalog
}

pub fn discover_provider(name: &str, homes: &Homes) -> anyhow::Result<Vec<SessionRef>> {
    let mut sessions = providers::provider_named(name, homes)?.discover()?;
    let ledger = providers::load_codex_import_ledger(
        &homes
            .codex
            .clone()
            .unwrap_or_else(crate::util::default_codex_home),
    );
    if let Some(ledger) = ledger.as_object() {
        for session in &mut sessions {
            let key = if session.provider == "dsh" {
                session.id.clone()
            } else {
                format!("{}:{}", session.provider, session.id)
            };
            session.migrated = ledger
                .keys()
                .any(|k| k == &key || k.ends_with(&format!(":{key}")));
        }
    }
    sort_sessions(&mut sessions);
    Ok(sessions)
}

fn sort_sessions(sessions: &mut [SessionRef]) {
    sessions.sort_by(|a, b| {
        b.updated_ms
            .or(b.created_ms)
            .cmp(&a.updated_ms.or(a.created_ms))
            .then(a.provider.cmp(&b.provider))
            .then(a.id.cmp(&b.id))
    });
}

pub fn matches(reference: &SessionRef, query: &str) -> bool {
    let haystack = format!(
        "{} {} {} {}",
        reference.provider,
        reference.id,
        reference.title.as_deref().unwrap_or_default(),
        reference.cwd.as_deref().unwrap_or_default()
    )
    .to_lowercase();
    query
        .split_whitespace()
        .all(|word| haystack.contains(&word.to_lowercase()))
}

pub fn resolve(references: &[SessionRef], query: &str) -> anyhow::Result<SessionRef> {
    let query = query.trim();
    anyhow::ensure!(!query.is_empty(), "enter a session id, id prefix or title");
    if let Some(exact) = references.iter().find(|r| r.id == query) {
        return Ok(exact.clone());
    }
    let lower = query.to_lowercase();
    let ids: Vec<_> = references
        .iter()
        .filter(|r| r.id.to_lowercase().starts_with(&lower))
        .collect();
    let hits = if ids.is_empty() {
        references
            .iter()
            .filter(|r| {
                r.title
                    .as_deref()
                    .is_some_and(|t| t.to_lowercase().contains(&lower))
            })
            .collect()
    } else {
        ids
    };
    match hits.as_slice() {
        [] => anyhow::bail!("no session matches {query:?}; run harness-bridge list to find its id"),
        [one] => Ok((*one).clone()),
        _ => anyhow::bail!(
            "multiple sessions match {query:?}; use a longer id:\n{}",
            hits.iter()
                .map(|r| format!("  {}  {}", r.id, r.title.as_deref().unwrap_or("(untitled)")))
                .collect::<Vec<_>>()
                .join("\n")
        ),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ExcerptMessage {
    pub role: &'static str,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionDetails {
    pub reference: SessionRef,
    pub events: usize,
    pub messages: usize,
    pub tool_calls: usize,
    pub resume_chars: usize,
    pub unavailable_context: Option<String>,
    pub excerpt: String,
    pub excerpt_messages: Vec<ExcerptMessage>,
}

pub fn inspect(reference: &SessionRef, homes: &Homes) -> anyhow::Result<SessionDetails> {
    use crate::ir::{EventKind, Role};
    let session =
        providers::provider_named(&reference.provider, homes)?.read_for_export(reference)?;
    let mut messages = 0;
    let mut tool_calls = 0;
    let mut excerpts = Vec::new();
    let mut excerpt_messages = Vec::new();
    for event in &session.events {
        match &event.kind {
            EventKind::Message { role, text, .. } => {
                messages += 1;
                if excerpts.len() < 6 && !matches!(role, Role::Developer) {
                    let text = text.chars().take(1200).collect::<String>();
                    excerpts.push(format!(
                        "{}\n{}",
                        if *role == Role::User {
                            "You"
                        } else {
                            "Assistant"
                        },
                        text
                    ));
                    excerpt_messages.push(ExcerptMessage {
                        role: if *role == Role::User {
                            "user"
                        } else {
                            "assistant"
                        },
                        text,
                    });
                }
            }
            EventKind::ToolCall { .. } => tool_calls += 1,
            _ => {}
        }
    }
    Ok(SessionDetails {
        reference: reference.clone(),
        events: session.events.len(),
        messages,
        tool_calls,
        resume_chars: serde_json::to_string(
            session.resume_events.as_ref().unwrap_or(&session.events),
        )?
        .chars()
        .count(),
        unavailable_context: session.resume_context_unavailable,
        excerpt: excerpts.join("\n\n"),
        excerpt_messages,
    })
}

fn default_budget() -> usize {
    crate::resume::DEFAULT_MAX_CHARS
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransferRequest {
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub file: Option<PathBuf>,
    pub to: String,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub include_subagents: bool,
    #[serde(default = "default_budget")]
    pub resume_max_chars: usize,
    #[serde(default)]
    pub prune_resume_context: bool,
    #[serde(default)]
    pub rebuild_resume_context: bool,
}

impl TransferRequest {
    pub fn new(reference: &SessionRef, destination: &str) -> Self {
        Self {
            from: reference.provider.clone(),
            id: reference.id.clone(),
            file: None,
            to: destination.into(),
            cwd: None,
            name: None,
            include_subagents: false,
            resume_max_chars: default_budget(),
            prune_resume_context: false,
            rebuild_resume_context: false,
        }
    }
}

pub struct TransferPlan {
    bundle: Bundle,
    homes: Homes,
    request: TransferRequest,
    web_reference: Option<SessionRef>,
    pub preview: WriteOutcome,
    pub warnings: Vec<String>,
    destination_stamps: Vec<(String, Option<FileStamp>)>,
}

impl TransferPlan {
    pub fn prepare(mut request: TransferRequest, homes: &Homes) -> anyhow::Result<Self> {
        let destination_info = providers::info(&request.to)?;
        anyhow::ensure!(
            destination_info.writable,
            "{} is a read-only source; choose a writable destination",
            destination_info.label
        );
        request.to = destination_info.name.into();
        request.cwd = request.cwd.filter(|value| !value.trim().is_empty());
        request.name = request.name.filter(|value| !value.trim().is_empty());
        let (bundle, web_reference) = if let Some(file) = &request.file {
            anyhow::ensure!(
                request.from.is_empty() && request.id.is_empty(),
                "choose either a portable file or a native session"
            );
            (Bundle::load(file)?, None)
        } else {
            let from = providers::info(&request.from)?.name;
            anyhow::ensure!(
                from != request.to,
                "source and destination must differ; use the command line for DSH preset copies"
            );
            let source = providers::provider_named(from, homes)?;
            let reference = source
                .discover()?
                .into_iter()
                .find(|r| r.id == request.id)
                .context("the selected session is no longer available; refresh the list")?;
            if providers::info(from)?.export_required {
                anyhow::ensure!(
                    request.cwd.is_some(),
                    "choose a destination folder for this web chat and its attachments"
                );
            }
            let bundle = Bundle::collect(
                source.as_ref(),
                reference.clone(),
                request.include_subagents,
            )?;
            (
                bundle,
                providers::info(from)?.export_required.then_some(reference),
            )
        };
        let destination = providers::provider_named(&request.to, homes)?;
        let mut preview = crate::family::migrate(
            &bundle,
            destination.as_ref(),
            bundle.root(),
            &WriteOpts {
                cwd: request.cwd.clone(),
                name: request.name.clone(),
                dry_run: true,
            },
            true,
            request.resume_max_chars,
            crate::resume::Options {
                prune: request.prune_resume_context,
                rebuild: request.rebuild_resume_context,
            },
        )?;
        if let Some(reference) = &web_reference {
            let source = providers::provider_named(&reference.provider, homes)?;
            let web = source
                .as_any()
                .downcast_ref::<providers::web::WebProvider>()
                .context("expected web provider")?;
            preview.extra["attachments"] =
                web.copy_attachments(reference, Path::new(request.cwd.as_deref().unwrap()), true)?;
        }
        let destination_stamps = destination_paths(&preview)
            .into_iter()
            .map(|path| {
                let before = stamp(&path);
                (path, before)
            })
            .collect::<Vec<_>>();
        let mut warnings = vec![destination_info.note.into(), "Importing the same source again uses the same destination id and can replace turns added there. Preserve that copy before reimporting.".into()];
        if destination_stamps.iter().any(|(_, stamp)| stamp.is_some()) {
            warnings
                .push("The destination artifact already exists. Writing will update it.".into());
        }
        let root = bundle.root();
        if let Some(cwd) = request.cwd.as_deref().or(root.cwd.as_deref()) {
            if !Path::new(cwd).is_dir() {
                warnings.push(format!("The project folder {cwd} does not exist on this computer. Create it or choose another folder before resuming."));
            }
        }
        if request.prune_resume_context {
            warnings.push("Resume context may shorten reasoning and tool outputs; the full archive is retained.".into());
        }
        if request.rebuild_resume_context {
            warnings.push("Context recovery cannot recover hidden state. Check the imported task and decisions before continuing.".into());
        }
        Ok(Self {
            bundle,
            homes: homes.clone(),
            request,
            web_reference,
            preview,
            warnings,
            destination_stamps,
        })
    }

    pub fn report(&self) -> Value {
        json!({"outcome":self.preview, "warnings":self.warnings})
    }

    pub fn execute(self) -> anyhow::Result<WriteOutcome> {
        anyhow::ensure!(
            self.destination_stamps
                .iter()
                .all(|(path, before)| stamp(path) == *before),
            "the destination changed after preview; preview again before writing"
        );
        let destination = providers::provider_named(&self.request.to, &self.homes)?;
        let mut outcome = crate::family::migrate(
            &self.bundle,
            destination.as_ref(),
            self.bundle.root(),
            &WriteOpts {
                cwd: self.request.cwd.clone(),
                name: self.request.name,
                dry_run: false,
            },
            true,
            self.request.resume_max_chars,
            crate::resume::Options {
                prune: self.request.prune_resume_context,
                rebuild: self.request.rebuild_resume_context,
            },
        )?;
        if let Some(reference) = self.web_reference {
            let source = providers::provider_named(&reference.provider, &self.homes)?;
            let web = source
                .as_any()
                .downcast_ref::<providers::web::WebProvider>()
                .context("expected web provider")?;
            outcome.extra["attachments"] = web.copy_attachments(
                &reference,
                Path::new(self.request.cwd.as_deref().unwrap()),
                false,
            )?;
        }
        Ok(outcome)
    }
}

type FileStamp = (u64, Option<std::time::SystemTime>);

fn destination_paths(outcome: &WriteOutcome) -> Vec<String> {
    let mut paths = Vec::new();
    let location = &outcome.location;
    if outcome.provider == "zcode" {
        let database = location
            .rsplit_once('#')
            .map_or(location.as_str(), |(path, _)| path);
        paths.extend([database.to_owned(), format!("{database}-wal")]);
        if let Some(index) = outcome.extra["desktop_index"].as_str() {
            paths.extend([index.to_owned(), format!("{index}-wal")]);
        }
    } else {
        paths.push(location.clone());
    }
    if let Some(archive) = outcome.extra["archive"].as_str() {
        paths.push(archive.into());
    }
    if let Some(children) = outcome.extra["child_sessions"].as_array() {
        for child in children {
            if let Ok(child) = serde_json::from_value::<WriteOutcome>(child.clone()) {
                paths.extend(destination_paths(&child));
            }
        }
    }
    paths
}

fn stamp(location: &str) -> Option<FileStamp> {
    std::fs::metadata(location)
        .ok()
        .filter(|meta| meta.is_file())
        .map(|meta| (meta.len(), meta.modified().ok()))
}

pub fn export(
    reference: &SessionRef,
    homes: &Homes,
    path: &Path,
    include_children: bool,
    dry_run: bool,
) -> anyhow::Result<Value> {
    let source = providers::provider_named(&reference.provider, homes)?;
    Bundle::collect(source.as_ref(), reference.clone(), include_children)?.save(path, dry_run)
}
