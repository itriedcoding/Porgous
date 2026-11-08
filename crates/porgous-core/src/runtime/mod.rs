//! Container runtime abstraction: local Docker, remote node agent, and an
//! in-memory runtime used by tests.

use crate::models::RuntimeStats;
use anyhow::{bail, Result};
use async_trait::async_trait;
use futures_util::Stream;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use tokio::io::AsyncWrite;

pub mod fake;
pub mod local;
pub mod remote;

pub use fake::FakeRuntime;
pub use local::LocalRuntime;
pub use remote::RemoteRuntime;

pub const CONTAINER_PREFIX: &str = "porgous-";
pub const VOLUME_MOUNT: &str = "/home/container";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortMapping {
    pub host_ip: String,
    pub host_port: u16,
    pub container_port: u16,
    pub protocol: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerSpec {
    /// Container name, usually `porgous-<server uuid>`.
    pub name: String,
    pub image: String,
    pub env: BTreeMap<String, String>,
    /// Shell command line executed in the container.
    pub cmd: Option<String>,
    pub entrypoint: Option<Vec<String>>,
    pub memory_mb: i64,
    pub swap_mb: i64,
    pub cpu_percent: i64,
    pub pids_limit: i64,
    pub io_weight: i64,
    /// Server UUID, used to locate the volume on the node.
    pub volume_id: String,
    pub ports: Vec<PortMapping>,
    pub labels: BTreeMap<String, String>,
    pub user: Option<String>,
    pub network: String,
    pub timezone: String,
    pub stop_signal: Option<String>,
    pub hostname: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallSpec {
    pub image: String,
    pub script: String,
    pub entrypoint: Option<Vec<String>>,
    pub env: BTreeMap<String, String>,
    pub volume_id: String,
    pub memory_mb: i64,
    pub labels: BTreeMap<String, String>,
    pub network: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecResult {
    pub exit_code: i64,
    pub output: String,
}

pub struct AttachStream {
    pub output: Pin<Box<dyn Stream<Item = Result<Vec<u8>, String>> + Send>>,
    pub input: Pin<Box<dyn AsyncWrite + Send>>,
}

#[async_trait]
pub trait ContainerRuntime: Send + Sync {
    fn node_id(&self) -> &str;
    fn is_remote(&self) -> bool;
    /// Human readable runtime description for health information.
    fn description(&self) -> String;

    async fn ping(&self) -> Result<()>;
    async fn ensure_image(&self, image: &str) -> Result<()>;
    async fn create(&self, spec: &ContainerSpec) -> Result<String>;
    async fn start(&self, container: &str) -> Result<()>;
    async fn stop(&self, container: &str, timeout_secs: u64) -> Result<()>;
    async fn kill(&self, container: &str) -> Result<()>;
    async fn remove(&self, container: &str) -> Result<()>;
    async fn inspect(&self, container: &str) -> Result<RuntimeStats>;
    async fn stats(&self, container: &str) -> Result<RuntimeStats>;
    async fn logs(&self, container: &str, tail: u32) -> Result<String>;
    async fn logs_since(&self, container: &str, since_unix: i64, tail: u32) -> Result<Vec<String>>;
    async fn command(&self, container: &str, command: &str) -> Result<()>;
    async fn exec(&self, container: &str, cmd: Vec<String>) -> Result<ExecResult>;
    async fn run_install(&self, spec: &InstallSpec) -> Result<String>;
    /// Real-time attach used for local console streaming. Remote nodes use
    /// log polling through [`ContainerRuntime::logs_since`].
    async fn attach(&self, container: &str) -> Result<AttachStream>;
}

/// Holds the runtime for each node.
#[derive(Default)]
pub struct RuntimeRegistry {
    inner: RwLock<HashMap<String, Arc<dyn ContainerRuntime>>>,
}

impl std::fmt::Debug for RuntimeRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RuntimeRegistry")
    }
}

impl RuntimeRegistry {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn set(&self, node_id: impl Into<String>, runtime: Arc<dyn ContainerRuntime>) {
        self.inner.write().unwrap().insert(node_id.into(), runtime);
    }
    pub fn get(&self, node_id: &str) -> Result<Arc<dyn ContainerRuntime>> {
        self.inner
            .read()
            .unwrap()
            .get(node_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("node runtime is not available"))
    }
    pub fn remove(&self, node_id: &str) {
        self.inner.write().unwrap().remove(node_id);
    }
    pub fn node_ids(&self) -> Vec<String> {
        self.inner.read().unwrap().keys().cloned().collect()
    }
}

/// Resolve a server volume directory for a local node.
pub fn local_volume(volumes_dir: &std::path::Path, server_uuid: &str) -> PathBuf {
    volumes_dir.join(server_uuid)
}

/// Validate the environment variable names coming from templates.
pub fn validate_env(env: &BTreeMap<String, String>) -> Result<()> {
    if env.len() > 128 {
        bail!("too many environment variables");
    }
    for (key, value) in env {
        if key.is_empty() || key.len() > 128 {
            bail!("invalid environment variable name");
        }
        if !key
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        {
            bail!("environment variable '{key}' must be uppercase letters, digits or underscore");
        }
        if value.len() > 8192 {
            bail!("environment variable '{key}' is too long");
        }
        if value.contains('\0') {
            bail!("environment variable '{key}' contains a NUL byte");
        }
    }
    Ok(())
}

pub fn megabytes(n: i64) -> i64 {
    n.saturating_mul(1024 * 1024)
}

/// Convert CPU percent (100 = one core) to Docker nano CPUs.
pub fn nano_cpus(percent: i64) -> i64 {
    percent.clamp(1, 100 * 256).saturating_mul(10_000_000)
}
