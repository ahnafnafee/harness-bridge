# Interactive interfaces

The default build includes a terminal library with source filters, search, conversation excerpts, migration previews, and portable file import/export. Source reads and conversions run in the background. The terminal uses your existing font and colors.

```sh
harness-bridge
harness-bridge tui --provider qoder
```

| Key | Action |
| :-- | :-- |
| `↑` / `↓`, `j` / `k` | Select a conversation. |
| `Tab`, `←` / `→` | Change the source filter. |
| `/` | Search title, id, provider and folder. |
| `Enter` | Configure and preview a migration. |
| `v`, `e`, `i` | Read an excerpt, export a portable file, or import one. |
| `r`, `?`, `q` | Refresh, show keyboard help, or quit. |

Within a form, Tab changes fields, arrow keys change the destination or text cursor, and Space toggles an option. Enter prepares a preview without writing. Read the destination and conversion notes, then press **w** to write. Esc returns to the form. Text inputs support Unicode, paste, Home/End, Delete, and Ctrl+U to clear.

Pass `--export <path>` to browse a web chat data export. Home override flags work in both interfaces and regular commands. The bare command opens the terminal interface only when both input and output are interactive; otherwise it prints help.

## Local browser interface

Build with the optional feature:

```sh
cargo install --locked --path . --features web-ui
harness-bridge ui --open
```

The browser interface is written in Rust using [GPUI Kit](https://gpui-kit.com/) and rendered through WebAssembly. It opens a full-width session library with source and project filters, search, and date/title sorting. Selecting a session opens its transfer inspector; the Conversation tab shows role-labeled excerpts and recorded context counts. The inspector becomes a full-screen panel on narrow windows. History keeps the last 50 completed transfers and exports for the current browser tab, including paths available to copy, and survives refreshes when session storage is available. Source-location settings and portable file operations remain available from the header.

The interface listens only on `127.0.0.1` and chooses an available port by default. Its compressed WebAssembly module, icon, fonts and component icons are bundled locally; normal installation does not require a separate frontend toolchain. The browser needs graphics support through WebGPU or WebGL2. Keep the process running while using it; Ctrl+C stops it. Omit `--open` to copy the printed local URL into a browser yourself. `--port` selects a fixed port.

GPUI Kit describes its browser target as a [component showcase](https://gpui-kit.com/docs/webassembly/). This application exercises it as a local interface, with Windows Chrome as the validated browser. Screen-reader integration, real phone keyboard/IME behavior and native desktop packaging have not been validated. The terminal and command interfaces remain available independently.

Open the complete printed URL: it contains the per-run access key. Every conversation endpoint requires that key. Source locations entered in the interface apply only while that process is running; blank fields use default locations. Portable-file forms accept paths on the computer running the tool.

## Preview and confirmation

Migration previews hold an immutable source snapshot, so writing uses the conversation that was reviewed. Both interfaces require a separate write action and reject destination artifacts that changed after preview. Browser previews expire after ten minutes and are usable once; changing source locations invalidates pending previews.

Reimporting still uses deterministic ids and can replace later destination turns. Preserve those turns before approving an update, and close the destination conversation while replacing it. A family preflight validates every member before writes, but the native stores are separate files or databases rather than one cross-provider transaction: an I/O failure during writing can leave a partial family.

Context shortening and recovery are explicit options. Character budgets are not model token limits, and recovery cannot restore hidden model state. Review the imported task and decisions before continuing a recovered conversation.

## Commands and diagnostics

```sh
harness-bridge list --query "project name" --limit 20 --json
harness-bridge providers
harness-bridge doctor
```

`doctor` reports source discovery and provider requirements. It does not authenticate with a model service or guarantee that a destination app can resume a conversion. JSON listings contain `providers` and `sessions`; unavailable sources appear in the provider report instead of corrupting the JSON output. Other data commands also accept `--json`; progress messages use standard error.

A command-only build remains available with `cargo install --locked --path . --no-default-features`. To build just the browser interface and commands, add `--no-default-features --features web-ui`.

## Qoder

Use `qoder` (also accepted as `qoder-cli`) for native CLI reads and writes, and `qoder-ide` for IDE reads:

```sh
harness-bridge list --provider qoder
harness-bridge migrate "<id-prefix>" --from qoder --to claude --dry-run
harness-bridge migrate "<id-prefix>" --from claude --to qoder --cwd "/path/to/project"
harness-bridge list --provider qoder-ide
```

Qoder CLI discovery reads `projects/<project>/<id>.jsonl` under `QODER_CONFIG_DIR` or `~/.qoder`. The reader follows the committed `active-leaf` parent chain instead of replaying discarded branches. Native writes include a resume checkpoint when needed and a separate `migration-archive.json` retaining the complete normalized archive. Resume the resulting id with `qoder --resume <id>` from its project folder. The CLI writer does not register conversations in the IDE sidebar.

The IDE reader accepts [hook transcripts](https://docs.qoder.com/extensions/hooks#transcript-file-format) under `projects/<project>/transcript/` and recent desktop `main.sqlite` history. When an exact desktop session also has a local CLI transcript, that transcript supplies its retained context and the desktop database supplies its title. Visible desktop history alone requires explicit `--rebuild-resume-context`; it cannot establish the original model checkpoint. Unsupported content replacements or compression decisions also require recovery, while malformed content and incomplete checkpoints fail explicitly. Qoder attachment references become readable notices; binary attachment files are not transferred.

Use `--qoder-home` and `--qoder-ide-db` for custom locations. An explicit Qoder home disables automatic discovery of the real desktop database unless a database override is also supplied. The verified Windows desktop location is `%APPDATA%/com.qoder.app.stable/main.sqlite`. IDE writes are unavailable because its native registration contract has not been verified.

Native Qoder 1.1.85 discovery was checked with isolated generated sessions. Authenticated continuation and remote model acceptance are outside that check. See [development](development.md) for the repeatable probe.
