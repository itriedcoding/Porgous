//! Background jobs: crash detection, metric sampling, scheduled tasks and
//! periodic maintenance.

use crate::db::{get_i64, get_opt_str, get_str, now_ms};
use crate::models::{Schedule, ScheduleTask, Server, STATUS_OFFLINE, STATUS_RUNNING, STATUS_STARTING};
use crate::server_ops;
use crate::state::AppState;
use chrono::{DateTime, Datelike, Duration as ChronoDuration, Timelike, Utc};
use sqlx::any::AnyRow;
use std::collections::HashMap;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct CronSpec {
    minutes: Vec<u32>,
    hours: Vec<u32>,
    days: Vec<u32>,
    months: Vec<u32>,
    weekdays: Vec<u32>,
}

impl CronSpec {
    pub fn parse(expression: &str) -> anyhow::Result<Self> {
        let fields: Vec<&str> = expression.split_whitespace().collect();
        if fields.len() != 5 {
            anyhow::bail!("cron expression must have 5 fields (minute hour day month weekday)");
        }
        Ok(Self {
            minutes: parse_field(fields[0], 0, 59)?,
            hours: parse_field(fields[1], 0, 23)?,
            days: parse_field(fields[2], 1, 31)?,
            months: parse_field(fields[3], 1, 12)?,
            weekdays: parse_field(fields[4], 0, 6)?,
        })
    }

    pub fn next_after(&self, from: DateTime<Utc>) -> DateTime<Utc> {
        let mut candidate = from
            .with_second(0)
            .unwrap_or(from)
            .with_nanosecond(0)
            .unwrap_or(from)
            + ChronoDuration::minutes(1);
        for _ in 0..(366 * 24 * 60) {
            let weekday = candidate.weekday().num_days_from_sunday();
            if self.minutes.contains(&candidate.minute())
                && self.hours.contains(&candidate.hour())
                && self.days.contains(&candidate.day())
                && self.months.contains(&candidate.month())
                && self.weekdays.contains(&weekday)
            {
                return candidate;
            }
            candidate += ChronoDuration::minutes(1);
        }
        from + ChronoDuration::days(365)
    }
}

fn parse_field(field: &str, min: u32, max: u32) -> anyhow::Result<Vec<u32>> {
    let mut values = Vec::new();
    for part in field.split(',') {
        let part = part.trim();
        if part.is_empty() {
            anyhow::bail!("empty cron field");
        }
        let (range_part, step) = match part.split_once('/') {
            Some((range, step)) => (
                range,
                step.parse::<u32>()
                    .map_err(|_| anyhow::anyhow!("invalid cron step"))?,
            ),
            None => (part, 1),
        };
        if step == 0 {
            anyhow::bail!("cron step must be positive");
        }
        let (start, end) = if range_part == "*" {
            (min, max)
        } else if let Some((a, b)) = range_part.split_once('-') {
            (
                a.parse::<u32>()
                    .map_err(|_| anyhow::anyhow!("invalid cron range"))?,
                b.parse::<u32>()
                    .map_err(|_| anyhow::anyhow!("invalid cron range"))?,
            )
        } else {
            let value = range_part
                .parse::<u32>()
                .map_err(|_| anyhow::anyhow!("invalid cron value"))?;
            (value, value)
        };
        if start < min || end > max || start > end {
            anyhow::bail!("cron value out of range");
        }
        let mut value = start;
        while value <= end {
            if !values.contains(&value) {
                values.push(value);
            }
            value += step;
        }
    }
    values.sort_unstable();
    Ok(values)
}

pub fn spawn(state: AppState) {
    tokio::spawn(async move {
        let mut tick: u64 = 0;
        let mut disk_cache: HashMap<String, (i64, i64)> = HashMap::new();
        // Let the HTTP server come up before doing work.
        tokio::time::sleep(Duration::from_secs(5)).await;
        loop {
            tick = tick.wrapping_add(5);
            if let Err(err) = crash_scan(&state).await {
                tracing::warn!(error = %err, "crash scan failed");
            }
            if tick % 15 == 0 {
                if let Err(err) = sample_metrics(&state, &mut disk_cache).await {
                    tracing::warn!(error = %err, "metric sampling failed");
                }
            }
            if tick % 30 == 0 {
                if let Err(err) = run_schedules(&state).await {
                    tracing::warn!(error = %err, "scheduled tasks failed");
                }
            }
            if tick % 3600 == 0 {
                cleanup(&state).await;
                auto_update(&state).await;
                tick = 0;
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });
}

async fn crash_scan(state: &AppState) -> anyhow::Result<()> {
    let rows =
        sqlx::query(&state.db.sql(
            "SELECT * FROM servers WHERE status IN ('running', 'starting') AND container_id IS NOT NULL",
        ))
        .fetch_all(&state.db.pool)
        .await?;
    let now = now_ms();
    for row in rows {
        let server = Server::from_row(&row);
        if now - server.updated_at < 8000 {
            continue;
        }
        let Ok(runtime) = state.runtimes.get(&server.node_id) else {
            continue;
        };
        let Some(container) = &server.container_id else {
            continue;
        };
        let stats = match runtime.inspect(container).await {
            Ok(stats) => stats,
            Err(_) => continue,
        };
        if stats.running {
            if server.status == STATUS_STARTING {
                server_ops::set_status(state, &server.id, STATUS_RUNNING, None).await;
                if let Ok(fresh) = server_ops::load_server(state, &server.id).await {
                    state.console.ensure_attached(state.runtimes.clone(), fresh);
                }
            }
            continue;
        }
        // The container exited unexpectedly.
        let crash_count = server.crash_count + 1;
        sqlx::query(&state.db.sql(
            "UPDATE servers SET crash_count = ?, last_crashed_at = ?, status = 'offline', updated_at = ? WHERE id = ?",
        ))
        .bind(crash_count)
        .bind(now)
        .bind(now)
        .bind(&server.id)
        .execute(&state.db.pool)
        .await?;
        state.console.publish(
            &server.id,
            crate::console::ConsoleEvent::Status(STATUS_OFFLINE.into()),
        );
        state.console.detach(&server.id);
        crate::audit::record(
            &state.db,
            None,
            "system",
            "server.crash",
            &server.uuid,
            None,
            serde_json::json!({ "exit_code": stats.exit_code, "crash_count": crash_count }),
        )
        .await;
        crate::audit::notify(
            &state.db,
            &server.owner_id,
            "Server crashed",
            &format!(
                "{} stopped unexpectedly (exit code {:?})",
                server.name, stats.exit_code
            ),
            "error",
        )
        .await;
        if will_restart(&server, crash_count) {
            let delay = (crash_count.clamp(1, 6) as u64) * 5;
            let state_clone = state.clone();
            let server_clone = server.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(delay)).await;
                if let Ok(fresh) = server_ops::load_server(&state_clone, &server_clone.id).await {
                    if fresh.suspended {
                        return;
                    }
                    server_ops::set_status(&state_clone, &fresh.id, STATUS_STARTING, None).await;
                    if let Err(err) = server_ops::start_server(&state_clone, &fresh).await {
                        tracing::warn!(server = %fresh.uuid, error = %err, "automatic restart failed");
                    }
                }
            });
        }
    }
    Ok(())
}

fn will_restart(server: &Server, crash_count: i64) -> bool {
    server.auto_restart
        && !server.suspended
        && crash_count <= 5
        && matches!(server.restart_policy.as_str(), "always" | "on-failure")
}

async fn sample_metrics(
    state: &AppState,
    disk_cache: &mut HashMap<String, (i64, i64)>,
) -> anyhow::Result<()> {
    let rows = sqlx::query(
        &state
            .db
            .sql("SELECT * FROM servers WHERE status = 'running' AND container_id IS NOT NULL"),
    )
    .fetch_all(&state.db.pool)
    .await?;
    let now = now_ms();
    for row in rows {
        let server = Server::from_row(&row);
        let Ok(runtime) = state.runtimes.get(&server.node_id) else {
            continue;
        };
        let Some(container) = &server.container_id else {
            continue;
        };
        let stats = match tokio::time::timeout(Duration::from_secs(10), runtime.stats(container)).await {
            Ok(Ok(stats)) => stats,
            _ => continue,
        };
        let cached = disk_cache.get(&server.id).copied();
        let disk = match cached {
            Some((bytes, at)) if now - at < 60_000 => bytes,
            _ => {
                let bytes = match crate::files_service::FileService::for_server(state, &server).await {
                    Ok(files) => files.usage().await.unwrap_or(0) as i64,
                    Err(_) => 0,
                };
                disk_cache.insert(server.id.clone(), (bytes, now));
                bytes
            }
        };
        let _ = sqlx::query(&state.db.sql(
            "INSERT INTO metrics (server_id, cpu, memory_bytes, disk_bytes, net_rx, net_tx, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        ))
        .bind(&server.id)
        .bind(stats.cpu_percent)
        .bind(stats.memory_bytes)
        .bind(disk)
        .bind(stats.net_rx_bytes)
        .bind(stats.net_tx_bytes)
        .bind(now)
        .execute(&state.db.pool)
        .await;
        state.console.publish_stats(
            &server.id,
            stats.cpu_percent,
            stats.memory_bytes,
            disk,
            stats.net_rx_bytes,
            stats.net_tx_bytes,
        );
    }
    Ok(())
}

async fn run_schedules(state: &AppState) -> anyhow::Result<()> {
    let rows = sqlx::query(
        &state
            .db
            .sql("SELECT * FROM schedules WHERE enabled = 1 AND (next_run_at IS NULL OR next_run_at <= ?)"),
    )
    .bind(now_ms())
    .fetch_all(&state.db.pool)
    .await?;
    for row in rows {
        let schedule = Schedule::from_row(&row);
        let cron = match CronSpec::parse(&schedule.cron) {
            Ok(cron) => cron,
            Err(err) => {
                tracing::warn!(schedule = %schedule.id, error = %err, "invalid cron; disabling schedule");
                let _ = sqlx::query(&state.db.sql("UPDATE schedules SET enabled = 0 WHERE id = ?"))
                    .bind(&schedule.id)
                    .execute(&state.db.pool)
                    .await;
                continue;
            }
        };
        let now = Utc::now();
        let next = cron.next_after(now);
        sqlx::query(
            &state
                .db
                .sql("UPDATE schedules SET last_run_at = ?, next_run_at = ? WHERE id = ?"),
        )
        .bind(now_ms())
        .bind(next.timestamp_millis())
        .bind(&schedule.id)
        .execute(&state.db.pool)
        .await?;
        let state_clone = state.clone();
        tokio::spawn(async move {
            execute_schedule(&state_clone, schedule).await;
        });
    }
    Ok(())
}

pub async fn execute_schedule(state: &AppState, schedule: Schedule) {
    let Ok(server) = server_ops::load_server(state, &schedule.server_id).await else {
        return;
    };
    if server.suspended {
        return;
    }
    for task in &schedule.tasks {
        if task.offset_secs > 0 {
            tokio::time::sleep(Duration::from_secs(task.offset_secs.min(3600))).await;
        }
        let result = execute_task(state, &server, task).await;
        match result {
            Ok(()) => {
                crate::audit::record(
                    &state.db,
                    None,
                    "system",
                    "schedule.run",
                    &server.uuid,
                    None,
                    serde_json::json!({ "schedule": schedule.name, "action": task.action }),
                )
                .await;
            }
            Err(err) => {
                crate::audit::notify(
                    &state.db,
                    &server.owner_id,
                    "Scheduled task failed",
                    &format!("{} / {}: {err}", schedule.name, task.action),
                    "error",
                )
                .await;
            }
        }
    }
}

async fn execute_task(state: &AppState, server: &Server, task: &ScheduleTask) -> anyhow::Result<()> {
    let action = task.action.trim().to_lowercase();
    match action.as_str() {
        "command" => {
            let Some(container) = &server.container_id else {
                anyhow::bail!("server has no container");
            };
            if server.status != STATUS_RUNNING {
                anyhow::bail!("server is not running");
            }
            state
                .console
                .send_command(&state.runtimes, server, container, &task.payload)
                .await
                .map_err(|e| anyhow::anyhow!(e))?;
        }
        "power" | "power:start" | "start" => {
            if action == "start" || task.payload.trim().is_empty() || task.payload.trim() == "start" {
                server_ops::start_server(state, server)
                    .await
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
            } else {
                power_task(state, server, task.payload.trim()).await?;
            }
        }
        "power:stop" => server_ops::stop_server(state, server)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?,
        "power:restart" => server_ops::restart_server(state, server)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?,
        "power:kill" => server_ops::kill_server(state, server)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?,
        "backup" => {
            server_ops::create_backup(state, server, task.payload.as_str())
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        other => anyhow::bail!("unknown task action '{other}'"),
    }
    Ok(())
}

async fn power_task(state: &AppState, server: &Server, action: &str) -> anyhow::Result<()> {
    match action {
        "start" => server_ops::start_server(state, server)
            .await
            .map_err(|e| anyhow::anyhow!("{e}")),
        "stop" => server_ops::stop_server(state, server)
            .await
            .map_err(|e| anyhow::anyhow!("{e}")),
        "restart" => server_ops::restart_server(state, server)
            .await
            .map_err(|e| anyhow::anyhow!("{e}")),
        "kill" => server_ops::kill_server(state, server)
            .await
            .map_err(|e| anyhow::anyhow!("{e}")),
        other => anyhow::bail!("unknown power action '{other}'"),
    }
}

async fn cleanup(state: &AppState) {
    let cutoff = now_ms() - 7 * 24 * 60 * 60 * 1000;
    let _ = sqlx::query(&state.db.sql("DELETE FROM metrics WHERE created_at < ?"))
        .bind(cutoff)
        .execute(&state.db.pool)
        .await;
    let _ = sqlx::query(&state.db.sql("DELETE FROM sessions WHERE expires_at < ?"))
        .bind(now_ms())
        .execute(&state.db.pool)
        .await;
    let audit_cutoff = now_ms() - 365 * 24 * 60 * 60 * 1000;
    let _ = sqlx::query(&state.db.sql("DELETE FROM audit_logs WHERE created_at < ?"))
        .bind(audit_cutoff)
        .execute(&state.db.pool)
        .await;
    // Truncate installed update build artifacts older than 30 days.
    if let Ok(entries) = std::fs::read_dir(state.config.tmp_dir()) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                if meta
                    .modified()
                    .ok()
                    .and_then(|m| m.elapsed().ok())
                    .map(|e| e.as_secs() > 30 * 86400)
                    .unwrap_or(false)
                {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }
}

async fn auto_update(state: &AppState) {
    let enabled = state
        .db
        .setting_bool("auto_update", state.config.updates.auto_apply)
        .await;
    if !enabled || !state.config.updates.enabled || state.config.updates.public_key.trim().is_empty() {
        return;
    }
    match crate::updater::latest_release(state, &state.config.updates.channel).await {
        Ok(Some(release)) if release.update_available => {
            tracing::info!(version = %release.version, "installing automatic update");
            match crate::updater::apply_latest(state, true).await {
                Ok(_) => {
                    crate::audit::notify_admins(
                        &state.db,
                        "Update installed",
                        &format!(
                            "Porgous {} was installed automatically. Restart when convenient.",
                            release.version
                        ),
                        "success",
                    )
                    .await;
                }
                Err(err) => {
                    tracing::error!(error = %err, "automatic update failed");
                    crate::audit::notify_admins(&state.db, "Automatic update failed", &err.message, "error")
                        .await;
                }
            }
        }
        _ => {}
    }
}

/// Current schedule info for the UI, including the computed next run time.
pub async fn schedule_view(state: &AppState, row: &AnyRow) -> Schedule {
    let schedule = Schedule::from_row(row);
    let _ = state;
    schedule
}

pub fn next_run(expression: &str, from: DateTime<Utc>) -> Option<DateTime<Utc>> {
    CronSpec::parse(expression).ok().map(|c| c.next_after(from))
}

/// Load schedules and compute missing next-run times.
pub async fn repair_schedules(state: &AppState) {
    let rows = sqlx::query(
        &state
            .db
            .sql("SELECT * FROM schedules WHERE enabled = 1 AND next_run_at IS NULL"),
    )
    .fetch_all(&state.db.pool)
    .await
    .unwrap_or_default();
    for row in rows {
        let id = get_str(&row, "id");
        let cron = get_str(&row, "cron");
        if let Ok(spec) = CronSpec::parse(&cron) {
            let next = spec.next_after(Utc::now()).timestamp_millis();
            let _ = sqlx::query(&state.db.sql("UPDATE schedules SET next_run_at = ? WHERE id = ?"))
                .bind(next)
                .bind(id)
                .execute(&state.db.pool)
                .await;
        }
    }
    let _ = state;
    let _ = get_opt_str;
    let _ = get_i64;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_cron_expressions() {
        let spec = CronSpec::parse("*/5 * * * *").unwrap();
        let from = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let next = spec.next_after(from);
        assert_eq!(next.minute(), 5);
        assert!(CronSpec::parse("0 3 * * 1-5").is_ok());
        assert!(CronSpec::parse("bad").is_err());
        assert!(CronSpec::parse("61 * * * *").is_err());
    }

    #[test]
    fn computes_daily_schedule() {
        let spec = CronSpec::parse("30 4 * * *").unwrap();
        let from = DateTime::parse_from_rfc3339("2026-05-01T05:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let next = spec.next_after(from);
        assert_eq!(next.hour(), 4);
        assert_eq!(next.minute(), 30);
        assert!(next > from);
    }

    #[test]
    fn restart_policy() {
        let mut server = crate::models::Server {
            id: "x".into(),
            uuid: "x".into(),
            name: "x".into(),
            description: String::new(),
            owner_id: "o".into(),
            node_id: "n".into(),
            egg_id: "e".into(),
            image: "alpine".into(),
            startup: String::new(),
            stop_command: None,
            env: serde_json::json!({}),
            memory_mb: 512,
            swap_mb: 0,
            disk_mb: 1024,
            cpu_percent: 100,
            pids_limit: 256,
            io_weight: 500,
            auto_restart: true,
            restart_policy: "on-failure".into(),
            crash_count: 0,
            status: STATUS_RUNNING.into(),
            container_id: None,
            last_error: None,
            install_log: None,
            suspended: false,
            created_at: 0,
            updated_at: 0,
            last_started_at: None,
            last_crashed_at: None,
        };
        assert!(will_restart(&server, 1));
        assert!(!will_restart(&server, 9));
        server.restart_policy = "never".into();
        assert!(!will_restart(&server, 1));
    }
}
