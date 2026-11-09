## harness-bridge __VERSION__

Browse, preview and migrate coding sessions between Codex, Claude Code, Qoder CLI, DeepSeek Harness, ZCode and Antigravity. Includes a terminal workbench and a local GPUI Kit browser interface.

### Downloads

| Archive | Platform | Architecture |
| :-- | :-- | :-- |
| `harness-bridge-__VERSION__-x86_64-pc-windows-msvc.zip` | Windows | x64 |
| `harness-bridge-__VERSION__-x86_64-unknown-linux-gnu.tar.gz` | Linux | x64 |
| `harness-bridge-__VERSION__-aarch64-apple-darwin.tar.gz` | macOS | Apple Silicon |

### Install

Extract the archive and put the `harness-bridge` executable on your `PATH`. Run `harness-bridge` for the terminal workbench, `harness-bridge ui --open` for the browser, or `harness-bridge list` to use the CLI.

The downloads include both interfaces. Python, Node.js and a frontend compiler are not needed.

To build from source:

```sh
cargo install --locked --git https://github.com/ahnafnafee/harness-bridge --tag __VERSION__ --features web-ui
```

### Verify

SHA-256 checksums are attached in `sha256sums.txt`. Compare the entry for your archive with `Get-FileHash <archive> -Algorithm SHA256` in PowerShell, `sha256sum <archive>` on Linux, or `shasum -a 256 <archive>` on macOS.

### Compatibility

- Packaged binaries are checked against isolated fixtures on all three platforms. Real native stores and browser behavior are primarily validated on Windows.
- Qoder IDE and web chat exports are read-only sources. Qoder CLI imports do not populate the IDE sidebar.
- Antigravity writes support text turns; tool activity remains in the normalized archive. Portable files reference attachments but do not embed them.
- Sources stay intact. Reimports can replace later destination turns, so preserve that destination copy before repeating a transfer.
- Migration guidance, interface controls, third-party licenses and the development guide are included in each archive.

### Changes
