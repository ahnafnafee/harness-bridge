# Portable session format, versions 1 and 2

The `export` command writes a standalone UTF-8 JSON file, conventionally named `*.hbridge.json`. The receiving machine's `import` command converts it through the same native writers as a local migration. The extension is a convention; detection uses the document's format and version fields.

```sh
harness-bridge export "<session-id-or-title>" --from claude --output session.hbridge.json --include-subagents
harness-bridge import session.hbridge.json --to codex --cwd "/path/on/receiving/machine" --dry-run
harness-bridge import session.hbridge.json --to codex --cwd "/path/on/receiving/machine"
```

The source and destination can each be `dsh`, `codex`, `claude`, `zcode`, or `agy`, including the same harness on different machines. Both PCs need harness-bridge 0.3.0 or later. The exporting PC needs only its source store; the receiving PC needs only the chosen destination's initialized store and any required local template.

## Envelope

| Field | Meaning |
| :-- | :-- |
| `format` | Exactly `harness-bridge-session`. |
| `version` | Integer `1` for ordinary exports, or `2` for exports with unavailable resume context. Unsupported versions fail before native writes. |
| `exported_at` | RFC 3339 UTC timestamp identifying when the file was made. |
| `sha256` | Lowercase hexadecimal SHA-256 of the normalized JSON payload described below. |
| `payload.root_session_id` | The source-native id of the selected root. |
| `payload.sessions` | One root and optionally its saved descendants, as normalized sessions. |

Session and event fields follow [the shared model](../src/ir.rs). The schema retains source provider/id, title, cwd, preset, origin, parent id, creation/update times, and timestamped events. Events represent messages, visible reasoning text, tool calls/results, turn boundaries, persisted compaction summaries, and supported structured source metadata.

`events` is the full normalized archive. When present, `resume_events` is the separate retained model context. An absent field means the full archive supplies context; an empty array means the source retained no events. Export preserves that distinction and does not prune or enforce a destination context budget. Import applies its selected budget to each family member before any native write, and pruning changes only the imported resume context. The transfer file stays unchanged.

Version 2 adds the optional `resume_context_unavailable` reason on each session. If present, the complete retained context cannot be reconstructed. Any `resume_events` then contains only readable items from the latest checkpoint and later turns, available as a recovery seed. Import rejects the entire family before writes unless `--rebuild-resume-context` is supplied. The receiver uses that seed, or the readable archive when no seed exists, adds a recovery notice, and runs the usual context budget/pruning preflight. Encrypted compaction/hidden reasoning is never decoded or fabricated. Recovery may lose information available only in encrypted state, and archive fallback can include superseded history.

These files require harness-bridge 0.3.1 or later on both machines. Export emits version 2 only if a family member has unavailable context, so 0.3.0 readers reject such files instead of silently replaying incomplete context. Version 1 cannot carry `resume_context_unavailable`; normal version 1 exports and imports remain supported.

## Checksum

The checksum covers the complete `payload` JSON value before session deserialization. Recursively sort every object's keys, preserving array order, then serialize that value as compact UTF-8 JSON using `serde_json` and hash those bytes with SHA-256. Whitespace and object-key order in the file do not affect verification. Changing any payload field or inserting a field without updating the checksum is rejected. Envelope metadata is validated separately.

This checksum detects transfer damage and changed payloads. It is not a signature or encryption. The file contains transcript text and paths as recorded; text already containing sensitive information remains present. Source authentication settings, project files, attachments, provider-specific encrypted reasoning, and executable hidden state are not packaged. Native records that a reader cannot represent remain subject to the provider's documented fidelity limits.

## Child sessions and placement

`export --include-subagents` follows native saved child links recursively. Without it, only the selected root is included. Every non-root session uses its parent's canonical source id inside the file. A root may retain an original parent outside the exported family; import detaches it and preserves that original parent as source metadata.

Import always handles every session included in the file. It rejects empty families, duplicate ids, missing parents, disconnected/cyclic links, mixed source providers, invalid timestamps, and families exceeding 1,024 members. Source-native ids are retained as identity inputs, while native destination ids and parent links are calculated on the receiving machine. Previously recorded migration placement metadata cannot override the receiver's newly calculated parent location or depth.

`--cwd` sets the destination base directory for the whole family, and `--name` changes the root's title. Paths embedded in messages and tool arguments remain unchanged. Project files must be transferred separately. Reimporting the same source ids uses deterministic native ids and may replace turns added in the destination since the previous import.

## Command behavior

`export --output <file>` creates a new file, including missing parent directories, and refuses to overwrite an existing path. `export --dry-run` performs source reading, family validation, serialization, and checksum calculation without creating the file or its directories.

`import <file> --to <harness>` validates the file, preflights every family member, writes native sessions, and performs the selected harness's usual registration. `--dry-run`, `--resume-max-chars`, `--prune-resume-context`, `--cwd`, `--name`, and global destination-home overrides work as with local migration. Destination conversion failures during preflight write nothing; a later I/O or registry failure can leave already-written family members, since separate native stores have no shared transaction.
