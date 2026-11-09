//! Command parsing, session selection, and migration orchestration.

use crate::{
    ir::{self, SessionRef},
    providers,
};
use clap::{CommandFactory, Parser, Subcommand};
use std::io::IsTerminal;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "harness-bridge",
    version,
    about = "Browse, preview and migrate coding sessions between harnesses, including Qoder, or continue web chats from their data exports",
    long_about = None
)]
struct Cli {
    /// Print structured output for scripts.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Option<Command>,
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
    /// Qoder home (default QODER_CONFIG_DIR or ~/.qoder).
    #[arg(long, global = true)]
    qoder_home: Option<String>,
    /// Qoder IDE/desktop main.sqlite (read-only).
    #[arg(long, global = true)]
    qoder_ide_db: Option<String>,
    /// Web chat export for chatgpt, claude-web or gemini-web: the zip, its
    /// extracted folder, or the conversations file.
    #[arg(long, global = true)]
    export: Option<String>,
}

#[derive(Subcommand)]
enum Command {
    /// Open the interactive terminal workbench.
    Tui {
        /// Start with one source provider selected.
        #[arg(long)]
        provider: Option<String>,
    },
    /// Serve the optional browser interface on this computer.
    Ui {
        /// Loopback port; 0 chooses an available port.
        #[arg(long, default_value_t = 0)]
        port: u16,
        /// Open the interface in the default browser.
        #[arg(long)]
        open: bool,
    },
    /// Check local source discovery and explain provider requirements.
    Doctor,
    /// Show supported providers and their read/write capabilities.
    Providers,
    /// List migratable sessions (all providers, or one).
    List {
        #[arg(long)]
        provider: Option<String>,
        /// Search title, id, provider and working directory.
        #[arg(long, short)]
        query: Option<String>,
        /// Show at most this many sessions (newest first).
        #[arg(long)]
        limit: Option<usize>,
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
        /// Rebuild unavailable context from readable checkpoint items or the transcript.
        #[arg(long)]
        rebuild_resume_context: bool,
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
        /// Rebuild unavailable context from readable checkpoint items or the transcript.
        #[arg(long)]
        rebuild_resume_context: bool,
    },
}

fn print_sessions(references: &[SessionRef]) {
    for reference in references {
        let date = reference
            .updated_ms
            .or(reference.created_ms)
            .and_then(chrono::DateTime::from_timestamp_millis)
            .map_or("-".to_string(), |date| date.format("%Y-%m-%d").to_string());
        println!(
            "{:<11}  {:38}  {}  {}\n  {}{}",
            reference.provider,
            reference.id,
            date,
            reference.title.as_deref().unwrap_or("(untitled)"),
            reference.cwd.as_deref().unwrap_or(""),
            if reference.migrated {
                "  [imported into Codex]"
            } else {
                ""
            }
        );
    }
}

fn start_tui(homes: providers::Homes, provider: Option<String>) -> anyhow::Result<()> {
    #[cfg(feature = "tui")]
    {
        crate::tui::run(homes, provider)
    }
    #[cfg(not(feature = "tui"))]
    {
        let _ = (homes, provider);
        anyhow::bail!(
            "terminal interface is not included in this build; install with --features tui"
        )
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
        export: cli.export.as_ref().map(PathBuf::from),
        qoder: cli.qoder_home.as_ref().map(PathBuf::from),
        qoder_ide_db: cli.qoder_ide_db.as_ref().map(PathBuf::from),
    };
    let Some(command) = cli.command else {
        if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() && !cli.json {
            return start_tui(homes, None);
        }
        Cli::command().print_help()?;
        println!();
        return Ok(());
    };
    match command {
        Command::Tui { provider } => start_tui(homes, provider),
        Command::Ui { port, open } => {
            #[cfg(feature = "web-ui")]
            {
                crate::web_ui::serve(homes, port, open)
            }
            #[cfg(not(feature = "web-ui"))]
            {
                let _ = (port, open);
                anyhow::bail!("browser interface is optional; build with cargo install --locked --path . --features web-ui")
            }
        }
        Command::Providers => {
            if cli.json {
                println!("{}", serde_json::to_string_pretty(providers::CATALOG)?);
            } else {
                for info in providers::CATALOG {
                    println!(
                        "{:<12} {:<20} {}\n  {}",
                        info.name,
                        info.label,
                        if info.writable {
                            "read + write"
                        } else {
                            "read only"
                        },
                        info.note
                    );
                }
            }
            Ok(())
        }
        Command::Doctor => {
            let catalog = crate::app::discover(&homes);
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&catalog.providers)?);
            } else {
                for status in catalog.providers {
                    println!(
                        "{}: {}",
                        status.info.label,
                        status
                            .problem
                            .unwrap_or_else(|| format!("{} sessions found", status.sessions))
                    );
                    println!("  {}", status.info.note);
                }
            }
            Ok(())
        }
        Command::List {
            provider,
            query,
            limit,
        } => {
            let provider = match provider {
                None if homes.export.is_some() => {
                    let export = providers::web::Export::open(homes.export.as_deref().unwrap())?;
                    Some(providers::web::Format::detect(&export)?.name().to_owned())
                }
                other => other,
            };
            let mut catalog = if let Some(name) = provider {
                let sessions = crate::app::discover_provider(&name, &homes)?;
                crate::app::Catalog {
                    providers: vec![crate::app::ProviderStatus {
                        info: *providers::info(&name)?,
                        sessions: sessions.len(),
                        problem: None,
                    }],
                    sessions,
                }
            } else {
                crate::app::discover(&homes)
            };
            if let Some(query) = query {
                catalog
                    .sessions
                    .retain(|reference| crate::app::matches(reference, &query));
            }
            if let Some(limit) = limit {
                catalog.sessions.truncate(limit);
            }
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&catalog)?);
            } else {
                print_sessions(&catalog.sessions);
                for status in catalog.providers {
                    if let Some(problem) = status.problem {
                        eprintln!("{}: {problem}", status.info.name);
                    }
                }
                if catalog.sessions.is_empty() {
                    println!("No matching sessions. Run harness-bridge doctor to check source locations.");
                }
            }
            Ok(())
        }
        Command::Export {
            query,
            from,
            output,
            include_subagents,
            dry_run,
        } => {
            let source = providers::provider_named(&from, &homes)?;
            let root = crate::app::resolve(&source.discover()?, &query)?;
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
            rebuild_resume_context,
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
                crate::resume::Options {
                    prune: prune_resume_context,
                    rebuild: rebuild_resume_context,
                },
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
            rebuild_resume_context,
        } => {
            let from = providers::info(&from)?.name.to_owned();
            let to = providers::info(&to)?.name.to_owned();
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
            let r = crate::app::resolve(&refs, &query)?;

            if from == "dsh" && to == "dsh" {
                anyhow::ensure!(!include_subagents && !prune_resume_context && !rebuild_resume_context && resume_max_chars == crate::resume::DEFAULT_MAX_CHARS,
                    "DSH preset copies preserve the raw transcript; context-budget/pruning and --include-subagents apply to cross-harness migration, not --preset");
                let dsh = src
                    .as_any()
                    .downcast_ref::<providers::dsh::DshProvider>()
                    .ok_or_else(|| anyhow::anyhow!("internal: dsh provider downcast failed"))?;
                if !cli.json {
                    eprintln!(
                        "porting dsh session {} to preset {} ...",
                        r.id,
                        preset.as_deref().unwrap_or("?")
                    );
                }
                let opts = ir::WriteOpts { cwd, name, dry_run };
                let outcome = dsh.port_preset(&r, preset.as_deref().unwrap(), &opts)?;
                println!("{}", serde_json::to_string_pretty(&outcome)?);
                return Ok(());
            }

            // Web chats have no working directory, and their attachments are
            // copied relative to the one the new session gets.
            let web = src.as_any().downcast_ref::<providers::web::WebProvider>();
            if let Some(web) = web {
                anyhow::ensure!(
                    cwd.is_some(),
                    "{} chats have no working directory; pass --cwd <folder> for the new session (attachments are copied there)",
                    web.format().name()
                );
            }
            let dst = providers::provider_named(&to, &homes)?;
            if !cli.json {
                eprintln!("reading {} session {} ...", r.provider, r.id);
            }
            let opts = ir::WriteOpts { cwd, name, dry_run };
            let mut outcome = crate::family::migrate(
                src.as_ref(),
                dst.as_ref(),
                r.clone(),
                &opts,
                include_subagents,
                resume_max_chars,
                crate::resume::Options {
                    prune: prune_resume_context,
                    rebuild: rebuild_resume_context,
                },
            )?;
            if let (Some(web), Some(base)) = (web, opts.cwd.as_deref()) {
                outcome.extra["attachments"] =
                    web.copy_attachments(&r, std::path::Path::new(base), dry_run)?;
            }
            println!("{}", serde_json::to_string_pretty(&outcome)?);
            Ok(())
        }
    }
}
