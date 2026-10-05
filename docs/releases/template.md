## harness-bridge __VERSION__

Migrate coding sessions between DeepSeek Harness, Codex Desktop / CLI, Claude Code, ZCode, and agy. Provider-specific compatibility limits are documented in the README.

### Downloads

| Archive | Platform | Architecture |
| :-- | :-- | :-- |
| `harness-bridge-__VERSION__-x86_64-pc-windows-msvc.zip` | Windows | x64 |
| `harness-bridge-__VERSION__-x86_64-unknown-linux-gnu.tar.gz` | Linux | x64 |
| `harness-bridge-__VERSION__-aarch64-apple-darwin.tar.gz` | macOS | Apple Silicon |

### Install

Extract the archive and put the `harness-bridge` executable on your `PATH`, then run `harness-bridge list`.

To build from source:

```sh
cargo install --locked --git https://github.com/ahnafnafee/harness-bridge --tag __VERSION__
```

### Verify

SHA-256 checksums are attached in `sha256sums.txt`. Run `sha256sum -c sha256sums.txt` from the directory containing the downloaded archives.

### Compatibility

- Local validation is Windows-first. Linux/macOS paths default to `$HOME` and were not exercised against real harness stores.
- DSH imports retain visible history while restoring compacted model context for Codex resume.
- agy writes support text turns and omit tool calls/results. Attachment and media transfer is not implemented.
- Migration guidance and the development guide are included in the archive.

### Changes
