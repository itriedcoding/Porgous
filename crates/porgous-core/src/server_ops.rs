//! Server lifecycle operations shared by the HTTP API and the scheduler.

use crate::backup::{self, BackupPaths};
use crate::db::{get_i64, get_opt_str, get_str, now_ms};
use crate::eggs::{Egg, EggStore};
use crate::error::{ApiError, ApiResult};
use crate::files_service::Download;
use crate::models::{
    Allocation, Backup, Node, Server, User, STATUS_CREATING, STATUS_ERROR, STATUS_INSTALLING, STATUS_OFFLINE,
    STATUS_RUNNING, STATUS_STARTING, STATUS_STOPPING, STATUS_SUSPENDED,
};
use crate::runtime::{ContainerRuntime, ContainerSpec, InstallSpec, PortMapping, VOLUME_MOUNT};
use crate::state::AppState;
use crate::util::urlencode;
use anyhow::{anyhow, bail, Context, Result};
use reqwest::Method;
use serde::Deserialize;
use serde_json::json;
use sqlx::any::AnyRow;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, Deserialize)]
pub struct CreateServerRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub egg_id: String,
    #[serde(default)]
    pub node_id: Option<String>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default = "default_memory")]
    pub memory_mb: i64,
    #[serde(default = "default_disk")]
    pub disk_mb: i64,
    #[serde(default = "default_cpu")]
    pub cpu_percent: i64,
    #[serde(default)]
    pub swap_mb: i64,
    #[serde(default)]
    pub pids_limit: Option<i64>,
    #[serde(default)]
    pub io_weight: Option<i64>,
    #[serde(default)]
    pub allocation_id: Option<String>,
    #[serde(default)]
    pub variables: BTreeMap<String, String>,
    #[serde(default)]
    pub start_on_create: bool,
    #[serde(default = "default_true")]
    pub auto_restart: bool,
    #[serde(default)]
    pub restart_policy: Option<String>,
}

fn default_memory() -> i64 {
    1024
}
fn default_disk() -> i64 {
    5120
}
fn default_cpu() -> i64 {
    100
}
fn default_true() -> bool {
    true
}

pub async fn load_server(state: &AppState, id: &str) -> ApiResult<Server> {
    let row: Option<AnyRow> = sqlx::query(&state.db.sql("SELECT * FROM servers WHERE id = ? OR uuid = ?"))
        .bind(id)
        .bind(id)
        .fetch_optional(&state.db.pool)
        .await?;
    row.map(|r| Server::from_row(&r))
        .ok_or_else(|| ApiError::not_found("Server not found"))
}

pub async fn load_node_for(state: &AppState, server: &Server) -> ApiResult<Node> {
    crate::state::load_node(&state.db, &server.node_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Node not found"))
}

pub async fn allocations(state: &AppState, server_id: &str) -> ApiResult<Vec<Allocation>> {
    let rows = sqlx::query(
        &state
            .db
            .sql("SELECT * FROM allocations WHERE server_id = ? ORDER BY is_primary DESC, port ASC"),
    )
    .bind(server_id)
    .fetch_all(&state.db.pool)
    .await?;
    Ok(rows.iter().map(Allocation::from_row).collect())
}

pub async fn variables(state: &AppState, server_id: &str) -> ApiResult<Vec<crate::models::ServerVariable>> {
    let rows = sqlx::query(
        &state
            .db
            .sql("SELECT * FROM server_variables WHERE server_id = ? ORDER BY key ASC"),
    )
    .bind(server_id)
    .fetch_all(&state.db.pool)
    .await?;
    Ok(rows.iter().map(crate::models::ServerVariable::from_row).collect())
}

pub async fn set_status(state: &AppState, server_id: &str, status: &str, error: Option<&str>) {
    let _ = sqlx::query(
        &state
            .db
            .sql("UPDATE servers SET status = ?, last_error = ?, updated_at = ? WHERE id = ?"),
    )
    .bind(status)
    .bind(error)
    .bind(now_ms())
    .bind(server_id)
    .execute(&state.db.pool)
    .await;
    state.console.publish(
        server_id,
        crate::console::ConsoleEvent::Status(status.to_string()),
    );
}

async fn validate_name(name: &str) -> ApiResult<String> {
    let name = name.trim();
    if name.len() < 1 || name.len() > 100 {
        return Err(ApiError::bad_request("Server name must be 1-100 characters"));
    }
    if name.chars().any(|c| c.is_control()) {
        return Err(ApiError::bad_request("Server name contains invalid characters"));
    }
    Ok(name.to_string())
}

fn clamp_limits(req: &mut CreateServerRequest) -> ApiResult<()> {
    if req.memory_mb < 128 || req.memory_mb > 1_048_576 {
        return Err(ApiError::bad_request("Memory must be between 128 and 1048576 MB"));
    }
    if req.disk_mb < 256 || req.disk_mb > 10_485_760 {
        return Err(ApiError::bad_request("Disk must be between 256 and 10485760 MB"));
    }
    if req.cpu_percent < 10 || req.cpu_percent > 64_000 {
        return Err(ApiError::bad_request(
            "CPU limit must be between 10 and 64000 percent",
        ));
    }
    if req.swap_mb < 0 {
        req.swap_mb = 0;
    }
    Ok(())
}

async fn pick_node(state: &AppState, node_id: Option<&str>) -> ApiResult<Node> {
    let node = match node_id {
        Some(id) => crate::state::load_node(&state.db, id)
            .await?
            .ok_or_else(|| ApiError::not_found("Node not found"))?,
        None => {
            let row: Option<AnyRow> =
                sqlx::query(&state.db.sql("SELECT * FROM nodes WHERE is_local = 1 LIMIT 1"))
                    .fetch_optional(&state.db.pool)
                    .await?;
            row.map(|r| Node::from_row(&r))
                .ok_or_else(|| ApiError::not_found("No node is available"))?
        }
    };
    if node.maintenance {
        return Err(ApiError::conflict("The selected node is in maintenance mode"));
    }
    Ok(node)
}

async fn node_capacity_ok(state: &AppState, node: &Node, memory_mb: i64, disk_mb: i64) -> ApiResult<()> {
    if node.memory_mb > 0 {
        let row: Option<AnyRow> = sqlx::query(
            &state
                .db
                .sql("SELECT COALESCE(SUM(memory_mb), 0) AS total FROM servers WHERE node_id = ?"),
        )
        .bind(&node.id)
        .fetch_optional(&state.db.pool)
        .await?;
        let used = row.map(|r| get_i64(&r, "total")).unwrap_or(0);
        if used + memory_mb > node.memory_mb {
            return Err(ApiError::conflict(format!(
                "Node memory capacity exceeded: {} MB used of {} MB",
                used, node.memory_mb
            )));
        }
    }
    if node.disk_mb > 0 {
        let row: Option<AnyRow> = sqlx::query(
            &state
                .db
                .sql("SELECT COALESCE(SUM(disk_mb), 0) AS total FROM servers WHERE node_id = ?"),
        )
        .bind(&node.id)
        .fetch_optional(&state.db.pool)
        .await?;
        let used = row.map(|r| get_i64(&r, "total")).unwrap_or(0);
        if used + disk_mb > node.disk_mb {
            return Err(ApiError::conflict("Node disk capacity exceeded"));
        }
    }
    Ok(())
}

async fn allocate(state: &AppState, node: &Node, requested: Option<&str>) -> ApiResult<Allocation> {
    if let Some(id) = requested {
        let row: Option<AnyRow> = sqlx::query(&state.db.sql("SELECT * FROM allocations WHERE id = ?"))
            .bind(id)
            .fetch_optional(&state.db.pool)
            .await?;
        let allocation = row
            .map(|r| Allocation::from_row(&r))
            .ok_or_else(|| ApiError::not_found("Allocation not found"))?;
        if allocation.node_id != node.id || allocation.server_id.is_some() {
            return Err(ApiError::conflict(
                "That allocation is not available on this node",
            ));
        }
        return Ok(allocation);
    }
    let row: Option<AnyRow> =
        sqlx::query(&state.db.sql(
            "SELECT * FROM allocations WHERE node_id = ? AND server_id IS NULL ORDER BY port ASC LIMIT 1",
        ))
        .bind(&node.id)
        .fetch_optional(&state.db.pool)
        .await?;
    if let Some(row) = row {
        return Ok(Allocation::from_row(&row));
    }
    // Create a free port from the node's configured range.
    let start = node.port_range_start.max(1024) as u32;
    let end = node.port_range_end.max(node.port_range_start) as u32;
    if end < start {
        return Err(ApiError::unavailable("Node has no port range configured"));
    }
    for _ in 0..200 {
        use rand::Rng;
        let port = rand::thread_rng().gen_range(start..=end) as u16;
        let existing: Option<AnyRow> = sqlx::query(
            &state
                .db
                .sql("SELECT id FROM allocations WHERE node_id = ? AND ip = ? AND port = ?"),
        )
        .bind(&node.id)
        .bind(&node.fqdn)
        .bind(port as i64)
        .fetch_optional(&state.db.pool)
        .await?;
        if existing.is_some() {
            continue;
        }
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(&state.db.sql(
            "INSERT INTO allocations (id, node_id, ip, port, is_primary, alias, created_at) VALUES (?, ?, ?, ?, 0, NULL, ?)",
        ))
        .bind(&id)
        .bind(&node.id)
        .bind(&node.fqdn)
        .bind(port as i64)
        .bind(now_ms())
        .execute(&state.db.pool)
        .await?;
        return Ok(Allocation {
            id,
            node_id: node.id.clone(),
            ip: node.fqdn.clone(),
            port,
            server_id: None,
            is_primary: false,
            alias: None,
        });
    }
    Err(ApiError::unavailable(
        "Could not find a free port in the node's range",
    ))
}

pub async fn create_server(
    state: &AppState,
    owner: &User,
    mut req: CreateServerRequest,
    ip: Option<&str>,
) -> ApiResult<Server> {
    let name = validate_name(&req.name).await?;
    clamp_limits(&mut req)?;
    let egg = state
        .eggs
        .get(&req.egg_id)
        .ok_or_else(|| ApiError::not_found("Egg not found"))?;
    let node = pick_node(state, req.node_id.as_deref()).await?;
    node_capacity_ok(state, &node, req.memory_mb, req.disk_mb).await?;
    let image = req
        .image
        .clone()
        .unwrap_or_else(|| egg.default_image().unwrap_or_default().to_string());
    if !egg.docker_images.iter().any(|i| i == &image) {
        return Err(ApiError::bad_request(
            "The selected image is not provided by this egg",
        ));
    }
    let allocation = allocate(state, &node, req.allocation_id.as_deref()).await?;

    // Merge and validate environment variables against the egg definition.
    let mut env: BTreeMap<String, String> = egg.variable_defaults().into_iter().collect();
    for (key, value) in &req.variables {
        let variable = egg
            .variables
            .iter()
            .find(|v| &v.key == key)
            .ok_or_else(|| ApiError::bad_request(format!("Unknown variable '{key}'")))?;
        crate::eggs::validate_variable(variable, value).map_err(|e| ApiError::bad_request(e.to_string()))?;
        env.insert(key.clone(), value.clone());
    }
    for variable in &egg.variables {
        if let Some(value) = env.get(&variable.key) {
            crate::eggs::validate_variable(variable, value)
                .map_err(|e| ApiError::bad_request(e.to_string()))?;
        }
    }
    let container_port = primary_container_port(&egg, &allocation);
    env.insert("SERVER_MEMORY".into(), req.memory_mb.to_string());
    env.insert("SERVER_PORT".into(), container_port.to_string());
    env.insert("SERVER_IP".into(), "0.0.0.0".into());
    env.insert("TZ".into(), state.config.docker.timezone.clone());
    crate::runtime::validate_env(&env).map_err(|e| ApiError::bad_request(e.to_string()))?;

    // Resolve placeholders such as {{SERVER_MEMORY}} inside environment values.
    let env_snapshot: std::collections::HashMap<String, String> =
        env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    for value in env.values_mut() {
        *value = crate::eggs::render_template(value, &env_snapshot);
    }
    let id = uuid::Uuid::new_v4().to_string();
    let server_uuid = uuid::Uuid::new_v4().to_string();
    let now = now_ms();
    let restart_policy = req.restart_policy.clone().unwrap_or_else(|| "on-failure".into());
    if !matches!(restart_policy.as_str(), "always" | "on-failure" | "never") {
        return Err(ApiError::bad_request("Invalid restart policy"));
    }
    sqlx::query(&state.db.sql(
        "INSERT INTO servers (id, uuid, name, description, owner_id, node_id, egg_id, image, startup, stop_command, env,
            memory_mb, swap_mb, disk_mb, cpu_percent, pids_limit, io_weight, auto_restart, restart_policy, crash_count,
            status, suspended, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?, 0, ?, ?)",
    ))
    .bind(&id)
    .bind(&server_uuid)
    .bind(&name)
    .bind(req.description.chars().take(500).collect::<String>())
    .bind(&owner.id)
    .bind(&node.id)
    .bind(&egg.id)
    .bind(&image)
    .bind(&egg.startup)
    .bind(&egg.stop_command)
    .bind(serde_json::to_string(&env).unwrap_or_else(|_| "{}".into()))
    .bind(req.memory_mb)
    .bind(req.swap_mb)
    .bind(req.disk_mb)
    .bind(req.cpu_percent)
    .bind(req.pids_limit.unwrap_or(512))
    .bind(req.io_weight.unwrap_or(500))
    .bind(if req.auto_restart { 1i64 } else { 0i64 })
    .bind(&restart_policy)
    .bind(STATUS_CREATING)
    .bind(now)
    .bind(now)
    .execute(&state.db.pool)
    .await?;
    for variable in &egg.variables {
        let value = env.get(&variable.key).cloned().unwrap_or_default();
        sqlx::query(&state.db.sql(
            "INSERT INTO server_variables (id, server_id, key, value, created_at) VALUES (?, ?, ?, ?, ?)",
        ))
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(&id)
        .bind(&variable.key)
        .bind(&value)
        .bind(now)
        .execute(&state.db.pool)
        .await?;
    }
    sqlx::query(
        &state
            .db
            .sql("UPDATE allocations SET server_id = ?, is_primary = 1 WHERE id = ?"),
    )
    .bind(&id)
    .bind(&allocation.id)
    .execute(&state.db.pool)
    .await?;
    crate::audit::record(
        &state.db,
        Some(&owner.id),
        &owner.username,
        "server.create",
        &server_uuid,
        ip,
        json!({ "name": name, "egg": egg.id, "node": node.name }),
    )
    .await;
    let server = load_server(state, &id).await?;
    let start = req.start_on_create;
    let state_clone = state.clone();
    let server_clone = server.clone();
    let egg_clone = egg.clone();
    tokio::spawn(async move {
        provision(state_clone, server_clone, egg_clone, start).await;
    });
    Ok(server)
}

fn primary_container_port(egg: &Egg, allocation: &Allocation) -> u16 {
    egg.ports.first().map(|p| p.port).unwrap_or(allocation.port)
}

pub async fn create_container(state: &AppState, server: &Server) -> ApiResult<String> {
    let runtime = state
        .runtimes
        .get(&server.node_id)
        .map_err(|e| ApiError::unavailable(e.to_string()))?;
    let spec = build_container_spec(state, server).await?;
    let container = runtime.create(&spec).await.map_err(|e| ApiError::internal(e))?;
    sqlx::query(
        &state
            .db
            .sql("UPDATE servers SET container_id = ?, status = ?, updated_at = ? WHERE id = ?"),
    )
    .bind(&container)
    .bind(STATUS_OFFLINE)
    .bind(now_ms())
    .bind(&server.id)
    .execute(&state.db.pool)
    .await?;
    Ok(container)
}

/// Recreate a container after settings changed, preserving the running state.
pub async fn recreate_container(state: &AppState, server: &Server) -> ApiResult<()> {
    let was_running = server.status == STATUS_RUNNING;
    if was_running {
        stop_server(state, server).await?;
    }
    let fresh = load_server(state, &server.id).await?;
    if let Some(container) = &fresh.container_id {
        if let Ok(runtime) = state.runtimes.get(&fresh.node_id) {
            let _ = runtime.remove(container).await;
        }
    }
    create_container(state, &fresh).await?;
    if was_running {
        let updated = load_server(state, &server.id).await?;
        start_server(state, &updated).await?;
    }
    Ok(())
}

async fn build_container_spec(state: &AppState, server: &Server) -> ApiResult<ContainerSpec> {
    let egg = state
        .eggs
        .get(&server.egg_id)
        .ok_or_else(|| ApiError::internal("Server egg is missing"))?;
    let allocs = allocations(state, &server.id).await?;
    let primary = allocs.iter().find(|a| a.is_primary).or_else(|| allocs.first());
    let container_port = primary.map(|a| primary_container_port(&egg, a)).unwrap_or(25565);
    let mut ports = Vec::new();
    for (index, allocation) in allocs.iter().enumerate() {
        ports.push(PortMapping {
            host_ip: allocation.ip.clone(),
            host_port: allocation.port,
            container_port: if index == 0 {
                container_port
            } else {
                allocation.port
            },
            protocol: egg
                .ports
                .first()
                .map(|p| p.protocol.clone())
                .unwrap_or_else(|| "tcp".into()),
        });
    }
    let mut env = server.env_map();
    env.insert("SERVER_PORT".into(), container_port.to_string());
    env.entry("SERVER_IP".to_string())
        .or_insert_with(|| "0.0.0.0".into());
    let env_btree: BTreeMap<String, String> = env.into_iter().collect();
    let labels = BTreeMap::from([
        ("porgous.managed".to_string(), "true".to_string()),
        ("porgous.server".to_string(), server.uuid.clone()),
        ("porgous.server_id".to_string(), server.id.clone()),
    ]);
    Ok(ContainerSpec {
        name: format!("server-{}", server.uuid),
        image: server.image.clone(),
        env: env_btree,
        cmd: if server.startup.trim().is_empty() {
            None
        } else {
            Some(server.render_startup())
        },
        entrypoint: egg.entrypoint.clone(),
        memory_mb: server.memory_mb,
        swap_mb: server.swap_mb,
        cpu_percent: server.cpu_percent,
        pids_limit: server.pids_limit,
        io_weight: server.io_weight,
        volume_id: server.uuid.clone(),
        ports,
        labels,
        user: None,
        network: state.config.docker.network.clone(),
        timezone: state.config.docker.timezone.clone(),
        stop_signal: Some("SIGTERM".into()),
        hostname: None,
    })
}

async fn provision(state: AppState, server: Server, egg: Egg, start_on_create: bool) {
    let result = provision_inner(&state, &server, &egg).await;
    match result {
        Ok(()) => {
            if start_on_create {
                if let Err(err) = start_server(&state, &server).await {
                    set_status(&state, &server.id, STATUS_ERROR, Some(&err.to_string())).await;
                    crate::audit::notify(
                        &state.db,
                        &server.owner_id,
                        "Server failed to start",
                        &format!("{} could not be started: {err}", server.name),
                        "error",
                    )
                    .await;
                }
            } else {
                set_status(&state, &server.id, STATUS_OFFLINE, None).await;
            }
        }
        Err(err) => {
            tracing::error!(server = %server.uuid, error = %err, "provisioning failed");
            set_status(&state, &server.id, STATUS_ERROR, Some(&err.to_string())).await;
            crate::audit::notify(
                &state.db,
                &server.owner_id,
                "Server installation failed",
                &format!("{}: {err}", server.name),
                "error",
            )
            .await;
        }
    }
}

async fn provision_inner(state: &AppState, server: &Server, egg: &Egg) -> Result<()> {
    set_status(state, &server.id, STATUS_INSTALLING, None).await;
    let runtime = state.runtimes.get(&server.node_id)?;
    runtime.ensure_image(&server.image).await?;
    if let Some(install) = &egg.install {
        let env = server.env_map().into_iter().collect();
        let install_log = runtime
            .run_install(&InstallSpec {
                image: install.image.clone(),
                script: install.script.clone(),
                entrypoint: install.entrypoint.clone(),
                env,
                volume_id: server.uuid.clone(),
                memory_mb: server.memory_mb.max(512),
                labels: BTreeMap::from([("porgous.install".to_string(), server.uuid.clone())]),
                network: state.config.docker.network.clone(),
            })
            .await?;
        sqlx::query(
            &state
                .db
                .sql("UPDATE servers SET install_log = ?, updated_at = ? WHERE id = ?"),
        )
        .bind(install_log)
        .bind(now_ms())
        .bind(&server.id)
        .execute(&state.db.pool)
        .await?;
    }
    create_container(state, server).await?;
    crate::audit::record(
        &state.db,
        Some(&server.owner_id),
        "system",
        "server.installed",
        &server.uuid,
        None,
        json!({ "server": server.uuid, "egg": egg.id }),
    )
    .await;
    Ok(())
}

async fn runtime_for(state: &AppState, server: &Server) -> ApiResult<std::sync::Arc<dyn ContainerRuntime>> {
    let node = load_node_for(state, server).await?;
    if node.maintenance {
        return Err(ApiError::conflict("The node is in maintenance mode"));
    }
    state
        .runtimes
        .get(&server.node_id)
        .map_err(|e| ApiError::unavailable(e.to_string()))
}

pub async fn start_server(state: &AppState, server: &Server) -> ApiResult<()> {
    if server.suspended {
        return Err(ApiError::forbidden("Server is suspended"));
    }
    if !server.is_installed() {
        return Err(ApiError::conflict("Server is still installing"));
    }
    let runtime = runtime_for(state, server).await?;
    let container = match server.container_id.clone() {
        Some(container) => container,
        None => {
            let egg = state
                .eggs
                .get(&server.egg_id)
                .ok_or_else(|| ApiError::internal("Server egg is missing"))?;
            provision_inner(state, server, &egg)
                .await
                .map_err(ApiError::internal)?;
            load_server(state, &server.id)
                .await?
                .container_id
                .ok_or_else(|| ApiError::internal("Container missing"))?
        }
    };
    set_status(state, &server.id, STATUS_STARTING, None).await;
    state.console.publish(
        &server.id,
        crate::console::ConsoleEvent::Status(STATUS_STARTING.into()),
    );
    runtime
        .ensure_image(&server.image)
        .await
        .map_err(ApiError::internal)?;
    runtime
        .start(&container)
        .await
        .map_err(|e| ApiError::unavailable(format!("Could not start the container: {e}")))?;
    sqlx::query(
        &state
            .db
            .sql("UPDATE servers SET last_started_at = ?, updated_at = ? WHERE id = ?"),
    )
    .bind(now_ms())
    .bind(now_ms())
    .bind(&server.id)
    .execute(&state.db.pool)
    .await?;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    match runtime.inspect(&container).await {
        Ok(stats) if stats.running => {
            set_status(state, &server.id, STATUS_RUNNING, None).await;
            if let Ok(updated) = load_server(state, &server.id).await {
                state.console.ensure_attached(state.runtimes.clone(), updated);
            }
        }
        Ok(stats) => {
            let reason = format!(
                "Container exited during startup (exit code {:?})",
                stats.exit_code
            );
            set_status(state, &server.id, STATUS_ERROR, Some(&reason)).await;
            return Err(ApiError::unavailable(reason));
        }
        Err(err) => {
            set_status(state, &server.id, STATUS_ERROR, Some(&err.to_string())).await;
            return Err(ApiError::unavailable(format!("Could not verify startup: {err}")));
        }
    }
    crate::audit::record(
        &state.db,
        None,
        "system",
        "server.start",
        &server.uuid,
        None,
        json!({}),
    )
    .await;
    Ok(())
}

pub async fn stop_server(state: &AppState, server: &Server) -> ApiResult<()> {
    let Some(container) = server.container_id.clone() else {
        set_status(state, &server.id, STATUS_OFFLINE, None).await;
        return Ok(());
    };
    let runtime = runtime_for(state, server).await?;
    set_status(state, &server.id, STATUS_STOPPING, None).await;
    if let Some(command) = server.stop_command.as_deref().filter(|c| !c.trim().is_empty()) {
        let _ = state
            .console
            .send_command(&state.runtimes, server, &container, command)
            .await;
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    if let Err(err) = runtime
        .stop(&container, state.config.docker.stop_timeout_secs)
        .await
    {
        tracing::warn!(server = %server.uuid, error = %err, "graceful stop failed; killing");
        let _ = runtime.kill(&container).await;
    }
    set_status(state, &server.id, STATUS_OFFLINE, None).await;
    state.console.detach(&server.id);
    crate::audit::record(
        &state.db,
        None,
        "system",
        "server.stop",
        &server.uuid,
        None,
        json!({}),
    )
    .await;
    Ok(())
}

pub async fn kill_server(state: &AppState, server: &Server) -> ApiResult<()> {
    let Some(container) = server.container_id.clone() else {
        set_status(state, &server.id, STATUS_OFFLINE, None).await;
        return Ok(());
    };
    let runtime = runtime_for(state, server).await?;
    runtime
        .kill(&container)
        .await
        .map_err(|e| ApiError::unavailable(e.to_string()))?;
    set_status(state, &server.id, STATUS_OFFLINE, None).await;
    state.console.detach(&server.id);
    crate::audit::record(
        &state.db,
        None,
        "system",
        "server.kill",
        &server.uuid,
        None,
        json!({}),
    )
    .await;
    Ok(())
}

pub async fn restart_server(state: &AppState, server: &Server) -> ApiResult<()> {
    stop_server(state, server).await?;
    let fresh = load_server(state, &server.id).await?;
    start_server(state, &fresh).await
}

pub async fn set_suspended(state: &AppState, server: &Server, suspended: bool) -> ApiResult<()> {
    if suspended {
        let _ = kill_server(state, server).await;
        set_status(state, &server.id, STATUS_SUSPENDED, None).await;
    } else {
        set_status(state, &server.id, STATUS_OFFLINE, None).await;
    }
    sqlx::query(
        &state
            .db
            .sql("UPDATE servers SET suspended = ?, updated_at = ? WHERE id = ?"),
    )
    .bind(if suspended { 1i64 } else { 0i64 })
    .bind(now_ms())
    .bind(&server.id)
    .execute(&state.db.pool)
    .await?;
    Ok(())
}

pub async fn delete_server(state: &AppState, server: &Server) -> ApiResult<()> {
    if let Some(container) = server.container_id.clone() {
        if let Ok(runtime) = state.runtimes.get(&server.node_id) {
            let _ = runtime.kill(&container).await;
            let _ = runtime.remove(&container).await;
        }
    }
    state.console.detach(&server.id);
    // Remove files and backups on the node.
    if let Ok(node) = load_node_for(state, server).await {
        if node.is_local {
            let _ = std::fs::remove_dir_all(state.config.volumes_dir().join(&server.uuid));
            let _ = std::fs::remove_dir_all(state.config.backups_dir().join(&server.uuid));
        } else if let Ok(client) = state.node_client(&node) {
            let _ = client
                .send(
                    Method::POST,
                    "/v1/volumes/delete",
                    Some(&json!({ "volume": server.uuid })),
                )
                .await;
        }
    }
    for table in ["server_variables", "subusers", "schedules", "metrics"] {
        let _ = sqlx::query(&state.db.sql(&format!("DELETE FROM {table} WHERE server_id = ?")))
            .bind(&server.id)
            .execute(&state.db.pool)
            .await;
    }
    let _ = sqlx::query(&state.db.sql("DELETE FROM backups WHERE server_id = ?"))
        .bind(&server.id)
        .execute(&state.db.pool)
        .await;
    let _ = sqlx::query(
        &state
            .db
            .sql("DELETE FROM notifications WHERE user_id = ? AND title LIKE 'Server%'"),
    )
    .bind(&server.owner_id)
    .execute(&state.db.pool)
    .await;
    sqlx::query(
        &state
            .db
            .sql("UPDATE allocations SET server_id = NULL, is_primary = 0 WHERE server_id = ?"),
    )
    .bind(&server.id)
    .execute(&state.db.pool)
    .await?;
    sqlx::query(&state.db.sql("DELETE FROM servers WHERE id = ?"))
        .bind(&server.id)
        .execute(&state.db.pool)
        .await?;
    crate::audit::record(
        &state.db,
        None,
        "system",
        "server.delete",
        &server.uuid,
        None,
        json!({}),
    )
    .await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Backups
// ---------------------------------------------------------------------------

async fn update_backup(
    state: &AppState,
    id: &str,
    status: &str,
    bytes: i64,
    checksum: Option<&str>,
    error: Option<&str>,
) {
    let _ = sqlx::query(&state.db.sql(
        "UPDATE backups SET status = ?, bytes = ?, checksum = ?, error = ?, completed_at = ? WHERE id = ?",
    ))
    .bind(status)
    .bind(bytes)
    .bind(checksum)
    .bind(error)
    .bind(if status == "completed" {
        Some(now_ms())
    } else {
        None
    })
    .bind(id)
    .execute(&state.db.pool)
    .await;
}

pub async fn create_backup(state: &AppState, server: &Server, name: &str) -> ApiResult<Backup> {
    let name = if name.trim().is_empty() {
        "Manual backup".to_string()
    } else {
        name.trim().to_string()
    };
    if name.len() > 100 {
        return Err(ApiError::bad_request("Backup name is too long"));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let backup_uuid = uuid::Uuid::new_v4().to_string();
    let filename = format!("{backup_uuid}.tar.gz");
    sqlx::query(&state.db.sql(
        "INSERT INTO backups (id, server_id, uuid, name, filename, bytes, status, created_at)
         VALUES (?, ?, ?, ?, ?, 0, 'creating', ?)",
    ))
    .bind(&id)
    .bind(&server.id)
    .bind(&backup_uuid)
    .bind(&name)
    .bind(&filename)
    .bind(now_ms())
    .execute(&state.db.pool)
    .await?;
    let state_clone = state.clone();
    let server_clone = server.clone();
    let id_clone = id.clone();
    tokio::spawn(async move {
        let result = create_backup_job(&state_clone, &server_clone, &backup_uuid).await;
        match result {
            Ok((bytes, checksum)) => {
                update_backup(&state_clone, &id_clone, "completed", bytes, Some(&checksum), None).await;
                crate::audit::notify(
                    &state_clone.db,
                    &server_clone.owner_id,
                    "Backup completed",
                    &format!("Backup for {} completed ({bytes} bytes)", server_clone.name),
                    "success",
                )
                .await;
            }
            Err(err) => {
                update_backup(&state_clone, &id_clone, "failed", 0, None, Some(&err.to_string())).await;
                crate::audit::notify(
                    &state_clone.db,
                    &server_clone.owner_id,
                    "Backup failed",
                    &format!("Backup for {} failed: {err}", server_clone.name),
                    "error",
                )
                .await;
            }
        }
    });
    let row: Option<AnyRow> = sqlx::query(&state.db.sql("SELECT * FROM backups WHERE id = ?"))
        .bind(&id)
        .fetch_optional(&state.db.pool)
        .await?;
    row.map(|r| Backup::from_row(&r))
        .ok_or_else(|| ApiError::internal("backup row missing"))
}

async fn create_backup_job(state: &AppState, server: &Server, backup_uuid: &str) -> Result<(i64, String)> {
    let node = load_node_for(state, server).await.map_err(|e| anyhow!("{e}"))?;
    if node.is_local {
        let volumes = state.config.volumes_dir().join(&server.uuid);
        let paths = BackupPaths::new(state.config.backups_dir());
        let destination = paths.archive(&server.uuid, backup_uuid);
        let (size, checksum) =
            tokio::task::spawn_blocking(move || backup::create_archive(&volumes, &destination))
                .await
                .context("backup task failed")?
                .context("creating backup")?;
        Ok((size as i64, checksum))
    } else {
        let client = state.node_client(&node).map_err(|e| anyhow!("{e}"))?;
        let value = client
            .send_with_timeout(
                Method::POST,
                "/v1/backups",
                Some(&json!({ "volume": server.uuid, "backup_uuid": backup_uuid })),
                Duration::from_secs(1900),
            )
            .await?;
        Ok((
            value["bytes"].as_i64().unwrap_or(0),
            value["checksum"].as_str().unwrap_or_default().to_string(),
        ))
    }
}

pub async fn restore_backup(state: &AppState, server: &Server, backup: &Backup) -> ApiResult<()> {
    if backup.status != "completed" {
        return Err(ApiError::conflict("Backup is not complete"));
    }
    if server.is_active() {
        stop_server(state, server).await?;
    }
    let node = load_node_for(state, server).await?;
    if node.is_local {
        let paths = BackupPaths::new(state.config.backups_dir());
        let archive = paths.archive(&server.uuid, &backup.uuid);
        let checksum = backup
            .checksum
            .clone()
            .ok_or_else(|| ApiError::conflict("Backup has no checksum"))?;
        let volume = state.config.volumes_dir().join(&server.uuid);
        let max_bytes = (server.disk_mb as u64)
            .saturating_mul(1024 * 1024)
            .saturating_mul(4)
            .max(1024 * 1024 * 1024);
        tokio::task::spawn_blocking(move || {
            backup::verify_checksum(&archive, &checksum)?;
            backup::restore_archive(&archive, &volume, max_bytes)
        })
        .await
        .map_err(ApiError::internal)?
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    } else {
        let client = state.node_client(&node)?;
        client
            .send_with_timeout(
                Method::POST,
                "/v1/backups/restore",
                Some(&json!({ "volume": server.uuid, "backup_uuid": backup.uuid, "checksum": backup.checksum })),
                Duration::from_secs(1900),
            )
            .await
            .map_err(ApiError::internal)?;
    }
    crate::audit::record(
        &state.db,
        None,
        "system",
        "backup.restore",
        &server.uuid,
        None,
        json!({ "backup": backup.uuid }),
    )
    .await;
    Ok(())
}

pub async fn delete_backup(state: &AppState, server: &Server, backup: &Backup) -> ApiResult<()> {
    let node = load_node_for(state, server).await?;
    if node.is_local {
        let paths = BackupPaths::new(state.config.backups_dir());
        let _ = backup::delete_archive(&paths.archive(&server.uuid, &backup.uuid));
    } else if let Ok(client) = state.node_client(&node) {
        let _ = client
            .send(
                Method::POST,
                "/v1/backups/delete",
                Some(&json!({ "volume": server.uuid, "backup_uuid": backup.uuid })),
            )
            .await;
    }
    sqlx::query(&state.db.sql("DELETE FROM backups WHERE id = ?"))
        .bind(&backup.id)
        .execute(&state.db.pool)
        .await?;
    crate::audit::record(
        &state.db,
        None,
        "system",
        "backup.delete",
        &server.uuid,
        None,
        json!({ "backup": backup.uuid }),
    )
    .await;
    Ok(())
}

pub async fn backup_download(state: &AppState, server: &Server, backup: &Backup) -> ApiResult<Download> {
    let node = load_node_for(state, server).await?;
    if node.is_local {
        let paths = BackupPaths::new(state.config.backups_dir());
        let path = paths.archive(&server.uuid, &backup.uuid);
        if !path.exists() {
            return Err(ApiError::not_found("Backup file not found"));
        }
        let file = std::fs::File::open(&path).map_err(ApiError::internal)?;
        let size = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Download::Local(tokio::fs::File::from_std(file), size))
    } else {
        let client = state.node_client(&node)?;
        let response = client
            .get_stream(&format!(
                "/v1/backups/download?volume={}&backup_uuid={}",
                urlencode(&server.uuid),
                urlencode(&backup.uuid)
            ))
            .await
            .map_err(|e| ApiError::bad_request(e.to_string()))?;
        Ok(Download::Remote(response))
    }
}

/// Find the egg and node name for display without extra round trips.
pub async fn server_summary(state: &AppState, server: &Server) -> serde_json::Value {
    let allocs = allocations(state, &server.id).await.unwrap_or_default();
    let primary = allocs.iter().find(|a| a.is_primary).or_else(|| allocs.first());
    let node_name = crate::state::load_node(&state.db, &server.node_id)
        .await
        .ok()
        .flatten()
        .map(|n| n.name)
        .unwrap_or_default();
    let owner = crate::auth::find_user(&state.db, &server.owner_id)
        .await
        .ok()
        .flatten()
        .map(|u| u.username)
        .unwrap_or_default();
    json!({
        "server": server,
        "egg_name": state.eggs.get(&server.egg_id).map(|e| e.name).unwrap_or_else(|| server.egg_id.clone()),
        "node_name": node_name,
        "owner_name": owner,
        "primary_allocation": primary.map(|a| a.address()),
        "allocations": allocs,
        "uptime_seconds": if server.status == STATUS_RUNNING { server.last_started_at.map(|s| (now_ms() - s) / 1000).unwrap_or(0) } else { 0 },
    })
}

pub fn local_volume_path(state: &AppState, server: &Server) -> PathBuf {
    state.config.volumes_dir().join(&server.uuid)
}

pub fn paths_for(state: &AppState) -> BackupPaths {
    BackupPaths::new(state.config.backups_dir())
}

pub fn egg_store(state: &AppState) -> &EggStore {
    &state.eggs
}

/// Read the environment variables for a server, including derived values.
pub async fn environment(state: &AppState, server: &Server) -> ApiResult<BTreeMap<String, String>> {
    let mut env: BTreeMap<String, String> = server.env_map().into_iter().collect();
    let allocs = allocations(state, &server.id).await?;
    if let Some(primary) = allocs.iter().find(|a| a.is_primary).or_else(|| allocs.first()) {
        env.insert(
            "SERVER_PORT".into(),
            primary_container_port(
                &state
                    .eggs
                    .get(&server.egg_id)
                    .unwrap_or_else(|| placeholder_egg()),
                primary,
            )
            .to_string(),
        );
        env.insert("SERVER_HOST_PORT".into(), primary.port.to_string());
        env.insert("SERVER_IP".into(), "0.0.0.0".into());
    }
    Ok(env)
}

fn placeholder_egg() -> Egg {
    Egg {
        id: "missing".into(),
        name: "Missing egg".into(),
        description: String::new(),
        author: String::new(),
        category: String::new(),
        docker_images: vec!["alpine".into()],
        entrypoint: None,
        startup: String::new(),
        stop_command: String::new(),
        startup_detection: None,
        install: None,
        variables: vec![],
        ports: vec![],
        notes: String::new(),
        version: 1,
    }
}

/// Ensure an allocation belongs to a server before attaching it.
pub async fn attach_allocation(
    state: &AppState,
    server: &Server,
    allocation_id: &str,
) -> ApiResult<Allocation> {
    let row: Option<AnyRow> = sqlx::query(&state.db.sql("SELECT * FROM allocations WHERE id = ?"))
        .bind(allocation_id)
        .fetch_optional(&state.db.pool)
        .await?;
    let allocation = row
        .map(|r| Allocation::from_row(&r))
        .ok_or_else(|| ApiError::not_found("Allocation not found"))?;
    if allocation.node_id != server.node_id {
        return Err(ApiError::bad_request("Allocation belongs to a different node"));
    }
    if allocation.server_id.is_some() {
        return Err(ApiError::conflict("Allocation is already in use"));
    }
    sqlx::query(
        &state
            .db
            .sql("UPDATE allocations SET server_id = ?, is_primary = 0 WHERE id = ?"),
    )
    .bind(&server.id)
    .bind(&allocation_id)
    .execute(&state.db.pool)
    .await?;
    if let Some(container) = &server.container_id {
        if let Ok(runtime) = state.runtimes.get(&server.node_id) {
            let _ = runtime.remove(container).await;
            if let Ok(egg) = state
                .eggs
                .get(&server.egg_id)
                .ok_or_else(|| anyhow!("missing egg"))
            {
                let spec = build_container_spec(state, server)
                    .await
                    .map_err(|e| anyhow!("{e}"))?;
                if let Ok(new_id) = runtime.create(&spec).await {
                    let _ = sqlx::query(&state.db.sql("UPDATE servers SET container_id = ? WHERE id = ?"))
                        .bind(&new_id)
                        .bind(&server.id)
                        .execute(&state.db.pool)
                        .await;
                }
                let _ = egg;
            }
        }
    }
    Ok(allocation)
}

pub async fn detach_allocation(state: &AppState, server: &Server, allocation: &Allocation) -> ApiResult<()> {
    if allocation.server_id.as_deref() != Some(server.id.as_str()) {
        return Err(ApiError::not_found("Allocation is not attached to this server"));
    }
    if allocation.is_primary {
        return Err(ApiError::bad_request("The primary allocation cannot be detached"));
    }
    sqlx::query(
        &state
            .db
            .sql("UPDATE allocations SET server_id = NULL, is_primary = 0 WHERE id = ?"),
    )
    .bind(&allocation.id)
    .execute(&state.db.pool)
    .await?;
    Ok(())
}

pub fn is_owner_or_admin(user: &User, server: &Server) -> bool {
    user.root_admin || user.id == server.owner_id
}

/// Validate a restart policy string.
pub fn valid_restart_policy(value: &str) -> bool {
    matches!(value, "always" | "on-failure" | "never")
}

/// Query helper used by the dashboard.
pub async fn owners_server_count(state: &AppState, user_id: &str) -> ApiResult<i64> {
    let row: Option<AnyRow> = sqlx::query(
        &state
            .db
            .sql("SELECT COUNT(*) AS count FROM servers WHERE owner_id = ?"),
    )
    .bind(user_id)
    .fetch_optional(&state.db.pool)
    .await?;
    Ok(row.map(|r| get_i64(&r, "count")).unwrap_or(0))
}

/// Check whether the container still exists and is running.
pub async fn refresh_state(state: &AppState, server: &Server) -> ApiResult<Server> {
    let Some(container) = &server.container_id else {
        return Ok(server.clone());
    };
    let Ok(runtime) = state.runtimes.get(&server.node_id) else {
        return Ok(server.clone());
    };
    if let Ok(stats) = runtime.inspect(container).await {
        let new_status = if stats.running {
            STATUS_RUNNING.to_string()
        } else if server.status == STATUS_STOPPING {
            STATUS_OFFLINE.to_string()
        } else if server.suspended {
            STATUS_SUSPENDED.to_string()
        } else {
            server.status.clone()
        };
        if new_status != server.status
            && !matches!(server.status.as_str(), STATUS_CREATING | STATUS_INSTALLING)
        {
            set_status(state, &server.id, &new_status, None).await;
            return load_server(state, &server.id).await;
        }
    }
    Ok(server.clone())
}

pub fn backup_path_for(state: &AppState, server: &Server, backup: &Backup) -> PathBuf {
    BackupPaths::new(state.config.backups_dir()).archive(&server.uuid, &backup.uuid)
}

pub fn server_dir_for(state: &AppState, server: &Server) -> PathBuf {
    state.config.volumes_dir().join(&server.uuid)
}

pub fn install_log(server: &Server) -> Option<&str> {
    server.install_log.as_deref()
}

pub fn get_string(row: &AnyRow, column: &str) -> String {
    get_str(row, column)
}

pub fn get_optional(row: &AnyRow, column: &str) -> Option<String> {
    get_opt_str(row, column)
}

pub fn mount_path() -> &'static str {
    VOLUME_MOUNT
}

pub fn require_node_runtime(state: &AppState, server: &Server) -> ApiResult<()> {
    state
        .runtimes
        .get(&server.node_id)
        .map(|_| ())
        .map_err(|e| ApiError::unavailable(e.to_string()))
}

pub fn is_suspended(server: &Server) -> bool {
    server.suspended || server.status == STATUS_SUSPENDED
}

pub fn is_active(server: &Server) -> bool {
    server.is_active()
}

pub fn validate_restart_policy(policy: &str) -> Result<()> {
    if valid_restart_policy(policy) {
        Ok(())
    } else {
        bail!("invalid restart policy")
    }
}
