# Web Chat Exports Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Read ChatGPT, claude.ai and Gemini data exports as read-only harness-bridge sources, so a web chat becomes a resumable Claude Code (or other harness) session.

**Architecture:** One provider module, `src/providers/web`, holds the export loader (zip, folder or file), format detection, attachment index and copy, and a shared event builder that guarantees call/result pairing. Three small format modules parse their JSON into typed records and feed the builder. The CLI gains `--export`, web-aware `list`, and attachment copying after `migrate`.

**Tech Stack:** Rust 2021, serde/serde_json, `zip` 9 (deflate only), anyhow, clap.

**Spec:** `docs/superpowers/specs/2026-10-13-web-chat-exports-design.md`

## Global Constraints

- Provider names: `chatgpt`, `claude-web`, `gemini-web`; all read-only.
- Attachment folder: `<provider>-attachments/<conversation id>/<file name>`, relative to `--cwd`.
- Tool names: `[A-Za-z0-9_-]`, at most 64 characters.
- Gemini time grouping gap: 30 minutes.
- Every `ToolResult` follows its `ToolCall`; every call gets a result.
- `cargo fmt --all -- --check` and `cargo test --locked` pass after every task.
- Conventional Commits; no AI attribution; US English; no em or en dashes in prose.

## Review Focus

- A conversation whose `current_node` (or leaf) points at a missing node or forms a cycle: the read fails with the id, never loops.
- An export with thousands of conversations: parsed once per run, not once per lookup.
- File names with spaces or non-ASCII characters in attachments: copied byte for byte, referenced verbatim.
- A Takeout zip whose paths contain a localized `My Activity` folder name: detection looks for `Gemini Apps/MyActivity.json` at any depth.
- Re-running a migration: attachments already present with the same size are not rewritten.

---

### Task 1: Shared web core

**Files:** Create `src/providers/web/mod.rs`, `src/providers/web/export.rs`, `src/providers/web/tests.rs`; modify `src/providers/mod.rs` (module only), `Cargo.toml` (zip).

**Produces:**
- `Export::open(path: &Path) -> anyhow::Result<Export>`; `Export::files() -> &[String]`; `Export::read_text(entry: &str) -> anyhow::Result<String>`; `Export::copy_to(entry: &str, dest: &Path) -> anyhow::Result<bool>`; `Export::find_name(name: &str) -> Option<&str>`; `Export::find_prefix(id: &str) -> Option<&str>`.
- `Format::{ChatGpt, ClaudeWeb, GeminiWeb}` with `name()`, `detect(&Export) -> anyhow::Result<Format>`.
- `Builder` with `user(time, text)`, `assistant(time, text)`, `reasoning(time, text)`, `call(time, id, name, args) `, `result(time, call_id: Option<&str>, tool, text)`, `finish() -> Vec<Event>`.
- Helpers: `safe_tool_name`, `strip_citations`, `html_to_text`, `iso_ms`.

- [ ] Tests first: zip, folder and file inputs list the same entries and read the same text; detection for each format, `MyActivity.html`-only, `.tgz`, unknown; builder closes turns, pairs results, labels orphan output, fills missing results; helper cases.
- [ ] Implement until green; `cargo fmt`, `cargo test --locked`; commit `feat(web): add the shared export loader and event builder`.

### Task 2: ChatGPT format

**Files:** Create `src/providers/web/chatgpt.rs`; extend `tests.rs`.

- [ ] Tests first: branch selection with a regenerated sibling and a missing `current_node`; each content type; hidden and skipped records; tool pairing including joined outputs; citation markers; attachments found and missing; shards; cycle and missing-node errors.
- [ ] Implement; commit `feat(web): read ChatGPT exports`.

### Task 3: claude.ai format

**Files:** Create `src/providers/web/claude_web.rs`; extend `tests.rs`.

- [ ] Tests first: tree with an edited branch, `current_leaf_message_uuid`, flat legacy order; text, thinking, tool_use and tool_result with and without ids; `text` fallback; extracted attachments inlined; files referenced.
- [ ] Implement; commit `feat(web): read claude.ai exports`.

### Task 4: Gemini format

**Files:** Create `src/providers/web/gemini_web.rs`; extend `tests.rs`.

- [ ] Tests first: grouping by `titleUrl` id; time-gap grouping without it; the `Prompted` and `safeHtmlItem` shape, the `details` shape and the `userInteractions` shape; HTML to text; titles from the first prompt; attached and generated files.
- [ ] Implement; commit `feat(web): read Gemini Takeout activity`.

### Task 5: Provider, CLI and portable wiring

**Files:** Modify `src/providers/mod.rs`, `src/cli.rs`, `src/portable.rs`; extend tests.

- [ ] Tests first: `provider_named` for the three names with and without `--export`; mismatch error; write error; portable accepts the names; a web session written by the Claude provider reads back with its turns and tools; attachment copy plan, copy, skip of identical files.
- [ ] Implement `--export`, `list` handling, the `--cwd` requirement and attachment copying with counts in the output; commit `feat(cli): migrate web chat exports with their attachments`.

### Task 6: End-to-end, skill and docs

- [ ] Build a zip fixture for each format, migrate each to Claude in a scratch project, resume one with the real Claude Code CLI and check the answer, then remove the test sessions.
- [ ] Write the global skill and test it with a subagent.
- [ ] Update README, AGENTS.md, portable format and development docs; commit `docs: document web chat exports`.
