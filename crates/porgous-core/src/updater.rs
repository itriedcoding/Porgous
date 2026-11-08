//! GitHub release updater with Ed25519 signature and SHA-256 verification.
//!
//! Nothing is installed unless the checksums file carries a valid signature
//! from the configured public key and the downloaded asset matches its checksum.

use crate::db::now_ms;
use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, VERSION};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseInfo {
    pub version: String,
    pub tag: String,
    pub name: String,
    pub notes: String,
    pub published_at: String,
    pub prerelease: bool,
    pub asset_name: Option<String>,
    pub asset_url: Option<String>,
    pub sums_url: Option<String>,
    pub signature_url: Option<String>,
    pub update_available: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateStatus {
    pub current_version: String,
    pub channel: String,
    pub latest: Option<ReleaseInfo>,
    pub update_available: bool,
    pub auto_update: bool,
    pub signing_configured: bool,
    pub last_check_error: Option<String>,
}

pub fn target_triple() -> String {
    let arch = std::env::consts::ARCH;
    format!("{arch}-{}", std::env::consts::OS)
}

/// Asset name convention produced by the release workflow.
pub fn asset_name(prefix: &str) -> String {
    let mut name = format!("{prefix}-{}", target_triple());
    if cfg!(windows) {
        name.push_str(".exe");
    }
    name
}

fn semver_of(tag: &str) -> Option<semver::Version> {
    let trimmed = tag.trim().trim_start_matches('v');
    semver::Version::parse(trimmed).ok()
}

pub async fn fetch_releases(state: &AppState, channel: &str) -> Result<Vec<ReleaseInfo>> {
    let url = format!(
        "https://api.github.com/repos/{}/releases?per_page=30",
        state.config.updates.repo
    );
    let response = state
        .http
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .context("contacting GitHub releases")?;
    if !response.status().is_success() {
        bail!("GitHub API returned {}", response.status());
    }
    let releases: Vec<Value> = response.json().await.context("parsing GitHub releases")?;
    let want_asset = asset_name(&state.config.updates.asset_prefix);
    let mut out = Vec::new();
    for release in releases {
        if release["draft"].as_bool().unwrap_or(false) {
            continue;
        }
        let prerelease = release["prerelease"].as_bool().unwrap_or(false);
        if channel == "stable" && prerelease {
            continue;
        }
        let tag = release["tag_name"].as_str().unwrap_or_default().to_string();
        let Some(version) = semver_of(&tag) else {
            continue;
        };
        let assets = release["assets"].as_array().cloned().unwrap_or_default();
        let find = |name: &str| {
            assets
                .iter()
                .find(|a| a["name"].as_str() == Some(name))
                .and_then(|a| a["browser_download_url"].as_str())
                .map(|s| s.to_string())
        };
        let asset_url = find(&want_asset);
        let sums_url = find("SHA256SUMS");
        let signature_url = find("SHA256SUMS.sig");
        out.push(ReleaseInfo {
            update_available: version
                > semver::Version::parse(VERSION).unwrap_or_else(|_| semver::Version::new(0, 0, 0)),
            version: version.to_string(),
            tag,
            name: release["name"].as_str().unwrap_or_default().to_string(),
            notes: release["body"]
                .as_str()
                .unwrap_or_default()
                .chars()
                .take(20_000)
                .collect(),
            published_at: release["published_at"].as_str().unwrap_or_default().to_string(),
            prerelease,
            asset_name: asset_url.as_ref().map(|_| want_asset.clone()),
            asset_url,
            sums_url,
            signature_url,
        });
    }
    out.sort_by(|a, b| {
        let av = semver::Version::parse(&a.version).unwrap_or_else(|_| semver::Version::new(0, 0, 0));
        let bv = semver::Version::parse(&b.version).unwrap_or_else(|_| semver::Version::new(0, 0, 0));
        bv.cmp(&av)
    });
    Ok(out)
}

pub async fn latest_release(state: &AppState, channel: &str) -> Result<Option<ReleaseInfo>> {
    Ok(fetch_releases(state, channel).await?.into_iter().next())
}

pub async fn status(state: &AppState) -> UpdateStatus {
    let channel = state
        .db
        .setting("update_channel")
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| state.config.updates.channel.clone());
    let auto_update = state
        .db
        .setting_bool("auto_update", state.config.updates.auto_apply)
        .await;
    match latest_release(state, &channel).await {
        Ok(latest) => UpdateStatus {
            current_version: VERSION.to_string(),
            channel,
            update_available: latest.as_ref().map(|l| l.update_available).unwrap_or(false),
            latest,
            auto_update,
            signing_configured: !state.config.updates.public_key.trim().is_empty(),
            last_check_error: None,
        },
        Err(err) => UpdateStatus {
            current_version: VERSION.to_string(),
            channel,
            latest: None,
            update_available: false,
            auto_update,
            signing_configured: !state.config.updates.public_key.trim().is_empty(),
            last_check_error: Some(err.to_string()),
        },
    }
}

async fn download(_state: &AppState, url: &str, limit: usize) -> Result<Vec<u8>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    let response = client.get(url).send().await.context("downloading update asset")?;
    if !response.status().is_success() {
        bail!("download failed with {}", response.status());
    }
    if let Some(len) = response.content_length() {
        if len as usize > limit {
            bail!("download exceeds the allowed size");
        }
    }
    let bytes = response.bytes().await.context("reading update asset")?;
    if bytes.len() > limit {
        bail!("download exceeds the allowed size");
    }
    Ok(bytes.to_vec())
}

fn parse_checksums(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let hash = parts.next()?;
            let name = parts.next()?.trim_start_matches('*');
            if hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
                Some((name.to_string(), hash.to_lowercase()))
            } else {
                None
            }
        })
        .collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    data_encoding::HEXLOWER.encode(&Sha256::digest(bytes))
}

pub fn updates_dir(state: &AppState) -> PathBuf {
    state.config.updates_dir()
}

/// Download, authenticate and install the newest release for this platform.
pub async fn apply_latest(state: &AppState, restart: bool) -> ApiResult<Value> {
    if !state.config.updates.enabled {
        return Err(ApiError::bad_request("Updates are disabled in the configuration"));
    }
    if state.config.updates.public_key.trim().is_empty() {
        return Err(ApiError::bad_request(
            "No signing public key is configured; refusing to install an unverified update",
        ));
    }
    let channel = state
        .db
        .setting("update_channel")
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| state.config.updates.channel.clone());
    let latest = latest_release(state, &channel)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("No releases found"))?;
    if !latest.update_available {
        return Ok(
            serde_json::json!({ "updated": false, "message": "Already up to date", "version": VERSION }),
        );
    }
    let (Some(asset_url), Some(sums_url), Some(sig_url)) = (
        latest.asset_url.clone(),
        latest.sums_url.clone(),
        latest.signature_url.clone(),
    ) else {
        return Err(ApiError::unavailable(
            "The release is missing the updater asset or signature files",
        ));
    };
    let asset_name_expected = asset_name(&state.config.updates.asset_prefix);
    let sums = download(state, &sums_url, 1024 * 1024)
        .await
        .map_err(ApiError::internal)?;
    let signature = download(state, &sig_url, 16 * 1024)
        .await
        .map_err(ApiError::internal)?;
    let signature_bytes = crate::crypto::decode_signature(&signature)
        .ok_or_else(|| ApiError::internal("release signature is malformed"))?;
    if !crate::crypto::ed25519_verify(&state.config.updates.public_key, &sums, &signature_bytes) {
        return Err(ApiError::forbidden(
            "Release signature verification failed; update aborted",
        ));
    }
    let checksums = parse_checksums(&String::from_utf8_lossy(&sums));
    let (_, expected) = checksums
        .iter()
        .find(|(name, _)| name == &asset_name_expected)
        .ok_or_else(|| ApiError::unavailable(format!("No checksum published for {asset_name_expected}")))?;
    let asset = download(state, &asset_url, 200 * 1024 * 1024)
        .await
        .map_err(ApiError::internal)?;
    let actual = sha256_hex(&asset);
    if !crate::crypto::constant_time_eq(actual.as_bytes(), expected.as_bytes()) {
        return Err(ApiError::forbidden(
            "Update checksum verification failed; update aborted",
        ));
    }
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query(&state.db.sql(
        "INSERT INTO updates (id, version, channel, status, log, created_at) VALUES (?, ?, ?, 'installing', ?, ?)",
    ))
    .bind(&id)
    .bind(&latest.version)
    .bind(&channel)
    .bind(format!("verified asset {asset_name_expected} ({actual})"))
    .bind(now_ms())
    .execute(&state.db.pool)
    .await
    .ok();

    let install = install_binary(&asset, &latest.version, restart).await;
    match &install {
        Ok(log) => {
            let _ = sqlx::query(
                &state
                    .db
                    .sql("UPDATE updates SET status = 'installed', log = ?, completed_at = ? WHERE id = ?"),
            )
            .bind(log)
            .bind(now_ms())
            .bind(&id)
            .execute(&state.db.pool)
            .await;
            crate::audit::record(
                &state.db,
                None,
                "system",
                "update.install",
                &latest.version,
                None,
                serde_json::json!({ "restart": restart }),
            )
            .await;
            Ok(serde_json::json!({
                "updated": true,
                "version": latest.version,
                "restart_required": true,
                "message": "Update verified and installed. Restart the panel to finish."
            }))
        }
        Err(err) => {
            let _ = sqlx::query(
                &state
                    .db
                    .sql("UPDATE updates SET status = 'failed', log = ?, completed_at = ? WHERE id = ?"),
            )
            .bind(err.to_string())
            .bind(now_ms())
            .bind(&id)
            .execute(&state.db.pool)
            .await;
            Err(ApiError::internal(err))
        }
    }
}

async fn install_binary(asset: &[u8], version: &str, restart: bool) -> Result<String> {
    let current = std::env::current_exe().context("locating the running executable")?;
    let dir = current
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("porgous-updates");
    std::fs::create_dir_all(&dir)?;
    let new_path = dir.join(format!("porgous-{version}.new{}", std::env::consts::EXE_SUFFIX));
    {
        use std::io::Write;
        let mut file = std::fs::File::create(&new_path)?;
        file.write_all(asset)?;
        file.sync_all()?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&new_path, std::fs::Permissions::from_mode(0o755))?;
    }
    // Smoke test the new binary before swapping it in.
    let output = tokio::process::Command::new(&new_path)
        .arg("--version")
        .output()
        .await
        .context("running the downloaded binary")?;
    if !output.status.success() {
        let _ = std::fs::remove_file(&new_path);
        bail!("the downloaded binary failed its version check");
    }
    let backup = dir.join(format!("porgous-{VERSION}.bak{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(&current, &backup).context("backing up the current binary")?;
    #[cfg(windows)]
    {
        // Windows permits renaming a running executable, not overwriting it.
        let old = dir.join(format!("porgous-old-{}.exe", crate::crypto::random_token(4)));
        std::fs::rename(&current, &old)?;
        if let Err(err) = std::fs::rename(&new_path, &current) {
            let _ = std::fs::rename(&old, &current);
            return Err(err.into());
        }
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(&new_path, &current).context("replacing the current binary")?;
    }
    let log = format!(
        "installed {version} over {VERSION}; previous binary backed up at {}",
        backup.display()
    );
    if restart {
        let command = restart_command();
        if !command.is_empty() {
            let mut parts = command.split_whitespace();
            if let Some(bin) = parts.next() {
                let _ = tokio::process::Command::new(bin).args(parts).output().await;
                return Ok(format!("{log}; restart command executed"));
            }
        }
    }
    Ok(log)
}

fn restart_command() -> String {
    std::env::var("PORGOUS_RESTART_COMMAND").unwrap_or_default()
}

/// Restore the newest backup created by an update.
pub async fn rollback(state: &AppState) -> ApiResult<Value> {
    let current = std::env::current_exe().map_err(ApiError::internal)?;
    let dir = current
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("porgous-updates");
    let mut backups: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map_err(ApiError::internal)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension().and_then(|e| e.to_str()) == Some("bak") || p.to_string_lossy().ends_with(".bak.exe")
        })
        .collect();
    backups.sort();
    let Some(last) = backups.pop() else {
        return Err(ApiError::not_found(
            "No update backup is available to roll back to",
        ));
    };
    std::fs::copy(&last, &current).map_err(ApiError::internal)?;
    let name = last
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    crate::audit::record(
        &state.db,
        None,
        "system",
        "update.rollback",
        &name,
        None,
        serde_json::json!({}),
    )
    .await;
    Ok(serde_json::json!({
        "rolled_back": true,
        "from": name,
        "message": "Previous binary restored. Restart the panel to finish."
    }))
}

/// Read an SHA256SUMS file for local verification.
pub fn verify_local_file(path: &Path, expected: &str) -> Result<()> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let actual = data_encoding::HEXLOWER.encode(&hasher.finalize());
    if !crate::crypto::constant_time_eq(actual.as_bytes(), expected.as_bytes()) {
        bail!("checksum mismatch");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_checksums() {
        let text = "abc  porgous-x86_64-linux\n".to_string() + &"0".repeat(64) + " *porgous-aarch64-linux\n";
        let parsed = parse_checksums(&text);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].0, "porgous-aarch64-linux");
    }

    #[test]
    fn semver_ordering() {
        assert!(semver_of("v0.2.0").unwrap() > semver_of("0.1.9").unwrap());
        assert!(semver_of("not-a-version").is_none());
    }

    #[test]
    fn asset_name_matches_current_platform() {
        let name = asset_name("porgous");
        assert!(name.contains(&target_triple()));
    }
}
