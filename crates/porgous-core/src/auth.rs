//! Authentication, sessions, TOTP, API keys and authorization.

use crate::crypto::{
    constant_time_eq, generate_api_key, hash_password, random_token, sha256_hex, verify_api_key,
    verify_password,
};
use crate::db::{get_i64, get_opt_i64, get_opt_str, get_str, now_ms, Db};
use crate::error::{ApiError, ApiResult};
use crate::models::{Server, ServerAccess, User, ALL_SERVER_PERMISSIONS};
use crate::state::AppState;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, Method};
use data_encoding::BASE32_NOPAD;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use sqlx::any::AnyRow;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Mutex;

pub const SESSION_COOKIE: &str = "porgous_session";
pub const CSRF_HEADER: &str = "x-csrf-token";
const TOTP_STEP: u64 = 30;
const TOTP_DIGITS: u32 = 6;

#[derive(Debug, Clone)]
pub enum AuthMethod {
    Session { id: String, csrf: String },
    ApiKey { id: String, scopes: Vec<String> },
}

#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user: User,
    pub method: AuthMethod,
}

impl AuthUser {
    pub fn is_admin(&self) -> bool {
        self.user.root_admin
    }

    pub fn session(&self) -> Option<(&str, &str)> {
        match &self.method {
            AuthMethod::Session { id, csrf } => Some((id, csrf)),
            _ => None,
        }
    }

    pub fn api_key_scopes(&self) -> Option<&[String]> {
        match &self.method {
            AuthMethod::ApiKey { scopes, .. } => Some(scopes),
            _ => None,
        }
    }

    /// Check an API key scope. Cookie sessions are always allowed.
    pub fn has_scope(&self, scope: &str) -> bool {
        let Some(scopes) = self.api_key_scopes() else {
            return true;
        };
        scopes.iter().any(|s| {
            s == "*"
                || s == scope
                || (s.ends_with('*') && scope.starts_with(s.trim_end_matches('*')))
                || (s == "admin" && scope.starts_with("admin"))
        })
    }

    pub fn require_scope(&self, scope: &str) -> ApiResult<()> {
        if self.has_scope(scope) {
            Ok(())
        } else {
            Err(ApiError::forbidden(format!(
                "API key is missing the '{scope}' scope"
            )))
        }
    }

    /// Root administration. API keys additionally need the `admin` scope.
    pub fn require_admin(&self) -> ApiResult<()> {
        if !self.user.root_admin {
            return Err(ApiError::forbidden("Administrator access required"));
        }
        self.require_scope("admin")
    }
}

fn client_ip(parts: &Parts) -> Option<String> {
    if let Some(addr) = parts.extensions.get::<axum::extract::ConnectInfo<SocketAddr>>() {
        return Some(addr.0.ip().to_string());
    }
    parts
        .headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|v| v.trim().to_string())
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let ip = client_ip(parts);
        let bearer = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(|v| v.trim().to_string());

        if let Some(token) = bearer {
            if token.starts_with("porgous_") {
                let auth = authenticate_api_key(state, &token, ip.as_deref()).await?;
                return Ok(auth);
            }
            let (user, session) = authenticate_session(state, &token, ip.as_deref()).await?;
            ensure_csrf(parts, &session.csrf_token)?;
            return Ok(AuthUser {
                user,
                method: AuthMethod::Session {
                    id: session.id,
                    csrf: session.csrf_token,
                },
            });
        }

        let token = cookie_value(&parts.headers, SESSION_COOKIE)
            .ok_or_else(|| ApiError::unauthorized("Authentication required"))?;
        let (user, session) = authenticate_session(state, &token, ip.as_deref()).await?;
        ensure_csrf(parts, &session.csrf_token)?;
        Ok(AuthUser {
            user,
            method: AuthMethod::Session {
                id: session.id,
                csrf: session.csrf_token,
            },
        })
    }
}

fn ensure_csrf(parts: &Parts, expected: &str) -> ApiResult<()> {
    if matches!(parts.method, Method::GET | Method::HEAD | Method::OPTIONS) {
        return Ok(());
    }
    let provided = parts
        .headers
        .get(CSRF_HEADER)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| ApiError::forbidden("Missing CSRF token"))?;
    if constant_time_eq(provided.trim().as_bytes(), expected.as_bytes()) {
        Ok(())
    } else {
        Err(ApiError::forbidden("Invalid CSRF token"))
    }
}

#[derive(Debug, Clone)]
pub struct SessionRecord {
    pub id: String,
    pub csrf_token: String,
    pub expires_at: i64,
}

async fn authenticate_session(
    state: &AppState,
    token: &str,
    _ip: Option<&str>,
) -> ApiResult<(User, SessionRecord)> {
    let hash = sha256_hex(token.as_bytes());
    let row: Option<AnyRow> = sqlx::query(&state.db.sql(
        "SELECT u.*, s.id AS session_id, s.csrf_token AS session_csrf, s.expires_at AS session_expires
         FROM sessions s JOIN users u ON u.id = s.user_id WHERE s.token_hash = ?",
    ))
    .bind(&hash)
    .fetch_optional(&state.db.pool)
    .await?;
    let row = row.ok_or_else(|| ApiError::unauthorized("Invalid or expired session"))?;
    let expires_at = get_i64(&row, "session_expires");
    if expires_at <= now_ms() {
        return Err(ApiError::unauthorized("Session expired"));
    }
    let user = User::from_row(&row);
    if user.suspended {
        return Err(ApiError::forbidden("This account is suspended"));
    }
    Ok((
        user,
        SessionRecord {
            id: get_str(&row, "session_id"),
            csrf_token: get_str(&row, "session_csrf"),
            expires_at,
        },
    ))
}

async fn authenticate_api_key(state: &AppState, key: &str, ip: Option<&str>) -> ApiResult<AuthUser> {
    let prefix =
        crate::crypto::api_key_prefix(key).ok_or_else(|| ApiError::unauthorized("Malformed API key"))?;
    let row: Option<AnyRow> = sqlx::query(
        &state
            .db
            .sql("SELECT id, user_id, key_hash, scopes, expires_at FROM api_keys WHERE prefix = ?"),
    )
    .bind(prefix)
    .fetch_optional(&state.db.pool)
    .await?;
    let row = row.ok_or_else(|| ApiError::unauthorized("Invalid API key"))?;
    if !verify_api_key(key, &get_str(&row, "key_hash")) {
        return Err(ApiError::unauthorized("Invalid API key"));
    }
    if let Some(expires) = get_opt_i64(&row, "expires_at") {
        if expires <= now_ms() {
            return Err(ApiError::unauthorized("API key expired"));
        }
    }
    let key_id = get_str(&row, "id");
    let user_id = get_str(&row, "user_id");
    let user_row: Option<AnyRow> = sqlx::query(&state.db.sql("SELECT * FROM users WHERE id = ?"))
        .bind(&user_id)
        .fetch_optional(&state.db.pool)
        .await?;
    let user = User::from_row(&user_row.ok_or_else(|| ApiError::unauthorized("Invalid API key"))?);
    if user.suspended {
        return Err(ApiError::forbidden("This account is suspended"));
    }
    let scopes: Vec<String> = serde_json::from_str(&get_str(&row, "scopes")).unwrap_or_default();
    let db = state.db.clone();
    let id = key_id.clone();
    let ip = ip.map(|s| s.to_string());
    tokio::spawn(async move {
        let _ = sqlx::query(&db.sql("UPDATE api_keys SET last_used_at = ? WHERE id = ?"))
            .bind(now_ms())
            .bind(id)
            .execute(&db.pool)
            .await;
        let _ = ip;
    });
    Ok(AuthUser {
        user,
        method: AuthMethod::ApiKey { id: key_id, scopes },
    })
}

pub fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_string())
}

pub fn session_cookie(token: &str, max_age_secs: i64, secure: bool) -> String {
    format!(
        "{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age_secs}{}",
        if secure { "; Secure" } else { "" }
    )
}

pub fn clear_session_cookie(secure: bool) -> String {
    format!(
        "{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{}",
        if secure { "; Secure" } else { "" }
    )
}

// ---------------------------------------------------------------------------
// Users and sessions
// ---------------------------------------------------------------------------

pub fn normalize_email(email: &str) -> ApiResult<String> {
    let email = email.trim().to_lowercase();
    if email.len() < 5 || email.len() > 254 || !email.contains('@') || email.contains(char::is_whitespace) {
        return Err(ApiError::bad_request("Invalid email address"));
    }
    let (local, domain) = email.split_once('@').unwrap_or(("", ""));
    if local.is_empty() || !domain.contains('.') || domain.starts_with('.') || domain.ends_with('.') {
        return Err(ApiError::bad_request("Invalid email address"));
    }
    Ok(email)
}

pub fn validate_username(username: &str) -> ApiResult<String> {
    let username = username.trim();
    if username.len() < 3 || username.len() > 32 {
        return Err(ApiError::bad_request("Username must be 3-32 characters"));
    }
    if !username
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    {
        return Err(ApiError::bad_request(
            "Username may only contain letters, numbers, dot, dash and underscore",
        ));
    }
    Ok(username.to_string())
}

pub async fn create_user(
    db: &Db,
    email: &str,
    username: &str,
    password: &str,
    root_admin: bool,
) -> ApiResult<User> {
    let email = normalize_email(email)?;
    let username = validate_username(username)?;
    let hash = hash_password(password).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let existing: Option<AnyRow> =
        sqlx::query(&db.sql("SELECT id FROM users WHERE email = ? OR username = ?"))
            .bind(&email)
            .bind(&username)
            .fetch_optional(&db.pool)
            .await?;
    if existing.is_some() {
        return Err(ApiError::conflict(
            "A user with that email or username already exists",
        ));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_ms();
    sqlx::query(&db.sql(
        "INSERT INTO users (id, email, username, password_hash, role, root_admin, suspended, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, 0, ?, ?)",
    ))
    .bind(&id)
    .bind(&email)
    .bind(&username)
    .bind(&hash)
    .bind(if root_admin { "admin" } else { "user" })
    .bind(if root_admin { 1i64 } else { 0i64 })
    .bind(now)
    .bind(now)
    .execute(&db.pool)
    .await?;
    Ok(User {
        id,
        email,
        username,
        password_hash: hash,
        role: if root_admin { "admin".into() } else { "user".into() },
        root_admin,
        suspended: false,
        totp_secret_enc: None,
        totp_enabled: false,
        totp_recovery: None,
        created_at: now,
        updated_at: now,
        last_login_at: None,
    })
}

pub async fn find_user_by_email(db: &Db, email: &str) -> ApiResult<Option<User>> {
    let email = email.trim().to_lowercase();
    let row: Option<AnyRow> = sqlx::query(&db.sql("SELECT * FROM users WHERE email = ?"))
        .bind(email)
        .fetch_optional(&db.pool)
        .await?;
    Ok(row.map(|r| User::from_row(&r)))
}

pub async fn find_user(db: &Db, id: &str) -> ApiResult<Option<User>> {
    let row: Option<AnyRow> = sqlx::query(&db.sql("SELECT * FROM users WHERE id = ?"))
        .bind(id)
        .fetch_optional(&db.pool)
        .await?;
    Ok(row.map(|r| User::from_row(&r)))
}

/// Tracks failed logins per account to slow down credential stuffing.
#[derive(Debug, Default)]
pub struct LoginGuard {
    failures: Mutex<HashMap<String, (u32, i64)>>,
}

impl LoginGuard {
    pub fn locked(&self, email: &str) -> Option<i64> {
        let map = self.failures.lock().unwrap();
        let (count, until) = map.get(email)?;
        if *count >= 5 && *until > now_ms() {
            Some(*until)
        } else {
            None
        }
    }

    pub fn fail(&self, email: &str) {
        let mut map = self.failures.lock().unwrap();
        let entry = map.entry(email.to_string()).or_insert((0, 0));
        entry.0 += 1;
        if entry.0 >= 5 {
            entry.1 = now_ms() + 5 * 60 * 1000;
            entry.0 = 0;
        }
    }

    pub fn success(&self, email: &str) {
        self.failures.lock().unwrap().remove(email);
    }
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
    #[serde(default)]
    pub totp: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct LoginSuccess {
    pub user: crate::models::UserView,
    pub csrf_token: String,
}

pub async fn login(
    state: &AppState,
    req: &LoginRequest,
    ip: Option<&str>,
    user_agent: Option<&str>,
) -> ApiResult<(LoginSuccess, String)> {
    let email = req.email.trim().to_lowercase();
    if let Some(until) = state.login_guard.locked(&email) {
        let secs = ((until - now_ms()) / 1000).max(1);
        return Err(ApiError::too_many(format!(
            "Too many failed attempts. Try again in {secs} seconds"
        )));
    }
    let user = match find_user_by_email(&state.db, &email).await? {
        Some(user) => user,
        None => {
            // Burn comparable time to avoid account enumeration through timing.
            let _ = verify_password(&req.password, DUMMY_HASH);
            state.login_guard.fail(&email);
            return Err(ApiError::unauthorized("Invalid email or password"));
        }
    };
    if !verify_password(&req.password, &user.password_hash) {
        state.login_guard.fail(&email);
        return Err(ApiError::unauthorized("Invalid email or password"));
    }
    if user.suspended {
        return Err(ApiError::forbidden("This account is suspended"));
    }
    if user.totp_enabled {
        let code = req.totp.as_deref().unwrap_or("");
        let mut ok = verify_totp_for_user(state, &user, code)?;
        if !ok {
            ok = consume_recovery_code(state, &user, code).await?;
        }
        if !ok {
            state.login_guard.fail(&email);
            return Err(ApiError::unauthorized("Invalid email or password"));
        }
    }
    state.login_guard.success(&email);
    let token = random_token(32);
    let csrf = random_token(24);
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_ms();
    let expires = now + state.config.session_ttl_hours * 3600 * 1000;
    sqlx::query(&state.db.sql(
        "INSERT INTO sessions (id, user_id, token_hash, csrf_token, ip, user_agent, created_at, expires_at, last_seen_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    ))
    .bind(&id)
    .bind(&user.id)
    .bind(sha256_hex(token.as_bytes()))
    .bind(&csrf)
    .bind(ip)
    .bind(user_agent)
    .bind(now)
    .bind(expires)
    .bind(now)
    .execute(&state.db.pool)
    .await?;
    sqlx::query(
        &state
            .db
            .sql("UPDATE users SET last_login_at = ?, updated_at = ? WHERE id = ?"),
    )
    .bind(now)
    .bind(now)
    .bind(&user.id)
    .execute(&state.db.pool)
    .await?;
    // Opportunistic cleanup keeps the sessions table small.
    let _ = sqlx::query(&state.db.sql("DELETE FROM sessions WHERE expires_at <= ?"))
        .bind(now)
        .execute(&state.db.pool)
        .await;
    Ok((
        LoginSuccess {
            user: user.view(),
            csrf_token: csrf,
        },
        token,
    ))
}

pub async fn logout(db: &Db, session_id: &str) -> ApiResult<()> {
    sqlx::query(&db.sql("DELETE FROM sessions WHERE id = ?"))
        .bind(session_id)
        .execute(&db.pool)
        .await?;
    Ok(())
}

// A valid Argon2id hash of a random password, used for constant-time failures.
const DUMMY_HASH: &str =
    "$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHRzb21lc2FsdA$RdescudvJCsgt3ub+b+dWRWJTmaaJObG";

// ---------------------------------------------------------------------------
// TOTP
// ---------------------------------------------------------------------------

pub fn generate_totp_secret() -> String {
    BASE32_NOPAD.encode(&crate::crypto::random_bytes(20))
}

pub fn totp_code(secret_b32: &str, unix_secs: u64) -> ApiResult<u32> {
    let key = BASE32_NOPAD
        .decode(secret_b32.trim().to_uppercase().as_bytes())
        .map_err(|_| ApiError::internal("invalid TOTP secret"))?;
    let counter = unix_secs / TOTP_STEP;
    let mut mac = Hmac::<Sha1>::new_from_slice(&key).map_err(|_| ApiError::internal("hmac init"))?;
    mac.update(&counter.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let offset = (digest[19] & 0x0f) as usize;
    let binary = ((digest[offset] as u32 & 0x7f) << 24)
        | ((digest[offset + 1] as u32) << 16)
        | ((digest[offset + 2] as u32) << 8)
        | digest[offset + 3] as u32;
    Ok(binary % 10u32.pow(TOTP_DIGITS))
}

pub fn verify_totp(secret_b32: &str, code: &str, now_secs: u64) -> bool {
    let code = code.trim().replace(' ', "");
    if code.len() != TOTP_DIGITS as usize {
        return false;
    }
    for drift in [-1i64, 0, 1] {
        let at = (now_secs as i64 + drift * TOTP_STEP as i64).max(0) as u64;
        if let Ok(expected) = totp_code(secret_b32, at) {
            let expected = format!("{expected:06}");
            if constant_time_eq(expected.as_bytes(), code.as_bytes()) {
                return true;
            }
        }
    }
    false
}

pub fn otpauth_url(issuer: &str, account: &str, secret: &str) -> String {
    format!(
        "otpauth://totp/{issuer}:{account}?secret={secret}&issuer={issuer}&algorithm=SHA1&digits={TOTP_DIGITS}&period={TOTP_STEP}"
    )
}

pub struct TotpSetup {
    pub secret: String,
    pub url: String,
}

pub async fn begin_totp_setup(state: &AppState, user: &User) -> ApiResult<TotpSetup> {
    let secret = generate_totp_secret();
    let enc = state.secrets.seal(&secret).map_err(ApiError::internal)?;
    // Storing the pending secret is idempotent; it is only trusted after enable.
    sqlx::query(
        &state
            .db
            .sql("UPDATE users SET totp_secret = ?, totp_enabled = 0, updated_at = ? WHERE id = ?"),
    )
    .bind(&enc)
    .bind(now_ms())
    .bind(&user.id)
    .execute(&state.db.pool)
    .await?;
    let account = user.email.clone();
    Ok(TotpSetup {
        url: otpauth_url("Porgous", &account, &secret),
        secret,
    })
}

pub async fn enable_totp(state: &AppState, user_id: &str, code: &str) -> ApiResult<Vec<String>> {
    let row: Option<AnyRow> = sqlx::query(&state.db.sql("SELECT totp_secret FROM users WHERE id = ?"))
        .bind(user_id)
        .fetch_optional(&state.db.pool)
        .await?;
    let row = row.ok_or_else(|| ApiError::not_found("User not found"))?;
    let enc =
        get_opt_str(&row, "totp_secret").ok_or_else(|| ApiError::bad_request("Start 2FA setup first"))?;
    let secret = state.secrets.open(&enc).map_err(ApiError::internal)?;
    if !verify_totp(&secret, code, chrono::Utc::now().timestamp() as u64) {
        return Err(ApiError::bad_request("Invalid authenticator code"));
    }
    let recovery = generate_recovery_codes();
    let hashes: Vec<String> = recovery
        .iter()
        .map(|c| sha256_hex(c.replace('-', "").as_bytes()))
        .collect();
    sqlx::query(
        &state
            .db
            .sql("UPDATE users SET totp_enabled = 1, totp_recovery = ?, updated_at = ? WHERE id = ?"),
    )
    .bind(serde_json::to_string(&hashes).unwrap_or_else(|_| "[]".into()))
    .bind(now_ms())
    .bind(user_id)
    .execute(&state.db.pool)
    .await?;
    Ok(recovery)
}

pub async fn disable_totp(state: &AppState, user_id: &str, password: &str, code: &str) -> ApiResult<()> {
    let user = find_user(&state.db, user_id)
        .await?
        .ok_or_else(|| ApiError::not_found("User not found"))?;
    if !verify_password(password, &user.password_hash) {
        return Err(ApiError::unauthorized("Invalid password"));
    }
    if user.totp_enabled && !verify_totp_for_user(state, &user, code)? {
        return Err(ApiError::bad_request("Invalid authenticator code"));
    }
    sqlx::query(&state.db.sql(
        "UPDATE users SET totp_enabled = 0, totp_secret = NULL, totp_recovery = NULL, updated_at = ? WHERE id = ?",
    ))
    .bind(now_ms())
    .bind(user_id)
    .execute(&state.db.pool)
    .await?;
    Ok(())
}

pub fn verify_totp_for_user(state: &AppState, user: &User, code: &str) -> ApiResult<bool> {
    let Some(enc) = user.totp_secret_enc.as_ref() else {
        return Ok(false);
    };
    let secret = state.secrets.open(enc).map_err(ApiError::internal)?;
    Ok(verify_totp(&secret, code, chrono::Utc::now().timestamp() as u64))
}

fn generate_recovery_codes() -> Vec<String> {
    (0..8)
        .map(|_| {
            let raw = random_token(6).to_uppercase().replace(['-', '_'], "X");
            let raw = raw.chars().take(8).collect::<String>();
            format!("{}-{}", &raw[..4.min(raw.len())], &raw[4.min(raw.len())..])
        })
        .collect()
}

pub async fn consume_recovery_code(state: &AppState, user: &User, code: &str) -> ApiResult<bool> {
    let Some(stored) = user.totp_recovery.as_ref() else {
        return Ok(false);
    };
    let normalized = code.trim().replace('-', "").to_uppercase();
    if normalized.len() < 6 {
        return Ok(false);
    }
    let target = sha256_hex(normalized.as_bytes());
    let mut hashes: Vec<String> = serde_json::from_str(stored).unwrap_or_default();
    let Some(pos) = hashes
        .iter()
        .position(|h| constant_time_eq(h.as_bytes(), target.as_bytes()))
    else {
        return Ok(false);
    };
    hashes.remove(pos);
    sqlx::query(
        &state
            .db
            .sql("UPDATE users SET totp_recovery = ?, updated_at = ? WHERE id = ?"),
    )
    .bind(serde_json::to_string(&hashes).unwrap_or_else(|_| "[]".into()))
    .bind(now_ms())
    .bind(&user.id)
    .execute(&state.db.pool)
    .await?;
    Ok(true)
}

// ---------------------------------------------------------------------------
// API keys
// ---------------------------------------------------------------------------

pub fn valid_api_scopes(scopes: &[String]) -> bool {
    const ALLOWED: &[&str] = &[
        "account:read",
        "account:write",
        "server:read",
        "server:console",
        "server:control",
        "server:files",
        "server:backups",
        "server:schedules",
        "server:admin",
        "admin",
        "admin:read",
        "admin:write",
        "*",
    ];
    scopes.iter().all(|s| ALLOWED.contains(&s.as_str()))
}

pub async fn create_api_key(
    state: &AppState,
    user_id: &str,
    name: &str,
    scopes: &[String],
    expires_at: Option<i64>,
) -> ApiResult<(String, crate::models::ApiKey)> {
    if name.trim().is_empty() || name.len() > 64 {
        return Err(ApiError::bad_request("API key name must be 1-64 characters"));
    }
    if scopes.is_empty() || !valid_api_scopes(scopes) {
        return Err(ApiError::bad_request("Invalid API key scopes"));
    }
    let material = generate_api_key();
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_ms();
    sqlx::query(&state.db.sql(
        "INSERT INTO api_keys (id, user_id, name, key_hash, prefix, scopes, created_at, expires_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    ))
    .bind(&id)
    .bind(user_id)
    .bind(name.trim())
    .bind(&material.hash)
    .bind(&material.prefix)
    .bind(serde_json::to_string(scopes).unwrap_or_else(|_| "[]".into()))
    .bind(now)
    .bind(expires_at)
    .execute(&state.db.pool)
    .await?;
    let view = crate::models::ApiKey {
        id,
        user_id: user_id.to_string(),
        name: name.trim().to_string(),
        prefix: material.prefix,
        scopes: scopes.to_vec(),
        created_at: now,
        last_used_at: None,
        expires_at,
    };
    Ok((material.key, view))
}

// ---------------------------------------------------------------------------
// Authorization
// ---------------------------------------------------------------------------

fn required_scope(permission: &str) -> &'static str {
    if permission.starts_with("console.") {
        "server:console"
    } else if permission.starts_with("control.") {
        "server:control"
    } else if permission.starts_with("files.") {
        "server:files"
    } else if permission.starts_with("backups.") {
        "server:backups"
    } else if permission.starts_with("schedules.") {
        "server:schedules"
    } else {
        "server:admin"
    }
}

/// Load a server and verify that the caller may perform `permission` on it.
pub async fn authorize_server(
    state: &AppState,
    auth: &AuthUser,
    server_id: &str,
    permission: &str,
) -> ApiResult<ServerAccess> {
    auth.require_scope(required_scope(permission))?;
    let row: Option<AnyRow> = sqlx::query(&state.db.sql("SELECT * FROM servers WHERE id = ? OR uuid = ?"))
        .bind(server_id)
        .bind(server_id)
        .fetch_optional(&state.db.pool)
        .await?;
    let server = row
        .map(|r| Server::from_row(&r))
        .ok_or_else(|| ApiError::not_found("Server not found"))?;
    if auth.is_admin() {
        return Ok(ServerAccess {
            server,
            is_owner: true,
            is_admin: true,
            permissions: ALL_SERVER_PERMISSIONS.iter().map(|s| s.to_string()).collect(),
        });
    }
    if server.owner_id == auth.user.id {
        return Ok(ServerAccess {
            server,
            is_owner: true,
            is_admin: false,
            permissions: ALL_SERVER_PERMISSIONS.iter().map(|s| s.to_string()).collect(),
        });
    }
    let sub: Option<AnyRow> = sqlx::query(
        &state
            .db
            .sql("SELECT permissions FROM subusers WHERE server_id = ? AND user_id = ?"),
    )
    .bind(&server.id)
    .bind(&auth.user.id)
    .fetch_optional(&state.db.pool)
    .await?;
    let Some(sub) = sub else {
        return Err(ApiError::forbidden("You do not have access to this server"));
    };
    let permissions: Vec<String> = serde_json::from_str(&get_str(&sub, "permissions")).unwrap_or_default();
    if !permissions.iter().any(|p| p == permission) {
        return Err(ApiError::forbidden(format!("Missing permission '{permission}'")));
    }
    Ok(ServerAccess {
        server,
        is_owner: false,
        is_admin: false,
        permissions,
    })
}

/// Verify that a user exists and can be added as a subuser.
pub async fn ensure_subuser_not_owner(
    db: &Db,
    server_id: &str,
    owner_id: &str,
    user_id: &str,
) -> ApiResult<()> {
    if user_id == owner_id {
        return Err(ApiError::bad_request("The owner already has full access"));
    }
    let exists: Option<AnyRow> = sqlx::query(&db.sql("SELECT id FROM users WHERE id = ?"))
        .bind(user_id)
        .fetch_optional(&db.pool)
        .await?;
    if exists.is_none() {
        return Err(ApiError::not_found("User not found"));
    }
    let dup: Option<AnyRow> =
        sqlx::query(&db.sql("SELECT id FROM subusers WHERE server_id = ? AND user_id = ?"))
            .bind(server_id)
            .bind(user_id)
            .fetch_optional(&db.pool)
            .await?;
    if dup.is_some() {
        return Err(ApiError::conflict("That user is already a subuser"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn totp_matches_rfc_vector() {
        // RFC 6238 test key "12345678901234567890" in base32.
        let secret = BASE32_NOPAD.encode(b"12345678901234567890");
        assert_eq!(totp_code(&secret, 59).unwrap(), 287082);
        assert!(verify_totp(&secret, "287082", 59));
        assert!(!verify_totp(&secret, "287083", 59));
    }

    #[test]
    fn emails_are_validated() {
        assert_eq!(
            normalize_email(" Player@Example.COM ").unwrap(),
            "player@example.com"
        );
        assert!(normalize_email("bad").is_err());
        assert!(normalize_email("a@b").is_err());
        assert!(normalize_email("a b@c.de").is_err());
    }

    #[test]
    fn usernames_are_validated() {
        assert!(validate_username("steve_01").is_ok());
        assert!(validate_username("x").is_err());
        assert!(validate_username("bad name").is_err());
    }

    #[test]
    fn cookie_parser() {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, "a=1; porgous_session=abc; b=2".parse().unwrap());
        assert_eq!(cookie_value(&headers, SESSION_COOKIE).as_deref(), Some("abc"));
        assert!(cookie_value(&headers, "nope").is_none());
    }

    #[test]
    fn api_scopes() {
        assert!(valid_api_scopes(&["server:read".into(), "server:files".into()]));
        assert!(!valid_api_scopes(&["server:everything".into()]));
    }
}
