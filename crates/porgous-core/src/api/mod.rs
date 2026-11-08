//! REST API.

pub mod servers;

use crate::auth::{self, clear_session_cookie, create_user, find_user, AuthUser, LoginRequest};
use crate::db::{get_i64, get_opt_i64, get_opt_str, get_str, now_ms};
use crate::error::{ApiError, ApiResult};
use crate::models::Node;
use crate::state::{load_node, AppState, VERSION};
use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::any::AnyRow;
use std::net::SocketAddr;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/meta", get(meta))
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        .route("/auth/session", get(session))
        .route("/auth/register", post(register))
        .route("/auth/password", post(change_password))
        .route("/auth/2fa/setup", post(totp_setup))
        .route("/auth/2fa/enable", post(totp_enable))
        .route("/auth/2fa/disable", post(totp_disable))
        .route("/account", patch(update_account))
        .route("/account/api-keys", get(list_api_keys).post(create_api_key))
        .route("/account/api-keys/{id}", delete(delete_api_key))
        .route("/account/sessions", get(list_sessions))
        .route("/account/sessions/{id}", delete(revoke_session))
        .route("/notifications", get(list_notifications))
        .route("/notifications/read-all", post(read_all_notifications))
        .route("/notifications/{id}/read", post(read_notification))
        .route("/notifications/{id}", delete(delete_notification))
        .route("/users", get(admin_list_users).post(admin_create_user))
        .route("/users/{id}", patch(admin_update_user).delete(admin_delete_user))
        .route("/users/{id}/reset-password", post(admin_reset_password))
        .route("/nodes", get(list_nodes).post(create_node))
        .route("/nodes/{id}", patch(update_node).delete(delete_node))
        .route("/nodes/{id}/rotate", post(rotate_node))
        .route("/nodes/{id}/test", post(test_node))
        .route(
            "/nodes/{id}/allocations",
            get(list_allocations).post(create_allocation),
        )
        .route("/allocations/{id}", delete(delete_allocation))
        .route("/eggs", get(list_eggs))
        .route("/eggs/{id}", get(get_egg).delete(delete_egg))
        .route("/eggs/import", post(import_egg))
        .route("/eggs/{id}/export", get(export_egg))
        .route("/admin/settings", get(get_settings).patch(update_settings))
        .route("/admin/audit", get(list_audit))
        .route("/admin/stats", get(admin_stats))
        .route("/updates", get(update_status))
        .route("/updates/apply", post(update_apply))
        .route("/updates/rollback", post(update_rollback))
        .route("/updates/history", get(update_history))
        .merge(servers::routes())
        .fallback(not_found)
}

async fn not_found() -> ApiError {
    ApiError::not_found("Endpoint not found")
}

fn ip_of(info: &ConnectInfo<SocketAddr>) -> Option<String> {
    Some(info.0.ip().to_string())
}

async fn meta(State(state): State<AppState>) -> Json<Value> {
    let registration = state
        .db
        .setting_bool("allow_registration", state.config.allow_registration)
        .await;
    let panel_name = state
        .db
        .setting("panel_name")
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| state.config.panel_name.clone());
    Json(json!({
        "panel_name": panel_name,
        "version": VERSION,
        "allow_registration": registration,
        "update_channel": state.config.updates.channel,
    }))
}

async fn login(
    State(state): State<AppState>,
    info: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<LoginRequest>,
) -> ApiResult<Response> {
    let ip = ip_of(&info);
    let user_agent = headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok());
    let (success, token) = auth::login(&state, &req, ip.as_deref(), user_agent).await?;
    let max_age = state.config.session_ttl_hours * 3600;
    let secure = state.config.secure_cookies || state.config.public_url.starts_with("https://");
    let cookie = auth::session_cookie(&token, max_age, secure);
    let mut response = Json(success).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(|_| ApiError::internal("cookie encoding"))?,
    );
    crate::audit::record(
        &state.db,
        None,
        &req.email,
        "auth.login",
        "",
        ip.as_deref(),
        json!({}),
    )
    .await;
    Ok(response)
}

async fn logout(State(state): State<AppState>, auth: AuthUser) -> ApiResult<Response> {
    if let Some((session_id, _)) = auth.session() {
        auth::logout(&state.db, session_id).await?;
    }
    let secure = state.config.secure_cookies || state.config.public_url.starts_with("https://");
    let mut response = Json(json!({ "ok": true })).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&clear_session_cookie(secure)).unwrap(),
    );
    Ok(response)
}

async fn session(auth: AuthUser) -> Json<Value> {
    let csrf = auth.session().map(|(_, csrf)| csrf.to_string());
    let scopes = auth.api_key_scopes().map(|s| s.to_vec());
    Json(json!({
        "user": auth.user.view(),
        "csrf_token": csrf,
        "is_admin": auth.user.root_admin,
        "scopes": scopes,
    }))
}

#[derive(Deserialize)]
struct RegisterRequest {
    email: String,
    username: String,
    password: String,
}

async fn register(
    State(state): State<AppState>,
    info: ConnectInfo<SocketAddr>,
    Json(req): Json<RegisterRequest>,
) -> ApiResult<Response> {
    if !state
        .db
        .setting_bool("allow_registration", state.config.allow_registration)
        .await
    {
        return Err(ApiError::forbidden("Registration is disabled"));
    }
    let password = req.password.clone();
    create_user(&state.db, &req.email, &req.username, &password, false).await?;
    let (success, token) = auth::login(
        &state,
        &LoginRequest {
            email: req.email.clone(),
            password,
            totp: None,
        },
        ip_of(&info).as_deref(),
        None,
    )
    .await?;
    crate::audit::record(
        &state.db,
        None,
        &req.email,
        "auth.register",
        "",
        ip_of(&info).as_deref(),
        json!({}),
    )
    .await;
    let secure = state.config.secure_cookies || state.config.public_url.starts_with("https://");
    let cookie = auth::session_cookie(&token, state.config.session_ttl_hours * 3600, secure);
    let mut response = Json(success).into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, HeaderValue::from_str(&cookie).unwrap());
    Ok(response)
}

#[derive(Deserialize)]
struct PasswordRequest {
    current_password: String,
    new_password: String,
}

async fn change_password(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(req): Json<PasswordRequest>,
) -> ApiResult<Json<Value>> {
    crate::state::verify_current_password(&state, &auth.user.id, &req.current_password).await?;
    let hash =
        crate::crypto::hash_password(&req.new_password).map_err(|e| ApiError::bad_request(e.to_string()))?;
    sqlx::query(
        &state
            .db
            .sql("UPDATE users SET password_hash = ?, updated_at = ? WHERE id = ?"),
    )
    .bind(&hash)
    .bind(now_ms())
    .bind(&auth.user.id)
    .execute(&state.db.pool)
    .await?;
    // Invalidate every other session.
    if let Some((current, _)) = auth.session() {
        sqlx::query(&state.db.sql("DELETE FROM sessions WHERE user_id = ? AND id != ?"))
            .bind(&auth.user.id)
            .bind(current)
            .execute(&state.db.pool)
            .await?;
    }
    crate::audit::record(
        &state.db,
        Some(&auth.user.id),
        &auth.user.username,
        "account.password",
        &auth.user.id,
        None,
        json!({}),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

async fn totp_setup(State(state): State<AppState>, auth: AuthUser) -> ApiResult<Json<Value>> {
    let setup = auth::begin_totp_setup(&state, &auth.user).await?;
    Ok(Json(json!({ "secret": setup.secret, "otpauth_url": setup.url })))
}

#[derive(Deserialize)]
struct CodeRequest {
    code: String,
}

async fn totp_enable(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(req): Json<CodeRequest>,
) -> ApiResult<Json<Value>> {
    let codes = auth::enable_totp(&state, &auth.user.id, &req.code).await?;
    crate::audit::record(
        &state.db,
        Some(&auth.user.id),
        &auth.user.username,
        "account.2fa_enable",
        &auth.user.id,
        None,
        json!({}),
    )
    .await;
    Ok(Json(json!({ "enabled": true, "recovery_codes": codes })))
}

#[derive(Deserialize)]
struct Disable2faRequest {
    password: String,
    code: String,
}

async fn totp_disable(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(req): Json<Disable2faRequest>,
) -> ApiResult<Json<Value>> {
    auth::disable_totp(&state, &auth.user.id, &req.password, &req.code).await?;
    crate::audit::record(
        &state.db,
        Some(&auth.user.id),
        &auth.user.username,
        "account.2fa_disable",
        &auth.user.id,
        None,
        json!({}),
    )
    .await;
    Ok(Json(json!({ "enabled": false })))
}

#[derive(Deserialize)]
struct UpdateAccountRequest {
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    email: Option<String>,
    password: String,
}

async fn update_account(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(req): Json<UpdateAccountRequest>,
) -> ApiResult<Json<Value>> {
    crate::state::verify_current_password(&state, &auth.user.id, &req.password).await?;
    let mut email = auth.user.email.clone();
    let mut username = auth.user.username.clone();
    if let Some(new_email) = &req.email {
        email = auth::normalize_email(new_email)?;
    }
    if let Some(new_username) = &req.username {
        username = auth::validate_username(new_username)?;
    }
    let duplicate: Option<AnyRow> = sqlx::query(
        &state
            .db
            .sql("SELECT id FROM users WHERE (email = ? OR username = ?) AND id != ?"),
    )
    .bind(&email)
    .bind(&username)
    .bind(&auth.user.id)
    .fetch_optional(&state.db.pool)
    .await?;
    if duplicate.is_some() {
        return Err(ApiError::conflict("Email or username already in use"));
    }
    sqlx::query(
        &state
            .db
            .sql("UPDATE users SET email = ?, username = ?, updated_at = ? WHERE id = ?"),
    )
    .bind(&email)
    .bind(&username)
    .bind(now_ms())
    .bind(&auth.user.id)
    .execute(&state.db.pool)
    .await?;
    Ok(Json(json!({ "ok": true, "email": email, "username": username })))
}

async fn list_api_keys(State(state): State<AppState>, auth: AuthUser) -> ApiResult<Json<Value>> {
    let rows = sqlx::query(&state.db.sql(
        "SELECT id, name, prefix, scopes, created_at, last_used_at, expires_at FROM api_keys WHERE user_id = ? ORDER BY created_at DESC",
    ))
    .bind(&auth.user.id)
    .fetch_all(&state.db.pool)
    .await?;
    let keys: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "id": get_str(row, "id"),
                "name": get_str(row, "name"),
                "prefix": get_str(row, "prefix"),
                "scopes": serde_json::from_str::<Vec<String>>(&get_str(row, "scopes")).unwrap_or_default(),
                "created_at": get_i64(row, "created_at"),
                "last_used_at": get_opt_i64(row, "last_used_at"),
                "expires_at": get_opt_i64(row, "expires_at"),
            })
        })
        .collect();
    Ok(Json(json!({ "api_keys": keys })))
}

#[derive(Deserialize)]
struct CreateApiKeyRequest {
    name: String,
    scopes: Vec<String>,
    #[serde(default)]
    expires_at: Option<i64>,
    password: String,
}

async fn create_api_key(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(req): Json<CreateApiKeyRequest>,
) -> ApiResult<Json<Value>> {
    crate::state::verify_current_password(&state, &auth.user.id, &req.password).await?;
    auth.require_scope("account:write")?;
    if !auth.is_admin() && req.scopes.iter().any(|s| s == "admin" || s == "*") {
        return Err(ApiError::forbidden(
            "Only administrators can create admin-scoped keys",
        ));
    }
    let (key, view) =
        auth::create_api_key(&state, &auth.user.id, &req.name, &req.scopes, req.expires_at).await?;
    crate::audit::record(
        &state.db,
        Some(&auth.user.id),
        &auth.user.username,
        "account.api_key_create",
        &view.id,
        None,
        json!({ "scopes": req.scopes }),
    )
    .await;
    Ok(Json(json!({ "key": key, "api_key": view })))
}

async fn delete_api_key(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    auth.require_scope("account:write")?;
    let result = sqlx::query(&state.db.sql("DELETE FROM api_keys WHERE id = ? AND user_id = ?"))
        .bind(&id)
        .bind(&auth.user.id)
        .execute(&state.db.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("API key not found"));
    }
    Ok(Json(json!({ "ok": true })))
}

async fn list_sessions(State(state): State<AppState>, auth: AuthUser) -> ApiResult<Json<Value>> {
    let rows = sqlx::query(&state.db.sql(
        "SELECT id, ip, user_agent, created_at, last_seen_at, expires_at FROM sessions WHERE user_id = ? ORDER BY created_at DESC",
    ))
    .bind(&auth.user.id)
    .fetch_all(&state.db.pool)
    .await?;
    let sessions: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "id": get_str(row, "id"),
                "ip": get_opt_str(row, "ip"),
                "user_agent": get_opt_str(row, "user_agent"),
                "created_at": get_i64(row, "created_at"),
                "last_seen_at": get_opt_i64(row, "last_seen_at"),
                "expires_at": get_i64(row, "expires_at"),
                "current": auth.session().map(|(id, _)| id == get_str(row, "id")).unwrap_or(false),
            })
        })
        .collect();
    Ok(Json(json!({ "sessions": sessions })))
}

async fn revoke_session(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    sqlx::query(&state.db.sql("DELETE FROM sessions WHERE id = ? AND user_id = ?"))
        .bind(&id)
        .bind(&auth.user.id)
        .execute(&state.db.pool)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

async fn list_notifications(State(state): State<AppState>, auth: AuthUser) -> ApiResult<Json<Value>> {
    let rows = sqlx::query(
        &state
            .db
            .sql("SELECT * FROM notifications WHERE user_id = ? ORDER BY created_at DESC LIMIT 100"),
    )
    .bind(&auth.user.id)
    .fetch_all(&state.db.pool)
    .await?;
    let notifications: Vec<_> = rows.iter().map(crate::models::Notification::from_row).collect();
    let unread = notifications.iter().filter(|n| !n.is_read).count();
    Ok(Json(json!({ "notifications": notifications, "unread": unread })))
}

async fn read_notification(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    sqlx::query(
        &state
            .db
            .sql("UPDATE notifications SET is_read = 1 WHERE id = ? AND user_id = ?"),
    )
    .bind(&id)
    .bind(&auth.user.id)
    .execute(&state.db.pool)
    .await?;
    Ok(Json(json!({ "ok": true })))
}

async fn read_all_notifications(State(state): State<AppState>, auth: AuthUser) -> ApiResult<Json<Value>> {
    sqlx::query(
        &state
            .db
            .sql("UPDATE notifications SET is_read = 1 WHERE user_id = ?"),
    )
    .bind(&auth.user.id)
    .execute(&state.db.pool)
    .await?;
    Ok(Json(json!({ "ok": true })))
}

async fn delete_notification(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    sqlx::query(
        &state
            .db
            .sql("DELETE FROM notifications WHERE id = ? AND user_id = ?"),
    )
    .bind(&id)
    .bind(&auth.user.id)
    .execute(&state.db.pool)
    .await?;
    Ok(Json(json!({ "ok": true })))
}

// ---------------------------------------------------------------------------
// Administration: users
// ---------------------------------------------------------------------------

async fn admin_list_users(State(state): State<AppState>, auth: AuthUser) -> ApiResult<Json<Value>> {
    auth.require_admin()?;
    let rows = sqlx::query(&state.db.sql("SELECT * FROM users ORDER BY created_at ASC"))
        .fetch_all(&state.db.pool)
        .await?;
    let users: Vec<_> = rows
        .iter()
        .map(|row| {
            let user = crate::models::User::from_row(row);
            let mut value = serde_json::to_value(user.view()).unwrap_or_default();
            value["server_count"] = json!(0);
            value
        })
        .collect();
    Ok(Json(json!({ "users": users })))
}

#[derive(Deserialize)]
struct AdminCreateUserRequest {
    email: String,
    username: String,
    password: String,
    #[serde(default)]
    root_admin: bool,
}

async fn admin_create_user(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(req): Json<AdminCreateUserRequest>,
) -> ApiResult<Json<Value>> {
    auth.require_admin()?;
    let user = create_user(
        &state.db,
        &req.email,
        &req.username,
        &req.password,
        req.root_admin,
    )
    .await?;
    crate::audit::record(
        &state.db,
        Some(&auth.user.id),
        &auth.user.username,
        "user.create",
        &user.id,
        None,
        json!({ "root_admin": req.root_admin }),
    )
    .await;
    Ok(Json(json!({ "user": user.view() })))
}

#[derive(Deserialize)]
struct AdminUpdateUserRequest {
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    root_admin: Option<bool>,
    #[serde(default)]
    suspended: Option<bool>,
}

async fn admin_update_user(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
    Json(req): Json<AdminUpdateUserRequest>,
) -> ApiResult<Json<Value>> {
    auth.require_admin()?;
    let user = find_user(&state.db, &id)
        .await?
        .ok_or_else(|| ApiError::not_found("User not found"))?;
    if user.id == auth.user.id && req.root_admin == Some(false) {
        return Err(ApiError::bad_request(
            "You cannot remove your own administrator access",
        ));
    }
    if let Some(email) = &req.email {
        let normalized = auth::normalize_email(email)?;
        sqlx::query(&state.db.sql("UPDATE users SET email = ? WHERE id = ?"))
            .bind(normalized)
            .bind(&id)
            .execute(&state.db.pool)
            .await?;
    }
    if let Some(username) = &req.username {
        let normalized = auth::validate_username(username)?;
        sqlx::query(&state.db.sql("UPDATE users SET username = ? WHERE id = ?"))
            .bind(normalized)
            .bind(&id)
            .execute(&state.db.pool)
            .await?;
    }
    if let Some(root) = req.root_admin {
        if !root {
            let admins: Option<AnyRow> = sqlx::query(
                &state
                    .db
                    .sql("SELECT COUNT(*) AS count FROM users WHERE root_admin = 1 AND id != ?"),
            )
            .bind(&id)
            .fetch_optional(&state.db.pool)
            .await?;
            if admins.map(|r| get_i64(&r, "count")).unwrap_or(0) == 0 {
                return Err(ApiError::conflict("At least one administrator is required"));
            }
        }
        sqlx::query(
            &state
                .db
                .sql("UPDATE users SET root_admin = ?, role = ? WHERE id = ?"),
        )
        .bind(if root { 1i64 } else { 0i64 })
        .bind(if root { "admin" } else { "user" })
        .bind(&id)
        .execute(&state.db.pool)
        .await?;
    }
    if let Some(suspended) = req.suspended {
        sqlx::query(&state.db.sql("UPDATE users SET suspended = ? WHERE id = ?"))
            .bind(if suspended { 1i64 } else { 0i64 })
            .bind(&id)
            .execute(&state.db.pool)
            .await?;
        if suspended {
            sqlx::query(&state.db.sql("DELETE FROM sessions WHERE user_id = ?"))
                .bind(&id)
                .execute(&state.db.pool)
                .await?;
        }
    }
    sqlx::query(&state.db.sql("UPDATE users SET updated_at = ? WHERE id = ?"))
        .bind(now_ms())
        .bind(&id)
        .execute(&state.db.pool)
        .await?;
    let updated = find_user(&state.db, &id)
        .await?
        .ok_or_else(|| ApiError::not_found("User not found"))?;
    crate::audit::record(
        &state.db,
        Some(&auth.user.id),
        &auth.user.username,
        "user.update",
        &id,
        None,
        json!({}),
    )
    .await;
    Ok(Json(json!({ "user": updated.view() })))
}

async fn admin_reset_password(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    auth.require_admin()?;
    let user = find_user(&state.db, &id)
        .await?
        .ok_or_else(|| ApiError::not_found("User not found"))?;
    let password = format!("{}{}", crate::crypto::random_token(18), "aA1!");
    let hash = crate::crypto::hash_password(&password).map_err(ApiError::internal)?;
    sqlx::query(&state.db.sql(
        "UPDATE users SET password_hash = ?, totp_enabled = 0, totp_secret = NULL, totp_recovery = NULL, updated_at = ? WHERE id = ?",
    ))
    .bind(&hash)
    .bind(now_ms())
    .bind(&id)
    .execute(&state.db.pool)
    .await?;
    sqlx::query(&state.db.sql("DELETE FROM sessions WHERE user_id = ?"))
        .bind(&id)
        .execute(&state.db.pool)
        .await?;
    crate::audit::record(
        &state.db,
        Some(&auth.user.id),
        &auth.user.username,
        "user.reset_password",
        &user.id,
        None,
        json!({}),
    )
    .await;
    Ok(Json(json!({ "password": password })))
}

async fn admin_delete_user(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    auth.require_admin()?;
    if id == auth.user.id {
        return Err(ApiError::bad_request("You cannot delete your own account"));
    }
    let user = find_user(&state.db, &id)
        .await?
        .ok_or_else(|| ApiError::not_found("User not found"))?;
    let owned: Option<AnyRow> = sqlx::query(
        &state
            .db
            .sql("SELECT COUNT(*) AS count FROM servers WHERE owner_id = ?"),
    )
    .bind(&id)
    .fetch_optional(&state.db.pool)
    .await?;
    if owned.map(|r| get_i64(&r, "count")).unwrap_or(0) > 0 {
        return Err(ApiError::conflict("Delete or transfer this user's servers first"));
    }
    if user.root_admin {
        let admins: Option<AnyRow> = sqlx::query(
            &state
                .db
                .sql("SELECT COUNT(*) AS count FROM users WHERE root_admin = 1 AND id != ?"),
        )
        .bind(&id)
        .fetch_optional(&state.db.pool)
        .await?;
        if admins.map(|r| get_i64(&r, "count")).unwrap_or(0) == 0 {
            return Err(ApiError::conflict("At least one administrator is required"));
        }
    }
    for table in ["sessions", "api_keys", "subusers", "notifications"] {
        let column = if table == "subusers" { "user_id" } else { "user_id" };
        let _ = sqlx::query(&state.db.sql(&format!("DELETE FROM {table} WHERE {column} = ?")))
            .bind(&id)
            .execute(&state.db.pool)
            .await;
    }
    let _ = sqlx::query(&state.db.sql("DELETE FROM audit_logs WHERE user_id = ?"))
        .bind(&id)
        .execute(&state.db.pool)
        .await;
    sqlx::query(&state.db.sql("DELETE FROM users WHERE id = ?"))
        .bind(&id)
        .execute(&state.db.pool)
        .await?;
    crate::audit::record(
        &state.db,
        Some(&auth.user.id),
        &auth.user.username,
        "user.delete",
        &id,
        None,
        json!({}),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

// ---------------------------------------------------------------------------
// Administration: nodes
// ---------------------------------------------------------------------------

fn node_view_with_usage(node: &Node, server_count: i64, memory: i64, disk: i64) -> Value {
    serde_json::to_value(node.view(server_count, memory, disk)).unwrap_or_default()
}

async fn node_usage(state: &AppState, node: &Node) -> (i64, i64) {
    let row = sqlx::query(&state.db.sql(
        "SELECT COUNT(*) AS count, COALESCE(SUM(memory_mb),0) AS memory, COALESCE(SUM(disk_mb),0) AS disk FROM servers WHERE node_id = ?",
    ))
    .bind(&node.id)
    .fetch_optional(&state.db.pool)
    .await
    .ok()
    .flatten();
    match row {
        Some(row) => (get_i64(&row, "memory"), get_i64(&row, "disk")),
        None => (0, 0),
    }
}

async fn list_nodes(State(state): State<AppState>, auth: AuthUser) -> ApiResult<Json<Value>> {
    auth.require_admin()?;
    let nodes = crate::state::load_nodes(&state.db).await?;
    let mut out = Vec::new();
    for node in nodes {
        let (memory, disk) = node_usage(&state, &node).await;
        let count = sqlx::query(
            &state
                .db
                .sql("SELECT COUNT(*) AS count FROM servers WHERE node_id = ?"),
        )
        .bind(&node.id)
        .fetch_optional(&state.db.pool)
        .await?
        .map(|r| get_i64(&r, "count"))
        .unwrap_or(0);
        let mut value = node_view_with_usage(&node, count, memory, disk);
        value["online"] = json!(state
            .runtimes
            .get(&node.id)
            .map(|r| !r.is_remote())
            .unwrap_or(false));
        out.push(value);
    }
    Ok(Json(json!({ "nodes": out })))
}

#[derive(Deserialize)]
struct CreateNodeRequest {
    name: String,
    #[serde(default)]
    description: String,
    fqdn: String,
    #[serde(default = "default_scheme")]
    scheme: String,
    #[serde(default = "default_node_port")]
    port: u16,
    #[serde(default)]
    memory_mb: i64,
    #[serde(default)]
    disk_mb: i64,
    #[serde(default = "default_port_start")]
    port_range_start: u16,
    #[serde(default = "default_port_end")]
    port_range_end: u16,
    #[serde(default = "default_sftp")]
    sftp_port: u16,
}

fn default_scheme() -> String {
    "http".into()
}
fn default_node_port() -> u16 {
    8081
}
fn default_port_start() -> u16 {
    25000
}
fn default_port_end() -> u16 {
    26000
}
fn default_sftp() -> u16 {
    2022
}

fn validate_fqdn(value: &str) -> ApiResult<()> {
    if value.is_empty() || value.len() > 253 || value.contains(char::is_whitespace) || value.contains('/') {
        return Err(ApiError::bad_request("Invalid node hostname or IP"));
    }
    Ok(())
}

async fn create_node(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(req): Json<CreateNodeRequest>,
) -> ApiResult<Json<Value>> {
    auth.require_admin()?;
    validate_fqdn(&req.fqdn)?;
    if !matches!(req.scheme.as_str(), "http" | "https") {
        return Err(ApiError::bad_request("Node scheme must be http or https"));
    }
    if req.name.trim().is_empty() || req.name.len() > 80 {
        return Err(ApiError::bad_request("Node name must be 1-80 characters"));
    }
    let (token, secret) = crate::nodes::generate_node_credentials();
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_ms();
    sqlx::query(&state.db.sql(
        "INSERT INTO nodes (id, name, description, is_local, fqdn, scheme, port, token_enc, secret_enc, memory_mb, disk_mb,
            port_range_start, port_range_end, maintenance, sftp_port, created_at, updated_at)
         VALUES (?, ?, ?, 0, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?, ?, ?)",
    ))
    .bind(&id)
    .bind(req.name.trim())
    .bind(req.description.chars().take(500).collect::<String>())
    .bind(&req.fqdn)
    .bind(&req.scheme)
    .bind(req.port as i64)
    .bind(state.secrets.seal(&token).map_err(ApiError::internal)?)
    .bind(state.secrets.seal(&secret).map_err(ApiError::internal)?)
    .bind(req.memory_mb)
    .bind(req.disk_mb)
    .bind(req.port_range_start as i64)
    .bind(req.port_range_end as i64)
    .bind(req.sftp_port as i64)
    .bind(now)
    .bind(now)
    .execute(&state.db.pool)
    .await?;
    let node = load_node(&state.db, &id)
        .await?
        .ok_or_else(|| ApiError::internal("node missing"))?;
    if let Ok(runtime) = crate::state::build_runtime(&state.config, &state.secrets, &node).await {
        state.runtimes.set(node.id.clone(), runtime);
    }
    crate::audit::record(
        &state.db,
        Some(&auth.user.id),
        &auth.user.username,
        "node.create",
        &node.id,
        None,
        json!({ "fqdn": node.fqdn }),
    )
    .await;
    Ok(Json(json!({
        "node": node_view_with_usage(&node, 0, 0, 0),
        "credentials": { "token": token, "secret": secret },
        "hint": "Put token and secret in agent.toml. They are shown only once."
    })))
}

#[derive(Deserialize)]
struct UpdateNodeRequest {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    fqdn: Option<String>,
    #[serde(default)]
    scheme: Option<String>,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    memory_mb: Option<i64>,
    #[serde(default)]
    disk_mb: Option<i64>,
    #[serde(default)]
    port_range_start: Option<u16>,
    #[serde(default)]
    port_range_end: Option<u16>,
    #[serde(default)]
    maintenance: Option<bool>,
    #[serde(default)]
    sftp_port: Option<u16>,
}

async fn update_node(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
    Json(req): Json<UpdateNodeRequest>,
) -> ApiResult<Json<Value>> {
    auth.require_admin()?;
    let node = load_node(&state.db, &id)
        .await?
        .ok_or_else(|| ApiError::not_found("Node not found"))?;
    if node.is_local && req.fqdn.is_some() {
        return Err(ApiError::bad_request(
            "The local node hostname is managed by configuration",
        ));
    }
    if let Some(name) = &req.name {
        if name.trim().is_empty() || name.len() > 80 {
            return Err(ApiError::bad_request("Invalid node name"));
        }
        sqlx::query(&state.db.sql("UPDATE nodes SET name = ? WHERE id = ?"))
            .bind(name.trim())
            .bind(&id)
            .execute(&state.db.pool)
            .await?;
    }
    if let Some(description) = &req.description {
        sqlx::query(&state.db.sql("UPDATE nodes SET description = ? WHERE id = ?"))
            .bind(description.chars().take(500).collect::<String>())
            .bind(&id)
            .execute(&state.db.pool)
            .await?;
    }
    if let Some(fqdn) = &req.fqdn {
        validate_fqdn(fqdn)?;
        sqlx::query(&state.db.sql("UPDATE nodes SET fqdn = ? WHERE id = ?"))
            .bind(fqdn)
            .bind(&id)
            .execute(&state.db.pool)
            .await?;
    }
    if let Some(scheme) = &req.scheme {
        if !matches!(scheme.as_str(), "http" | "https") {
            return Err(ApiError::bad_request("Node scheme must be http or https"));
        }
        sqlx::query(&state.db.sql("UPDATE nodes SET scheme = ? WHERE id = ?"))
            .bind(scheme)
            .bind(&id)
            .execute(&state.db.pool)
            .await?;
    }
    if let Some(port) = req.port {
        sqlx::query(&state.db.sql("UPDATE nodes SET port = ? WHERE id = ?"))
            .bind(port as i64)
            .bind(&id)
            .execute(&state.db.pool)
            .await?;
    }
    if let Some(value) = req.memory_mb {
        sqlx::query(&state.db.sql("UPDATE nodes SET memory_mb = ? WHERE id = ?"))
            .bind(value.max(0))
            .bind(&id)
            .execute(&state.db.pool)
            .await?;
    }
    if let Some(value) = req.disk_mb {
        sqlx::query(&state.db.sql("UPDATE nodes SET disk_mb = ? WHERE id = ?"))
            .bind(value.max(0))
            .bind(&id)
            .execute(&state.db.pool)
            .await?;
    }
    if let Some(value) = req.port_range_start {
        sqlx::query(&state.db.sql("UPDATE nodes SET port_range_start = ? WHERE id = ?"))
            .bind(value as i64)
            .bind(&id)
            .execute(&state.db.pool)
            .await?;
    }
    if let Some(value) = req.port_range_end {
        sqlx::query(&state.db.sql("UPDATE nodes SET port_range_end = ? WHERE id = ?"))
            .bind(value as i64)
            .bind(&id)
            .execute(&state.db.pool)
            .await?;
    }
    if let Some(value) = req.maintenance {
        sqlx::query(&state.db.sql("UPDATE nodes SET maintenance = ? WHERE id = ?"))
            .bind(if value { 1i64 } else { 0i64 })
            .bind(&id)
            .execute(&state.db.pool)
            .await?;
    }
    if let Some(value) = req.sftp_port {
        sqlx::query(&state.db.sql("UPDATE nodes SET sftp_port = ? WHERE id = ?"))
            .bind(value as i64)
            .bind(&id)
            .execute(&state.db.pool)
            .await?;
    }
    sqlx::query(&state.db.sql("UPDATE nodes SET updated_at = ? WHERE id = ?"))
        .bind(now_ms())
        .bind(&id)
        .execute(&state.db.pool)
        .await?;
    let node = load_node(&state.db, &id)
        .await?
        .ok_or_else(|| ApiError::not_found("Node not found"))?;
    if let Ok(runtime) = crate::state::build_runtime(&state.config, &state.secrets, &node).await {
        state.runtimes.set(node.id.clone(), runtime);
    }
    Ok(Json(
        json!({ "node": serde_json::to_value(node.view(0, 0, 0)).unwrap_or_default() }),
    ))
}

async fn delete_node(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    auth.require_admin()?;
    let node = load_node(&state.db, &id)
        .await?
        .ok_or_else(|| ApiError::not_found("Node not found"))?;
    if node.is_local {
        return Err(ApiError::bad_request("The local node cannot be deleted"));
    }
    let count: i64 = sqlx::query(
        &state
            .db
            .sql("SELECT COUNT(*) AS count FROM servers WHERE node_id = ?"),
    )
    .bind(&id)
    .fetch_optional(&state.db.pool)
    .await?
    .map(|r| get_i64(&r, "count"))
    .unwrap_or(0);
    if count > 0 {
        return Err(ApiError::conflict(
            "Move or delete the servers on this node first",
        ));
    }
    sqlx::query(&state.db.sql("DELETE FROM allocations WHERE node_id = ?"))
        .bind(&id)
        .execute(&state.db.pool)
        .await?;
    sqlx::query(&state.db.sql("DELETE FROM nodes WHERE id = ?"))
        .bind(&id)
        .execute(&state.db.pool)
        .await?;
    state.runtimes.remove(&id);
    crate::audit::record(
        &state.db,
        Some(&auth.user.id),
        &auth.user.username,
        "node.delete",
        &id,
        None,
        json!({}),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

async fn rotate_node(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    auth.require_admin()?;
    let _existing = load_node(&state.db, &id)
        .await?
        .ok_or_else(|| ApiError::not_found("Node not found"))?;
    let (token, secret) = crate::nodes::generate_node_credentials();
    sqlx::query(
        &state
            .db
            .sql("UPDATE nodes SET token_enc = ?, secret_enc = ?, updated_at = ? WHERE id = ?"),
    )
    .bind(state.secrets.seal(&token).map_err(ApiError::internal)?)
    .bind(state.secrets.seal(&secret).map_err(ApiError::internal)?)
    .bind(now_ms())
    .bind(&id)
    .execute(&state.db.pool)
    .await?;
    let node = load_node(&state.db, &id)
        .await?
        .ok_or_else(|| ApiError::not_found("Node not found"))?;
    if let Ok(runtime) = crate::state::build_runtime(&state.config, &state.secrets, &node).await {
        state.runtimes.set(node.id.clone(), runtime);
    }
    crate::audit::record(
        &state.db,
        Some(&auth.user.id),
        &auth.user.username,
        "node.rotate_credentials",
        &id,
        None,
        json!({}),
    )
    .await;
    Ok(Json(
        json!({ "credentials": { "token": token, "secret": secret } }),
    ))
}

async fn test_node(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    auth.require_admin()?;
    let runtime = state
        .runtimes
        .get(&id)
        .map_err(|e| ApiError::unavailable(e.to_string()))?;
    match tokio::time::timeout(std::time::Duration::from_secs(15), runtime.ping()).await {
        Ok(Ok(())) => Ok(Json(json!({ "online": true, "detail": runtime.description() }))),
        Ok(Err(err)) => Ok(Json(json!({ "online": false, "detail": err.to_string() }))),
        Err(_) => Ok(Json(json!({ "online": false, "detail": "connection timed out" }))),
    }
}

async fn list_allocations(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    auth.require_admin()?;
    let rows = sqlx::query(
        &state
            .db
            .sql("SELECT * FROM allocations WHERE node_id = ? ORDER BY port ASC"),
    )
    .bind(&id)
    .fetch_all(&state.db.pool)
    .await?;
    let allocations: Vec<_> = rows.iter().map(crate::models::Allocation::from_row).collect();
    Ok(Json(json!({ "allocations": allocations })))
}

#[derive(Deserialize)]
struct CreateAllocationRequest {
    #[serde(default)]
    ip: Option<String>,
    port: u16,
    #[serde(default)]
    alias: Option<String>,
}

async fn create_allocation(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
    Json(req): Json<CreateAllocationRequest>,
) -> ApiResult<Json<Value>> {
    auth.require_admin()?;
    let node = load_node(&state.db, &id)
        .await?
        .ok_or_else(|| ApiError::not_found("Node not found"))?;
    if req.port < 1024 {
        return Err(ApiError::bad_request("Ports below 1024 are not allowed"));
    }
    let ip = req.ip.clone().unwrap_or_else(|| node.fqdn.clone());
    validate_fqdn(&ip)?;
    let existing: Option<AnyRow> = sqlx::query(
        &state
            .db
            .sql("SELECT id FROM allocations WHERE node_id = ? AND ip = ? AND port = ?"),
    )
    .bind(&id)
    .bind(&ip)
    .bind(req.port as i64)
    .fetch_optional(&state.db.pool)
    .await?;
    if existing.is_some() {
        return Err(ApiError::conflict("That allocation already exists"));
    }
    let allocation_id = uuid::Uuid::new_v4().to_string();
    sqlx::query(&state.db.sql(
        "INSERT INTO allocations (id, node_id, ip, port, is_primary, alias, created_at) VALUES (?, ?, ?, ?, 0, ?, ?)",
    ))
    .bind(&allocation_id)
    .bind(&id)
    .bind(&ip)
    .bind(req.port as i64)
    .bind(req.alias.as_deref().map(|a| a.chars().take(80).collect::<String>()))
    .bind(now_ms())
    .execute(&state.db.pool)
    .await?;
    Ok(Json(json!({
        "allocation": { "id": allocation_id, "node_id": id, "ip": ip, "port": req.port, "server_id": null, "is_primary": false, "alias": req.alias }
    })))
}

async fn delete_allocation(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    auth.require_admin()?;
    let row: Option<AnyRow> = sqlx::query(&state.db.sql("SELECT * FROM allocations WHERE id = ?"))
        .bind(&id)
        .fetch_optional(&state.db.pool)
        .await?;
    let allocation = row
        .map(|r| crate::models::Allocation::from_row(&r))
        .ok_or_else(|| ApiError::not_found("Allocation not found"))?;
    if allocation.server_id.is_some() {
        return Err(ApiError::conflict("That allocation is assigned to a server"));
    }
    sqlx::query(&state.db.sql("DELETE FROM allocations WHERE id = ?"))
        .bind(&id)
        .execute(&state.db.pool)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

// ---------------------------------------------------------------------------
// Eggs
// ---------------------------------------------------------------------------

async fn list_eggs(State(state): State<AppState>, _auth: AuthUser) -> Json<Value> {
    Json(json!({ "eggs": state.eggs.list() }))
}

async fn get_egg(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let egg = state
        .eggs
        .get(&id)
        .ok_or_else(|| ApiError::not_found("Egg not found"))?;
    Ok(Json(json!({ "egg": egg })))
}

async fn import_egg(
    State(state): State<AppState>,
    auth: AuthUser,
    Query(params): Query<std::collections::HashMap<String, String>>,
    Json(egg): Json<crate::eggs::Egg>,
) -> ApiResult<Json<Value>> {
    auth.require_admin()?;
    let overwrite = params.get("overwrite").map(|v| v == "true").unwrap_or(false);
    state
        .eggs
        .import(egg.clone(), overwrite)
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    crate::audit::record(
        &state.db,
        Some(&auth.user.id),
        &auth.user.username,
        "egg.import",
        &egg.id,
        None,
        json!({ "overwrite": overwrite }),
    )
    .await;
    Ok(Json(json!({ "egg": egg })))
}

async fn delete_egg(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    auth.require_admin()?;
    let count: i64 = sqlx::query(
        &state
            .db
            .sql("SELECT COUNT(*) AS count FROM servers WHERE egg_id = ?"),
    )
    .bind(&id)
    .fetch_optional(&state.db.pool)
    .await?
    .map(|r| get_i64(&r, "count"))
    .unwrap_or(0);
    if count > 0 {
        return Err(ApiError::conflict("Servers still use this egg"));
    }
    state.eggs.delete(&id).map_err(|e| ApiError::internal(e))?;
    crate::audit::record(
        &state.db,
        Some(&auth.user.id),
        &auth.user.username,
        "egg.delete",
        &id,
        None,
        json!({}),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

async fn export_egg(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let egg = state
        .eggs
        .get(&id)
        .ok_or_else(|| ApiError::not_found("Egg not found"))?;
    let body = serde_json::to_vec_pretty(&egg).map_err(ApiError::internal)?;
    let mut response = Response::new(axum::body::Body::from(body));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{}.json\"", egg.id)).unwrap(),
    );
    Ok(response)
}

// ---------------------------------------------------------------------------
// Settings, audit, stats, updates
// ---------------------------------------------------------------------------

async fn get_settings(State(state): State<AppState>, auth: AuthUser) -> ApiResult<Json<Value>> {
    auth.require_admin()?;
    let panel_name = state
        .db
        .setting("panel_name")
        .await?
        .unwrap_or_else(|| state.config.panel_name.clone());
    let registration = state
        .db
        .setting_bool("allow_registration", state.config.allow_registration)
        .await;
    let user_servers = state
        .db
        .setting_bool("allow_user_servers", state.config.allow_user_servers)
        .await;
    let channel = state
        .db
        .setting("update_channel")
        .await?
        .unwrap_or_else(|| state.config.updates.channel.clone());
    let auto_update = state
        .db
        .setting_bool("auto_update", state.config.updates.auto_apply)
        .await;
    Ok(Json(json!({
        "panel_name": panel_name,
        "allow_registration": registration,
        "allow_user_servers": user_servers,
        "update_channel": channel,
        "auto_update": auto_update,
        "version": VERSION,
        "database": format!("{:?}", state.db.backend),
        "docker_enabled": state.config.docker.enabled,
        "signing_configured": !state.config.updates.public_key.trim().is_empty(),
        "tls": state.config.tls_enabled(),
        "update_repo": state.config.updates.repo,
    })))
}

#[derive(Deserialize)]
struct SettingsPatch {
    #[serde(default)]
    panel_name: Option<String>,
    #[serde(default)]
    allow_registration: Option<bool>,
    #[serde(default)]
    allow_user_servers: Option<bool>,
    #[serde(default)]
    update_channel: Option<String>,
    #[serde(default)]
    auto_update: Option<bool>,
}

async fn update_settings(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(req): Json<SettingsPatch>,
) -> ApiResult<Json<Value>> {
    auth.require_admin()?;
    if let Some(name) = &req.panel_name {
        if name.trim().is_empty() || name.len() > 60 {
            return Err(ApiError::bad_request("Panel name must be 1-60 characters"));
        }
        state.db.set_setting("panel_name", name.trim()).await?;
    }
    if let Some(value) = req.allow_registration {
        state
            .db
            .set_setting("allow_registration", if value { "true" } else { "false" })
            .await?;
    }
    if let Some(value) = req.allow_user_servers {
        state
            .db
            .set_setting("allow_user_servers", if value { "true" } else { "false" })
            .await?;
    }
    if let Some(channel) = &req.update_channel {
        if !matches!(channel.as_str(), "stable" | "prerelease") {
            return Err(ApiError::bad_request(
                "Update channel must be stable or prerelease",
            ));
        }
        state.db.set_setting("update_channel", channel).await?;
    }
    if let Some(value) = req.auto_update {
        state
            .db
            .set_setting("auto_update", if value { "true" } else { "false" })
            .await?;
    }
    crate::audit::record(
        &state.db,
        Some(&auth.user.id),
        &auth.user.username,
        "settings.update",
        "panel",
        None,
        json!({}),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

async fn list_audit(
    State(state): State<AppState>,
    auth: AuthUser,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    auth.require_admin()?;
    let limit = params
        .get("limit")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(50)
        .clamp(1, 200);
    let offset = params
        .get("offset")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0)
        .max(0);
    let rows = sqlx::query(
        &state
            .db
            .sql("SELECT * FROM audit_logs ORDER BY created_at DESC LIMIT ? OFFSET ?"),
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db.pool)
    .await?;
    let logs: Vec<_> = rows.iter().map(crate::models::AuditLog::from_row).collect();
    Ok(Json(json!({ "logs": logs, "limit": limit, "offset": offset })))
}

async fn admin_stats(State(state): State<AppState>, auth: AuthUser) -> ApiResult<Json<Value>> {
    auth.require_admin()?;
    async fn count(state: &AppState, query: &str) -> i64 {
        sqlx::query(&state.db.sql(query))
            .fetch_optional(&state.db.pool)
            .await
            .ok()
            .flatten()
            .map(|r| get_i64(&r, "count"))
            .unwrap_or(0)
    }
    let users = count(&state, "SELECT COUNT(*) AS count FROM users").await;
    let servers = count(&state, "SELECT COUNT(*) AS count FROM servers").await;
    let running = count(
        &state,
        "SELECT COUNT(*) AS count FROM servers WHERE status = 'running'",
    )
    .await;
    let nodes = count(&state, "SELECT COUNT(*) AS count FROM nodes").await;
    let backups = count(&state, "SELECT COUNT(*) AS count FROM backups").await;
    let memory: i64 = sqlx::query(
        &state
            .db
            .sql("SELECT COALESCE(SUM(memory_mb),0) AS total FROM servers"),
    )
    .fetch_optional(&state.db.pool)
    .await
    .ok()
    .flatten()
    .map(|r| get_i64(&r, "total"))
    .unwrap_or(0);
    Ok(Json(json!({
        "users": users,
        "servers": servers,
        "running_servers": running,
        "nodes": nodes,
        "backups": backups,
        "allocated_memory_mb": memory,
        "version": VERSION,
    })))
}

async fn update_status(State(state): State<AppState>, auth: AuthUser) -> ApiResult<Json<Value>> {
    auth.require_admin()?;
    let status = crate::updater::status(&state).await;
    Ok(Json(serde_json::to_value(status).unwrap_or_default()))
}

async fn update_apply(
    State(state): State<AppState>,
    auth: AuthUser,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    auth.require_admin()?;
    let restart = params.get("restart").map(|v| v == "true").unwrap_or(false);
    let result = crate::updater::apply_latest(&state, restart).await?;
    Ok(Json(result))
}

async fn update_rollback(State(state): State<AppState>, auth: AuthUser) -> ApiResult<Json<Value>> {
    auth.require_admin()?;
    let result = crate::updater::rollback(&state).await?;
    Ok(Json(result))
}

async fn update_history(State(state): State<AppState>, auth: AuthUser) -> ApiResult<Json<Value>> {
    auth.require_admin()?;
    let rows = sqlx::query(
        &state
            .db
            .sql("SELECT * FROM updates ORDER BY created_at DESC LIMIT 50"),
    )
    .fetch_all(&state.db.pool)
    .await?;
    let updates: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "id": get_str(row, "id"),
                "version": get_str(row, "version"),
                "channel": get_str(row, "channel"),
                "status": get_str(row, "status"),
                "log": get_str(row, "log"),
                "created_at": get_i64(row, "created_at"),
                "completed_at": get_opt_i64(row, "completed_at"),
            })
        })
        .collect();
    Ok(Json(json!({ "updates": updates })))
}
