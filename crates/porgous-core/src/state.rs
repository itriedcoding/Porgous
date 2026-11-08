//! Shared application state and first-run bootstrap.

use crate::auth::{create_user, LoginGuard};
use crate::config::{load_or_create_secret, Config};
use crate::console::ConsoleHub;
use crate::crypto::{random_token, sha256_hex, SecretBox};
use crate::db::{now_ms, Db};
use crate::eggs::EggStore;
use crate::error::{ApiError, ApiResult};
use crate::models::Node;
use crate::nodes::NodeClient;
use crate::ratelimit::RateLimiter;
use crate::runtime::{ContainerRuntime, LocalRuntime, RemoteRuntime, RuntimeRegistry};
use anyhow::{Context, Result};
use sqlx::any::AnyRow;
use std::path::PathBuf;
use std::sync::Arc;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: Db,
    pub secrets: SecretBox,
    pub eggs: Arc<EggStore>,
    pub runtimes: Arc<RuntimeRegistry>,
    pub console: Arc<ConsoleHub>,
    pub login_guard: Arc<LoginGuard>,
    pub rate: Arc<RateLimiter>,
    pub http: reqwest::Client,
    pub started_at: i64,
}

impl AppState {
    pub async fn bootstrap(config: Config) -> Result<Self> {
        config.ensure_dirs()?;
        let db = Db::connect(&config.database_url).await?;
        let key = match config.secret_key.as_deref() {
            Some(value) => crate::config::decode_secret_key(value)?,
            None => load_or_create_secret(&config.data_dir.join("secret.key"))?,
        };
        let secrets = SecretBox::new(&key);
        let eggs = Arc::new(EggStore::load(&config.eggs_dir())?);
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent(concat!("porgous-panel/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        seed_settings(&db, &config).await?;
        let local_node = ensure_local_node(&db, &config, &secrets).await?;
        bootstrap_admin(&db, &config).await?;

        let runtimes = Arc::new(RuntimeRegistry::new());
        let nodes = load_nodes(&db).await?;
        for node in &nodes {
            match build_runtime(&config, &secrets, node).await {
                Ok(runtime) => runtimes.set(node.id.clone(), runtime),
                Err(err) => {
                    tracing::warn!(node = %node.name, error = %err, "failed to initialize node runtime")
                }
            }
        }
        if let Some(node) = nodes.iter().find(|n| n.id == local_node.id) {
            if runtimes.get(&node.id).is_err() {
                let runtime = build_runtime(&config, &secrets, node).await?;
                runtimes.set(node.id.clone(), runtime);
            }
        }

        Ok(Self {
            config: Arc::new(config),
            db,
            secrets,
            eggs,
            runtimes,
            console: Arc::new(ConsoleHub::new()),
            login_guard: Arc::new(LoginGuard::default()),
            rate: Arc::new(RateLimiter::new()),
            http,
            started_at: now_ms(),
        })
    }

    pub fn node_client(&self, node: &Node) -> ApiResult<NodeClient> {
        let token = self
            .secrets
            .open(&node.token_enc)
            .map_err(|_| ApiError::internal("node token cannot be decrypted"))?;
        let secret = self
            .secrets
            .open(&node.secret_enc)
            .map_err(|_| ApiError::internal("node secret cannot be decrypted"))?;
        Ok(NodeClient::new(node, token, secret.into_bytes()))
    }

    pub fn local_volume(&self, server_uuid: &str) -> PathBuf {
        self.config.volumes_dir().join(server_uuid)
    }
}

pub async fn load_nodes(db: &Db) -> Result<Vec<Node>> {
    let rows: Vec<AnyRow> = sqlx::query(&db.sql("SELECT * FROM nodes ORDER BY created_at ASC"))
        .fetch_all(&db.pool)
        .await?;
    Ok(rows.iter().map(Node::from_row).collect())
}

pub async fn load_node(db: &Db, id: &str) -> Result<Option<Node>> {
    let row: Option<AnyRow> = sqlx::query(&db.sql("SELECT * FROM nodes WHERE id = ?"))
        .bind(id)
        .fetch_optional(&db.pool)
        .await?;
    Ok(row.as_ref().map(Node::from_row))
}

pub async fn build_runtime(
    config: &Config,
    secrets: &SecretBox,
    node: &Node,
) -> Result<Arc<dyn ContainerRuntime>> {
    if node.is_local {
        let runtime = LocalRuntime::connect(
            node.id.clone(),
            config.volumes_dir(),
            config.docker.network.clone(),
            config.docker.timezone.clone(),
            config.docker.default_image.clone(),
            config.docker.container_user.clone(),
            config.docker.host.clone(),
        )
        .await;
        Ok(Arc::new(runtime))
    } else {
        let token = secrets.open(&node.token_enc).context("decrypting node token")?;
        let secret = secrets.open(&node.secret_enc).context("decrypting node secret")?;
        Ok(Arc::new(RemoteRuntime::new(
            node.clone(),
            token,
            secret.into_bytes(),
        )))
    }
}

async fn seed_settings(db: &Db, config: &Config) -> Result<()> {
    let defaults = [
        ("panel_name", config.panel_name.clone()),
        ("allow_registration", config.allow_registration.to_string()),
        ("allow_user_servers", config.allow_user_servers.to_string()),
        ("update_channel", config.updates.channel.clone()),
        ("auto_update", config.updates.auto_apply.to_string()),
    ];
    for (key, value) in defaults {
        if db.setting(key).await?.is_none() {
            db.set_setting(key, &value).await?;
        }
    }
    Ok(())
}

async fn ensure_local_node(db: &Db, config: &Config, secrets: &SecretBox) -> Result<Node> {
    let existing: Option<AnyRow> = sqlx::query(&db.sql("SELECT * FROM nodes WHERE is_local = 1 LIMIT 1"))
        .fetch_optional(&db.pool)
        .await?;
    if let Some(row) = existing {
        return Ok(Node::from_row(&row));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let token = random_token(32);
    let secret = random_token(32);
    let token_enc = secrets.seal(&token)?;
    let secret_enc = secrets.seal(&secret)?;
    let port = config
        .node
        .bind
        .rsplit(':')
        .next()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(8081);
    let now = now_ms();
    sqlx::query(&db.sql(
        "INSERT INTO nodes (id, name, description, is_local, fqdn, scheme, port, token_enc, secret_enc,
            memory_mb, disk_mb, port_range_start, port_range_end, maintenance, sftp_port, created_at, updated_at)
         VALUES (?, ?, ?, 1, ?, 'http', ?, ?, ?, 0, 0, 25000, 26000, 0, ?, ?, ?)",
    ))
    .bind(&id)
    .bind(&config.node.name)
    .bind("Automatically registered local node")
    .bind("127.0.0.1")
    .bind(port as i64)
    .bind(token_enc)
    .bind(secret_enc)
    .bind(config.node.sftp_port as i64)
    .bind(now)
    .bind(now)
    .execute(&db.pool)
    .await?;
    tracing::info!(node = %config.node.name, "registered local node");
    load_node(db, &id)
        .await?
        .context("local node missing after creation")
}

async fn bootstrap_admin(db: &Db, config: &Config) -> Result<()> {
    let row: Option<AnyRow> = sqlx::query(&db.sql("SELECT COUNT(*) AS count FROM users"))
        .fetch_optional(&db.pool)
        .await?;
    let count = row.map(|r| crate::db::get_i64(&r, "count")).unwrap_or(0);
    if count > 0 {
        return Ok(());
    }
    let email = if config.admin.email.trim().is_empty() {
        "admin@porgous.local".to_string()
    } else {
        config.admin.email.clone()
    };
    let username = if config.admin.username.trim().is_empty() {
        "admin".to_string()
    } else {
        config.admin.username.clone()
    };
    let generated = config.admin.password.trim().is_empty();
    let password = if generated {
        format!("{}{}", random_token(18), "aA1!")
    } else {
        config.admin.password.clone()
    };
    create_user(db, &email, &username, &password, true)
        .await
        .map_err(|e| anyhow::anyhow!("creating bootstrap administrator: {e}"))?;
    if generated {
        let path = config.data_dir.join("initial-admin.txt");
        let content = format!(
            "Porgous initial administrator\n\nEmail: {email}\nUsername: {username}\nPassword: {password}\n\nChange this password after signing in.\n"
        );
        std::fs::write(&path, content)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        tracing::warn!(path = %path.display(), "created initial administrator; credentials written to file");
    } else {
        tracing::info!(%email, "created initial administrator from configuration");
    }
    crate::audit::record(
        db,
        None,
        "system",
        "user.bootstrap",
        &email,
        None,
        serde_json::json!({}),
    )
    .await;
    Ok(())
}

/// Verify a password for the current user.
pub async fn verify_current_password(state: &AppState, user_id: &str, password: &str) -> ApiResult<()> {
    let user = crate::auth::find_user(&state.db, user_id)
        .await?
        .ok_or_else(|| ApiError::not_found("User not found"))?;
    if crate::crypto::verify_password(password, &user.password_hash) {
        Ok(())
    } else {
        Err(ApiError::unauthorized("Invalid password"))
    }
}

/// Hash a node token for constant-time verification on the agent side.
pub fn node_token_hash(token: &str) -> String {
    sha256_hex(token.as_bytes())
}
