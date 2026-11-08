//! Database models. Structs contain secrets only where required internally;
//! API responses use the dedicated view types that never include them.

use crate::db::{get_bool, get_i64, get_opt_i64, get_opt_str, get_str};
use serde::{Deserialize, Serialize};
use sqlx::any::AnyRow;

pub const STATUS_OFFLINE: &str = "offline";
pub const STATUS_STARTING: &str = "starting";
pub const STATUS_RUNNING: &str = "running";
pub const STATUS_STOPPING: &str = "stopping";
pub const STATUS_INSTALLING: &str = "installing";
pub const STATUS_CREATING: &str = "creating";
pub const STATUS_SUSPENDED: &str = "suspended";
pub const STATUS_ERROR: &str = "error";

pub const ALL_SERVER_PERMISSIONS: &[&str] = &[
    "console.read",
    "console.write",
    "control.start",
    "control.stop",
    "control.restart",
    "control.kill",
    "files.read",
    "files.write",
    "files.delete",
    "backups.read",
    "backups.create",
    "backups.restore",
    "backups.delete",
    "schedules.read",
    "schedules.write",
    "settings.read",
    "settings.write",
    "users.read",
    "users.write",
];

#[derive(Debug, Clone)]
pub struct User {
    pub id: String,
    pub email: String,
    pub username: String,
    pub password_hash: String,
    pub role: String,
    pub root_admin: bool,
    pub suspended: bool,
    pub totp_secret_enc: Option<String>,
    pub totp_enabled: bool,
    pub totp_recovery: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_login_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UserView {
    pub id: String,
    pub email: String,
    pub username: String,
    pub role: String,
    pub root_admin: bool,
    pub suspended: bool,
    pub totp_enabled: bool,
    pub created_at: i64,
    pub last_login_at: Option<i64>,
}

impl User {
    pub fn from_row(row: &AnyRow) -> Self {
        Self {
            id: get_str(row, "id"),
            email: get_str(row, "email"),
            username: get_str(row, "username"),
            password_hash: get_str(row, "password_hash"),
            role: get_str(row, "role"),
            root_admin: get_bool(row, "root_admin"),
            suspended: get_bool(row, "suspended"),
            totp_secret_enc: get_opt_str(row, "totp_secret"),
            totp_enabled: get_bool(row, "totp_enabled"),
            totp_recovery: get_opt_str(row, "totp_recovery"),
            created_at: get_i64(row, "created_at"),
            updated_at: get_i64(row, "updated_at"),
            last_login_at: get_opt_i64(row, "last_login_at"),
        }
    }

    pub fn view(&self) -> UserView {
        UserView {
            id: self.id.clone(),
            email: self.email.clone(),
            username: self.username.clone(),
            role: self.role.clone(),
            root_admin: self.root_admin,
            suspended: self.suspended,
            totp_enabled: self.totp_enabled,
            created_at: self.created_at,
            last_login_at: self.last_login_at,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ApiKey {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub prefix: String,
    pub scopes: Vec<String>,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
    pub expires_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub id: String,
    pub name: String,
    pub description: String,
    pub is_local: bool,
    pub fqdn: String,
    pub scheme: String,
    pub port: u16,
    pub token_enc: String,
    pub secret_enc: String,
    pub memory_mb: i64,
    pub disk_mb: i64,
    pub port_range_start: u16,
    pub port_range_end: u16,
    pub maintenance: bool,
    pub sftp_port: u16,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct NodeView {
    pub id: String,
    pub name: String,
    pub description: String,
    pub is_local: bool,
    pub fqdn: String,
    pub scheme: String,
    pub port: u16,
    pub memory_mb: i64,
    pub disk_mb: i64,
    pub port_range_start: u16,
    pub port_range_end: u16,
    pub maintenance: bool,
    pub sftp_port: u16,
    pub created_at: i64,
    pub updated_at: i64,
    pub server_count: i64,
    pub allocated_memory_mb: i64,
    pub allocated_disk_mb: i64,
    pub online: bool,
}

impl Node {
    pub fn from_row(row: &AnyRow) -> Self {
        Self {
            id: get_str(row, "id"),
            name: get_str(row, "name"),
            description: get_str(row, "description"),
            is_local: get_bool(row, "is_local"),
            fqdn: get_str(row, "fqdn"),
            scheme: get_str(row, "scheme"),
            port: get_i64(row, "port") as u16,
            token_enc: get_str(row, "token_enc"),
            secret_enc: get_str(row, "secret_enc"),
            memory_mb: get_i64(row, "memory_mb"),
            disk_mb: get_i64(row, "disk_mb"),
            port_range_start: get_i64(row, "port_range_start") as u16,
            port_range_end: get_i64(row, "port_range_end") as u16,
            maintenance: get_bool(row, "maintenance"),
            sftp_port: get_i64(row, "sftp_port") as u16,
            created_at: get_i64(row, "created_at"),
            updated_at: get_i64(row, "updated_at"),
        }
    }

    pub fn base_url(&self) -> String {
        format!("{}://{}:{}", self.scheme, self.fqdn, self.port)
    }

    pub fn view(&self, server_count: i64, allocated_memory_mb: i64, allocated_disk_mb: i64) -> NodeView {
        NodeView {
            id: self.id.clone(),
            name: self.name.clone(),
            description: self.description.clone(),
            is_local: self.is_local,
            fqdn: self.fqdn.clone(),
            scheme: self.scheme.clone(),
            port: self.port,
            memory_mb: self.memory_mb,
            disk_mb: self.disk_mb,
            port_range_start: self.port_range_start,
            port_range_end: self.port_range_end,
            maintenance: self.maintenance,
            sftp_port: self.sftp_port,
            created_at: self.created_at,
            updated_at: self.updated_at,
            server_count,
            allocated_memory_mb,
            allocated_disk_mb,
            online: true,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Server {
    pub id: String,
    pub uuid: String,
    pub name: String,
    pub description: String,
    pub owner_id: String,
    pub node_id: String,
    pub egg_id: String,
    pub image: String,
    pub startup: String,
    pub stop_command: Option<String>,
    pub env: serde_json::Value,
    pub memory_mb: i64,
    pub swap_mb: i64,
    pub disk_mb: i64,
    pub cpu_percent: i64,
    pub pids_limit: i64,
    pub io_weight: i64,
    pub auto_restart: bool,
    pub restart_policy: String,
    pub crash_count: i64,
    pub status: String,
    pub container_id: Option<String>,
    pub last_error: Option<String>,
    pub install_log: Option<String>,
    pub suspended: bool,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_started_at: Option<i64>,
    pub last_crashed_at: Option<i64>,
}

impl Server {
    pub fn from_row(row: &AnyRow) -> Self {
        let env_raw = get_str(row, "env");
        Self {
            id: get_str(row, "id"),
            uuid: get_str(row, "uuid"),
            name: get_str(row, "name"),
            description: get_str(row, "description"),
            owner_id: get_str(row, "owner_id"),
            node_id: get_str(row, "node_id"),
            egg_id: get_str(row, "egg_id"),
            image: get_str(row, "image"),
            startup: get_str(row, "startup"),
            stop_command: get_opt_str(row, "stop_command"),
            env: serde_json::from_str(&env_raw).unwrap_or_else(|_| serde_json::json!({})),
            memory_mb: get_i64(row, "memory_mb"),
            swap_mb: get_i64(row, "swap_mb"),
            disk_mb: get_i64(row, "disk_mb"),
            cpu_percent: get_i64(row, "cpu_percent"),
            pids_limit: get_i64(row, "pids_limit"),
            io_weight: get_i64(row, "io_weight"),
            auto_restart: get_bool(row, "auto_restart"),
            restart_policy: get_str(row, "restart_policy"),
            crash_count: get_i64(row, "crash_count"),
            status: get_str(row, "status"),
            container_id: get_opt_str(row, "container_id"),
            last_error: get_opt_str(row, "last_error"),
            install_log: get_opt_str(row, "install_log"),
            suspended: get_bool(row, "suspended"),
            created_at: get_i64(row, "created_at"),
            updated_at: get_i64(row, "updated_at"),
            last_started_at: get_opt_i64(row, "last_started_at"),
            last_crashed_at: get_opt_i64(row, "last_crashed_at"),
        }
    }

    pub fn is_active(&self) -> bool {
        matches!(
            self.status.as_str(),
            STATUS_RUNNING | STATUS_STARTING | STATUS_STOPPING
        )
    }

    pub fn is_installed(&self) -> bool {
        !matches!(self.status.as_str(), STATUS_CREATING | STATUS_INSTALLING)
    }

    pub fn env_map(&self) -> std::collections::HashMap<String, String> {
        self.env
            .as_object()
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Substitute `{{VAR}}` placeholders in a startup command.
    pub fn render_startup(&self) -> String {
        crate::eggs::render_template(&self.startup, &self.env_map())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Allocation {
    pub id: String,
    pub node_id: String,
    pub ip: String,
    pub port: u16,
    pub server_id: Option<String>,
    pub is_primary: bool,
    pub alias: Option<String>,
}

impl Allocation {
    pub fn from_row(row: &AnyRow) -> Self {
        Self {
            id: get_str(row, "id"),
            node_id: get_str(row, "node_id"),
            ip: get_str(row, "ip"),
            port: get_i64(row, "port") as u16,
            server_id: get_opt_str(row, "server_id"),
            is_primary: get_bool(row, "is_primary"),
            alias: get_opt_str(row, "alias"),
        }
    }
    pub fn address(&self) -> String {
        format!("{}:{}", self.ip, self.port)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ServerVariable {
    pub id: String,
    pub key: String,
    pub value: String,
}

impl ServerVariable {
    pub fn from_row(row: &AnyRow) -> Self {
        Self {
            id: get_str(row, "id"),
            key: get_str(row, "key"),
            value: get_str(row, "value"),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Subuser {
    pub id: String,
    pub server_id: String,
    pub user_id: String,
    pub username: String,
    pub email: String,
    pub permissions: Vec<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Backup {
    pub id: String,
    pub server_id: String,
    pub uuid: String,
    pub name: String,
    pub bytes: i64,
    pub checksum: Option<String>,
    pub status: String,
    pub error: Option<String>,
    pub created_at: i64,
    pub completed_at: Option<i64>,
}

impl Backup {
    pub fn from_row(row: &AnyRow) -> Self {
        Self {
            id: get_str(row, "id"),
            server_id: get_str(row, "server_id"),
            uuid: get_str(row, "uuid"),
            name: get_str(row, "name"),
            bytes: get_i64(row, "bytes"),
            checksum: get_opt_str(row, "checksum"),
            status: get_str(row, "status"),
            error: get_opt_str(row, "error"),
            created_at: get_i64(row, "created_at"),
            completed_at: get_opt_i64(row, "completed_at"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleTask {
    pub action: String,
    #[serde(default)]
    pub payload: String,
    #[serde(default)]
    pub offset_secs: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Schedule {
    pub id: String,
    pub server_id: String,
    pub name: String,
    pub cron: String,
    pub tasks: Vec<ScheduleTask>,
    pub enabled: bool,
    pub last_run_at: Option<i64>,
    pub next_run_at: Option<i64>,
    pub created_at: i64,
}

impl Schedule {
    pub fn from_row(row: &AnyRow) -> Self {
        let raw = get_str(row, "tasks");
        Self {
            id: get_str(row, "id"),
            server_id: get_str(row, "server_id"),
            name: get_str(row, "name"),
            cron: get_str(row, "cron"),
            tasks: serde_json::from_str(&raw).unwrap_or_default(),
            enabled: get_bool(row, "enabled"),
            last_run_at: get_opt_i64(row, "last_run_at"),
            next_run_at: get_opt_i64(row, "next_run_at"),
            created_at: get_i64(row, "created_at"),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AuditLog {
    pub id: String,
    pub user_id: Option<String>,
    pub actor: String,
    pub action: String,
    pub target: String,
    pub ip: Option<String>,
    pub metadata: serde_json::Value,
    pub created_at: i64,
}

impl AuditLog {
    pub fn from_row(row: &AnyRow) -> Self {
        let raw = get_str(row, "metadata");
        Self {
            id: get_str(row, "id"),
            user_id: get_opt_str(row, "user_id"),
            actor: get_str(row, "actor"),
            action: get_str(row, "action"),
            target: get_str(row, "target"),
            ip: get_opt_str(row, "ip"),
            metadata: serde_json::from_str(&raw).unwrap_or_else(|_| serde_json::json!({})),
            created_at: get_i64(row, "created_at"),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Notification {
    pub id: String,
    pub user_id: String,
    pub title: String,
    pub body: String,
    pub level: String,
    pub is_read: bool,
    pub created_at: i64,
}

impl Notification {
    pub fn from_row(row: &AnyRow) -> Self {
        Self {
            id: get_str(row, "id"),
            user_id: get_str(row, "user_id"),
            title: get_str(row, "title"),
            body: get_str(row, "body"),
            level: get_str(row, "level"),
            is_read: get_bool(row, "is_read"),
            created_at: get_i64(row, "created_at"),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Metric {
    pub server_id: String,
    pub cpu: f64,
    pub memory_bytes: i64,
    pub disk_bytes: i64,
    pub net_rx: i64,
    pub net_tx: i64,
    pub created_at: i64,
}

impl Metric {
    pub fn from_row(row: &AnyRow) -> Self {
        Self {
            server_id: get_str(row, "server_id"),
            cpu: crate::db::get_f64(row, "cpu"),
            memory_bytes: get_i64(row, "memory_bytes"),
            disk_bytes: get_i64(row, "disk_bytes"),
            net_rx: get_i64(row, "net_rx"),
            net_tx: get_i64(row, "net_tx"),
            created_at: get_i64(row, "created_at"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RuntimeStats {
    pub running: bool,
    pub cpu_percent: f64,
    pub memory_bytes: i64,
    pub memory_limit_bytes: i64,
    pub disk_bytes: i64,
    pub net_rx_bytes: i64,
    pub net_tx_bytes: i64,
    pub uptime_secs: i64,
    pub exit_code: Option<i64>,
    pub started_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServerAccess {
    pub server: Server,
    pub is_owner: bool,
    pub is_admin: bool,
    pub permissions: Vec<String>,
}
