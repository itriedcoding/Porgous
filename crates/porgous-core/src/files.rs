//! Sandboxed file access for server volumes.
//!
//! Every path is resolved lexically, then canonicalized, and must remain inside
//! the server's volume root. Symlinks that point outside the root are rejected.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub size: u64,
    pub modified: Option<i64>,
    pub mode: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct LocalFiles {
    root: PathBuf,
}

impl LocalFiles {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root).with_context(|| format!("creating volume {}", root.display()))?;
        let root = root
            .canonicalize()
            .with_context(|| format!("canonicalizing {}", root.display()))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve a client supplied relative path inside the volume.
    pub fn resolve(&self, relative: &str, allow_missing: bool) -> Result<PathBuf> {
        let relative = relative.trim();
        if relative.starts_with('/') || relative.starts_with('\\') {
            bail!("absolute paths are not allowed");
        }
        let relative = relative.trim_start_matches("./");
        if relative.contains('\0') {
            bail!("path contains a NUL byte");
        }
        let candidate = Path::new(relative);
        for component in candidate.components() {
            match component {
                Component::Normal(_) | Component::CurDir => {}
                _ => bail!("path traversal is not allowed"),
            }
        }
        let joined = self.root.join(candidate);
        if joined.exists() {
            let canonical = joined.canonicalize().context("resolving path")?;
            if !canonical.starts_with(&self.root) {
                bail!("path escapes the server volume");
            }
            return Ok(canonical);
        }
        if !allow_missing {
            bail!("file not found");
        }
        // For new files, canonicalize the deepest existing ancestor.
        let mut ancestor = joined
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| self.root.clone());
        while !ancestor.exists() {
            match ancestor.parent() {
                Some(parent) => ancestor = parent.to_path_buf(),
                None => bail!("invalid path"),
            }
        }
        let canonical_ancestor = ancestor.canonicalize().context("resolving parent")?;
        if !canonical_ancestor.starts_with(&self.root) {
            bail!("path escapes the server volume");
        }
        Ok(joined)
    }

    pub fn list(&self, relative: &str) -> Result<Vec<FileEntry>> {
        let dir = self.resolve(relative, false)?;
        if !dir.is_dir() {
            bail!("not a directory");
        }
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(&dir).context("reading directory")? {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            let metadata = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            let file_type = entry.file_type().ok();
            let relative_path = entry
                .path()
                .strip_prefix(&self.root)
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            entries.push(FileEntry {
                name: entry.file_name().to_string_lossy().to_string(),
                path: relative_path,
                is_dir: metadata.is_dir(),
                is_symlink: file_type.map(|t| t.is_symlink()).unwrap_or(false),
                size: metadata.len(),
                modified: metadata.modified().ok().and_then(|t| {
                    t.duration_since(std::time::UNIX_EPOCH)
                        .ok()
                        .map(|d| d.as_millis() as i64)
                }),
                mode: mode_of(&metadata),
            });
        }
        entries.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        Ok(entries)
    }

    pub fn read(&self, relative: &str, max_bytes: u64) -> Result<Vec<u8>> {
        let path = self.resolve(relative, false)?;
        if !path.is_file() {
            bail!("not a file");
        }
        let metadata = std::fs::metadata(&path)?;
        if metadata.len() > max_bytes {
            bail!("file is larger than the {} byte limit", max_bytes);
        }
        std::fs::read(path).context("reading file")
    }

    pub fn write(&self, relative: &str, bytes: &[u8]) -> Result<()> {
        let path = self.resolve(relative, true)?;
        if path == self.root || path.is_dir() {
            bail!("path is a directory");
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Atomic write: temp file in the same directory, then rename.
        let tmp = path.with_extension(format!(
            "{}.tmp",
            path.extension().and_then(|e| e.to_str()).unwrap_or("file")
        ));
        {
            use std::io::Write;
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(bytes)?;
            file.sync_all()?;
        }
        std::fs::rename(&tmp, &path).context("committing file")?;
        Ok(())
    }

    pub fn mkdir(&self, relative: &str) -> Result<()> {
        let path = self.resolve(relative, true)?;
        std::fs::create_dir_all(path)?;
        Ok(())
    }

    pub fn delete(&self, relative: &str) -> Result<()> {
        let path = self.resolve(relative, false)?;
        if path == self.root {
            bail!("refusing to delete the volume root");
        }
        if path.is_dir() {
            std::fs::remove_dir_all(path)?;
        } else {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }

    pub fn rename(&self, from: &str, to: &str) -> Result<()> {
        let from = self.resolve(from, false)?;
        let to = self.resolve(to, true)?;
        if from == self.root {
            bail!("refusing to move the volume root");
        }
        if to.starts_with(&from) {
            bail!("cannot move a directory inside itself");
        }
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(from, to)?;
        Ok(())
    }

    #[cfg(unix)]
    pub fn chmod(&self, relative: &str, mode: u32) -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        if mode > 0o777 {
            bail!("invalid mode");
        }
        let path = self.resolve(relative, false)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
        Ok(())
    }

    #[cfg(not(unix))]
    pub fn chmod(&self, _relative: &str, _mode: u32) -> Result<()> {
        bail!("chmod is only supported on unix nodes")
    }

    pub fn disk_usage(&self) -> u64 {
        dir_size(&self.root)
    }

    pub fn open_for_read(&self, relative: &str) -> Result<(std::fs::File, u64)> {
        let path = self.resolve(relative, false)?;
        if !path.is_file() {
            bail!("not a file");
        }
        let file = std::fs::File::open(&path)?;
        let size = file.metadata()?.len();
        Ok((file, size))
    }
}

fn mode_of(metadata: &std::fs::Metadata) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Some(metadata.permissions().mode() & 0o777)
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        None
    }
}

pub fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.is_dir() {
            total += dir_size(&entry.path());
        } else {
            total += metadata.len();
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service() -> (tempfile::TempDir, LocalFiles) {
        let dir = tempfile::tempdir().unwrap();
        let files = LocalFiles::new(dir.path().join("volume")).unwrap();
        (dir, files)
    }

    #[test]
    fn rejects_traversal_and_absolute_paths() {
        let (_dir, files) = service();
        assert!(files.resolve("../escape", true).is_err());
        assert!(files.resolve("/etc/passwd", true).is_err());
        assert!(files.resolve("a/../../escape", true).is_err());
        assert!(files.resolve("safe/new.txt", true).is_ok());
    }

    #[test]
    fn rejects_symlink_escape() {
        let (dir, files) = service();
        let outside = dir.path().join("outside.txt");
        std::fs::write(&outside, b"secret").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, files.root().join("link")).unwrap();
            assert!(files.resolve("link", false).is_err());
            assert!(files.read("link", 1024).is_err());
        }
    }

    #[test]
    fn write_read_list_delete_rename() {
        let (_dir, files) = service();
        files.write("config/server.properties", b"motd=hi").unwrap();
        assert_eq!(files.read("config/server.properties", 1024).unwrap(), b"motd=hi");
        files.mkdir("plugins").unwrap();
        files
            .rename("config/server.properties", "server.properties")
            .unwrap();
        let list = files.list("").unwrap();
        assert!(list.iter().any(|e| e.name == "server.properties" && !e.is_dir));
        assert!(list.iter().any(|e| e.name == "plugins" && e.is_dir));
        files.delete("plugins").unwrap();
        assert!(files.list("").unwrap().iter().all(|e| e.name != "plugins"));
    }

    #[test]
    fn read_limit_enforced() {
        let (_dir, files) = service();
        files.write("big.bin", &vec![0u8; 4096]).unwrap();
        assert!(files.read("big.bin", 1024).is_err());
        assert!(files.read("big.bin", 8192).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn chmod_works() {
        let (_dir, files) = service();
        files.write("script.sh", b"#!/bin/sh\n").unwrap();
        files.chmod("script.sh", 0o755).unwrap();
        let list = files.list("").unwrap();
        assert_eq!(
            list.iter().find(|e| e.name == "script.sh").unwrap().mode,
            Some(0o755)
        );
    }
}
