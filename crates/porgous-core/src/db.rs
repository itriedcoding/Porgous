//! Database access built on sqlx's `Any` driver.
//!
//! All SQL is written with `?` placeholders. For PostgreSQL they are rewritten
//! to `$1`, `$2`, ... before execution, which keeps one query text working on
//! both supported backends.

use anyhow::{Context, Result};
use sqlx::any::{AnyPoolOptions, AnyRow};
use sqlx::{Any, AnyPool, Row};
use std::path::Path;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Sqlite,
    Postgres,
}

#[derive(Clone, Debug)]
pub struct Db {
    pub pool: AnyPool,
    pub backend: Backend,
}

static DRIVERS: OnceLock<()> = OnceLock::new();

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl Db {
    pub async fn connect(url: &str) -> Result<Self> {
        DRIVERS.get_or_init(sqlx::any::install_default_drivers);
        let backend = if url.starts_with("postgres:") {
            Backend::Postgres
        } else {
            Backend::Sqlite
        };
        if backend == Backend::Sqlite {
            if let Some(path) = sqlite_path(url) {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)
                        .with_context(|| format!("creating database directory {}", parent.display()))?;
                }
            }
        }
        let max_conns = if backend == Backend::Sqlite { 8 } else { 20 };
        let pool = AnyPoolOptions::new()
            .max_connections(max_conns)
            .min_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(15))
            .connect(url)
            .await
            .with_context(|| format!("connecting to database ({backend:?})"))?;
        let db = Self { pool, backend };
        if backend == Backend::Sqlite {
            // WAL greatly improves concurrent reads while a write is in progress.
            let _ = sqlx::query("PRAGMA journal_mode=WAL").execute(&db.pool).await;
            let _ = sqlx::query("PRAGMA synchronous=NORMAL").execute(&db.pool).await;
        }
        db.migrate().await?;
        Ok(db)
    }

    pub async fn migrate(&self) -> Result<()> {
        self.exec("CREATE TABLE IF NOT EXISTS schema_migrations (version BIGINT PRIMARY KEY, applied_at BIGINT NOT NULL)")
            .await?;
        for (index, statements) in crate::schema::MIGRATIONS.iter().enumerate() {
            let version = (index + 1) as i64;
            let applied: Option<AnyRow> =
                sqlx::query(&self.sql("SELECT version FROM schema_migrations WHERE version = ?"))
                    .bind(version)
                    .fetch_optional(&self.pool)
                    .await
                    .context("checking migrations")?;
            if applied.is_some() {
                continue;
            }
            tracing::info!(version, "applying database migration");
            for statement in statements.iter() {
                self.exec(statement)
                    .await
                    .with_context(|| format!("migration {version} failed"))?;
            }
            sqlx::query(&self.sql("INSERT INTO schema_migrations (version, applied_at) VALUES (?, ?)"))
                .bind(version)
                .bind(now_ms())
                .execute(&self.pool)
                .await
                .context("recording migration")?;
        }
        Ok(())
    }

    /// Rewrite `?` placeholders for the active backend.
    pub fn sql(&self, query: &str) -> String {
        match self.backend {
            Backend::Sqlite => query.to_string(),
            Backend::Postgres => rewrite_placeholders(query),
        }
    }

    pub async fn exec(&self, query: &str) -> Result<()> {
        sqlx::query(&self.sql(query)).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn setting(&self, key: &str) -> Result<Option<String>> {
        let row: Option<AnyRow> = sqlx::query(&self.sql("SELECT value FROM settings WHERE key = ?"))
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| get_str(&r, "value")))
    }

    pub async fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        let existing: Option<AnyRow> = sqlx::query(&self.sql("SELECT key FROM settings WHERE key = ?"))
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        if existing.is_some() {
            sqlx::query(&self.sql("UPDATE settings SET value = ? WHERE key = ?"))
                .bind(value)
                .bind(key)
                .execute(&self.pool)
                .await?;
        } else {
            sqlx::query(&self.sql("INSERT INTO settings (key, value) VALUES (?, ?)"))
                .bind(key)
                .bind(value)
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }

    pub async fn setting_bool(&self, key: &str, default: bool) -> bool {
        self.setting(key)
            .await
            .ok()
            .flatten()
            .map(|v| v == "true" || v == "1")
            .unwrap_or(default)
    }
}

pub fn sqlite_path(url: &str) -> Option<&Path> {
    let rest = url
        .strip_prefix("sqlite://")
        .or_else(|| url.strip_prefix("sqlite:"))?;
    let path = rest.split('?').next().unwrap_or(rest);
    if path.is_empty() || path == ":memory:" {
        None
    } else {
        Some(Path::new(path))
    }
}

/// Rewrite `?` placeholders outside of string literals and quoted identifiers.
pub fn rewrite_placeholders(query: &str) -> String {
    let bytes = query.as_bytes();
    let mut out = String::with_capacity(query.len() + 8);
    let mut i = 0usize;
    let mut n = 0usize;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = quote {
            out.push(b as char);
            if b == q {
                if i + 1 < bytes.len() && bytes[i + 1] == q {
                    out.push(q as char);
                    i += 2;
                    continue;
                }
                quote = None;
            }
            i += 1;
            continue;
        }
        match b {
            b'\'' | b'"' => {
                quote = Some(b);
                out.push(b as char);
            }
            b'?' => {
                n += 1;
                out.push('$');
                out.push_str(&n.to_string());
            }
            _ => out.push(b as char),
        }
        i += 1;
    }
    out
}

pub fn get_str(row: &AnyRow, column: &str) -> String {
    row.try_get::<String, _>(column).unwrap_or_default()
}

pub fn get_opt_str(row: &AnyRow, column: &str) -> Option<String> {
    row.try_get::<Option<String>, _>(column).ok().flatten()
}

pub fn get_i64(row: &AnyRow, column: &str) -> i64 {
    row.try_get::<i64, _>(column)
        .or_else(|_| row.try_get::<Option<i64>, _>(column).map(|v| v.unwrap_or(0)))
        .unwrap_or(0)
}

pub fn get_opt_i64(row: &AnyRow, column: &str) -> Option<i64> {
    row.try_get::<Option<i64>, _>(column).ok().flatten()
}

pub fn get_f64(row: &AnyRow, column: &str) -> f64 {
    row.try_get::<f64, _>(column)
        .or_else(|_| row.try_get::<Option<f64>, _>(column).map(|v| v.unwrap_or(0.0)))
        .unwrap_or(0.0)
}

pub fn get_bool(row: &AnyRow, column: &str) -> bool {
    get_i64(row, column) != 0
}

pub type Tx = sqlx::Transaction<'static, Any>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_placeholders_only_outside_quotes() {
        let sql = "INSERT INTO t (a, b) VALUES (?, '?') AND c = \"?\" AND d = ?";
        assert_eq!(
            rewrite_placeholders(sql),
            "INSERT INTO t (a, b) VALUES ($1, '?') AND c = \"?\" AND d = $2"
        );
    }

    #[test]
    fn handles_escaped_quotes() {
        assert_eq!(
            rewrite_placeholders("SELECT 'it''s ?' , ?"),
            "SELECT 'it''s ?' , $1"
        );
    }

    #[tokio::test]
    async fn sqlite_migrates_and_persists_settings() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let url = format!("sqlite:{}?mode=rwc", db_path.to_string_lossy().replace('\\', "/"));
        let db = Db::connect(&url).await.unwrap();
        db.set_setting("panel_name", "Test").await.unwrap();
        assert_eq!(db.setting("panel_name").await.unwrap().as_deref(), Some("Test"));
        assert_eq!(db.backend, Backend::Sqlite);
        let db2 = Db::connect(&url).await.unwrap();
        assert_eq!(db2.setting("panel_name").await.unwrap().as_deref(), Some("Test"));
    }
}
