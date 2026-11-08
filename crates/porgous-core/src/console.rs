//! Live console fan-out and per-server event channels.

use crate::models::{Server, STATUS_RUNNING};
use crate::runtime::RuntimeRegistry;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::broadcast;

const CHANNEL_CAPACITY: usize = 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ConsoleEvent {
    Output(String),
    Status(String),
    Stats(serde_json::Value),
    Notice(String),
}

impl ConsoleEvent {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self)
            .unwrap_or_else(|_| r#"{"type":"notice","data":"serialization error"}"#.into())
    }
}

/// Fans server output and metric events out to WebSocket clients.
pub struct ConsoleHub {
    channels: Mutex<HashMap<String, broadcast::Sender<String>>>,
    attached: Mutex<HashSet<String>>,
    inputs: Mutex<HashMap<String, Pin<Box<dyn AsyncWrite + Send>>>>,
}

impl std::fmt::Debug for ConsoleHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ConsoleHub")
    }
}

impl Default for ConsoleHub {
    fn default() -> Self {
        Self::new()
    }
}

impl ConsoleHub {
    pub fn new() -> Self {
        Self {
            channels: Mutex::new(HashMap::new()),
            attached: Mutex::new(HashSet::new()),
            inputs: Mutex::new(HashMap::new()),
        }
    }

    pub fn sender(&self, server_id: &str) -> broadcast::Sender<String> {
        let mut channels = self.channels.lock().unwrap();
        channels
            .entry(server_id.to_string())
            .or_insert_with(|| broadcast::channel(CHANNEL_CAPACITY).0)
            .clone()
    }

    pub fn subscribe(&self, server_id: &str) -> broadcast::Receiver<String> {
        self.sender(server_id).subscribe()
    }

    pub fn publish(&self, server_id: &str, event: ConsoleEvent) {
        let sender = self.sender(server_id);
        // A send failure only means nobody is listening.
        let _ = sender.send(event.to_json());
    }

    pub fn publish_stats(&self, server_id: &str, cpu: f64, memory: i64, disk: i64, net_rx: i64, net_tx: i64) {
        self.publish(
            server_id,
            ConsoleEvent::Stats(json!({
                "cpu": cpu,
                "memory_bytes": memory,
                "disk_bytes": disk,
                "net_rx_bytes": net_rx,
                "net_tx_bytes": net_tx,
                "timestamp": crate::db::now_ms(),
            })),
        );
    }

    pub fn is_attached(&self, server_id: &str) -> bool {
        self.attached.lock().unwrap().contains(server_id)
    }

    /// Start streaming a running server's output into its channel.
    pub fn ensure_attached(self: &Arc<Self>, runtimes: Arc<RuntimeRegistry>, server: Server) {
        if !matches!(server.status.as_str(), STATUS_RUNNING) {
            return;
        }
        let Some(container) = server.container_id.clone() else {
            return;
        };
        {
            let mut attached = self.attached.lock().unwrap();
            if !attached.insert(server.id.clone()) {
                return;
            }
        }
        let hub = self.clone();
        tokio::spawn(async move {
            let result = async {
                let runtime = runtimes.get(&server.node_id)?;
                if runtime.is_remote() {
                    hub.remote_poll(runtime, &server, &container).await;
                    return Ok::<(), anyhow::Error>(());
                }
                let stream = runtime.attach(&container).await?;
                let mut output = stream.output;
                hub.inputs.lock().unwrap().insert(server.id.clone(), stream.input);
                while let Some(chunk) = output.next().await {
                    match chunk {
                        Ok(bytes) => {
                            let text = String::from_utf8_lossy(&bytes).to_string();
                            for line in text.split_inclusive('\n') {
                                let clean = crate::util::strip_ansi(line.trim_end_matches(['\n', '\r']));
                                if !clean.is_empty() {
                                    hub.publish(&server.id, ConsoleEvent::Output(clean));
                                }
                            }
                        }
                        Err(err) => {
                            hub.publish(
                                &server.id,
                                ConsoleEvent::Notice(format!("console stream error: {err}")),
                            );
                            break;
                        }
                    }
                }
                Ok::<(), anyhow::Error>(())
            }
            .await;
            if let Err(err) = result {
                hub.publish(
                    &server.id,
                    ConsoleEvent::Notice(format!("console unavailable: {err}")),
                );
            }
            hub.inputs.lock().unwrap().remove(&server.id);
            hub.attached.lock().unwrap().remove(&server.id);
        });
    }

    async fn remote_poll(
        &self,
        runtime: Arc<dyn crate::runtime::ContainerRuntime>,
        server: &Server,
        container: &str,
    ) {
        let mut since = chrono::Utc::now().timestamp() - 1;
        let mut recent: std::collections::VecDeque<String> = std::collections::VecDeque::new();
        let mut idle_ticks = 0u32;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            match runtime.logs_since(container, since, 200).await {
                Ok(lines) => {
                    idle_ticks = 0;
                    since = chrono::Utc::now().timestamp() - 1;
                    for line in lines {
                        let clean = crate::util::strip_ansi(&line);
                        if clean.is_empty() || recent.contains(&clean) {
                            continue;
                        }
                        recent.push_back(clean.clone());
                        if recent.len() > 64 {
                            recent.pop_front();
                        }
                        self.publish(&server.id, ConsoleEvent::Output(clean));
                    }
                }
                Err(err) => {
                    idle_ticks += 1;
                    if idle_ticks == 5 {
                        self.publish(
                            &server.id,
                            ConsoleEvent::Notice(format!("console polling error: {err}")),
                        );
                    }
                }
            }
            if idle_ticks >= 120 {
                break;
            }
            match runtime.inspect(container).await {
                Ok(stats) => {
                    if !stats.running {
                        break;
                    }
                }
                Err(_) => {
                    idle_ticks += 3;
                }
            }
        }
    }

    /// Send a console command to the server's main process.
    pub async fn send_command(
        &self,
        runtimes: &RuntimeRegistry,
        server: &Server,
        container: &str,
        command: &str,
    ) -> Result<(), String> {
        if command.trim().is_empty() {
            return Err("command is empty".into());
        }
        if command.len() > 4096 {
            return Err("command is too long".into());
        }
        if command.contains('\0') {
            return Err("command contains invalid characters".into());
        }
        let runtime = runtimes.get(&server.node_id).map_err(|e| e.to_string())?;
        if runtime.is_remote() {
            return runtime
                .command(container, command)
                .await
                .map_err(|e| e.to_string());
        }
        let writer = { self.inputs.lock().unwrap().remove(&server.id) };
        let Some(mut writer) = writer else {
            return Err("console is not attached yet; try again in a moment".into());
        };
        let result = async {
            writer
                .write_all(command.as_bytes())
                .await
                .map_err(|e| e.to_string())?;
            writer.write_all(b"\n").await.map_err(|e| e.to_string())?;
            writer.flush().await.map_err(|e| e.to_string())?;
            Ok::<(), String>(())
        }
        .await;
        self.inputs.lock().unwrap().insert(server.id.clone(), writer);
        result
    }

    pub fn detach(&self, server_id: &str) {
        self.inputs.lock().unwrap().remove(server_id);
        self.attached.lock().unwrap().remove(server_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_serialize() {
        let event = ConsoleEvent::Output("hello".into());
        assert_eq!(event.to_json(), r#"{"type":"output","data":"hello"}"#);
        let stats = ConsoleEvent::Stats(json!({"cpu": 1.5}));
        assert!(stats.to_json().contains("\"stats\""));
    }

    #[test]
    fn publish_and_subscribe() {
        let hub = ConsoleHub::new();
        let mut rx = hub.subscribe("server-1");
        hub.publish("server-1", ConsoleEvent::Output("line".into()));
        let msg = rx.try_recv().unwrap();
        assert!(msg.contains("line"));
    }
}
