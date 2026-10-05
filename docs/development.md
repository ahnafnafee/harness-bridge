# Development

`harness-bridge` is a Rust CLI with provider-specific native readers and writers. Keep format details inside the relevant provider and use the shared session model for data that crosses provider boundaries.

## Code layout

| Module | Responsibility |
| :-- | :-- |
| `src/main.rs` | Entry point and error reporting. |
| `src/cli.rs` | Command parsing, query resolution, and migration orchestration. |
| `src/ir.rs` | Sessions, events, discovery references, and write options/results. |
| `src/util.rs` | Shared paths, ids, timestamps, compression, and format helpers. |
| `src/providers/mod.rs` | Provider interface, construction, import ledger, and git metadata. |
| `src/providers/dsh.rs` | DSH discovery, native writes, and preset copies. |
| `src/providers/dsh/context.rs` | DSH archive decoding and ordered model-context replay. |
| `src/providers/codex.rs` | Codex discovery, reading, and rollout/turn assembly. |
| `src/providers/codex/response.rs` | Native response-item encoding shared by the transcript and resume checkpoint. |
| `src/providers/codex/registry.rs` | Desktop sidebar, SQLite thread registration, and ledger updates. |
| `src/providers/codex/tests.rs` | DSH-to-Codex conversion regression coverage. |
| `src/providers/{claude,zcode,agy}.rs` | Other providers' native formats and registration. |
| `scripts/verify_codex_resume.py` | Isolated end-to-end Codex resume probe. |

## Context and transcript fidelity

`Session.events` contains the full normalized archive. `Session.resume_events` is optional and contains the source's retained model context after replacement and pruning. Keeping these separate prevents display history from accidentally becoming the entire next model request.

DSH replacement bounds refer to positions in the current context surface. A newer summary can precede an older retained message, so sorting or deleting by numeric sequence range is incorrect. Replay the operations in log order, splice between the referenced positions, and restore tool calls for surviving results. Invalid or unsupported operations fail conversion rather than silently replaying superseded history.

Codex encodes retained events into a native `compacted.payload.replacement_history`. The visible rollout still contains the full converted archive. Tests cover summaries, pruned outputs, reordered sequence bounds, retained call/result pairs, invalid operations, and legacy transcripts without surface metadata.

## Local validation

```sh
cargo fmt --all -- --check
cargo test --locked
cargo build --locked
```

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

`--max-input-chars` sets the stub's input limit; the default is 1,000,000 characters. `--timeout` bounds the probe, and `--codex` selects the executable. This is a replay and request-shape check, not a tokenizer estimate or a guarantee that every remote model configuration accepts the context.

## Provider changes

Implement `Provider::discover`, `read`, and `write`, then add construction in `provider_named`. Preserve native timestamps, message roles, and tool pair ids where supported. Document records the target cannot represent instead of claiming universal losslessness.

Keep serialization and registration concerns local to each provider. Extract a helper when it removes duplicated format logic or makes an independently complex operation easier to inspect; avoid creating a new abstraction for every record type.

For agy's protobuf fallback reader, the [community format notes](https://gist.github.com/ArcticWinterSturm/718637afb3094814a94dc77261e0b5e0) describe the step map used by the implementation.
