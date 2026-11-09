# Development

`harness-bridge` is a Rust CLI with provider-specific native readers and writers. Keep format details inside the relevant provider and use the shared session model for data that crosses provider boundaries.

## Code layout

| Module | Responsibility |
| :-- | :-- |
| `src/main.rs` | Entry point and error reporting. |
| `src/lib.rs`, `src/app.rs` | Shared application operations, provider catalog, session inspection and immutable preview/confirmation plans. |
| `src/tui.rs` | Optional terminal workbench, Unicode text entry, background operations and terminal restoration. |
| `src/web_ui.rs`, `src/web/` | Optional loopback browser server and embedded interface. |
| `ui/` | GPUI Kit Rust frontend, compiled separately to WebAssembly. |
| `src/cli.rs` | Command parsing, query resolution, and migration orchestration. |
| `src/family.rs` | Family discovery, translated parent ids, whole-family conversion preflight and writes. |
| `src/portable.rs` | Versioned portable JSON, checksums, family validation, and a file-backed import source. |
| `src/portable/tests.rs` | Portable fidelity, corrupt-file handling, context preflight, pruning, and receiver placement checks. |
| `src/resume.rs` | Shared context-size preflight and explicit output pruning. |
| `src/ir.rs` | Sessions, events, discovery references, and write options/results. |
| `src/util.rs` | Shared paths, ids, timestamps, compression, and format helpers. |
| `src/providers/mod.rs` | Provider interface, construction, import ledger, and git metadata. |
| `src/providers/dsh.rs` | DSH discovery, native writes, and preset copies. |
| `src/providers/dsh/context.rs` | DSH archive decoding and ordered model-context replay. |
| `src/providers/codex.rs` | Codex discovery, reading, and rollout/turn assembly. |
| `src/providers/codex/response.rs` | Native response-item encoding shared by the transcript and resume checkpoint. |
| `src/providers/{claude,codex,zcode}/context.rs` | Source-native checkpoint reconstruction. |
| `src/providers/codex/registry.rs` | Desktop sidebar, SQLite thread registration, and ledger updates. |
| `src/providers/codex/tests.rs` | DSH-to-Codex conversion regression coverage. |
| `src/providers/{claude,zcode,agy}.rs` | Other providers' native formats and registration. |
| `src/providers/qoder.rs`, `src/providers/qoder/` | Qoder CLI branch/context handling, native writer, IDE transcript reader and read-only desktop history. |
| `src/providers/web/mod.rs` | Web export detection, the read-only web provider, attachment references and copies, and the event builder that keeps tool calls and results paired. |
| `src/providers/web/export.rs` | Zip, folder and single-file access to an export. |
| `src/providers/web/{chatgpt,claude_web,gemini_web}.rs` | Each web app's export records and visible-branch conversion. |
| `src/providers/web/tests/` | Fixture exports for all three apps, read as folders and as zips. |
| `scripts/verify_codex_resume.py` | Isolated end-to-end Codex resume probe. |
| `scripts/verify_portable_transfer.py` | CLI export/move/import/read-back matrix for Codex, Claude Code, DSH and ZCode, with native Codex continuation probes. |

## Context and transcript fidelity

`Session.events` contains the full normalized archive. `Session.resume_events` is optional and contains the source's retained model context after replacement and pruning. Keeping these separate prevents display history from accidentally becoming the entire next model request.

DSH replacement bounds refer to positions in the current context surface. A newer summary can precede an older retained message, so sorting or deleting by numeric sequence range is incorrect. Replay the operations in log order, splice between the referenced positions, and restore tool calls for surviving results. Invalid or unsupported operations fail conversion rather than silently replaying superseded history.

Claude's latest boundary retains its summary, declared preserved records, and later messages. Codex reads plaintext replacement histories; opaque checkpoints require explicit recovery on import/migration but do not block readable archive export. ZCode respects persisted summaries and provider visibility. Codex writes retained events into a native `compacted.payload.replacement_history`, while the visible rollout still contains the full converted archive. Other writers use their native checkpoint/visibility mechanisms; agy uses retained text turns plus a separate normalized archive.

The shared size policy checks serialized normalized event characters before writing. Pruning is opt-in, preserves protected text and tool arguments, and never changes the archive. It is not a token estimate. Family tests cover nested parent links, deterministic reimports, native Codex registration, both ZCode parent schemas, and preflight failures leaving the destination unwritten. Temporary Claude homes must never register sessions in the user's real desktop sidebar.

Portable exports serialize that same model into the [portable format](portable-format.md), preserving archive/context separation without applying a destination budget. Files with unavailable context use version 2 and retain a reason plus any readable checkpoint items as a recovery seed. `--rebuild-resume-context` reconstructs from that seed or the readable archive, adds a notice, and preflights the entire family before writes. Unknown/malformed active items remain hard errors. Loading validates the raw payload checksum before deserializing it, then validates the complete family. A read-only in-memory provider exposes those sessions to the existing family pipeline, so the receiving PC never opens source stores. Receiver-generated parent placement metadata takes precedence over historical migration metadata.

## Local validation

```sh
cargo fmt --all -- --check
cargo test --locked
cargo build --locked
```

Check the optional interfaces and the command-only configuration too:

```sh
cargo test --locked --all-features
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo check --locked --no-default-features
cargo check --locked --no-default-features --features web-ui
node --check src/web/boot.js
cargo +stable test --locked --manifest-path ui/Cargo.toml
python scripts/build_ui.py --check
```

`tui` is enabled by default; `web-ui` embeds the GPUI Kit browser bundle and adds a small local HTTP server. The graphical interface is Rust code under `ui/src`, with only startup and failure handling in JavaScript. Its model tests cover source/project filtering, Unicode search, deterministic sorting and session identity. IBM Plex fonts and selected GPUI component icons are embedded, so the application makes no external asset requests. Licenses are preserved under `assets/`. Inspection responses include structured role/text excerpts alongside the plain excerpt used by the terminal. The conversion library requires neither interface.

The checked-in JavaScript bindings and compressed WebAssembly module allow a stable Rust installation to build the browser feature. `build.json` records source and artifact checksums; the browser-feature Rust tests reject stale bundles. To rebuild after changing frontend Rust code, install the pinned toolchain and matching binding generator, then run:

```sh
rustup toolchain install nightly-2026-10-15 --profile minimal --target wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.127 --locked
python scripts/build_ui.py
```

The script also accepts `--wasm-bindgen <executable>`. GPUI Kit is pinned at 0.9.0 in a separate Cargo project, keeping its rendering dependencies and nightly requirement outside the conversion engine. The 8 MiB WebAssembly stack is configured in `ui/.cargo/config.toml`. The server sends the module with `application/wasm` and gzip encoding, and permits WebAssembly compilation in its content policy while retaining same-origin scripts and connections. GPUI's platform uses inline styles for its canvas and input mirror. Browser screen-reader integration and real phone IME behavior remain unverified; see [interfaces](interfaces.md).

`app::TransferPlan` captures a portable source snapshot, preflights the whole family, records destination artifact stamps, and writes that snapshot only after confirmation. Browser plans are single-use and expire; source-location changes invalidate them. The server enforces loopback Host/Origin checks, per-run access keys on reads and writes, bounded JSON bodies, and restrictive asset policies. It does not expose a general file server.

Exercise real CLI/UI operations against temporary stores, including access checks, preview immutability, single-use confirmation, portable export/import and settings invalidation:

```sh
cargo build --locked --all-features
python scripts/verify_workbench.py --bridge target/debug/harness-bridge
```

Use `target/debug/harness-bridge.exe` on Windows. Optionally pass `--qoder <executable-or-qodercli.js>` to check that Qoder's own `--list-sessions` recognizes the generated transcript. The verified official npm Qoder CLI 1.1.85 listed it successfully; an unauthenticated resume probe stopped at Qoder's sign-in requirement. This is a native discovery check, not authenticated continuation or model generation. No source credentials are copied and no model requests are made. IDE hook-transcript fixtures follow the [documented format](https://docs.qoder.com/extensions/hooks#transcript-file-format); desktop database tests use an isolated schema fixture. Windows desktop data was also inspected read-only. Other platforms' desktop layouts and versions need explicit fixtures before widening compatibility claims.

Use throwaway destination stores while investigating malformed conversions. Source stores must remain untouched. A dry run performs conversion but cannot verify whether the target app will load or resume the result.

For a real isolated Codex write:

1. Create a test home with a `sessions` directory.
2. Copy a top-level Codex Desktop rollout there as a template. It must contain Desktop session metadata and a turn context.
3. Run the migration with `--codex-home` pointing at that test home.
4. Probe the generated rollout with the actual Codex executable.

For example, after preparing the template:

```powershell
harness-bridge migrate "<id-prefix>" --from dsh --to codex --codex-home "C:\Temp\hb-test"
python scripts/verify_codex_resume.py "C:\Temp\hb-test\sessions\YYYY\MM\DD\rollout-....jsonl" --codex "C:\path\to\codex.exe"
```

The Python probe uses only the standard library. It copies the rollout into a separate temporary home, initializes the real app-server, resumes the thread, pages displayed turns, and starts a turn against a local Responses API stub. It checks the actual next request size and successful turn completion without credentials or remote model calls.

Current app-server builds enforce a saved chat's provider identity. The probe retags only the disposable copy's session metadata to its loopback provider; transcript and retained-context items, and the original rollout, remain unchanged. This tests replay and continuation through a local stub rather than switching a live chat between remote providers.

`--max-input-chars` sets the stub's input limit; the default is 1,000,000 characters. `--timeout` bounds the probe, and `--codex` selects the executable. This is a replay and request-shape check, not a tokenizer estimate or a guarantee that every remote model configuration accepts the context.

Exercise encrypted-checkpoint export, transfer to a separate receiving home, import and native read-back through all 16 pairs of Codex, Claude Code, DSH and ZCode:

```powershell
python scripts/verify_portable_transfer.py --bridge "target\debug\harness-bridge.exe" --codex "C:\path\to\codex.exe"
```

The script checks nested saved children, archive preservation, unavailable-context guards, context budgets, explicit recovery/pruning, dry runs and native Codex tool continuations. `--source-rollout <path>` additionally copies an actual encrypted Codex session read-only into an isolated source home, then exports, imports and probes that copy. No remote models are called, and agy is outside this end-to-end matrix.

## Release packaging

The release workflow builds Windows x64, Linux x64 and macOS Apple Silicon with both interfaces. Each runner tests the Rust library, then verifies the packaged archive's checksum, contents, executable architecture and version. The extracted executable also runs the workbench API probe against temporary stores; its served icon and fonts must match the packaged files.

Run the workflow manually with a version such as `v0.5.1` to validate all three packages without creating a release. A `v*` tag runs the same checks and prepares a draft containing the archives and `sha256sums.txt`. Download and inspect those assets before publishing the draft. The workflow does not publish a release automatically.

To verify a downloaded archive locally, use Python 3.12 or newer:

```sh
python scripts/verify_release.py harness-bridge-v0.5.1-x86_64-pc-windows-msvc.zip --version 0.5.1 --checksums sha256sums.txt --smoke
```

Select the archive for your platform when using `--smoke`. Omit that flag to check another platform's archive without executing it. The binary itself does not require Python.

## Provider changes

Implement `Provider::discover`, `read`, and `write`, then add construction in `provider_named`. Preserve native timestamps, message roles, and tool pair ids where supported. Document records the target cannot represent instead of claiming universal losslessness.

Keep serialization and registration concerns local to each provider. Extract a helper when it removes duplicated format logic or makes an independently complex operation easier to inspect; avoid creating a new abstraction for every record type.

Web exports are read-only sources; their formats live in `src/providers/web`. Exports change without notice, so when an app ships a new layout or record type, add a fixture for that shape to the format's test file before changing the parser. Every conversion must keep a tool result directly behind its call, because every writer rejects unpaired results.

For agy's protobuf fallback reader, the [community format notes](https://gist.github.com/ArcticWinterSturm/718637afb3094814a94dc77261e0b5e0) describe the step map used by the implementation.
