//! Docker runtime backed by the Docker Engine API (bollard).

use super::{
    megabytes, nano_cpus, AttachStream, ContainerRuntime, ContainerSpec, ExecResult, InstallSpec,
    CONTAINER_PREFIX, VOLUME_MOUNT,
};
use crate::models::RuntimeStats;
use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use bollard::container::{
    AttachContainerOptions, Config, CreateContainerOptions, InspectContainerOptions, KillContainerOptions,
    LogsOptions, RemoveContainerOptions, StartContainerOptions, StatsOptions, StopContainerOptions,
    WaitContainerOptions,
};
use bollard::exec::{CreateExecOptions, StartExecOptions, StartExecResults};
use bollard::image::CreateImageOptions;
use bollard::models::{HostConfig, HostConfigLogConfig, PortBinding};
use bollard::network::CreateNetworkOptions;
use bollard::Docker;
use futures_util::{StreamExt, TryStreamExt};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone)]
pub struct LocalRuntime {
    node_id: String,
    docker: Option<Docker>,
    error: Option<String>,
    volumes_dir: PathBuf,
    network: String,
    timezone: String,
    default_image: String,
    container_user: Option<String>,
}

impl std::fmt::Debug for LocalRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalRuntime")
            .field("node_id", &self.node_id)
            .field("docker", &self.docker.is_some())
            .finish()
    }
}

impl LocalRuntime {
    pub async fn connect(
        node_id: impl Into<String>,
        volumes_dir: PathBuf,
        network: String,
        timezone: String,
        default_image: String,
        container_user: Option<String>,
        docker_host: Option<String>,
    ) -> Self {
        let node_id = node_id.into();
        std::fs::create_dir_all(&volumes_dir).ok();
        let connect_result = async {
            let docker = match docker_host.as_deref() {
                Some(host) if host.starts_with("unix://") => {
                    Docker::connect_with_socket(host, 120, bollard::API_DEFAULT_VERSION)
                        .map_err(|e| anyhow!("{e}"))?
                }
                Some(host) if host.starts_with("tcp://") || host.starts_with("http://") => {
                    Docker::connect_with_http(host, 120, bollard::API_DEFAULT_VERSION)
                        .map_err(|e| anyhow!("{e}"))?
                }
                Some(host) => bail!("unsupported docker host '{host}'"),
                None => Docker::connect_with_local_defaults().map_err(|e| anyhow!("{e}"))?,
            };
            docker.ping().await.context("cannot reach the Docker daemon")?;
            Ok::<Docker, anyhow::Error>(docker)
        }
        .await;
        let (docker, error) = match connect_result {
            Ok(docker) => (Some(docker), None),
            Err(err) => (None, Some(err.to_string())),
        };
        if let Some(err) = &error {
            tracing::warn!(node = %node_id, error = %err, "docker runtime unavailable");
        }
        let runtime = Self {
            node_id,
            docker,
            error,
            volumes_dir,
            network,
            timezone,
            default_image,
            container_user,
        };
        if let Some(docker) = &runtime.docker {
            if runtime.network != "bridge" && runtime.network != "host" && runtime.network != "none" {
                let options = CreateNetworkOptions {
                    name: runtime.network.clone(),
                    check_duplicate: true,
                    driver: "bridge".into(),
                    ..Default::default()
                };
                if let Err(err) = docker.create_network(options).await {
                    tracing::debug!(error = %err, "network already exists or could not be created");
                }
            }
        }
        runtime
    }

    fn docker(&self) -> Result<&Docker> {
        self.docker.as_ref().ok_or_else(|| {
            anyhow!(
                "Docker is not available on this node: {}",
                self.error.clone().unwrap_or_else(|| "unknown error".into())
            )
        })
    }

    fn volume_host(&self, volume_id: &str) -> Result<PathBuf> {
        if volume_id.is_empty()
            || !volume_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            bail!("invalid volume id");
        }
        let path = self.volumes_dir.join(volume_id);
        std::fs::create_dir_all(&path)?;
        let canonical = path.canonicalize()?;
        if !canonical.starts_with(
            self.volumes_dir
                .canonicalize()
                .unwrap_or_else(|_| self.volumes_dir.clone()),
        ) {
            bail!("volume escapes the node data directory");
        }
        Ok(canonical)
    }

    fn host_config(
        &self,
        spec_ports: &[super::PortMapping],
        memory_mb: i64,
        swap_mb: i64,
        cpu: i64,
        pids: i64,
        io: i64,
        volume: &str,
    ) -> HostConfig {
        let memory = megabytes(memory_mb.max(128));
        let swap = if swap_mb > 0 {
            memory.saturating_add(megabytes(swap_mb))
        } else {
            memory
        };
        let mut port_bindings: HashMap<String, Option<Vec<PortBinding>>> = HashMap::new();
        for port in spec_ports {
            port_bindings.insert(
                format!("{}/{}", port.container_port, port.protocol),
                Some(vec![PortBinding {
                    host_ip: Some(port.host_ip.clone()),
                    host_port: Some(port.host_port.to_string()),
                }]),
            );
        }
        HostConfig {
            binds: Some(vec![format!("{volume}:{VOLUME_MOUNT}:rw")]),
            memory: Some(memory),
            memory_swap: Some(swap),
            nano_cpus: Some(nano_cpus(cpu)),
            pids_limit: Some(pids.max(64)),
            cpu_shares: Some(io.clamp(2, 1024)),
            port_bindings: Some(port_bindings),
            network_mode: Some(self.network.clone()),
            restart_policy: None,
            auto_remove: Some(false),
            privileged: Some(false),
            readonly_rootfs: Some(false),
            security_opt: Some(vec!["no-new-privileges:true".into()]),
            cap_drop: None,
            shm_size: Some(megabytes(256)),
            tmpfs: Some(HashMap::from([(
                "/tmp".to_string(),
                "rw,noexec,nosuid,size=128m".to_string(),
            )])),
            init: Some(true),
            log_config: Some(HostConfigLogConfig {
                typ: Some("json-file".into()),
                config: Some(HashMap::from([
                    ("max-size".into(), "10m".into()),
                    ("max-file".into(), "3".into()),
                ])),
            }),
            ..Default::default()
        }
    }

    fn container_name(name: &str) -> String {
        let clean: String = name
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        format!("{CONTAINER_PREFIX}{clean}")
    }
}

fn memory_used(stats: &bollard::container::Stats) -> i64 {
    let usage = stats.memory_stats.usage.unwrap_or(0) as i64;
    let cache = match &stats.memory_stats.stats {
        Some(bollard::container::MemoryStatsStats::V1(v1)) => v1.total_inactive_file as i64,
        Some(bollard::container::MemoryStatsStats::V2(v2)) => v2.inactive_file as i64,
        None => 0,
    };
    (usage - cache).max(0)
}

fn cpu_percent(stats: &bollard::container::Stats) -> f64 {
    let cpu = stats.cpu_stats.cpu_usage.total_usage as f64;
    let pre_cpu = stats.precpu_stats.cpu_usage.total_usage as f64;
    let system = stats.cpu_stats.system_cpu_usage.unwrap_or(0) as f64;
    let pre_system = stats.precpu_stats.system_cpu_usage.unwrap_or(0) as f64;
    let cpu_delta = cpu - pre_cpu;
    let system_delta = system - pre_system;
    if cpu_delta > 0.0 && system_delta > 0.0 {
        let cpus = stats.cpu_stats.online_cpus.unwrap_or(1).max(1) as f64;
        ((cpu_delta / system_delta) * cpus * 100.0 * 100.0).round() / 100.0
    } else {
        0.0
    }
}

#[async_trait]
impl ContainerRuntime for LocalRuntime {
    fn node_id(&self) -> &str {
        &self.node_id
    }
    fn is_remote(&self) -> bool {
        false
    }
    fn description(&self) -> String {
        match &self.docker {
            Some(_) => "local Docker daemon".into(),
            None => format!("Docker unavailable: {}", self.error.clone().unwrap_or_default()),
        }
    }

    async fn ping(&self) -> Result<()> {
        self.docker()?.ping().await.context("docker ping failed")?;
        Ok(())
    }

    async fn ensure_image(&self, image: &str) -> Result<()> {
        crate::eggs::validate_image_reference(image)?;
        let docker = self.docker()?;
        if docker.inspect_image(image).await.is_ok() {
            return Ok(());
        }
        tracing::info!(image, "pulling docker image");
        let mut stream = docker.create_image(
            Some(CreateImageOptions::<String> {
                from_image: image.to_string(),
                ..Default::default()
            }),
            None,
            None,
        );
        while let Some(item) = stream.next().await {
            let info = item.map_err(|e| anyhow!("image pull failed: {e}"))?;
            if let Some(status) = info.status {
                tracing::debug!(image, status, "image pull");
            }
        }
        Ok(())
    }

    async fn create(&self, spec: &ContainerSpec) -> Result<String> {
        super::validate_env(&spec.env)?;
        crate::eggs::validate_image_reference(&spec.image)?;
        let volume = self.volume_host(&spec.volume_id)?;
        let volume = volume.to_string_lossy().to_string();
        let docker = self.docker()?;
        let env: Vec<String> = spec.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let host_config = self.host_config(
            &spec.ports,
            spec.memory_mb,
            spec.swap_mb,
            spec.cpu_percent,
            spec.pids_limit,
            spec.io_weight,
            &volume,
        );
        let cmd = spec
            .cmd
            .as_ref()
            .map(|c| vec!["/bin/sh".to_string(), "-c".to_string(), c.clone()]);
        let labels: HashMap<String, String> =
            spec.labels.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        let config = Config {
            image: Some(spec.image.clone()),
            env: Some(env),
            cmd,
            exposed_ports: Some(
                spec.ports
                    .iter()
                    .map(|p| (format!("{}/{}", p.container_port, p.protocol), HashMap::new()))
                    .collect(),
            ),
            entrypoint: spec.entrypoint.clone(),
            working_dir: Some(VOLUME_MOUNT.into()),
            attach_stdin: Some(true),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            open_stdin: Some(true),
            stdin_once: Some(false),
            tty: Some(false),
            user: spec.user.clone().or_else(|| self.container_user.clone()),
            hostname: spec.hostname.clone(),
            labels: Some(labels),
            stop_signal: spec.stop_signal.clone(),
            stop_timeout: Some(30),
            host_config: Some(host_config),
            ..Default::default()
        };
        let options = Some(CreateContainerOptions {
            name: LocalRuntime::container_name(&spec.name),
            platform: None,
        });
        let created = docker
            .create_container(options, config)
            .await
            .with_context(|| format!("creating container for image {}", spec.image))?;
        if !created.warnings.is_empty() {
            tracing::warn!(warnings = ?created.warnings, "docker container warnings");
        }
        Ok(created.id)
    }

    async fn start(&self, container: &str) -> Result<()> {
        self.docker()?
            .start_container(container, None::<StartContainerOptions<String>>)
            .await
            .context("starting container")?;
        Ok(())
    }

    async fn stop(&self, container: &str, timeout_secs: u64) -> Result<()> {
        self.docker()?
            .stop_container(
                container,
                Some(StopContainerOptions {
                    t: timeout_secs.min(600) as i64,
                }),
            )
            .await
            .context("stopping container")?;
        Ok(())
    }

    async fn kill(&self, container: &str) -> Result<()> {
        self.docker()?
            .kill_container(container, None::<KillContainerOptions<String>>)
            .await
            .context("killing container")?;
        Ok(())
    }

    async fn remove(&self, container: &str) -> Result<()> {
        self.docker()?
            .remove_container(
                container,
                Some(RemoveContainerOptions {
                    force: true,
                    v: true,
                    link: false,
                }),
            )
            .await
            .context("removing container")?;
        Ok(())
    }

    async fn inspect(&self, container: &str) -> Result<RuntimeStats> {
        let info = self
            .docker()?
            .inspect_container(container, None::<InspectContainerOptions>)
            .await
            .context("inspecting container")?;
        let state = info.state.unwrap_or_default();
        let started_at = state
            .started_at
            .as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.timestamp());
        let uptime = started_at
            .map(|s| (chrono::Utc::now().timestamp() - s).max(0))
            .unwrap_or(0);
        Ok(RuntimeStats {
            running: state.running.unwrap_or(false),
            exit_code: state.exit_code,
            started_at,
            uptime_secs: uptime,
            ..Default::default()
        })
    }

    async fn stats(&self, container: &str) -> Result<RuntimeStats> {
        let docker = self.docker()?;
        let mut stream = docker.stats(
            container,
            Some(StatsOptions {
                stream: false,
                one_shot: true,
            }),
        );
        let stats = stream
            .next()
            .await
            .ok_or_else(|| anyhow!("no stats returned"))?
            .context("reading container stats")?;
        let net: (i64, i64) = stats
            .networks
            .as_ref()
            .map(|nets| {
                nets.values().fold((0i64, 0i64), |acc, n| {
                    (acc.0 + n.rx_bytes as i64, acc.1 + n.tx_bytes as i64)
                })
            })
            .or_else(|| stats.network.map(|n| (n.rx_bytes as i64, n.tx_bytes as i64)))
            .unwrap_or((0, 0));
        let inspect = self.inspect(container).await.unwrap_or_default();
        Ok(RuntimeStats {
            running: inspect.running,
            cpu_percent: cpu_percent(&stats),
            memory_bytes: memory_used(&stats),
            memory_limit_bytes: stats.memory_stats.limit.unwrap_or(0) as i64,
            net_rx_bytes: net.0,
            net_tx_bytes: net.1,
            uptime_secs: inspect.uptime_secs,
            exit_code: inspect.exit_code,
            started_at: inspect.started_at,
            ..Default::default()
        })
    }

    async fn logs(&self, container: &str, tail: u32) -> Result<String> {
        let docker = self.docker()?;
        let options = Some(LogsOptions::<String> {
            stdout: true,
            stderr: true,
            tail: tail.to_string(),
            timestamps: false,
            ..Default::default()
        });
        let stream = docker.logs(container, options);
        let collected: Vec<_> = stream.try_collect().await.context("reading container logs")?;
        Ok(collected
            .into_iter()
            .map(|chunk| String::from_utf8_lossy(chunk.as_ref()).to_string())
            .collect::<Vec<_>>()
            .join(""))
    }

    async fn logs_since(&self, container: &str, since_unix: i64, tail: u32) -> Result<Vec<String>> {
        let docker = self.docker()?;
        let options = Some(LogsOptions::<String> {
            stdout: true,
            stderr: true,
            since: since_unix.max(0),
            tail: tail.to_string(),
            timestamps: false,
            ..Default::default()
        });
        let stream = docker.logs(container, options);
        let collected: Vec<_> = stream.try_collect().await.context("reading container logs")?;
        let text = collected
            .into_iter()
            .map(|chunk| String::from_utf8_lossy(chunk.as_ref()).to_string())
            .collect::<String>();
        Ok(text.lines().map(|l| l.to_string()).collect())
    }

    async fn command(&self, container: &str, command: &str) -> Result<()> {
        let docker = self.docker()?;
        let options = Some(AttachContainerOptions::<String> {
            stdin: Some(true),
            stdout: Some(false),
            stderr: Some(false),
            stream: Some(true),
            logs: Some(false),
            detach_keys: None,
        });
        let mut attached = docker
            .attach_container(container, options)
            .await
            .context("attaching to container")?;
        use tokio::io::AsyncWriteExt;
        attached.input.write_all(command.as_bytes()).await?;
        attached.input.write_all(b"\n").await?;
        attached.input.flush().await?;
        tokio::time::sleep(Duration::from_millis(50)).await;
        Ok(())
    }

    async fn exec(&self, container: &str, cmd: Vec<String>) -> Result<ExecResult> {
        let docker = self.docker()?;
        let exec = docker
            .create_exec(
                container,
                CreateExecOptions {
                    cmd: Some(cmd),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await
            .context("creating exec")?;
        let mut output = String::new();
        match docker
            .start_exec(
                &exec.id,
                Some(StartExecOptions {
                    detach: false,
                    ..Default::default()
                }),
            )
            .await
            .context("starting exec")?
        {
            StartExecResults::Attached {
                output: mut stream, ..
            } => {
                while let Some(chunk) = stream.next().await {
                    match chunk {
                        Ok(chunk) => output.push_str(&String::from_utf8_lossy(chunk.as_ref())),
                        Err(err) => {
                            output.push_str(&format!("\n[exec error: {err}]"));
                            break;
                        }
                    }
                }
            }
            StartExecResults::Detached => bail!("exec unexpectedly detached"),
        }
        let inspect = docker.inspect_exec(&exec.id).await.context("inspecting exec")?;
        Ok(ExecResult {
            exit_code: inspect.exit_code.unwrap_or(-1),
            output,
        })
    }

    async fn run_install(&self, spec: &InstallSpec) -> Result<String> {
        if spec.script.len() > 64 * 1024 {
            bail!("install script is too large");
        }
        crate::eggs::validate_image_reference(&spec.image)?;
        let docker = self.docker()?;
        self.ensure_image(&spec.image).await?;
        let volume = self.volume_host(&spec.volume_id)?.to_string_lossy().to_string();
        let env: Vec<String> = spec.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let labels: HashMap<String, String> =
            spec.labels.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        let name = LocalRuntime::container_name(&format!("install-{}", spec.volume_id));
        let options = Some(CreateContainerOptions {
            name: name.clone(),
            platform: None,
        });
        let config = Config {
            image: Some(spec.image.clone()),
            env: Some(env),
            entrypoint: spec.entrypoint.clone(),
            cmd: Some(vec!["/bin/sh".into(), "-c".into(), spec.script.clone()]),
            working_dir: Some("/mnt/server".into()),
            labels: Some(labels),
            host_config: Some(HostConfig {
                binds: Some(vec![format!("{volume}:/mnt/server:rw")]),
                memory: Some(megabytes(spec.memory_mb.max(512))),
                network_mode: Some(spec.network.clone()),
                security_opt: Some(vec!["no-new-privileges:true".into()]),
                auto_remove: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        };
        let _ = docker
            .remove_container(
                &name,
                Some(RemoveContainerOptions {
                    force: true,
                    v: false,
                    link: false,
                }),
            )
            .await;
        let created = docker
            .create_container(options, config)
            .await
            .context("creating install container")?;
        docker
            .start_container(&created.id, None::<StartContainerOptions<String>>)
            .await
            .context("starting install container")?;
        let wait = docker.wait_container(&created.id, None::<WaitContainerOptions<String>>);
        let status = tokio::time::timeout(Duration::from_secs(30 * 60), wait.into_future())
            .await
            .map_err(|_| anyhow!("install script timed out after 30 minutes"))?
            .0
            .transpose()?
            .ok_or_else(|| anyhow!("install container exited without a status"))?;
        let logs = self.logs(&created.id, 400).await.unwrap_or_default();
        let _ = docker
            .remove_container(
                &created.id,
                Some(RemoveContainerOptions {
                    force: true,
                    v: false,
                    link: false,
                }),
            )
            .await;
        if status.status_code != 0 {
            bail!(
                "install script failed with exit code {}: {}",
                status.status_code,
                tail(&logs, 40)
            );
        }
        Ok(tail(&logs, 200))
    }

    async fn attach(&self, container: &str) -> Result<AttachStream> {
        let docker = self.docker()?;
        let options = Some(AttachContainerOptions::<String> {
            stdin: Some(true),
            stdout: Some(true),
            stderr: Some(true),
            stream: Some(true),
            logs: Some(false),
            detach_keys: None,
        });
        let attached = docker
            .attach_container(container, options)
            .await
            .context("attaching to container console")?;
        let output = attached.output.map(|item| {
            item.map(|log| log.into_bytes().to_vec())
                .map_err(|e| e.to_string())
        });
        Ok(AttachStream {
            output: Box::pin(output),
            input: attached.input,
        })
    }
}

fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// Default image used when an egg does not specify one.
impl LocalRuntime {
    pub fn default_image(&self) -> &str {
        &self.default_image
    }
    pub fn timezone(&self) -> &str {
        &self.timezone
    }
}

/// Convert a map to the env vector Docker expects.
pub fn env_vec(env: &BTreeMap<String, String>) -> Vec<String> {
    env.iter().map(|(k, v)| format!("{k}={v}")).collect()
}
