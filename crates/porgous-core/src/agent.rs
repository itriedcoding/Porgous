//! Node agent API. Every request except `/v1/health` must carry a valid bearer
//! token and an HMAC signature with a fresh timestamp.

use crate::backup::{self, BackupPaths};
use crate::config::Config;
use crate::db::now_ms;
use crate::error::{ApiError, ApiResult};
use crate::files::LocalFiles;
use crate::nodes::{SIG_HEADER, TOKEN_HEADER, TS_HEADER};
use crate::runtime::{ContainerRuntime, ContainerSpec, InstallSpec, LocalRuntime};
use crate::state::{node_token_hash, VERSION};
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use tokio_util::io::ReaderStream;

#[derive(Clone)]
pub struct AgentState {
    pub config: Arc<Config>,
    pub runtime: Arc<LocalRuntime>,
    pub volumes: PathBuf,
    pub backups: PathBuf,
    pub token_hash: String,
    pub secret: Vec<u8>,
}

impl AgentState {
    fn files(&self, volume: &str) -> ApiResult<LocalFiles> {
        validate_volume(volume)?;
        LocalFiles::new(self.volumes.join(volume)).map_err(ApiError::internal)
    }
    fn backup_path(&self, volume: &str, backup_uuid: &str) -> ApiResult<PathBuf> {
        validate_volume(volume)?;
        validate_volume(backup_uuid)?;
        Ok(BackupPaths::new(self.backups.clone()).archive(volume, backup_uuid))
    }
}

fn validate_volume(volume: &str) -> ApiResult<()> {
    if volume.is_empty()
        || volume.len() > 64
        || !volume
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(ApiError::bad_request("Invalid volume id"));
    }
    Ok(())
}

pub fn router(state: AgentState) -> Router {
    let protected = Router::new()
        .route("/containers", post(create_container))
        .route("/containers/{id}/start", post(start))
        .route("/containers/{id}/stop", post(stop))
        .route("/containers/{id}/kill", post(kill))
        .route("/containers/{id}/remove", post(remove))
        .route("/containers/{id}/inspect", get(inspect))
        .route("/containers/{id}/stats", get(stats))
        .route("/containers/{id}/logs", get(logs))
        .route("/containers/{id}/command", post(command))
        .route("/containers/{id}/exec", post(exec))
        .route("/install", post(install))
        .route("/images", post(ensure_image))
        .route("/files", get(list_files))
        .route("/files/download", get(download_file))
        .route("/files/write", axum::routing::put(write_file))
        .route("/files/mkdir", post(mkdir))
        .route("/files/delete", post(delete_path))
        .route("/files/rename", post(rename))
        .route("/files/chmod", post(chmod))
        .route("/files/usage", get(usage))
        .route("/backups", post(create_backup))
        .route("/backups/restore", post(restore_backup))
        .route("/backups/delete", post(delete_backup))
        .route("/backups/download", get(download_backup))
        .route("/volumes/delete", post(delete_volume))
        .route_layer(middleware::from_fn_with_state(state.clone(), authenticate))
        .with_state(state.clone());
    Router::new()
        .route("/v1/health", get(health))
        .nest("/v1", protected)
        .with_state(state)
}

async fn health(State(state): State<AgentState>) -> Json<Value> {
    let docker = state.runtime.ping().await.is_ok();
    Json(json!({
        "ok": true,
        "version": VERSION,
        "docker": docker,
        "docker_detail": state.runtime.description(),
        "time": now_ms(),
    }))
}

fn unauthorized(message: &str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "error": { "code": "unauthorized", "message": message } })),
    )
        .into_response()
}

async fn authenticate(State(state): State<AgentState>, req: Request, next: Next) -> Response {
    let (parts, body) = req.into_parts();
    let body_bytes = match axum::body::to_bytes(body, state.config.max_upload_bytes as usize).await {
        Ok(bytes) => bytes,
        Err(_) => return (StatusCode::PAYLOAD_TOO_LARGE, "request body too large").into_response(),
    };
    let token = parts
        .headers
        .get(TOKEN_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if node_token_hash(token) != state.token_hash {
        return unauthorized("invalid node token");
    }
    let timestamp: i64 = parts
        .headers
        .get(TS_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let signature = parts
        .headers
        .get(SIG_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let path = parts
        .uri
        .path_and_query()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| parts.uri.path().to_string());
    if let Err(err) = crate::nodes::verify_signature(
        &state.secret,
        parts.method.as_str(),
        &path,
        timestamp,
        signature,
        &body_bytes,
        chrono::Utc::now().timestamp(),
    ) {
        tracing::warn!(error = %err, path, "rejected node request");
        return unauthorized("invalid request signature");
    }
    let req = Request::from_parts(parts, Body::from(body_bytes));
    next.run(req).await
}

async fn create_container(
    State(state): State<AgentState>,
    Json(spec): Json<ContainerSpec>,
) -> ApiResult<Json<Value>> {
    let id = state.runtime.create(&spec).await.map_err(ApiError::internal)?;
    Ok(Json(json!({ "id": id })))
}

#[derive(Deserialize, Default)]
struct TimeoutRequest {
    #[serde(default)]
    timeout: Option<u64>,
}

async fn start(State(state): State<AgentState>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    state.runtime.start(&id).await.map_err(ApiError::internal)?;
    Ok(Json(json!({ "ok": true })))
}

async fn stop(
    State(state): State<AgentState>,
    Path(id): Path<String>,
    Json(req): Json<TimeoutRequest>,
) -> ApiResult<Json<Value>> {
    state
        .runtime
        .stop(&id, req.timeout.unwrap_or(30))
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(json!({ "ok": true })))
}

async fn kill(State(state): State<AgentState>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    state.runtime.kill(&id).await.map_err(ApiError::internal)?;
    Ok(Json(json!({ "ok": true })))
}

async fn remove(State(state): State<AgentState>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    state.runtime.remove(&id).await.map_err(ApiError::internal)?;
    Ok(Json(json!({ "ok": true })))
}

async fn inspect(State(state): State<AgentState>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let stats = state.runtime.inspect(&id).await.map_err(ApiError::internal)?;
    Ok(Json(serde_json::to_value(stats).unwrap_or_default()))
}

async fn stats(State(state): State<AgentState>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let stats = state.runtime.stats(&id).await.map_err(ApiError::internal)?;
    Ok(Json(serde_json::to_value(stats).unwrap_or_default()))
}

#[derive(Deserialize)]
struct LogQuery {
    #[serde(default)]
    tail: Option<u32>,
    #[serde(default)]
    since: Option<i64>,
}

async fn logs(
    State(state): State<AgentState>,
    Path(id): Path<String>,
    Query(query): Query<LogQuery>,
) -> ApiResult<Json<Value>> {
    let tail = query.tail.unwrap_or(200).clamp(1, 5000);
    if let Some(since) = query.since {
        let lines = state
            .runtime
            .logs_since(&id, since, tail)
            .await
            .map_err(ApiError::internal)?;
        Ok(Json(json!({ "lines": lines })))
    } else {
        let text = state.runtime.logs(&id, tail).await.map_err(ApiError::internal)?;
        Ok(Json(
            json!({ "logs": text, "lines": text.lines().collect::<Vec<_>>() }),
        ))
    }
}

#[derive(Deserialize)]
struct CommandRequest {
    command: String,
}

async fn command(
    State(state): State<AgentState>,
    Path(id): Path<String>,
    Json(req): Json<CommandRequest>,
) -> ApiResult<Json<Value>> {
    if req.command.len() > 4096 {
        return Err(ApiError::bad_request("Command is too long"));
    }
    state
        .runtime
        .command(&id, &req.command)
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct ExecRequest {
    cmd: Vec<String>,
}

async fn exec(
    State(state): State<AgentState>,
    Path(id): Path<String>,
    Json(req): Json<ExecRequest>,
) -> ApiResult<Json<Value>> {
    let result = state
        .runtime
        .exec(&id, req.cmd)
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(serde_json::to_value(result).unwrap_or_default()))
}

async fn install(State(state): State<AgentState>, Json(spec): Json<InstallSpec>) -> ApiResult<Json<Value>> {
    let log = state
        .runtime
        .run_install(&spec)
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(json!({ "log": log })))
}

#[derive(Deserialize)]
struct ImageRequest {
    image: String,
}

async fn ensure_image(
    State(state): State<AgentState>,
    Json(req): Json<ImageRequest>,
) -> ApiResult<Json<Value>> {
    state
        .runtime
        .ensure_image(&req.image)
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct FileQuery {
    volume: String,
    #[serde(default)]
    path: Option<String>,
}

async fn list_files(
    State(state): State<AgentState>,
    Query(query): Query<FileQuery>,
) -> ApiResult<Json<Value>> {
    let files = state.files(&query.volume)?;
    let path = query.path.unwrap_or_default();
    let entries = tokio::task::spawn_blocking(move || files.list(&path))
        .await
        .map_err(ApiError::internal)?
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    Ok(Json(json!({ "entries": entries })))
}

async fn download_file(
    State(state): State<AgentState>,
    Query(query): Query<FileQuery>,
) -> ApiResult<Response> {
    let files = state.files(&query.volume)?;
    let path = query
        .path
        .ok_or_else(|| ApiError::bad_request("Path is required"))?;
    let (file, size) = tokio::task::spawn_blocking(move || files.open_for_read(&path))
        .await
        .map_err(ApiError::internal)?
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    let reader = tokio::fs::File::from_std(file);
    let mut response = Response::new(Body::from_stream(ReaderStream::new(reader)));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&size.to_string()).unwrap(),
    );
    Ok(response)
}

async fn write_file(
    State(state): State<AgentState>,
    Query(query): Query<FileQuery>,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let files = state.files(&query.volume)?;
    let path = query
        .path
        .ok_or_else(|| ApiError::bad_request("Path is required"))?;
    tokio::task::spawn_blocking(move || files.write(&path, &body))
        .await
        .map_err(ApiError::internal)?
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct MkdirRequest {
    volume: String,
    path: String,
}

async fn mkdir(State(state): State<AgentState>, Json(req): Json<MkdirRequest>) -> ApiResult<Json<Value>> {
    let files = state.files(&req.volume)?;
    tokio::task::spawn_blocking(move || files.mkdir(&req.path))
        .await
        .map_err(ApiError::internal)?
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    Ok(Json(json!({ "ok": true })))
}

async fn delete_path(
    State(state): State<AgentState>,
    Json(req): Json<MkdirRequest>,
) -> ApiResult<Json<Value>> {
    let files = state.files(&req.volume)?;
    tokio::task::spawn_blocking(move || files.delete(&req.path))
        .await
        .map_err(ApiError::internal)?
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct RenameRequest {
    volume: String,
    from: String,
    to: String,
}

async fn rename(State(state): State<AgentState>, Json(req): Json<RenameRequest>) -> ApiResult<Json<Value>> {
    let files = state.files(&req.volume)?;
    tokio::task::spawn_blocking(move || files.rename(&req.from, &req.to))
        .await
        .map_err(ApiError::internal)?
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct ChmodRequest {
    volume: String,
    path: String,
    mode: u32,
}

async fn chmod(State(state): State<AgentState>, Json(req): Json<ChmodRequest>) -> ApiResult<Json<Value>> {
    let files = state.files(&req.volume)?;
    tokio::task::spawn_blocking(move || files.chmod(&req.path, req.mode))
        .await
        .map_err(ApiError::internal)?
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct VolumeQuery {
    volume: String,
}

async fn usage(State(state): State<AgentState>, Query(query): Query<VolumeQuery>) -> ApiResult<Json<Value>> {
    let files = state.files(&query.volume)?;
    let bytes = tokio::task::spawn_blocking(move || files.disk_usage())
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(json!({ "bytes": bytes })))
}

#[derive(Deserialize)]
struct CreateBackupRequest {
    volume: String,
    backup_uuid: String,
}

async fn create_backup(
    State(state): State<AgentState>,
    Json(req): Json<CreateBackupRequest>,
) -> ApiResult<Json<Value>> {
    validate_volume(&req.backup_uuid)?;
    let volume = state.volumes.join(&req.volume);
    validate_volume(&req.volume)?;
    let destination = state.backup_path(&req.volume, &req.backup_uuid)?;
    let (bytes, checksum) =
        tokio::task::spawn_blocking(move || backup::create_archive(&volume, &destination))
            .await
            .map_err(ApiError::internal)?
            .map_err(ApiError::internal)?;
    Ok(Json(json!({ "bytes": bytes, "checksum": checksum })))
}

#[derive(Deserialize)]
struct RestoreBackupRequest {
    volume: String,
    backup_uuid: String,
    #[serde(default)]
    checksum: Option<String>,
}

async fn restore_backup(
    State(state): State<AgentState>,
    Json(req): Json<RestoreBackupRequest>,
) -> ApiResult<Json<Value>> {
    let archive = state.backup_path(&req.volume, &req.backup_uuid)?;
    let volume = state.volumes.join(&req.volume);
    if let Some(checksum) = &req.checksum {
        backup::verify_checksum(&archive, checksum).map_err(|e| ApiError::bad_request(e.to_string()))?;
    }
    tokio::task::spawn_blocking(move || backup::restore_archive(&archive, &volume, 50 * 1024 * 1024 * 1024))
        .await
        .map_err(ApiError::internal)?
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    Ok(Json(json!({ "ok": true })))
}

async fn delete_backup(
    State(state): State<AgentState>,
    Json(req): Json<CreateBackupRequest>,
) -> ApiResult<Json<Value>> {
    let archive = state.backup_path(&req.volume, &req.backup_uuid)?;
    let _ = std::fs::remove_file(archive);
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct BackupDownloadQuery {
    volume: String,
    backup_uuid: String,
}

async fn download_backup(
    State(state): State<AgentState>,
    Query(query): Query<BackupDownloadQuery>,
) -> ApiResult<Response> {
    let archive = state.backup_path(&query.volume, &query.backup_uuid)?;
    if !archive.exists() {
        return Err(ApiError::not_found("Backup not found"));
    }
    let file = tokio::fs::File::open(&archive)
        .await
        .map_err(ApiError::internal)?;
    let size = file.metadata().await.map(|m| m.len()).unwrap_or(0);
    let mut response = Response::new(Body::from_stream(ReaderStream::new(file)));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("application/gzip"));
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&size.to_string()).unwrap(),
    );
    Ok(response)
}

async fn delete_volume(
    State(state): State<AgentState>,
    Json(req): Json<VolumeQuery>,
) -> ApiResult<Json<Value>> {
    validate_volume(&req.volume)?;
    let _ = std::fs::remove_dir_all(state.volumes.join(&req.volume));
    let _ = std::fs::remove_dir_all(state.backups.join(&req.volume));
    Ok(Json(json!({ "ok": true })))
}

/// Headers helper used by tests and diagnostics.
pub fn signature_headers(token: &str, secret: &[u8], method: &str, path: &str, body: &[u8]) -> HeaderMap {
    let mut headers = HeaderMap::new();
    let timestamp = chrono::Utc::now().timestamp();
    headers.insert(TOKEN_HEADER, HeaderValue::from_str(token).unwrap());
    headers.insert(TS_HEADER, HeaderValue::from_str(&timestamp.to_string()).unwrap());
    headers.insert(
        SIG_HEADER,
        HeaderValue::from_str(&crate::nodes::sign(secret, method, path, timestamp, body)).unwrap(),
    );
    headers
}
