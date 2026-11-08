//! Server-scoped API endpoints.

use crate::auth::{authorize_server, AuthUser};
use crate::db::{get_i64, get_str, now_ms};
use crate::error::{ApiError, ApiResult};
use crate::files_service::{safe_path, Download, FileService};
use crate::models::{
    AuditLog, Backup, Schedule, ScheduleTask, Server, Subuser, ALL_SERVER_PERMISSIONS, STATUS_RUNNING,
};
use crate::scheduler::CronSpec;
use crate::server_ops::{self, CreateServerRequest};
use crate::state::AppState;
use axum::body::{Body, Bytes};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::any::AnyRow;
use std::collections::HashMap;
use std::time::Duration;
use tokio_util::io::ReaderStream;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/servers", get(list_servers).post(create_server))
        .route(
            "/servers/{id}",
            get(server_detail).patch(update_server).delete(delete_server),
        )
        .route("/servers/{id}/power", post(power))
        .route("/servers/{id}/command", post(send_command))
        .route("/servers/{id}/logs", get(console_logs))
        .route("/servers/{id}/metrics", get(metrics))
        .route("/servers/{id}/ws", get(console_ws))
        .route("/servers/{id}/files", get(files_list).delete(files_delete))
        .route("/servers/{id}/files/content", get(files_read).put(files_write))
        .route("/servers/{id}/files/upload", post(files_upload))
        .route("/servers/{id}/files/download", get(files_download))
        .route("/servers/{id}/files/mkdir", post(files_mkdir))
        .route("/servers/{id}/files/rename", post(files_rename))
        .route("/servers/{id}/files/chmod", post(files_chmod))
        .route(
            "/servers/{id}/allocations",
            get(server_allocations).post(attach_allocation),
        )
        .route(
            "/servers/{id}/allocations/{allocation_id}",
            delete(detach_allocation),
        )
        .route("/servers/{id}/backups", get(list_backups).post(create_backup))
        .route("/servers/{id}/backups/{backup_id}", delete(delete_backup))
        .route("/servers/{id}/backups/{backup_id}/restore", post(restore_backup))
        .route("/servers/{id}/backups/{backup_id}/download", get(download_backup))
        .route(
            "/servers/{id}/schedules",
            get(list_schedules).post(create_schedule),
        )
        .route(
            "/servers/{id}/schedules/{schedule_id}",
            patch(update_schedule).delete(delete_schedule),
        )
        .route("/servers/{id}/schedules/{schedule_id}/run", post(run_schedule))
        .route("/servers/{id}/subusers", get(list_subusers).post(create_subuser))
        .route(
            "/servers/{id}/subusers/{sub_id}",
            patch(update_subuser).delete(delete_subuser),
        )
}

async fn list_servers(State(state): State<AppState>, auth: AuthUser) -> ApiResult<Json<Value>> {
    auth.require_scope("server:read")?;
    let rows = if auth.is_admin() {
        sqlx::query(&state.db.sql("SELECT * FROM servers ORDER BY created_at DESC"))
            .fetch_all(&state.db.pool)
            .await?
    } else {
        sqlx::query(&state.db.sql(
            "SELECT * FROM servers WHERE owner_id = ? OR id IN (SELECT server_id FROM subusers WHERE user_id = ?) ORDER BY created_at DESC",
        ))
        .bind(&auth.user.id)
        .bind(&auth.user.id)
        .fetch_all(&state.db.pool)
        .await?
    };
    let mut servers = Vec::new();
    for row in rows {
        let server = Server::from_row(&row);
        servers.push(server_ops::server_summary(&state, &server).await);
    }
    Ok(Json(json!({ "servers": servers })))
}

async fn create_server(
    State(state): State<AppState>,
    auth: AuthUser,
    info: axum::extract::ConnectInfo<std::net::SocketAddr>,
    Json(req): Json<CreateServerRequest>,
) -> ApiResult<Json<Value>> {
    auth.require_scope("server:admin")?;
    let allowed = state
        .db
        .setting_bool("allow_user_servers", state.config.allow_user_servers)
        .await;
    if !allowed && !auth.is_admin() {
        return Err(ApiError::forbidden(
            "Server creation is restricted to administrators",
        ));
    }
    let ip = Some(info.0.ip().to_string());
    let server = server_ops::create_server(&state, &auth.user, req, ip.as_deref()).await?;
    let summary = server_ops::server_summary(&state, &server).await;
    Ok(Json(json!({ "server": server, "summary": summary })))
}

async fn server_detail(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let access = authorize_server(&state, &auth, &id, "settings.read").await?;
    let server = access.server;
    let mut summary = server_ops::server_summary(&state, &server).await;
    summary["permissions"] = json!(access.permissions);
    summary["is_owner"] = json!(access.is_owner);
    summary["is_admin"] = json!(access.is_admin);
    summary["environment"] = json!(server_ops::environment(&state, &server).await?);
    summary["variables"] = json!(server_ops::variables(&state, &server.id).await?);
    summary["install_log"] = json!(server.install_log);
    Ok(Json(summary))
}

#[derive(Deserialize)]
struct UpdateServerRequest {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    memory_mb: Option<i64>,
    #[serde(default)]
    disk_mb: Option<i64>,
    #[serde(default)]
    cpu_percent: Option<i64>,
    #[serde(default)]
    swap_mb: Option<i64>,
    #[serde(default)]
    pids_limit: Option<i64>,
    #[serde(default)]
    io_weight: Option<i64>,
    #[serde(default)]
    image: Option<String>,
    #[serde(default)]
    startup: Option<String>,
    #[serde(default)]
    auto_restart: Option<bool>,
    #[serde(default)]
    restart_policy: Option<String>,
    #[serde(default)]
    variables: Option<HashMap<String, String>>,
}

async fn update_server(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
    Json(req): Json<UpdateServerRequest>,
) -> ApiResult<Json<Value>> {
    let access = authorize_server(&state, &auth, &id, "settings.write").await?;
    if !access.is_owner && !access.is_admin {
        return Err(ApiError::forbidden("Only the owner can change server settings"));
    }
    let server = access.server;
    if let Some(name) = &req.name {
        if name.trim().is_empty() || name.len() > 100 {
            return Err(ApiError::bad_request("Invalid server name"));
        }
        sqlx::query(&state.db.sql("UPDATE servers SET name = ? WHERE id = ?"))
            .bind(name.trim())
            .bind(&server.id)
            .execute(&state.db.pool)
            .await?;
    }
    if let Some(description) = &req.description {
        sqlx::query(&state.db.sql("UPDATE servers SET description = ? WHERE id = ?"))
            .bind(description.chars().take(500).collect::<String>())
            .bind(&server.id)
            .execute(&state.db.pool)
            .await?;
    }
    if let Some(value) = req.memory_mb {
        if !(128..=1_048_576).contains(&value) {
            return Err(ApiError::bad_request("Invalid memory limit"));
        }
        sqlx::query(&state.db.sql("UPDATE servers SET memory_mb = ? WHERE id = ?"))
            .bind(value)
            .bind(&server.id)
            .execute(&state.db.pool)
            .await?;
    }
    if let Some(value) = req.disk_mb {
        if !(256..=10_485_760).contains(&value) {
            return Err(ApiError::bad_request("Invalid disk limit"));
        }
        sqlx::query(&state.db.sql("UPDATE servers SET disk_mb = ? WHERE id = ?"))
            .bind(value)
            .bind(&server.id)
            .execute(&state.db.pool)
            .await?;
    }
    if let Some(value) = req.cpu_percent {
        if !(10..=64_000).contains(&value) {
            return Err(ApiError::bad_request("Invalid CPU limit"));
        }
        sqlx::query(&state.db.sql("UPDATE servers SET cpu_percent = ? WHERE id = ?"))
            .bind(value)
            .bind(&server.id)
            .execute(&state.db.pool)
            .await?;
    }
    if let Some(value) = req.swap_mb {
        sqlx::query(&state.db.sql("UPDATE servers SET swap_mb = ? WHERE id = ?"))
            .bind(value.max(0))
            .bind(&server.id)
            .execute(&state.db.pool)
            .await?;
    }
    if let Some(value) = req.pids_limit {
        sqlx::query(&state.db.sql("UPDATE servers SET pids_limit = ? WHERE id = ?"))
            .bind(value.clamp(32, 65535))
            .bind(&server.id)
            .execute(&state.db.pool)
            .await?;
    }
    if let Some(value) = req.io_weight {
        sqlx::query(&state.db.sql("UPDATE servers SET io_weight = ? WHERE id = ?"))
            .bind(value.clamp(10, 1000))
            .bind(&server.id)
            .execute(&state.db.pool)
            .await?;
    }
    if let Some(image) = &req.image {
        let egg = state
            .eggs
            .get(&server.egg_id)
            .ok_or_else(|| ApiError::internal("Server egg is missing"))?;
        if !egg.docker_images.iter().any(|i| i == image) {
            return Err(ApiError::bad_request("Image is not provided by this egg"));
        }
        sqlx::query(&state.db.sql("UPDATE servers SET image = ? WHERE id = ?"))
            .bind(image)
            .bind(&server.id)
            .execute(&state.db.pool)
            .await?;
    }
    if let Some(startup) = &req.startup {
        if startup.len() > 4000 {
            return Err(ApiError::bad_request("Startup command is too long"));
        }
        sqlx::query(&state.db.sql("UPDATE servers SET startup = ? WHERE id = ?"))
            .bind(startup)
            .bind(&server.id)
            .execute(&state.db.pool)
            .await?;
    }
    if let Some(value) = req.auto_restart {
        sqlx::query(&state.db.sql("UPDATE servers SET auto_restart = ? WHERE id = ?"))
            .bind(if value { 1i64 } else { 0i64 })
            .bind(&server.id)
            .execute(&state.db.pool)
            .await?;
    }
    if let Some(policy) = &req.restart_policy {
        if !server_ops::valid_restart_policy(policy) {
            return Err(ApiError::bad_request("Invalid restart policy"));
        }
        sqlx::query(&state.db.sql("UPDATE servers SET restart_policy = ? WHERE id = ?"))
            .bind(policy)
            .bind(&server.id)
            .execute(&state.db.pool)
            .await?;
    }
    if let Some(vars) = &req.variables {
        let egg = state
            .eggs
            .get(&server.egg_id)
            .ok_or_else(|| ApiError::internal("Server egg is missing"))?;
        for (key, value) in vars {
            let variable = egg
                .variables
                .iter()
                .find(|v| &v.key == key)
                .ok_or_else(|| ApiError::bad_request(format!("Unknown variable '{key}'")))?;
            if !variable.user_editable && !auth.is_admin() {
                return Err(ApiError::forbidden(format!("{} cannot be edited", variable.name)));
            }
            crate::eggs::validate_variable(variable, value)
                .map_err(|e| ApiError::bad_request(e.to_string()))?;
            let row: Option<AnyRow> = sqlx::query(
                &state
                    .db
                    .sql("SELECT id FROM server_variables WHERE server_id = ? AND key = ?"),
            )
            .bind(&server.id)
            .bind(key)
            .fetch_optional(&state.db.pool)
            .await?;
            match row {
                Some(row) => {
                    sqlx::query(&state.db.sql("UPDATE server_variables SET value = ? WHERE id = ?"))
                        .bind(value)
                        .bind(get_str(&row, "id"))
                        .execute(&state.db.pool)
                        .await?;
                }
                None => {
                    sqlx::query(&state.db.sql(
                        "INSERT INTO server_variables (id, server_id, key, value, created_at) VALUES (?, ?, ?, ?, ?)",
                    ))
                    .bind(uuid::Uuid::new_v4().to_string())
                    .bind(&server.id)
                    .bind(key)
                    .bind(value)
                    .bind(now_ms())
                    .execute(&state.db.pool)
                    .await?;
                }
            }
        }
        let mut env = server.env_map();
        for (key, value) in vars {
            env.insert(key.clone(), value.clone());
        }
        sqlx::query(&state.db.sql("UPDATE servers SET env = ? WHERE id = ?"))
            .bind(serde_json::to_string(&env).unwrap_or_else(|_| "{}".into()))
            .bind(&server.id)
            .execute(&state.db.pool)
            .await?;
    }
    sqlx::query(&state.db.sql("UPDATE servers SET updated_at = ? WHERE id = ?"))
        .bind(now_ms())
        .bind(&server.id)
        .execute(&state.db.pool)
        .await?;
    crate::audit::record(
        &state.db,
        Some(&auth.user.id),
        &auth.user.username,
        "server.update",
        &server.uuid,
        None,
        json!({}),
    )
    .await;
    let updated = server_ops::load_server(&state, &server.id).await?;
    Ok(Json(json!({ "server": updated })))
}

async fn delete_server(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let access = authorize_server(&state, &auth, &id, "settings.write").await?;
    if !access.is_owner && !access.is_admin {
        return Err(ApiError::forbidden("Only the owner can delete a server"));
    }
    server_ops::delete_server(&state, &access.server).await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct PowerRequest {
    action: String,
}

async fn power(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
    info: axum::extract::ConnectInfo<std::net::SocketAddr>,
    Json(req): Json<PowerRequest>,
) -> ApiResult<Json<Value>> {
    let (action, permission) = match req.action.as_str() {
        "start" => ("start", "control.start"),
        "stop" => ("stop", "control.stop"),
        "restart" => ("restart", "control.restart"),
        "kill" => ("kill", "control.kill"),
        _ => {
            return Err(ApiError::bad_request(
                "Action must be start, stop, restart or kill",
            ))
        }
    };
    let access = authorize_server(&state, &auth, &id, permission).await?;
    if access.server.suspended {
        return Err(ApiError::forbidden("Server is suspended"));
    }
    match action {
        "start" => server_ops::start_server(&state, &access.server).await?,
        "stop" => server_ops::stop_server(&state, &access.server).await?,
        "restart" => server_ops::restart_server(&state, &access.server).await?,
        "kill" => server_ops::kill_server(&state, &access.server).await?,
        _ => unreachable!(),
    }
    crate::audit::record(
        &state.db,
        Some(&auth.user.id),
        &auth.user.username,
        &format!("server.{action}"),
        &access.server.uuid,
        Some(info.0.ip().to_string()).as_deref(),
        json!({}),
    )
    .await;
    let server = server_ops::load_server(&state, &access.server.id).await?;
    Ok(Json(json!({ "ok": true, "status": server.status })))
}

#[derive(Deserialize)]
struct CommandRequest {
    command: String,
}

async fn send_command(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
    Json(req): Json<CommandRequest>,
) -> ApiResult<Json<Value>> {
    let access = authorize_server(&state, &auth, &id, "console.write").await?;
    if access.server.status != STATUS_RUNNING {
        return Err(ApiError::conflict("Server is not running"));
    }
    let Some(container) = &access.server.container_id else {
        return Err(ApiError::conflict("Server has no container"));
    };
    match state
        .console
        .send_command(&state.runtimes, &access.server, container, &req.command)
        .await
    {
        Ok(()) => {}
        Err(err) => {
            let runtime = state
                .runtimes
                .get(&access.server.node_id)
                .map_err(|e| ApiError::unavailable(e.to_string()))?;
            runtime
                .command(container, &req.command)
                .await
                .map_err(|e| ApiError::unavailable(format!("{err}: {e}")))?;
        }
    }
    Ok(Json(json!({ "ok": true })))
}

async fn console_logs(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let access = authorize_server(&state, &auth, &id, "console.read").await?;
    let tail = params
        .get("lines")
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(200)
        .clamp(1, 5000);
    let Some(container) = &access.server.container_id else {
        return Ok(Json(json!({ "logs": "" })));
    };
    let runtime = state
        .runtimes
        .get(&access.server.node_id)
        .map_err(|e| ApiError::unavailable(e.to_string()))?;
    let logs = runtime.logs(container, tail).await.unwrap_or_default();
    Ok(Json(json!({ "logs": logs })))
}

async fn metrics(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let access = authorize_server(&state, &auth, &id, "settings.read").await?;
    let minutes = params
        .get("minutes")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(30)
        .clamp(1, 1440);
    let since = now_ms() - minutes * 60_000;
    let rows = sqlx::query(
        &state
            .db
            .sql("SELECT * FROM metrics WHERE server_id = ? AND created_at >= ? ORDER BY created_at ASC"),
    )
    .bind(&access.server.id)
    .bind(since)
    .fetch_all(&state.db.pool)
    .await?;
    let points: Vec<_> = rows.iter().map(crate::models::Metric::from_row).collect();
    let live = if access.server.status == STATUS_RUNNING {
        if let (Some(container), Ok(runtime)) = (
            &access.server.container_id,
            state.runtimes.get(&access.server.node_id),
        ) {
            tokio::time::timeout(Duration::from_secs(5), runtime.stats(container))
                .await
                .ok()
                .and_then(|r| r.ok())
        } else {
            None
        }
    } else {
        None
    };
    Ok(Json(
        json!({ "metrics": points, "live": live, "minutes": minutes }),
    ))
}

async fn console_ws(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    let access = authorize_server(&state, &auth, &id, "console.read").await?;
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        if !origin_matches(&state, origin) {
            return Err(ApiError::forbidden("WebSocket origin is not allowed"));
        }
    }
    let can_write =
        access.is_owner || access.is_admin || access.permissions.iter().any(|p| p == "console.write");
    let server = access.server;
    state
        .console
        .ensure_attached(state.runtimes.clone(), server.clone());
    let state_clone = state.clone();
    Ok(ws
        .on_upgrade(move |socket| console_socket(state_clone, server, can_write, socket))
        .into_response())
}

fn origin_matches(state: &AppState, origin: &str) -> bool {
    let expected = state.config.public_url.trim_end_matches('/');
    origin.trim_end_matches('/') == expected
}

async fn console_socket(state: AppState, server: Server, can_write: bool, socket: WebSocket) {
    let (mut sender, mut receiver) = socket.split();
    let mut rx = state.console.subscribe(&server.id);
    let initial = [
        crate::console::ConsoleEvent::Status(server.status.clone()).to_json(),
        crate::console::ConsoleEvent::Notice(format!("Connected to {}", server.name)).to_json(),
    ];
    for message in initial {
        if sender.send(Message::Text(message.into())).await.is_err() {
            return;
        }
    }
    if let (Some(container), Ok(runtime)) = (&server.container_id, state.runtimes.get(&server.node_id)) {
        if let Ok(logs) = runtime
            .logs(container, state.config.docker.console_history_lines)
            .await
        {
            for line in logs.lines().take(1000) {
                let clean = crate::util::strip_ansi(line);
                if clean.is_empty() {
                    continue;
                }
                let msg = crate::console::ConsoleEvent::Output(clean).to_json();
                if sender.send(Message::Text(msg.into())).await.is_err() {
                    return;
                }
            }
        }
    }
    let mut heartbeat = tokio::time::interval(Duration::from_secs(25));
    loop {
        tokio::select! {
            incoming = receiver.next() => {
                let Some(Ok(message)) = incoming else { break };
                match message {
                    Message::Text(text) => {
                        if !can_write {
                            let notice = crate::console::ConsoleEvent::Notice("You do not have console write permission".into()).to_json();
                            let _ = sender.send(Message::Text(notice.into())).await;
                            continue;
                        }
                        let command = parse_ws_command(&text);
                        if command.is_empty() {
                            continue;
                        }
                        if let Some(container) = &server.container_id {
                            if let Err(err) = state.console.send_command(&state.runtimes, &server, container, &command).await {
                                let notice = crate::console::ConsoleEvent::Notice(err).to_json();
                                let _ = sender.send(Message::Text(notice.into())).await;
                            }
                        }
                    }
                    Message::Ping(payload) => {
                        let _ = sender.send(Message::Pong(payload)).await;
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
            event = rx.recv() => {
                match event {
                    Ok(event) => {
                        if sender.send(Message::Text(event.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        let notice = crate::console::ConsoleEvent::Notice(format!("Console output dropped {skipped} events")).to_json();
                        let _ = sender.send(Message::Text(notice.into())).await;
                    }
                    Err(_) => break,
                }
            }
            _ = heartbeat.tick() => {
                if sender.send(Message::Ping(vec![].into())).await.is_err() {
                    break;
                }
            }
        }
    }
}

fn parse_ws_command(text: &str) -> String {
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        if value["type"].as_str() == Some("command") {
            return value["data"]
                .as_str()
                .unwrap_or_default()
                .chars()
                .take(4096)
                .collect();
        }
        return String::new();
    }
    text.chars().take(4096).collect()
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

async fn files_list(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let access = authorize_server(&state, &auth, &id, "files.read").await?;
    let path = safe_path(params.get("path").map(|s| s.as_str()).unwrap_or(""))?;
    let files = FileService::for_server(&state, &access.server).await?;
    let entries = files.list(&path).await?;
    let usage = files.usage().await.unwrap_or(0);
    Ok(Json(json!({
        "path": path,
        "entries": entries,
        "usage_bytes": usage,
        "disk_limit_bytes": access.server.disk_mb * 1024 * 1024,
    })))
}

async fn files_read(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let access = authorize_server(&state, &auth, &id, "files.read").await?;
    let path = safe_path(
        params
            .get("path")
            .ok_or_else(|| ApiError::bad_request("Path is required"))?,
    )?;
    if path.is_empty() {
        return Err(ApiError::bad_request("Path is required"));
    }
    let files = FileService::for_server(&state, &access.server).await?;
    let bytes = files.read(&path, 2 * 1024 * 1024).await?;
    let content =
        String::from_utf8(bytes).map_err(|_| ApiError::bad_request("File is not valid UTF-8 text"))?;
    Ok(Json(json!({ "path": path, "content": content })))
}

#[derive(Deserialize)]
struct WriteFileRequest {
    path: String,
    content: String,
}

async fn files_write(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
    Json(req): Json<WriteFileRequest>,
) -> ApiResult<Json<Value>> {
    let access = authorize_server(&state, &auth, &id, "files.write").await?;
    let path = safe_path(&req.path)?;
    if path.is_empty() {
        return Err(ApiError::bad_request("Path is required"));
    }
    if req.content.len() > 2 * 1024 * 1024 {
        return Err(ApiError::bad_request("File content is too large"));
    }
    let files = FileService::for_server(&state, &access.server).await?;
    files.write(&path, req.content.into_bytes()).await?;
    crate::audit::record(
        &state.db,
        Some(&auth.user.id),
        &auth.user.username,
        "file.write",
        &access.server.uuid,
        None,
        json!({ "path": path }),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

async fn files_upload(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let access = authorize_server(&state, &auth, &id, "files.write").await?;
    let path = safe_path(
        params
            .get("path")
            .ok_or_else(|| ApiError::bad_request("Path is required"))?,
    )?;
    if path.is_empty() || path.ends_with('/') {
        return Err(ApiError::bad_request("Upload path must include a file name"));
    }
    if body.len() as u64 > state.config.max_upload_bytes {
        return Err(ApiError::bad_request("Upload exceeds the configured size limit"));
    }
    let files = FileService::for_server(&state, &access.server).await?;
    files.write(&path, body.to_vec()).await?;
    crate::audit::record(
        &state.db,
        Some(&auth.user.id),
        &auth.user.username,
        "file.upload",
        &access.server.uuid,
        None,
        json!({ "path": path, "bytes": body.len() }),
    )
    .await;
    Ok(Json(json!({ "ok": true, "bytes": body.len() })))
}

async fn files_download(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult<Response> {
    let access = authorize_server(&state, &auth, &id, "files.read").await?;
    let path = safe_path(
        params
            .get("path")
            .ok_or_else(|| ApiError::bad_request("Path is required"))?,
    )?;
    if path.is_empty() {
        return Err(ApiError::bad_request("Path is required"));
    }
    let files = FileService::for_server(&state, &access.server).await?;
    let name = path.rsplit('/').next().unwrap_or("download").to_string();
    match files.download(&path).await? {
        Download::Local(file, size) => {
            let body = Body::from_stream(ReaderStream::new(file));
            let mut response = Response::new(body);
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_str(mime_guess::from_path(&name).first_or_octet_stream().as_ref()).unwrap(),
            );
            response.headers_mut().insert(
                header::CONTENT_LENGTH,
                HeaderValue::from_str(&size.to_string()).unwrap(),
            );
            response.headers_mut().insert(
                header::CONTENT_DISPOSITION,
                HeaderValue::from_str(&content_disposition(&name)).unwrap(),
            );
            Ok(response)
        }
        Download::Remote(response) => {
            let status = response.status();
            let stream = response
                .bytes_stream()
                .map(|item| item.map_err(std::io::Error::other));
            let mut output = Response::new(Body::from_stream(stream));
            *output.status_mut() = status;
            output.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/octet-stream"),
            );
            output.headers_mut().insert(
                header::CONTENT_DISPOSITION,
                HeaderValue::from_str(&content_disposition(&name)).unwrap(),
            );
            Ok(output)
        }
    }
}

fn content_disposition(name: &str) -> String {
    let safe: String = name
        .chars()
        .filter(|c| !c.is_control() && *c != '"' && *c != '\\' && *c != '/')
        .take(120)
        .collect();
    format!(
        "attachment; filename=\"{}\"",
        if safe.is_empty() { "download".into() } else { safe }
    )
}

async fn files_delete(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let access = authorize_server(&state, &auth, &id, "files.delete").await?;
    let path = safe_path(
        params
            .get("path")
            .ok_or_else(|| ApiError::bad_request("Path is required"))?,
    )?;
    if path.is_empty() {
        return Err(ApiError::bad_request("Path is required"));
    }
    let files = FileService::for_server(&state, &access.server).await?;
    files.delete(&path).await?;
    crate::audit::record(
        &state.db,
        Some(&auth.user.id),
        &auth.user.username,
        "file.delete",
        &access.server.uuid,
        None,
        json!({ "path": path }),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct PathRequest {
    path: String,
}

async fn files_mkdir(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
    Json(req): Json<PathRequest>,
) -> ApiResult<Json<Value>> {
    let access = authorize_server(&state, &auth, &id, "files.write").await?;
    let path = safe_path(&req.path)?;
    if path.is_empty() {
        return Err(ApiError::bad_request("Path is required"));
    }
    let files = FileService::for_server(&state, &access.server).await?;
    files.mkdir(&path).await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct RenameRequest {
    from: String,
    to: String,
}

async fn files_rename(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
    Json(req): Json<RenameRequest>,
) -> ApiResult<Json<Value>> {
    let access = authorize_server(&state, &auth, &id, "files.write").await?;
    let from = safe_path(&req.from)?;
    let to = safe_path(&req.to)?;
    let files = FileService::for_server(&state, &access.server).await?;
    files.rename(&from, &to).await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct ChmodRequest {
    path: String,
    mode: u32,
}

async fn files_chmod(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
    Json(req): Json<ChmodRequest>,
) -> ApiResult<Json<Value>> {
    let access = authorize_server(&state, &auth, &id, "files.write").await?;
    let path = safe_path(&req.path)?;
    let files = FileService::for_server(&state, &access.server).await?;
    files.chmod(&path, req.mode).await?;
    Ok(Json(json!({ "ok": true })))
}

// ---------------------------------------------------------------------------
// Allocations
// ---------------------------------------------------------------------------

async fn server_allocations(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let access = authorize_server(&state, &auth, &id, "settings.read").await?;
    let allocations = server_ops::allocations(&state, &access.server.id).await?;
    Ok(Json(json!({ "allocations": allocations })))
}

#[derive(Deserialize)]
struct AttachAllocationRequest {
    allocation_id: String,
}

async fn attach_allocation(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
    Json(req): Json<AttachAllocationRequest>,
) -> ApiResult<Json<Value>> {
    let access = authorize_server(&state, &auth, &id, "settings.write").await?;
    if !access.is_owner && !access.is_admin {
        return Err(ApiError::forbidden("Only the owner can manage allocations"));
    }
    let allocation = server_ops::attach_allocation(&state, &access.server, &req.allocation_id).await?;
    Ok(Json(json!({ "allocation": allocation })))
}

async fn detach_allocation(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((id, allocation_id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let access = authorize_server(&state, &auth, &id, "settings.write").await?;
    if !access.is_owner && !access.is_admin {
        return Err(ApiError::forbidden("Only the owner can manage allocations"));
    }
    let allocations = server_ops::allocations(&state, &access.server.id).await?;
    let allocation = allocations
        .iter()
        .find(|a| a.id == allocation_id)
        .ok_or_else(|| ApiError::not_found("Allocation not found"))?;
    server_ops::detach_allocation(&state, &access.server, allocation).await?;
    Ok(Json(json!({ "ok": true })))
}

// ---------------------------------------------------------------------------
// Backups
// ---------------------------------------------------------------------------

async fn list_backups(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let access = authorize_server(&state, &auth, &id, "backups.read").await?;
    let rows = sqlx::query(
        &state
            .db
            .sql("SELECT * FROM backups WHERE server_id = ? ORDER BY created_at DESC"),
    )
    .bind(&access.server.id)
    .fetch_all(&state.db.pool)
    .await?;
    let backups: Vec<_> = rows.iter().map(Backup::from_row).collect();
    Ok(Json(json!({ "backups": backups })))
}

#[derive(Deserialize)]
struct CreateBackupRequest {
    #[serde(default)]
    name: String,
}

async fn create_backup(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
    Json(req): Json<CreateBackupRequest>,
) -> ApiResult<Json<Value>> {
    let access = authorize_server(&state, &auth, &id, "backups.create").await?;
    let count: i64 = sqlx::query(
        &state
            .db
            .sql("SELECT COUNT(*) AS count FROM backups WHERE server_id = ?"),
    )
    .bind(&access.server.id)
    .fetch_optional(&state.db.pool)
    .await?
    .map(|r| get_i64(&r, "count"))
    .unwrap_or(0);
    if count >= 25 {
        return Err(ApiError::conflict(
            "Backup limit reached; delete an old backup first",
        ));
    }
    let backup = server_ops::create_backup(&state, &access.server, &req.name).await?;
    crate::audit::record(
        &state.db,
        Some(&auth.user.id),
        &auth.user.username,
        "backup.create",
        &access.server.uuid,
        None,
        json!({ "backup": backup.uuid }),
    )
    .await;
    Ok(Json(json!({ "backup": backup })))
}

async fn restore_backup(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((id, backup_id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let access = authorize_server(&state, &auth, &id, "backups.restore").await?;
    let backup = load_backup(&state, &access.server.id, &backup_id).await?;
    server_ops::restore_backup(&state, &access.server, &backup).await?;
    crate::audit::record(
        &state.db,
        Some(&auth.user.id),
        &auth.user.username,
        "backup.restore",
        &access.server.uuid,
        None,
        json!({ "backup": backup.uuid }),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

async fn delete_backup(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((id, backup_id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let access = authorize_server(&state, &auth, &id, "backups.delete").await?;
    let backup = load_backup(&state, &access.server.id, &backup_id).await?;
    server_ops::delete_backup(&state, &access.server, &backup).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn download_backup(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((id, backup_id)): Path<(String, String)>,
) -> ApiResult<Response> {
    let access = authorize_server(&state, &auth, &id, "backups.read").await?;
    let backup = load_backup(&state, &access.server.id, &backup_id).await?;
    let name = format!("{}-{}.tar.gz", access.server.name.replace(' ', "-"), backup.uuid);
    match server_ops::backup_download(&state, &access.server, &backup).await? {
        Download::Local(file, size) => {
            let mut response = Response::new(Body::from_stream(ReaderStream::new(file)));
            response
                .headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static("application/gzip"));
            response.headers_mut().insert(
                header::CONTENT_LENGTH,
                HeaderValue::from_str(&size.to_string()).unwrap(),
            );
            response.headers_mut().insert(
                header::CONTENT_DISPOSITION,
                HeaderValue::from_str(&content_disposition(&name)).unwrap(),
            );
            Ok(response)
        }
        Download::Remote(response) => {
            let stream = response
                .bytes_stream()
                .map(|item| item.map_err(std::io::Error::other));
            let mut output = Response::new(Body::from_stream(stream));
            output
                .headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static("application/gzip"));
            output.headers_mut().insert(
                header::CONTENT_DISPOSITION,
                HeaderValue::from_str(&content_disposition(&name)).unwrap(),
            );
            Ok(output)
        }
    }
}

async fn load_backup(state: &AppState, server_id: &str, backup_id: &str) -> ApiResult<Backup> {
    let row: Option<AnyRow> = sqlx::query(
        &state
            .db
            .sql("SELECT * FROM backups WHERE id = ? AND server_id = ?"),
    )
    .bind(backup_id)
    .bind(server_id)
    .fetch_optional(&state.db.pool)
    .await?;
    row.map(|r| Backup::from_row(&r))
        .ok_or_else(|| ApiError::not_found("Backup not found"))
}

// ---------------------------------------------------------------------------
// Schedules
// ---------------------------------------------------------------------------

async fn list_schedules(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let access = authorize_server(&state, &auth, &id, "schedules.read").await?;
    let rows = sqlx::query(
        &state
            .db
            .sql("SELECT * FROM schedules WHERE server_id = ? ORDER BY created_at ASC"),
    )
    .bind(&access.server.id)
    .fetch_all(&state.db.pool)
    .await?;
    let schedules: Vec<_> = rows.iter().map(Schedule::from_row).collect();
    Ok(Json(json!({ "schedules": schedules })))
}

#[derive(Deserialize)]
struct ScheduleRequest {
    name: String,
    cron: String,
    tasks: Vec<ScheduleTask>,
    #[serde(default = "default_true")]
    enabled: bool,
}

fn default_true() -> bool {
    true
}

fn validate_schedule(req: &ScheduleRequest) -> ApiResult<()> {
    if req.name.trim().is_empty() || req.name.len() > 100 {
        return Err(ApiError::bad_request("Schedule name must be 1-100 characters"));
    }
    CronSpec::parse(&req.cron).map_err(|e| ApiError::bad_request(e.to_string()))?;
    if req.tasks.is_empty() || req.tasks.len() > 20 {
        return Err(ApiError::bad_request("A schedule needs 1-20 tasks"));
    }
    for task in &req.tasks {
        let action = task.action.to_lowercase();
        if !matches!(
            action.as_str(),
            "command"
                | "power"
                | "power:start"
                | "power:stop"
                | "power:restart"
                | "power:kill"
                | "start"
                | "stop"
                | "restart"
                | "kill"
                | "backup"
        ) {
            return Err(ApiError::bad_request(format!(
                "Unknown task action '{}'",
                task.action
            )));
        }
        if task.payload.len() > 4096 {
            return Err(ApiError::bad_request("Task payload is too long"));
        }
        if action == "command" && task.payload.trim().is_empty() {
            return Err(ApiError::bad_request("Command tasks need a payload"));
        }
        if task.offset_secs > 86400 {
            return Err(ApiError::bad_request("Task offset is too large"));
        }
    }
    Ok(())
}

async fn create_schedule(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
    Json(req): Json<ScheduleRequest>,
) -> ApiResult<Json<Value>> {
    let access = authorize_server(&state, &auth, &id, "schedules.write").await?;
    validate_schedule(&req)?;
    let id = uuid::Uuid::new_v4().to_string();
    let next = CronSpec::parse(&req.cron)
        .ok()
        .map(|c| c.next_after(chrono::Utc::now()).timestamp_millis());
    sqlx::query(&state.db.sql(
        "INSERT INTO schedules (id, server_id, name, cron, tasks, enabled, next_run_at, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    ))
    .bind(&id)
    .bind(&access.server.id)
    .bind(req.name.trim())
    .bind(&req.cron)
    .bind(serde_json::to_string(&req.tasks).unwrap_or_else(|_| "[]".into()))
    .bind(if req.enabled { 1i64 } else { 0i64 })
    .bind(next)
    .bind(now_ms())
    .execute(&state.db.pool)
    .await?;
    let row: Option<AnyRow> = sqlx::query(&state.db.sql("SELECT * FROM schedules WHERE id = ?"))
        .bind(&id)
        .fetch_optional(&state.db.pool)
        .await?;
    let schedule = row
        .map(|r| Schedule::from_row(&r))
        .ok_or_else(|| ApiError::internal("schedule missing"))?;
    Ok(Json(json!({ "schedule": schedule })))
}

async fn update_schedule(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((id, schedule_id)): Path<(String, String)>,
    Json(req): Json<ScheduleRequest>,
) -> ApiResult<Json<Value>> {
    let access = authorize_server(&state, &auth, &id, "schedules.write").await?;
    validate_schedule(&req)?;
    let next = CronSpec::parse(&req.cron)
        .ok()
        .map(|c| c.next_after(chrono::Utc::now()).timestamp_millis());
    let result = sqlx::query(&state.db.sql(
        "UPDATE schedules SET name = ?, cron = ?, tasks = ?, enabled = ?, next_run_at = ? WHERE id = ? AND server_id = ?",
    ))
    .bind(req.name.trim())
    .bind(&req.cron)
    .bind(serde_json::to_string(&req.tasks).unwrap_or_else(|_| "[]".into()))
    .bind(if req.enabled { 1i64 } else { 0i64 })
    .bind(next)
    .bind(&schedule_id)
    .bind(&access.server.id)
    .execute(&state.db.pool)
    .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("Schedule not found"));
    }
    Ok(Json(json!({ "ok": true })))
}

async fn delete_schedule(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((id, schedule_id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let access = authorize_server(&state, &auth, &id, "schedules.write").await?;
    sqlx::query(
        &state
            .db
            .sql("DELETE FROM schedules WHERE id = ? AND server_id = ?"),
    )
    .bind(&schedule_id)
    .bind(&access.server.id)
    .execute(&state.db.pool)
    .await?;
    Ok(Json(json!({ "ok": true })))
}

async fn run_schedule(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((id, schedule_id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let access = authorize_server(&state, &auth, &id, "schedules.write").await?;
    let result = sqlx::query(
        &state
            .db
            .sql("UPDATE schedules SET next_run_at = ? WHERE id = ? AND server_id = ? AND enabled = 1"),
    )
    .bind(now_ms())
    .bind(&schedule_id)
    .bind(&access.server.id)
    .execute(&state.db.pool)
    .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("Schedule not found or disabled"));
    }
    Ok(Json(json!({ "queued": true })))
}

// ---------------------------------------------------------------------------
// Subusers
// ---------------------------------------------------------------------------

async fn list_subusers(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let access = authorize_server(&state, &auth, &id, "users.read").await?;
    let rows = sqlx::query(&state.db.sql(
        "SELECT s.id, s.server_id, s.user_id, s.permissions, s.created_at, u.username, u.email
         FROM subusers s JOIN users u ON u.id = s.user_id WHERE s.server_id = ? ORDER BY s.created_at ASC",
    ))
    .bind(&access.server.id)
    .fetch_all(&state.db.pool)
    .await?;
    let subusers: Vec<Subuser> = rows
        .iter()
        .map(|row| Subuser {
            id: get_str(row, "id"),
            server_id: get_str(row, "server_id"),
            user_id: get_str(row, "user_id"),
            username: get_str(row, "username"),
            email: get_str(row, "email"),
            permissions: serde_json::from_str(&get_str(row, "permissions")).unwrap_or_default(),
            created_at: get_i64(row, "created_at"),
        })
        .collect();
    Ok(Json(json!({ "subusers": subusers })))
}

#[derive(Deserialize)]
struct SubuserRequest {
    #[serde(default)]
    user_id: Option<String>,
    #[serde(default)]
    email: Option<String>,
    permissions: Vec<String>,
}

fn validate_permissions(permissions: &[String]) -> ApiResult<()> {
    for permission in permissions {
        if !ALL_SERVER_PERMISSIONS.contains(&permission.as_str()) {
            return Err(ApiError::bad_request(format!(
                "Unknown permission '{permission}'"
            )));
        }
    }
    Ok(())
}

async fn create_subuser(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
    Json(req): Json<SubuserRequest>,
) -> ApiResult<Json<Value>> {
    let access = authorize_server(&state, &auth, &id, "users.write").await?;
    validate_permissions(&req.permissions)?;
    let user_id = if let Some(user_id) = &req.user_id {
        user_id.clone()
    } else if let Some(email) = &req.email {
        crate::auth::find_user_by_email(&state.db, email)
            .await?
            .map(|u| u.id)
            .ok_or_else(|| ApiError::not_found("User not found"))?
    } else {
        return Err(ApiError::bad_request("user_id or email is required"));
    };
    crate::auth::ensure_subuser_not_owner(&state.db, &access.server.id, &access.server.owner_id, &user_id)
        .await?;
    sqlx::query(&state.db.sql(
        "INSERT INTO subusers (id, server_id, user_id, permissions, created_at) VALUES (?, ?, ?, ?, ?)",
    ))
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(&access.server.id)
    .bind(&user_id)
    .bind(serde_json::to_string(&req.permissions).unwrap_or_else(|_| "[]".into()))
    .bind(now_ms())
    .execute(&state.db.pool)
    .await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct SubuserPermissions {
    permissions: Vec<String>,
}

async fn update_subuser(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((id, sub_id)): Path<(String, String)>,
    Json(req): Json<SubuserPermissions>,
) -> ApiResult<Json<Value>> {
    let access = authorize_server(&state, &auth, &id, "users.write").await?;
    validate_permissions(&req.permissions)?;
    let result = sqlx::query(
        &state
            .db
            .sql("UPDATE subusers SET permissions = ? WHERE id = ? AND server_id = ?"),
    )
    .bind(serde_json::to_string(&req.permissions).unwrap_or_else(|_| "[]".into()))
    .bind(&sub_id)
    .bind(&access.server.id)
    .execute(&state.db.pool)
    .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("Subuser not found"));
    }
    Ok(Json(json!({ "ok": true })))
}

async fn delete_subuser(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((id, sub_id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let access = authorize_server(&state, &auth, &id, "users.write").await?;
    sqlx::query(
        &state
            .db
            .sql("DELETE FROM subusers WHERE id = ? AND server_id = ?"),
    )
    .bind(&sub_id)
    .bind(&access.server.id)
    .execute(&state.db.pool)
    .await?;
    Ok(Json(json!({ "ok": true })))
}

// Suppress unused warnings for helpers exported for future handlers.
#[allow(dead_code)]
fn _unused(_: AuditLog, _: StatusCode) {}
