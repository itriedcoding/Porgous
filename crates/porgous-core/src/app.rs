//! HTTP application assembly.

use crate::api;
use crate::state::{AppState, VERSION};
use axum::body::Body;
use axum::extract::{ConnectInfo, DefaultBodyLimit, OriginalUri, Request, State};
use axum::http::{header, HeaderName, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use rust_embed::RustEmbed;
use std::net::SocketAddr;
use tower_http::trace::TraceLayer;

#[derive(RustEmbed)]
#[folder = "../porgous-panel/assets"]
struct Assets;

pub fn router(state: AppState) -> Router {
    let api = api::router();
    Router::new()
        .route("/health", get(health))
        .nest("/api", api)
        .fallback(get(static_handler))
        .layer(TraceLayer::new_for_http())
        .layer(middleware::from_fn(security_headers))
        .layer(middleware::from_fn_with_state(state.clone(), rate_limit))
        .layer(DefaultBodyLimit::max(state.config.max_upload_bytes as usize))
        .with_state(state)
}

async fn health(State(state): State<AppState>) -> Response {
    let db_ok = sqlx::query("SELECT 1").execute(&state.db.pool).await.is_ok();
    let mut docker_ok = false;
    let mut docker_detail = "unavailable".to_string();
    if let Ok(nodes) = crate::state::load_nodes(&state.db).await {
        if let Some(local) = nodes.iter().find(|n| n.is_local) {
            if let Ok(runtime) = state.runtimes.get(&local.id) {
                docker_detail = runtime.description();
                docker_ok = runtime.ping().await.is_ok();
            }
        }
    }
    let status = if db_ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    let body = Json(serde_json::json!({
        "ok": db_ok,
        "version": VERSION,
        "database": if db_ok { "ok" } else { "error" },
        "docker": docker_ok,
        "docker_detail": docker_detail,
        "uptime_seconds": (crate::db::now_ms() - state.started_at) / 1000,
    }));
    (status, body).into_response()
}

async fn rate_limit(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if !state.config.rate_limit.enabled {
        return next.run(req).await;
    }
    let ip = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let path = req.uri().path().to_string();
    let (bucket, limit) = if path.contains("/auth/login") || path.contains("/auth/register") {
        ("auth", state.config.rate_limit.auth_per_minute)
    } else {
        ("api", state.config.rate_limit.requests_per_minute)
    };
    if !state.rate.check(&format!("{bucket}:{ip}"), limit) {
        let retry = state.rate.retry_after();
        let mut response = (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({ "error": { "code": "rate_limited", "message": "Too many requests" } })),
        )
            .into_response();
        if let Ok(value) = HeaderValue::from_str(&retry.to_string()) {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
        return response;
    }
    next.run(req).await
}

async fn security_headers(req: Request, next: Next) -> Response {
    let mut response = next.run(req).await;
    let headers = response.headers_mut();
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self' ws: wss:; font-src 'self' data:; frame-ancestors 'none'; base-uri 'self'; form-action 'self'",
        ),
    );
    headers.insert(
        HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
    response
}

async fn static_handler(State(state): State<AppState>, uri: OriginalUri) -> Response {
    let path = uri.0.path().trim_start_matches('/');
    if path.starts_with("api/") || path == "api" {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": { "code": "not_found", "message": "Endpoint not found" }
            })),
        )
            .into_response();
    }
    if path.contains("..") {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let requested = if path.is_empty() { "index.html" } else { path };
    if let Some(file) = Assets::get(requested) {
        return asset_response(requested, file).await;
    }
    // Single-page application fallback for client-side routes.
    match Assets::get("index.html") {
        Some(file) => {
            let mut response = asset_response("index.html", file).await;
            if state.config.tls_enabled() {
                response.headers_mut().insert(
                    header::STRICT_TRANSPORT_SECURITY,
                    HeaderValue::from_static("max-age=31536000"),
                );
            }
            response
        }
        None => (
            StatusCode::NOT_FOUND,
            "The web interface has not been built into this binary.",
        )
            .into_response(),
    }
}

async fn asset_response(path: &str, file: rust_embed::EmbeddedFile) -> Response {
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let cache = if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    let mut response = Response::new(Body::from(file.data.into_owned()));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(mime.as_ref()).unwrap(),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    response
}

pub async fn serve(state: AppState) -> anyhow::Result<()> {
    let addr = state.config.bind_addr()?;
    let app = router(state.clone());
    if state.config.tls_enabled() {
        let cert = state.config.tls.cert.clone().unwrap();
        let key = state.config.tls.key.clone().unwrap();
        tracing::info!(%addr, "panel listening with TLS");
        let config = axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key).await?;
        axum_server::bind_rustls(addr, config)
            .serve(app.into_make_service_with_connect_info::<SocketAddr>())
            .await?;
    } else {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        tracing::info!(%addr, "panel listening on http");
        axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await?;
    }
    Ok(())
}
