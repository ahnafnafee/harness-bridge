# AGENTS.md — using harness-bridge as an agent

Instructions for AI coding agents (Claude Code, Codex, ZCode, dsh subagents, …) that use
`harness-bridge` to migrate sessions on the user's behalf. Humans benefit from reading this too.

## What this tool is

`harness-bridge` converts coding-agent session transcripts between harnesses:
`dsh` (DeepSeek Harness) ↔ `codex` (Codex Desktop/CLI) ↔ `claude` (Claude Code) ↔ `zcode` ↔ `agy` (text-turn writes).
It reads the source, converts through a normalized model, and writes a native session the target
harness can list and resume. Nothing in the source is modified.

## When to use it

- The user asks to "move/copy/migrate a session to Codex/Claude/…", to "resume my old session in another tool",
  or to "import" a conversation from one assistant into another.
- The user wants a session to appear in a harness it was not created in.

Do NOT use it to: merge two sessions, edit conversation content, or export human-readable transcripts
(it writes native harness stores and portable session data, not markdown).

## Commands

```bash
harness-bridge list                                   # all providers, with ids, titles, migrated flags
harness-bridge list --provider dsh                    # one provider
harness-bridge migrate <id-prefix-or-title> --from dsh --to codex
harness-bridge migrate <query> --from dsh --to codex --cwd "F:\\target\\dir" --name "New name"
harness-bridge migrate <query> --dry-run              # full conversion, nothing written
harness-bridge export <query> --from claude --output session.hbridge.json --include-subagents
harness-bridge import session.hbridge.json --to codex --cwd "D:\\project" --dry-run
```

Conversion flags: `--cwd` (target base directory), `--name` (display title), `--dry-run`,
`--resume-max-chars` (normalized context character budget, default 750,000),
`--prune-resume-context` (explicit output shortening with the archive preserved),
`--rebuild-resume-context` (import/migrate: explicit recovery of unavailable context),
`--include-subagents` (saved children with translated parent links), plus home overrides
`--dsh-home`, `--codex-home`, `--claude-home`, `--zcode-db`, `--agy-dir` (use these for testing against
throwaway homes; see Testing below).

Portable exports preserve the normalized archive and retained context in a versioned JSON file. Export reads
only the source store and refuses to overwrite an existing file. Import validates the version/checksum/family,
reads no source stores, and uses the receiver's native writer and context preflight. All bundled children import
automatically. Initialize the destination harness and its required template on the receiving machine first.
See [the format specification](docs/portable-format.md). Import re-runs have the same destination-overwrite
caveat as local migrations; preserve any later destination turns before repeating an import.

## Decision rules for agents

1. **Resolve ambiguities before writing.** If `migrate` reports multiple matches, do not pick one silently —
   show the user the id/title list and let them choose (or pick only when the user's intent is unambiguous,
   e.g. exactly one session with user-level prompts and the rest are its subagent siblings).
2. **Prefer `--dry-run` first** for unfamiliar sources or when the user seems cautious. Report the event/item
   counts it prints, then run for real on confirmation — or directly when the user explicitly asked for the move.
3. **Ask about `--cwd` when it matters.** The default target cwd is the *source* session's original working
   directory. If the user wants the new thread "based" elsewhere (common for Codex), pass `--cwd`. Explain that
   `--cwd` changes only the new session's base directory — history content and file paths are never rewritten.
4. **Re-runs update in place.** A ledger (`~/.codex/dsh-imports.json`) maps source session → target thread, and writes
   are deterministic (same source → same target id). Re-running can overwrite turns added in the destination;
   preserve that destination copy before reimporting.
5. **Report outcomes faithfully.** After a real write, tell the user: the target location, the native id, and
   any caveats below that apply.

## dsh -> dsh preset porting

`harness-bridge migrate <q> --from dsh --to dsh --preset <name>` copies a conversation to another
agent preset with full event fidelity (raw transcript copy; header `agentPreset` rewritten; the
projcache clone gets the preset row patched). The preset must exist as a directory preset under
`~/.dsh/.agent-presets/<name>` — if it is missing, tell the user to install it (and run the
preset's `sync.mjs` for desktop visibility) rather than picking a different preset silently.
The port is idempotent: same source + preset -> same new session id.

## Provider caveats (tell the user when relevant)

- **Codex Desktop sidebar**: the desktop app's thread registry (`state_5.sqlite` `threads` table) is filled by a
  one-time backfill; files added later are invisible to the app. harness-bridge registers the thread automatically.
  The user may need to reopen/refresh the Codex window to see it.
- **dsh writes** are plain JSONL in `~/.dsh/sessions/<project-slug>/<id>/session.v4.jsonl` (dsh also reads
  zstd-compressed files; both are accepted). Whether the dsh UI lists it may require dsh to rescan.
- **Claude Code writes** drop harness-context messages (no developer channel): user prompts, assistant replies,
  reasoning, tool calls/results and compaction summaries are kept.
- **ZCode writes** go into the live `db.sqlite` (WAL). For zero-risk operations, copy the db and pass
  `--zcode-db` to the copy.
- **agy writes are text-turn native**: harness-bridge rebuilds the conversation db (steps table, summaries index,
  brain transcripts) from a template conversation. Tool calls/results are not yet representable in agy's protobuf
  steps and are omitted. Content that trips Google's safety filters may still be blocked when agy generates responses.
- **Resume preflight** applies across providers. Character budgets are not model token limits; smaller models
  may need a lower budget. Incomplete or unsupported checkpoints fail explicitly. Raw DSH preset copies retain
  their existing context semantics and do not use normalized context pruning.
- **Encrypted Codex compaction**: 0.3.1 exports readable archives even when complete retained context is unavailable.
  Such files use format v2 and require 0.3.1 on the receiving PC. Import/migrate requires explicit
  `--rebuild-resume-context`, using readable checkpoint items and later turns or the archive as fallback.
  A recovery notice describes missing hidden state. Budgets/pruning still apply; encrypted state cannot be recovered,
  and decisions present only there can be lost. Unknown/malformed active items still fail rather than being discarded.
- **Saved child links** use native destination fields where supported; agy uses the migration archive and ZCode
  uses a migration entry on schemas without `parent_id`. These imports do not start agents or translate tools.
- **Reasoning** is retained in the normalized archive. Active reasoning in Codex writes becomes labeled assistant
  text in a native resume checkpoint, including for uncompacted sources, because normalized text has no verifiable
  hidden state. Claude writes also use plain assistant text for active foreign reasoning. agy's native steps remain text-only.

## Testing pattern (when something looks wrong)

```bash
# 1. convert into throwaway homes
harness-bridge migrate <q> --from dsh --to codex --codex-home "C:\\temp\\hb-test" --dry-run
# 2. for a real isolated write, create the dir, copy one existing desktop rollout as template + the ledger,
#    then drop --dry-run and point --codex-home at it
# 3. verify with the real app-server (no model calls, safe):
#    CODEX_HOME=<test home> codex app-server   (JSON-RPC: initialize -> thread/resume {threadId} -> thread/turns/list)
```

A successful probe resumes the thread and pages turns/items. If `thread/resume` fails, the rollout is
malformed — do not hand it to the user's real home.

## Never do this

- Never edit or delete anything under the source harness's store (`~/.dsh`, `~/.claude`, `~/.zcode`, `~/.gemini`).
- Never write to the real codex home while validating a suspected-bad conversion — use a throwaway home.
- Never fabricate a "migrated" result: run the command, report its actual output.
