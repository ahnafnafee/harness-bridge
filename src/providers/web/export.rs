//! A web app's data export, opened as a zip, its extracted folder, or one file inside it.

use anyhow::Context;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Exports are shallow (`Takeout/My Activity/Gemini Apps/<file>` is the deepest
/// known layout), so a bounded walk keeps a stray `--export ~/Downloads/x.json`
/// from listing a whole download folder tree.
const MAX_DEPTH: usize = 5;

enum Source {
    Zip(PathBuf),
    Dir(PathBuf),
}

pub struct Export {
    source: Source,
    /// Every file in the export, relative, with `/` separators.
    files: Vec<String>,
    /// The file the user pointed at when they passed one file instead of the export.
    chosen: Option<String>,
}

impl Export {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let meta = std::fs::metadata(path)
            .with_context(|| format!("cannot open export {}", path.display()))?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        anyhow::ensure!(
            !(name.ends_with(".tgz") || name.ends_with(".tar.gz")),
            "{} is a .tgz archive; extract it and pass the extracted folder",
            path.display()
        );
        if meta.is_dir() {
            return Ok(Self {
                files: walk(path)?,
                source: Source::Dir(path.to_path_buf()),
                chosen: None,
            });
        }
        if name.ends_with(".zip") {
            let mut archive = open_zip(path)?;
            let mut files = Vec::new();
            for i in 0..archive.len() {
                let entry = archive.by_index(i)?;
                if !entry.is_dir() {
                    files.push(entry.name()?.into_owned());
                }
            }
            return Ok(Self {
                files,
                source: Source::Zip(path.to_path_buf()),
                chosen: None,
            });
        }
        // One file: its folder is the export root, so attachments beside it resolve.
        let root = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        Ok(Self {
            files: walk(root)?,
            chosen: path.file_name().map(|n| n.to_string_lossy().into_owned()),
            source: Source::Dir(root.to_path_buf()),
        })
    }

    pub fn files(&self) -> &[String] {
        &self.files
    }

    pub fn chosen(&self) -> Option<&str> {
        self.chosen.as_deref()
    }

    pub fn read_text(&self, entry: &str) -> anyhow::Result<String> {
        match &self.source {
            Source::Dir(root) => std::fs::read_to_string(root.join(entry))
                .with_context(|| format!("cannot read {entry} from the export")),
            Source::Zip(path) => {
                let mut archive = open_zip(path)?;
                let mut file = archive
                    .by_name(entry)
                    .with_context(|| format!("{entry} is missing from the export"))?;
                let mut text = String::new();
                file.read_to_string(&mut text)
                    .with_context(|| format!("cannot read {entry} from the export"))?;
                Ok(text)
            }
        }
    }

    /// Copies one file out of the export. Returns false when `dest` already holds
    /// a file of the same size, so repeated migrations leave copies alone.
    pub fn copy_to(&self, entry: &str, dest: &Path) -> anyhow::Result<bool> {
        let existing = std::fs::metadata(dest).ok().map(|m| m.len());
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        match &self.source {
            Source::Dir(root) => {
                let source = root.join(entry);
                if existing == Some(std::fs::metadata(&source)?.len()) {
                    return Ok(false);
                }
                std::fs::copy(&source, dest)
                    .with_context(|| format!("cannot copy {entry} to {}", dest.display()))?;
            }
            Source::Zip(path) => {
                let mut archive = open_zip(path)?;
                let mut file = archive.by_name(entry)?;
                if existing == Some(file.size()) {
                    return Ok(false);
                }
                let mut out = std::fs::File::create(dest)
                    .with_context(|| format!("cannot create {}", dest.display()))?;
                std::io::copy(&mut file, &mut out)?;
            }
        }
        Ok(true)
    }

    /// The file named exactly `name`, ignoring case when no exact match exists.
    pub fn find_name(&self, name: &str) -> Option<&str> {
        let base = |f: &&String| f.rsplit('/').next().unwrap_or(f).to_string();
        self.files
            .iter()
            .find(|f| base(f) == name)
            .or_else(|| {
                self.files
                    .iter()
                    .find(|f| base(f).eq_ignore_ascii_case(name))
            })
            .map(String::as_str)
    }

    /// The file whose name starts with an asset id followed by a separator
    /// (`file-ABC123-photo.png` for `file-ABC123`).
    pub fn find_prefix(&self, id: &str) -> Option<&str> {
        if id.is_empty() {
            return None;
        }
        self.files
            .iter()
            .find(|f| {
                let base = f.rsplit('/').next().unwrap_or(f);
                base.strip_prefix(id)
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with(['-', '.', '_']))
            })
            .map(String::as_str)
    }
}

fn open_zip(path: &Path) -> anyhow::Result<zip::ZipArchive<std::fs::File>> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("cannot open export {}", path.display()))?;
    zip::ZipArchive::new(file).with_context(|| format!("{} is not a readable zip", path.display()))
}

fn walk(root: &Path) -> anyhow::Result<Vec<String>> {
    let mut files = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        for entry in std::fs::read_dir(&dir)
            .with_context(|| format!("cannot list export folder {}", dir.display()))?
        {
            let path = entry?.path();
            if path.is_dir() {
                if depth + 1 < MAX_DEPTH {
                    stack.push((path, depth + 1));
                }
            } else if let Ok(relative) = path.strip_prefix(root) {
                files.push(relative.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    files.sort();
    Ok(files)
}
