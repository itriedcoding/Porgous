//! Server backup archives (tar.gz) with checksums and safe extraction.

use anyhow::{bail, Context, Result};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Component, Path, PathBuf};

pub fn sha256_file(path: &Path) -> Result<String> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(data_encoding::HEXLOWER.encode(&hasher.finalize()))
}

/// Create a tar.gz archive of `volume` at `destination`.
/// Returns the compressed size and SHA-256 checksum.
pub fn create_archive(volume: &Path, destination: &Path) -> Result<(u64, String)> {
    if !volume.is_dir() {
        bail!("server volume does not exist");
    }
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = destination.with_extension("tar.gz.partial");
    {
        let file = File::create(&tmp).context("creating backup archive")?;
        let writer = BufWriter::new(file);
        let encoder = GzEncoder::new(writer, Compression::default());
        let mut archive = tar::Builder::new(encoder);
        archive.follow_symlinks(false);
        archive
            .append_dir_all(".", volume)
            .context("adding server volume to archive")?;
        let encoder = archive.into_inner().context("finishing archive")?;
        let mut writer = encoder.finish().context("compressing archive")?;
        writer.flush()?;
    }
    std::fs::rename(&tmp, destination).context("committing backup archive")?;
    let size = std::fs::metadata(destination)?.len();
    let checksum = sha256_file(destination)?;
    Ok((size, checksum))
}

/// Extract a verified backup into `volume`. Refuses absolute paths, traversal,
/// device nodes and links, and caps the total extracted size.
pub fn restore_archive(archive_path: &Path, volume: &Path, max_bytes: u64) -> Result<()> {
    if !archive_path.is_file() {
        bail!("backup archive not found");
    }
    std::fs::create_dir_all(volume)?;
    let file = File::open(archive_path)?;
    let decoder = GzDecoder::new(BufReader::new(file));
    let mut archive = tar::Archive::new(decoder);
    let mut total = 0u64;
    for entry in archive.entries().context("reading backup archive")? {
        let mut entry = entry.context("reading archive entry")?;
        let path = entry.path().context("reading archive entry path")?.to_path_buf();
        if path
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
        {
            bail!("backup contains an unsafe path: {}", path.display());
        }
        let entry_type = entry.header().entry_type();
        if entry_type.is_symlink() || entry_type.is_hard_link() {
            continue;
        }
        if !entry_type.is_file() && !entry_type.is_dir() {
            continue;
        }
        total = total.saturating_add(entry.header().size().unwrap_or(0));
        if total > max_bytes {
            bail!("backup expands beyond the allowed size limit");
        }
        entry.unpack_in(volume).context("extracting archive entry")?;
    }
    Ok(())
}

pub fn delete_archive(path: &Path) -> Result<()> {
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

/// Verify an archive's checksum before restoring.
pub fn verify_checksum(path: &Path, expected: &str) -> Result<()> {
    let actual = sha256_file(path)?;
    if !crate::crypto::constant_time_eq(actual.as_bytes(), expected.trim().as_bytes()) {
        bail!("backup checksum mismatch");
    }
    Ok(())
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct BackupPaths {
    pub root: PathBuf,
}

impl BackupPaths {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    pub fn server_dir(&self, server_uuid: &str) -> PathBuf {
        self.root.join(server_uuid)
    }
    pub fn archive(&self, server_uuid: &str, backup_uuid: &str) -> PathBuf {
        self.server_dir(server_uuid).join(format!("{backup_uuid}.tar.gz"))
    }
    pub fn remove_server(&self, server_uuid: &str) {
        let _ = std::fs::remove_dir_all(self.server_dir(server_uuid));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_verify_and_restore() {
        let dir = tempfile::tempdir().unwrap();
        let volume = dir.path().join("volume");
        std::fs::create_dir_all(volume.join("world")).unwrap();
        std::fs::write(volume.join("server.properties"), b"motd=hello").unwrap();
        std::fs::write(volume.join("world/level.dat"), b"\x01\x02\x03").unwrap();

        let archive = dir.path().join("backup.tar.gz");
        let (size, checksum) = create_archive(&volume, &archive).unwrap();
        assert!(size > 0);
        verify_checksum(&archive, &checksum).unwrap();

        let restored = dir.path().join("restored");
        restore_archive(&archive, &restored, 10 * 1024 * 1024).unwrap();
        assert_eq!(
            std::fs::read(restored.join("server.properties")).unwrap(),
            b"motd=hello"
        );
        assert_eq!(
            std::fs::read(restored.join("world/level.dat")).unwrap(),
            b"\x01\x02\x03"
        );
    }

    #[test]
    fn checksum_mismatch_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let volume = dir.path().join("volume");
        std::fs::create_dir_all(&volume).unwrap();
        std::fs::write(volume.join("a.txt"), b"a").unwrap();
        let archive = dir.path().join("b.tar.gz");
        create_archive(&volume, &archive).unwrap();
        assert!(verify_checksum(&archive, "deadbeef").is_err());
    }

    #[test]
    fn rejects_symlink_entries() {
        let dir = tempfile::tempdir().unwrap();
        let volume = dir.path().join("volume");
        std::fs::create_dir_all(&volume).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc/passwd", volume.join("evil")).unwrap();
            let archive = dir.path().join("b.tar.gz");
            create_archive(&volume, &archive).unwrap();
            let restored = dir.path().join("restored");
            restore_archive(&archive, &restored, 1024 * 1024).unwrap();
            assert!(!restored.join("evil").exists());
        }
    }
}
