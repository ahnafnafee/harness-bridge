mod ir;
mod providers;
mod util;

use clap::{Parser, Subcommand};
use ir::SessionRef;
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
        /// Convert + verify without writing to the target harness.
        #[arg(long)]
        dry_run: bool,
    },
}

fn list_provider(name: &str, homes: &providers::Homes) -> anyhow::Result<()> {
    let p = providers::provider_named(name, homes)?;
    let mut refs = p.discover()?;
    let ledger_home = homes.codex.clone().unwrap_or_else(crate::util::default_codex_home);
    let ledger = providers::load_codex_import_ledger(&ledger_home);
    let ledger_keys: Vec<String> = ledger
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    for r in refs.iter_mut() {
        let key = if r.provider == "dsh" { r.id.clone() } else { format!("{}:{}", r.provider, r.id) };
        r.migrated = ledger_keys.iter().any(|k| *k == key || k.ends_with(&format!(":{key}")));
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
    let by_id: Vec<&SessionRef> = refs.iter().filter(|r| r.id.to_lowercase().starts_with(&q) || r.id == q).collect();
    let hits = if by_id.len() == 1 {
        by_id
    } else {
        let by_title: Vec<&SessionRef> = refs
            .iter()
            .filter(|r| r.title.as_deref().map(|t| t.to_lowercase().contains(&q)).unwrap_or(false))
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

fn main() {
    let cli = Cli::parse();
    let result = run(cli);
    if let Err(e) = result {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> anyhow::Result<()> {
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
        Command::Migrate { query, from, to, cwd, name, dry_run } => {
            if from == to {
                anyhow::bail!("--from and --to must differ");
            }
            let src = providers::provider_named(&from, &homes)?;
            let dst = providers::provider_named(&to, &homes)?;
            let refs = src.discover()?;
            let r = resolve(&refs, &query)?;
            println!("reading {} session {} ...", r.provider, r.id);
            let session = src.read(&r)?;
            println!(
                "  {} events, cwd {:?}, title {:?}",
                session.events.len(),
                session.cwd,
                session.title
            );
            let opts = ir::WriteOpts { cwd, name, dry_run };
            let outcome = dst.write(&session, &opts)?;
            println!("{}", serde_json::to_string_pretty(&outcome)?);
            Ok(())
        }
    }
}
