<div align="center"><a name="readme-top"></a>

<img src="assets/icon.png" alt="harness-bridge logo: a cobalt bridge with a silver span" width="112" height="112">

<h1>harness-bridge</h1>

**Move AI coding sessions between tools.**<br>
Native session migration for **Codex, Claude Code, Qoder CLI, DeepSeek Harness, ZCode and Antigravity**.

[**Quick start**](#quick-start) · [**Supported tools**](#supported-tools) · [**Documentation**](#documentation) · [**Changelog**](docs/releases/v0.5.0.md) · [**Feedback**][issues]

[![Build][build-badge]][build] [![Release][release-badge]][releases] [![Rust][rust-badge]](https://www.rust-lang.org/) [![License][license-badge]](LICENSE)<br>
[![Stars][stars-badge]][stars] [![Issues][issues-badge]][issues]

</div>

Search saved conversations, preview a transfer, and continue in another coding tool. harness-bridge is a Rust CLI with a searchable terminal interface and an optional local browser UI built with GPUI Kit. It reads the source without changing it and writes a native session the destination can list and resume.

You can also move sessions between computers with portable JSON files, or import **ChatGPT, claude.ai and Gemini data exports** into a local coding harness.

<details>
<summary><kbd>Table of contents</kbd></summary>

- [Quick start](#quick-start)
- [Features](#features)
- [Local browser UI](#local-browser-ui)
- [Supported tools](#supported-tools)
- [Migrate a coding session](#migrate-a-coding-session)
- [Move a session to another machine](#move-a-session-to-another-machine)
- [Continue a web chat](#continue-a-web-chat)
- [Before you transfer](#before-you-transfer)
- [Documentation](#documentation)
- [Development](#development)
- [License](#license)

</details>

<a id="install"></a>

## Quick start

Download **v0.5.0** for your platform, extract the archive, and put the executable on your `PATH`:

| Platform | Download |
| :-- | :-- |
| Windows x64 | [ZIP](https://github.com/ahnafnafee/harness-bridge/releases/download/v0.5.0/harness-bridge-v0.5.0-x86_64-pc-windows-msvc.zip) |
| Linux x64 | [tar.gz](https://github.com/ahnafnafee/harness-bridge/releases/download/v0.5.0/harness-bridge-v0.5.0-x86_64-unknown-linux-gnu.tar.gz) |
| macOS Apple Silicon | [tar.gz](https://github.com/ahnafnafee/harness-bridge/releases/download/v0.5.0/harness-bridge-v0.5.0-aarch64-apple-darwin.tar.gz) |

Both interfaces are included. [SHA-256 checksums](https://github.com/ahnafnafee/harness-bridge/releases/download/v0.5.0/sha256sums.txt) are available with the [release notes][release].

<details>
<summary>Build from source with stable Rust</summary>

```sh
cargo install --locked --git https://github.com/ahnafnafee/harness-bridge --tag v0.5.0 --features web-ui
```

For a smaller terminal-only build, omit `--features web-ui`.

</details>

Then open the terminal interface:

```sh
harness-bridge
```

Search for a session, press **Enter** to configure and preview a transfer, then press **w** to confirm the write. Prefer a graphical interface? Run `harness-bridge ui --open`.

The bundled interfaces do not require Python, Node.js or a separate frontend build.

## Features

| Feature | What it does |
| :-- | :-- |
| **Native session migration** | Convert messages, tool activity and saved summaries into the destination harness's session format. |
| **Searchable session library** | Browse by source, project folder, title or id in the terminal or local browser. |
| **Preview before writing** | Review a captured source snapshot and destination details, then confirm the transfer separately. |
| **Portable session files** | Carry the normalized archive, retained context and saved child links to another computer. |
| **Web chat imports** | Read official ChatGPT, claude.ai and Gemini exports as sources for local sessions. |

The archive and retained model context are handled separately, so supported compaction checkpoints do not replay all earlier history. [Conversion limits](#before-you-transfer) explain what cannot carry over.

## Local browser UI

```sh
harness-bridge ui --open
```

The GPUI Kit interface provides source and project filters, conversation excerpts, transfer previews, portable-file operations and per-tab history. It runs on `127.0.0.1`; its fonts, icons and WebAssembly module are bundled locally. Keep the process running while using the browser.

See the [interface guide](docs/interfaces.md) for keyboard controls, source locations and browser requirements. Windows Chrome is the validated browser; screen-reader integration and real phone keyboard/IME behavior still need validation.

<a id="compatibility"></a>

## Supported tools

| Tool | Provider id | Support |
| :-- | :-- | :-- |
| Codex CLI / Desktop | `codex` | Read and write |
| Claude Code | `claude` | Read and write |
| Qoder CLI | `qoder` | Read and write |
| DeepSeek Harness | `dsh` | Read and write |
| ZCode | `zcode` | Read and write |
| Antigravity | `agy` | Read; write text turns |
| Qoder IDE | `qoder-ide` | Read only |
| ChatGPT export | `chatgpt` | Read only |
| claude.ai export | `claude-web` | Read only |
| Gemini export | `gemini-web` | Read only |

Codex writes require an existing top-level Desktop session as a template. ZCode needs its initialized database, and Antigravity needs a template conversation. Antigravity tool activity remains in the migration archive rather than its native text turns. Qoder CLI imports do not register sessions in the Qoder IDE sidebar. See [provider details](docs/interfaces.md#qoder) and the [migration guide](docs/migration-guide.md).

## Migrate a coding session

Find the source session and preview the conversion:

```sh
harness-bridge list --provider codex
harness-bridge migrate "<session-id>" --from codex --to claude --dry-run
```

When the preview is correct, write it:

```sh
harness-bridge migrate "<session-id>" --from codex --to claude
```

The result includes the destination's native session id and saved location. Use an id prefix or title instead of the full id when it identifies one session; ambiguous queries stop and show the matches.

Add `--cwd "/path/to/project"` to set the new session's working directory, `--name "Session title"` to rename it, or `--include-subagents` to copy saved child conversations. Paths recorded inside the history are preserved. [All migration options →](docs/migration-guide.md#options)

## Move a session to another machine

On the source computer, export a portable file:

```sh
harness-bridge export "<session-id>" --from claude --output session.hbridge.json --include-subagents
```

Copy the file to the receiving computer, install the current version there, and import it into an initialized destination:

```sh
harness-bridge import session.hbridge.json --to codex --cwd "/path/to/project" --dry-run
harness-bridge import session.hbridge.json --to codex --cwd "/path/to/project"
```

The receiver reads the portable file without needing the source harness's installation. Every bundled child imports automatically. Project files and attachments must be moved separately. See the [portable format guide](docs/portable-format.md) for checksums, receiver versions and context recovery.

## Continue a web chat

Download a ChatGPT, claude.ai or Gemini data export, then list its chats:

```sh
harness-bridge list --export chatgpt.zip
```

Choose a chat and an existing project folder:

```sh
harness-bridge migrate "<chat-id>" --from chatgpt --to claude --export chatgpt.zip --cwd "./chats" --dry-run
```

Remove `--dry-run` to write the session. Web chat imports require `--cwd`; attachments are copied there only when the export contains them. Web apps are sources only, so harness-bridge cannot import a local session back into ChatGPT, claude.ai or Gemini.

[Export instructions and supported formats →](docs/migration-guide.md#continue-a-web-chat)

<a id="common-questions"></a>

## Before you transfer

- **The source stays intact.** Migration reads the original session and writes to the destination.
- **Reimports update the same destination.** Deterministic ids can replace turns added after an earlier transfer. Preserve that destination copy before importing again.
- **Hidden model state does not transfer.** Some encrypted or unsupported checkpoints require explicit `--rebuild-resume-context`; recovery cannot restore encrypted decisions.
- **Context budgets count characters, not tokens.** Oversized context fails unless you explicitly allow pruning with `--prune-resume-context`. The full normalized archive remains available.
- **Tool commands are not translated.** History can move between harnesses, but their tool names, permissions and runtime behavior can differ. Importing saved child sessions does not start agents.

For reasoning replay, native storage formats, Codex sidebar behavior and provider limitations, read the [migration guide](docs/migration-guide.md).

## Documentation

| Guide | Covers |
| :-- | :-- |
| [Interactive interfaces](docs/interfaces.md) | Terminal keys, browser setup, source locations and Qoder compatibility. |
| [Session migration](docs/migration-guide.md) | CLI options, web exports, compaction, Codex resume and child sessions. |
| [Portable file format](docs/portable-format.md) | Versioned exports, checksums, receiver requirements and recovery. |
| [Development](docs/development.md) | Architecture, provider implementations and isolated validation. |
| [0.5.0 release notes](docs/releases/v0.5.0.md) | Terminal workbench, GPUI Kit browser UI and Qoder support. |

## Development

```sh
cargo fmt --all -- --check
cargo test --locked --all-features
cargo +stable test --locked --manifest-path ui/Cargo.toml
```

The [development guide](docs/development.md) describes rebuilding the GPUI frontend and testing native resume behavior in throwaway stores. [AGENTS.md](AGENTS.md) documents using harness-bridge from a coding agent. Report reproducible problems or propose changes through [GitHub issues][issues].

## License

[MIT](LICENSE). Bundled fonts and graphical components retain their [third-party licenses](assets/licenses/README.md).

<div align="right">

[Back to top ↑](#readme-top)

</div>

[build-badge]: https://img.shields.io/github/actions/workflow/status/ahnafnafee/harness-bridge/ci.yml?branch=main&style=flat-square&label=tests&labelColor=30343b&color=193bd1
[build]: https://github.com/ahnafnafee/harness-bridge/actions/workflows/ci.yml
[release-badge]: https://img.shields.io/github/v/release/ahnafnafee/harness-bridge?style=flat-square&labelColor=30343b&color=193bd1
[rust-badge]: https://img.shields.io/badge/Rust-stable-193bd1?style=flat-square&labelColor=30343b
[license-badge]: https://img.shields.io/badge/license-MIT-193bd1?style=flat-square&labelColor=30343b
[stars-badge]: https://img.shields.io/github/stars/ahnafnafee/harness-bridge?style=flat-square&labelColor=30343b&color=637089
[issues-badge]: https://img.shields.io/github/issues/ahnafnafee/harness-bridge?style=flat-square&labelColor=30343b&color=637089
[releases]: https://github.com/ahnafnafee/harness-bridge/releases
[release]: https://github.com/ahnafnafee/harness-bridge/releases/tag/v0.5.0
[stars]: https://github.com/ahnafnafee/harness-bridge/stargazers
[issues]: https://github.com/ahnafnafee/harness-bridge/issues
