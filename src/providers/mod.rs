pub mod agy;
pub mod claude;
pub mod codex;
pub mod dsh;
pub mod qoder;
pub mod web;
pub mod zcode;

use crate::ir::{Session, SessionRef, WriteOpts, WriteOutcome};

pub trait Provider {
    fn name(&self) -> &'static str;

    fn discover(&self) -> anyhow::Result<Vec<SessionRef>>;

    fn read(&self, r: &SessionRef) -> anyhow::Result<Session>;

    /// Archive export can defer a known unavailable checkpoint to import.
    /// Other read/parse errors must still fail rather than inventing history.
    fn read_for_export(&self, r: &SessionRef) -> anyhow::Result<Session> {
        self.read(r)
    }

    fn write(&self, s: &Session, opts: &WriteOpts) -> anyhow::Result<WriteOutcome>;

    /// Direct children only; migration traverses the family with cycle checks.
    fn children(&self, _parent: &SessionRef) -> anyhow::Result<Vec<SessionRef>> {
        Ok(Vec::new())
    }

    /// Downcast support (dsh -> dsh preset porting).
    fn as_any(&self) -> &dyn std::any::Any;
}

/// Source ids already migrated into codex, shared with the Python-era tool:
/// `~/.codex/dsh-imports.json` maps source-native id -> {thread_id, rollout, ...}.
pub fn load_codex_import_ledger(codex_home: &std::path::Path) -> serde_json::Value {
    let p = codex_home.join("dsh-imports.json");
    std::fs::read_to_string(p)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| serde_json::json!({}))
}

pub fn save_codex_import_ledger(
    codex_home: &std::path::Path,
    ledger: &serde_json::Value,
) -> anyhow::Result<()> {
    let p = codex_home.join("dsh-imports.json");
    std::fs::write(p, serde_json::to_string_pretty(ledger)?)?;
    Ok(())
}

/// git metadata for the codex `session_meta.git` block (empty when not a repo).
pub fn git_info(cwd: &str) -> serde_json::Value {
    let mut info = serde_json::Map::new();
    let probes: Vec<(&str, Vec<&str>)> = vec![
        ("commit_hash", vec!["rev-parse", "HEAD"]),
        ("branch", vec!["branch", "--show-current"]),
        ("repository_url", vec!["remote", "get-url", "origin"]),
    ];
    for (key, args) in probes {
        if let Ok(o) = std::process::Command::new("git")
            .args(["-C", cwd])
            .args(args)
            .output()
        {
            if o.status.success() {
                let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                if !s.is_empty() {
                    info.insert(key.into(), serde_json::json!(s));
                }
            }
        }
    }
    serde_json::Value::Object(info)
}

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Homes {
    pub dsh: Option<std::path::PathBuf>,
    pub codex: Option<std::path::PathBuf>,
    pub claude: Option<std::path::PathBuf>,
    pub zcode_db: Option<std::path::PathBuf>,
    pub agy: Option<std::path::PathBuf>,
    pub qoder: Option<std::path::PathBuf>,
    pub qoder_ide_db: Option<std::path::PathBuf>,
    /// A web chat export (zip, folder or file) for the read-only web providers.
    pub export: Option<std::path::PathBuf>,
}

pub fn provider_named(name: &str, homes: &Homes) -> anyhow::Result<Box<dyn Provider>> {
    match name {
        "dsh" => Ok(Box::new(dsh::DshProvider::new(
            homes
                .dsh
                .clone()
                .unwrap_or_else(crate::util::default_dsh_home),
        ))),
        "codex" => Ok(Box::new(codex::CodexProvider::new(
            homes
                .codex
                .clone()
                .unwrap_or_else(crate::util::default_codex_home),
        ))),
        "claude" => Ok(Box::new(claude::ClaudeProvider::new(
            homes
                .claude
                .clone()
                .unwrap_or_else(crate::util::default_claude_home),
        ))),
        "zcode" => Ok(Box::new(zcode::ZcodeProvider::new(
            homes
                .zcode_db
                .clone()
                .unwrap_or_else(crate::util::default_zcode_db),
        ))),
        "agy" => Ok(Box::new(agy::AgyProvider::new(
            homes
                .agy
                .clone()
                .unwrap_or_else(crate::util::default_agy_dir),
        ))),
        "qoder" | "qoder-cli" => Ok(Box::new(qoder::QoderProvider::cli(
            homes.qoder.clone().unwrap_or_else(crate::util::default_qoder_home),
        ))),
        "qoder-ide" => Ok(Box::new(qoder::QoderProvider::ide(
            homes.qoder.clone().unwrap_or_else(crate::util::default_qoder_home),
            homes.qoder_ide_db.clone().or_else(|| homes.qoder.is_none().then(crate::util::default_qoder_ide_db).flatten()),
        ))),
        other => match web::Format::from_name(other) {
            Some(format) => Ok(Box::new(web::WebProvider::new(format, homes.export.clone()))),
            None => anyhow::bail!(
                "unknown provider {other:?} (expected: dsh | codex | claude | zcode | agy | qoder | qoder-ide | chatgpt | claude-web | gemini-web)"
            ),
        },
    }
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct ProviderInfo {
    pub name: &'static str,
    pub label: &'static str,
    pub writable: bool,
    pub export_required: bool,
    pub note: &'static str,
}

pub const CATALOG: &[ProviderInfo] = &[
    ProviderInfo { name: "dsh", label: "DeepSeek Harness", writable: true, export_required: false, note: "Native sessions and saved children; the app may need to rescan." },
    ProviderInfo { name: "codex", label: "Codex", writable: true, export_required: false, note: "Native sessions with Desktop registration. Initialize a Desktop session first; refresh the window after import." },
    ProviderInfo { name: "claude", label: "Claude Code", writable: true, export_required: false, note: "Native sessions. Developer-channel context cannot be represented." },
    ProviderInfo { name: "zcode", label: "ZCode", writable: true, export_required: false, note: "Writes to the selected SQLite database; initialize ZCode first." },
    ProviderInfo { name: "agy", label: "Antigravity", writable: true, export_required: false, note: "Text turns only; tool steps remain in the migration archive. Requires a local template conversation." },
    ProviderInfo { name: "qoder", label: "Qoder CLI", writable: true, export_required: false, note: "Project transcripts; resume with qoder --resume <id> from the destination folder." },
    ProviderInfo { name: "qoder-ide", label: "Qoder IDE", writable: false, export_required: false, note: "Read-only IDE transcripts and local desktop history. Import into Qoder CLI or another harness." },
    ProviderInfo { name: "chatgpt", label: "ChatGPT export", writable: false, export_required: true, note: "Select a data export with --export; a destination folder is required." },
    ProviderInfo { name: "claude-web", label: "claude.ai export", writable: false, export_required: true, note: "Select a data export with --export; a destination folder is required." },
    ProviderInfo { name: "gemini-web", label: "Gemini export", writable: false, export_required: true, note: "Select a Takeout JSON export with --export; a destination folder is required." },
];

pub fn info(name: &str) -> anyhow::Result<&'static ProviderInfo> {
    let name = if name == "qoder-cli" { "qoder" } else { name };
    CATALOG
        .iter()
        .find(|p| p.name == name)
        .ok_or_else(|| anyhow::anyhow!("unknown provider {name:?}; run harness-bridge providers"))
}
