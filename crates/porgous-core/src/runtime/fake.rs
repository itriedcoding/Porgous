//! In-memory runtime used by integration tests. It implements the same trait as
//! the real runtimes so API behaviour can be exercised without Docker.

use super::{AttachStream, ContainerRuntime, ContainerSpec, ExecResult, InstallSpec};
use crate::models::RuntimeStats;
use anyhow::{bail, Result};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Debug, Clone, Default)]
struct FakeContainer {
    running: bool,
    exit_code: Option<i64>,
    logs: Vec<String>,
    commands: Vec<String>,
}

#[derive(Debug, Default)]
struct FakeState {
    containers: HashMap<String, FakeContainer>,
    next: u64,
    images: Vec<String>,
    installs: Vec<InstallSpec>,
}

#[derive(Debug, Default)]
pub struct FakeRuntime {
    node_id: String,
    state: Mutex<FakeState>,
}

impl FakeRuntime {
    pub fn new(node_id: impl Into<String>) -> Self {
        Self {
            node_id: node_id.into(),
            state: Mutex::new(FakeState::default()),
        }
    }
    pub fn commands(&self, container: &str) -> Vec<String> {
        self.state
            .lock()
            .unwrap()
            .containers
            .get(container)
            .map(|c| c.commands.clone())
            .unwrap_or_default()
    }
    pub fn running(&self, container: &str) -> bool {
        self.state
            .lock()
            .unwrap()
            .containers
            .get(container)
            .map(|c| c.running)
            .unwrap_or(false)
    }
    pub fn push_log(&self, container: &str, line: &str) {
        if let Some(c) = self.state.lock().unwrap().containers.get_mut(container) {
            c.logs.push(line.to_string());
        }
    }
    pub fn images(&self) -> Vec<String> {
        self.state.lock().unwrap().images.clone()
    }
    pub fn installs(&self) -> Vec<InstallSpec> {
        self.state.lock().unwrap().installs.clone()
    }
}

#[async_trait]
impl ContainerRuntime for FakeRuntime {
    fn node_id(&self) -> &str {
        &self.node_id
    }
    fn is_remote(&self) -> bool {
        false
    }
    fn description(&self) -> String {
        "in-memory test runtime".into()
    }

    async fn ping(&self) -> Result<()> {
        Ok(())
    }

    async fn ensure_image(&self, image: &str) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        if !state.images.iter().any(|i| i == image) {
            state.images.push(image.to_string());
        }
        Ok(())
    }

    async fn create(&self, _spec: &ContainerSpec) -> Result<String> {
        let mut state = self.state.lock().unwrap();
        state.next += 1;
        let id = format!("fake-container-{}", state.next);
        state.containers.insert(
            id.clone(),
            FakeContainer {
                logs: vec!["[fake runtime] container created".into()],
                ..Default::default()
            },
        );
        Ok(id)
    }

    async fn start(&self, container: &str) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        let c = state
            .containers
            .get_mut(container)
            .ok_or_else(|| anyhow::anyhow!("no such container"))?;
        c.running = true;
        c.exit_code = None;
        c.logs.push("[fake runtime] started".into());
        Ok(())
    }

    async fn stop(&self, container: &str, _timeout_secs: u64) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        let c = state
            .containers
            .get_mut(container)
            .ok_or_else(|| anyhow::anyhow!("no such container"))?;
        c.running = false;
        c.exit_code = Some(0);
        c.logs.push("[fake runtime] stopped".into());
        Ok(())
    }

    async fn kill(&self, container: &str) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        let c = state
            .containers
            .get_mut(container)
            .ok_or_else(|| anyhow::anyhow!("no such container"))?;
        c.running = false;
        c.exit_code = Some(137);
        c.logs.push("[fake runtime] killed".into());
        Ok(())
    }

    async fn remove(&self, container: &str) -> Result<()> {
        self.state.lock().unwrap().containers.remove(container);
        Ok(())
    }

    async fn inspect(&self, container: &str) -> Result<RuntimeStats> {
        let state = self.state.lock().unwrap();
        let c = state
            .containers
            .get(container)
            .ok_or_else(|| anyhow::anyhow!("no such container"))?;
        Ok(RuntimeStats {
            running: c.running,
            exit_code: c.exit_code,
            started_at: Some(chrono::Utc::now().timestamp()),
            ..Default::default()
        })
    }

    async fn stats(&self, container: &str) -> Result<RuntimeStats> {
        let mut stats = self.inspect(container).await?;
        stats.cpu_percent = 12.5;
        stats.memory_bytes = 128 * 1024 * 1024;
        stats.memory_limit_bytes = 1024 * 1024 * 1024;
        Ok(stats)
    }

    async fn logs(&self, container: &str, tail: u32) -> Result<String> {
        let state = self.state.lock().unwrap();
        let c = state
            .containers
            .get(container)
            .ok_or_else(|| anyhow::anyhow!("no such container"))?;
        let start = c.logs.len().saturating_sub(tail as usize);
        Ok(c.logs[start..].join("\n"))
    }

    async fn logs_since(&self, container: &str, _since_unix: i64, tail: u32) -> Result<Vec<String>> {
        Ok(self
            .logs(container, tail)
            .await?
            .lines()
            .map(|l| l.to_string())
            .collect())
    }

    async fn command(&self, container: &str, command: &str) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        let c = state
            .containers
            .get_mut(container)
            .ok_or_else(|| anyhow::anyhow!("no such container"))?;
        if !c.running {
            bail!("container is not running");
        }
        c.commands.push(command.to_string());
        c.logs.push(format!("> {command}"));
        Ok(())
    }

    async fn exec(&self, _container: &str, cmd: Vec<String>) -> Result<ExecResult> {
        Ok(ExecResult {
            exit_code: 0,
            output: format!("fake exec: {}", cmd.join(" ")),
        })
    }

    async fn run_install(&self, spec: &InstallSpec) -> Result<String> {
        self.state.lock().unwrap().installs.push(spec.clone());
        Ok("fake install completed".into())
    }

    async fn attach(&self, _container: &str) -> Result<AttachStream> {
        Ok(AttachStream {
            output: Box::pin(futures_util::stream::empty()),
            input: Box::pin(tokio::io::sink()),
        })
    }
}
