pub mod agy;
pub mod claude;
pub mod codex;
pub mod dsh;
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

#[derive(Clone, Default)]
pub struct Homes {
    pub dsh: Option<std::path::PathBuf>,
    pub codex: Option<std::path::PathBuf>,
    pub claude: Option<std::path::PathBuf>,
    pub zcode_db: Option<std::path::PathBuf>,
    pub agy: Option<std::path::PathBuf>,
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
        other => anyhow::bail!(
            "unknown provider {other:?} (expected: dsh | codex | claude | zcode | agy)"
        ),
    }
}
