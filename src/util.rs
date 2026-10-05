use chrono::TimeZone;
use sha2::{Digest, Sha256};

/// Deterministic UUIDv7 anchored at a unix-ms timestamp (same ms+seed -> same uuid).
pub fn uuid7(ms: i64, seed: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!("{}|{}", ms, seed).as_bytes());
    let digest = hasher.finalize();
    let mut b = [0u8; 16];
    b.copy_from_slice(&digest[..16]);
    let ts = (ms as u64) & ((1u64 << 48) - 1);
    b[0] = (ts >> 40) as u8;
    b[1] = (ts >> 32) as u8;
    b[2] = (ts >> 24) as u8;
    b[3] = (ts >> 16) as u8;
    b[4] = (ts >> 8) as u8;
    b[5] = ts as u8;
    b[6] = (b[6] & 0x0F) | 0x70;
    b[8] = (b[8] & 0x3F) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

/// RFC3339 with millisecond precision + Z suffix, e.g. `2026-10-04T06:14:39.365Z`.
pub fn iso_ms(ms: i64) -> String {
    let dt = chrono::Utc
        .timestamp_millis_opt(ms)
        .single()
        .unwrap_or_else(chrono::Utc::now);
    dt.format("%Y-%m-%dT%H:%M:%S.%3fZ").to_string()
}

/// Read a text file, transparently decompressing zstd (by extension or magic bytes).
///
/// dsh appends compressed session chunks, producing multi-frame zstd files, so
/// frames are decoded one by one until the input is exhausted.
pub fn read_maybe_zstd(path: &std::path::Path) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    let is_zstd = path.extension().map(|e| e == "zstd").unwrap_or(false)
        || bytes.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]);
    if !is_zstd {
        return Ok(String::from_utf8_lossy(&bytes).into_owned());
    }
    use ruzstd::decoding::{BlockDecodingStrategy, FrameDecoder};

    let mut input: &[u8] = &bytes;
    let mut out: Vec<u8> = Vec::new();
    let mut dec = FrameDecoder::new();
    while !input.is_empty() {
        match dec.reset(&mut input) {
            Ok(()) => {}
            Err(ruzstd::decoding::errors::FrameDecoderError::ReadFrameHeaderError(
                ruzstd::decoding::errors::ReadFrameHeaderError::SkipFrame { length, .. },
            )) => {
                input = input.get(length as usize..).ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "zstd: truncated skip frame",
                    )
                })?;
                continue;
            }
            Err(e) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("zstd: {e}"),
                ));
            }
        }
        loop {
            dec.decode_blocks(&mut input, BlockDecodingStrategy::UptoBytes(1 << 20))
                .map_err(|e| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, format!("zstd: {e}"))
                })?;
            if dec.is_finished() || input.is_empty() {
                break;
            }
        }
        if !dec.is_finished() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "zstd: truncated frame",
            ));
        }
        dec.collect_to_writer(&mut out).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("zstd: {e}"))
        })?;
    }
    String::from_utf8(out).map_err(|e| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, format!("zstd: utf8: {e}"))
    })
}

pub fn home() -> std::path::PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

pub fn default_codex_home() -> std::path::PathBuf {
    home().join(".codex")
}
pub fn default_dsh_home() -> std::path::PathBuf {
    home().join(".dsh")
}
pub fn default_claude_home() -> std::path::PathBuf {
    home().join(".claude")
}
pub fn default_zcode_db() -> std::path::PathBuf {
    home()
        .join(".zcode")
        .join("cli")
        .join("db")
        .join("db.sqlite")
}
pub fn default_agy_dir() -> std::path::PathBuf {
    home().join(".gemini").join("antigravity-cli")
}

/// Claude Code project-slug: every char outside [A-Za-z0-9] becomes '-'.
pub fn claude_slug(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// ZCode project id, derived empirically from 115 real rows:
/// lowercase; keep [a-z0-9.]; drop `:` `(` `)`; map every other char to '-';
/// truncate to 85 chars total. E.g. `F:\Miscellaneous\GitHub\mod-smali`
/// -> `proj_f-miscellaneous-github-mod-smali`.
pub fn zcode_project_id(cwd: &str) -> String {
    let mut out = String::from("proj_");
    for c in cwd.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() || c == '.' {
            out.push(c);
        } else if c == ':' || c == '(' || c == ')' {
            // dropped entirely
        } else {
            out.push('-');
        }
    }
    out.chars().take(85).collect()
}

/// `\\?\` extended-length prefix used by Codex Desktop path columns.
pub fn extended_path(p: &str) -> String {
    if p.starts_with("\\\\?\\") {
        p.to_string()
    } else {
        format!("\\\\?\\{}", p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression anchor: the id the original warlock-crack migration produced.
    #[test]
    fn uuid7_is_deterministic_and_v7() {
        let id = uuid7(1791094479365, "warlock-crack");
        assert_eq!(id, "01a1058c-da05-7479-b073-19ea73b67c9f");
        assert_eq!(&id[14..15], "7"); // version 7
        assert!(
            b"89ab".contains(&id.as_bytes()[19]),
            "variant bits, got {}",
            &id[19..20]
        );
        assert_eq!(uuid7(1791094479365, "warlock-crack"), id);
        assert_ne!(uuid7(1791094479365, "other"), id);
    }

    #[test]
    fn iso_ms_formats_utc_millis() {
        assert_eq!(iso_ms(1791094479365), "2026-10-04T06:14:39.365Z");
    }

    #[test]
    fn claude_slug_replaces_everything_non_alnum() {
        assert_eq!(claude_slug("D:\\GitHub\\mod-smali"), "D--GitHub-mod-smali");
        assert_eq!(claude_slug("/home/u/my repo"), "-home-u-my-repo");
    }

    #[test]
    fn zcode_project_id_matches_real_rows() {
        assert_eq!(
            zcode_project_id("F:\\Miscellaneous\\GitHub\\mod-smali"),
            "proj_f-miscellaneous-github-mod-smali"
        );
        assert_eq!(
            zcode_project_id("D:\\Games\\Half-Life - Alyx"),
            "proj_d-games-half-life---alyx"
        );
        assert_eq!(
            zcode_project_id("D:\\GitHub\\mod-smali\\briteconnect\\.worktrees"),
            "proj_d-github-mod-smali-briteconnect-.worktrees"
        );
        // colons and parens drop, spaces map 1:1, cap at 85 chars
        let long = zcode_project_id("E:\\OneDrive\\OneDrive - George Mason University - O365 Production\\Courses\\3. Fall 2026\\CS584");
        assert_eq!(long.len(), 85);
        assert!(long.ends_with("-fall-"));
    }

    #[test]
    fn extended_path_is_idempotent() {
        assert_eq!(extended_path("C:\\x"), "\\\\?\\C:\\x");
        assert_eq!(extended_path("\\\\?\\C:\\x"), "\\\\?\\C:\\x");
    }
}
