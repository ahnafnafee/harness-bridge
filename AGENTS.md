# AGENTS.md — using harness-bridge as an agent

Instructions for AI coding agents (Claude Code, Codex, ZCode, dsh subagents, …) that use
`harness-bridge` to migrate sessions on the user's behalf. Humans benefit from reading this too.

## What this tool is

`harness-bridge` converts coding-agent session transcripts between harnesses:
`dsh` (DeepSeek Harness) ↔ `codex` (Codex Desktop/CLI) ↔ `claude` (Claude Code) ↔ `zcode` ↔ `agy` (read-only).
It reads the source, converts through a normalized model, and writes a native session the target
harness can list and resume. Nothing in the source is modified.

## When to use it

- The user asks to "move/copy/migrate a session to Codex/Claude/…", to "resume my old session in another tool",
  or to "import" a conversation from one assistant into another.
- The user wants a session to appear in a harness it was not created in.

Do NOT use it to: merge two sessions, edit conversation content, or export human-readable transcripts
(it writes native harness stores, not markdown).

## Commands

```bash
harness-bridge list                                   # all providers, with ids, titles, migrated flags
harness-bridge list --provider dsh                    # one provider
harness-bridge migrate <id-prefix-or-title> --from dsh --to codex
harness-bridge migrate <query> --from dsh --to codex --cwd "F:\\target\\dir" --name "New name"
harness-bridge migrate <query> --dry-run              # full conversion, nothing written
```

All flags: `--cwd` (target base directory), `--name` (display title), `--dry-run`, plus home overrides
`--dsh-home`, `--codex-home`, `--claude-home`, `--zcode-db`, `--agy-dir` (use these for testing against
throwaway homes; see Testing below).

## Decision rules for agents

1. **Resolve ambiguities before writing.** If `migrate` reports multiple matches, do not pick one silently —
   show the user the id/title list and let them choose (or pick only when the user's intent is unambiguous,
   e.g. exactly one session with user-level prompts and the rest are its subagent siblings).
2. **Prefer `--dry-run` first** for unfamiliar sources or when the user seems cautious. Report the event/item
   counts it prints, then run for real on confirmation — or directly when the user explicitly asked for the move.
3. **Ask about `--cwd` when it matters.** The default target cwd is the *source* session's original working
   directory. If the user wants the new thread "based" elsewhere (common for Codex), pass `--cwd`. Explain that
   `--cwd` changes only the new session's base directory — history content and file paths are never rewritten.
4. **Re-runs are safe.** A ledger (`~/.codex/dsh-imports.json`) maps source session → target thread, and writes
   are deterministic (same source → same target id). Re-running updates in place; it never duplicates.
5. **Report outcomes faithfully.** After a real write, tell the user: the target location, the native id, and
   any caveats below that apply.

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
- **agy is read-only** (experimental): transcripts come from `~/.gemini/antigravity-cli/brain/<id>/.system_generated/logs/`
  with a heuristic protobuf fallback; older conversations may convert partially. Writing to agy is unsupported.
- **Timestamps, tool call/result pairing, reasoning and compaction summaries are preserved** across all
  read/write paths; per-provider renderings differ (e.g. reasoning becomes Codex `reasoning` items,
  Claude `thinking` blocks).

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
