//! Command parsing, session selection, and migration orchestration.

use crate::{
    ir::{self, SessionRef},
    providers,
};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "harness-bridge",
    version,
    about = "Migrate coding-agent sessions between harnesses: dsh, Codex Desktop, Claude Code, ZCode, agy",
    long_about = None
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
    /// Codex home (default ~/.codex).
    #[arg(long, global = true)]
    codex_home: Option<String>,
    /// DeepSeek Harness home (default ~/.dsh).
    #[arg(long, global = true)]
    dsh_home: Option<String>,
    /// Claude Code home (default ~/.claude).
    #[arg(long, global = true)]
    claude_home: Option<String>,
    /// ZCode sqlite db (default ~/.zcode/cli/db/db.sqlite).
    #[arg(long, global = true)]
    zcode_db: Option<String>,
    /// agy data dir (default ~/.gemini/antigravity-cli).
    #[arg(long, global = true)]
    agy_dir: Option<String>,
}

#[derive(Subcommand)]
enum Command {
    /// List migratable sessions (all providers, or one).
    List {
        #[arg(long)]
        provider: Option<String>,
    },
    /// Export a portable session file for import on another machine.
    Export {
        /// Session id/prefix or title substring within the source harness.
        query: String,
        #[arg(long, default_value = "dsh")]
        from: String,
        /// New portable JSON file; existing files are never overwritten.
        #[arg(long, short)]
        output: PathBuf,
        /// Include saved child sessions recursively with their parent links.
        #[arg(long)]
        include_subagents: bool,
        /// Read and serialize the session without creating the export file.
        #[arg(long)]
        dry_run: bool,
    },
    /// Import a portable session file into this machine's chosen harness.
    Import {
        /// Portable JSON file created by harness-bridge export.
        file: PathBuf,
        #[arg(long, default_value = "codex")]
        to: String,
        /// Destination base directory; history paths remain as recorded.
        #[arg(long)]
        cwd: Option<String>,
        /// Display name for the imported root session (default: source title).
        #[arg(long)]
        name: Option<String>,
        /// Convert and preflight every bundled session without writing.
        #[arg(long)]
        dry_run: bool,
        /// Preflight character budget for retained context (not a model token limit).
        #[arg(long, default_value_t = crate::resume::DEFAULT_MAX_CHARS)]
        resume_max_chars: usize,
        /// Shorten reasoning/tool outputs in resume context; preserve the archive.
        #[arg(long)]
        prune_resume_context: bool,
    },
    /// Migrate one session from a source harness to a target harness.
    Migrate {
        /// Session id (or prefix) or title substring within the source provider.
        query: String,
        #[arg(long, default_value = "dsh")]
        from: String,
        #[arg(long, default_value = "codex")]
        to: String,
        /// Base directory recorded in the target session (default: the source cwd).
        #[arg(long)]
        cwd: Option<String>,
        /// Display name for the new session (default: source title).
        #[arg(long)]
        name: Option<String>,
        /// dsh -> dsh only: port the conversation to this agent preset
        /// (a directory preset under ~/.dsh/.agent-presets).
        #[arg(long)]
        preset: Option<String>,
        /// Convert without writing to the target harness.
        #[arg(long)]
        dry_run: bool,
        /// Preflight character budget for retained context (not a model token limit).
        #[arg(long, default_value_t = crate::resume::DEFAULT_MAX_CHARS)]
        resume_max_chars: usize,
        /// Shorten reasoning/tool outputs in resume context; preserve the full archive.
        #[arg(long)]
        prune_resume_context: bool,
        /// Import saved child sessions recursively and remap their parent links.
        #[arg(long)]
        include_subagents: bool,
    },
}

fn list_provider(name: &str, homes: &providers::Homes) -> anyhow::Result<()> {
    let p = providers::provider_named(name, homes)?;
    let mut refs = p.discover()?;
    let ledger_home = homes
        .codex
        .clone()
        .unwrap_or_else(crate::util::default_codex_home);
    let ledger = providers::load_codex_import_ledger(&ledger_home);
    let ledger_keys: Vec<String> = ledger
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    for r in refs.iter_mut() {
        let key = if r.provider == "dsh" {
            r.id.clone()
        } else {
            format!("{}:{}", r.provider, r.id)
        };
        r.migrated = ledger_keys
            .iter()
            .any(|k| *k == key || k.ends_with(&format!(":{key}")));
    }
    println!("== {} ({} sessions) ==", p.name(), refs.len());
    for r in &refs {
        println!(
            "  {:38}  {:8}  {}  {}",
            r.id,
            if r.migrated { "migrated" } else { "-" },
            r.title.as_deref().unwrap_or("(untitled)"),
            r.cwd.as_deref().unwrap_or("")
        );
    }
    Ok(())
}

fn resolve(refs: &[SessionRef], query: &str) -> anyhow::Result<SessionRef> {
    let q = query.trim().to_lowercase();
    let by_id: Vec<&SessionRef> = refs
        .iter()
        .filter(|r| r.id.to_lowercase().starts_with(&q) || r.id == q)
        .collect();
    let hits = if by_id.len() == 1 {
        by_id
    } else {
        let by_title: Vec<&SessionRef> = refs
            .iter()
            .filter(|r| {
                r.title
                    .as_deref()
                    .map(|t| t.to_lowercase().contains(&q))
                    .unwrap_or(false)
            })
            .collect();
        if by_id.len() > 1 {
            by_id
        } else {
            by_title
        }
    };
    match hits.len() {
        0 => anyhow::bail!("no session matches {query:?}"),
        1 => Ok(hits[0].clone()),
        n => {
            eprintln!("multiple matches for {query:?} ({n}):");
            for h in &hits {
                eprintln!("  {}  {}", h.id, h.title.as_deref().unwrap_or("(untitled)"));
            }
            anyhow::bail!("be more specific (use a longer id prefix or a more unique title)")
        }
    }
}

pub fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let homes = providers::Homes {
        dsh: cli.dsh_home.as_ref().map(PathBuf::from),
        codex: cli.codex_home.as_ref().map(PathBuf::from),
        claude: cli.claude_home.as_ref().map(PathBuf::from),
        zcode_db: cli.zcode_db.as_ref().map(PathBuf::from),
        agy: cli.agy_dir.as_ref().map(PathBuf::from),
    };
    match cli.command {
        Command::List { provider } => match provider {
            Some(p) => list_provider(&p, &homes),
            None => {
                for p in ["dsh", "codex", "claude", "zcode", "agy"] {
                    if let Err(e) = list_provider(p, &homes) {
                        println!("== {p}: unavailable ({e})");
                    }
                }
                Ok(())
            }
        },
        Command::Export {
            query,
            from,
            output,
            include_subagents,
            dry_run,
        } => {
            let source = providers::provider_named(&from, &homes)?;
            let root = resolve(&source.discover()?, &query)?;
            let bundle =
                crate::portable::Bundle::collect(source.as_ref(), root, include_subagents)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&bundle.save(&output, dry_run)?)?
            );
            Ok(())
        }
        Command::Import {
            file,
            to,
            cwd,
            name,
            dry_run,
            resume_max_chars,
            prune_resume_context,
        } => {
            let bundle = crate::portable::Bundle::load(&file)?;
            let destination = providers::provider_named(&to, &homes)?;
            let outcome = crate::family::migrate(
                &bundle,
                destination.as_ref(),
                bundle.root(),
                &ir::WriteOpts { cwd, name, dry_run },
                true,
                resume_max_chars,
                prune_resume_context,
            )?;
            println!("{}", serde_json::to_string_pretty(&outcome)?);
            Ok(())
        }
        Command::Migrate {
            query,
            from,
            to,
            cwd,
            name,
            preset,
            dry_run,
            resume_max_chars,
            prune_resume_context,
            include_subagents,
        } => {
            if from == to && from != "dsh" {
                anyhow::bail!("--from and --to must differ (dsh -> dsh is allowed with --preset)");
            }
            if from == to && from == "dsh" && preset.is_none() {
                anyhow::bail!("dsh -> dsh requires --preset <name> (port the conversation to another agent preset)");
            }
            if preset.is_some() && !(from == "dsh" && to == "dsh") {
                anyhow::bail!("--preset only applies to dsh -> dsh");
            }
            let src = providers::provider_named(&from, &homes)?;
            let refs = src.discover()?;
            let r = resolve(&refs, &query)?;

            if from == "dsh" && to == "dsh" {
                anyhow::ensure!(!include_subagents && !prune_resume_context && resume_max_chars == crate::resume::DEFAULT_MAX_CHARS,
                    "DSH preset copies preserve the raw transcript; context-budget/pruning and --include-subagents apply to cross-harness migration, not --preset");
                let dsh = src
                    .as_any()
                    .downcast_ref::<providers::dsh::DshProvider>()
                    .ok_or_else(|| anyhow::anyhow!("internal: dsh provider downcast failed"))?;
                println!(
                    "porting dsh session {} to preset {} ...",
                    r.id,
                    preset.as_deref().unwrap_or("?")
                );
                let opts = ir::WriteOpts { cwd, name, dry_run };
                let outcome = dsh.port_preset(&r, preset.as_deref().unwrap(), &opts)?;
                println!("{}", serde_json::to_string_pretty(&outcome)?);
                return Ok(());
            }

            let dst = providers::provider_named(&to, &homes)?;
            println!("reading {} session {} ...", r.provider, r.id);
            let opts = ir::WriteOpts { cwd, name, dry_run };
            let outcome = crate::family::migrate(
                src.as_ref(),
                dst.as_ref(),
                r,
                &opts,
                include_subagents,
                resume_max_chars,
                prune_resume_context,
            )?;
            println!("{}", serde_json::to_string_pretty(&outcome)?);
            Ok(())
        }
    }
}
