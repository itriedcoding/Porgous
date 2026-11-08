//! Audit logging and user notifications.

use crate::db::{now_ms, Db};
use serde_json::Value;

pub async fn record(
    db: &Db,
    user_id: Option<&str>,
    actor: &str,
    action: &str,
    target: &str,
    ip: Option<&str>,
    metadata: Value,
) {
    let id = uuid::Uuid::new_v4().to_string();
    let result = sqlx::query(&db.sql(
        "INSERT INTO audit_logs (id, user_id, actor, action, target, ip, metadata, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    ))
    .bind(id)
    .bind(user_id)
    .bind(actor)
    .bind(action)
    .bind(target)
    .bind(ip)
    .bind(metadata.to_string())
    .bind(now_ms())
    .execute(&db.pool)
    .await;
    if let Err(err) = result {
        tracing::warn!(error = %err, action, "failed to write audit log");
    }
}

pub async fn notify(db: &Db, user_id: &str, title: &str, body: &str, level: &str) {
    let id = uuid::Uuid::new_v4().to_string();
    let result = sqlx::query(&db.sql(
        "INSERT INTO notifications (id, user_id, title, body, level, is_read, created_at)
         VALUES (?, ?, ?, ?, ?, 0, ?)",
    ))
    .bind(id)
    .bind(user_id)
    .bind(title)
    .bind(body)
    .bind(level)
    .bind(now_ms())
    .execute(&db.pool)
    .await;
    if let Err(err) = result {
        tracing::warn!(error = %err, "failed to write notification");
    }
}

pub async fn notify_admins(db: &Db, title: &str, body: &str, level: &str) {
    let rows = sqlx::query(&db.sql("SELECT id FROM users WHERE root_admin = 1"))
        .fetch_all(&db.pool)
        .await
        .unwrap_or_default();
    for row in rows {
        let id: String = sqlx::Row::try_get(&row, "id").unwrap_or_default();
        if !id.is_empty() {
            notify(db, &id, title, body, level).await;
        }
    }
}
