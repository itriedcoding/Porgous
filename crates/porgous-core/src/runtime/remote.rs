//! Container runtime that executes on a remote node through its agent.

use super::{AttachStream, ContainerRuntime, ContainerSpec, ExecResult, InstallSpec};
use crate::models::{Node, RuntimeStats};
use crate::nodes::NodeClient;
use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use reqwest::Method;
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct RemoteRuntime {
    node_id: String,
    client: NodeClient,
}

impl RemoteRuntime {
    pub fn new(node: Node, token: String, secret: Vec<u8>) -> Self {
        let node_id = node.id.clone();
        Self {
            node_id,
            client: NodeClient::new(&node, token, secret),
        }
    }

    async fn post(&self, path: &str, body: Value) -> Result<Value> {
        self.client.send(Method::POST, path, Some(&body)).await
    }
}

#[async_trait]
impl ContainerRuntime for RemoteRuntime {
    fn node_id(&self) -> &str {
        &self.node_id
    }
    fn is_remote(&self) -> bool {
        true
    }
    fn description(&self) -> String {
        format!("remote agent at {}", self.client.base_url)
    }

    async fn ping(&self) -> Result<()> {
        self.client.health().await?;
        Ok(())
    }

    async fn ensure_image(&self, image: &str) -> Result<()> {
        self.post("/v1/images", json!({ "image": image })).await?;
        Ok(())
    }

    async fn create(&self, spec: &ContainerSpec) -> Result<String> {
        let value = self.post("/v1/containers", serde_json::to_value(spec)?).await?;
        value["id"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow!("node did not return a container id"))
    }

    async fn start(&self, container: &str) -> Result<()> {
        self.post(&format!("/v1/containers/{container}/start"), json!({}))
            .await?;
        Ok(())
    }

    async fn stop(&self, container: &str, timeout_secs: u64) -> Result<()> {
        self.post(
            &format!("/v1/containers/{container}/stop"),
            json!({ "timeout": timeout_secs }),
        )
        .await?;
        Ok(())
    }

    async fn kill(&self, container: &str) -> Result<()> {
        self.post(&format!("/v1/containers/{container}/kill"), json!({}))
            .await?;
        Ok(())
    }

    async fn remove(&self, container: &str) -> Result<()> {
        self.post(&format!("/v1/containers/{container}/remove"), json!({}))
            .await?;
        Ok(())
    }

    async fn inspect(&self, container: &str) -> Result<RuntimeStats> {
        let value = self
            .client
            .send(Method::GET, &format!("/v1/containers/{container}/inspect"), None)
            .await?;
        Ok(serde_json::from_value(value)?)
    }

    async fn stats(&self, container: &str) -> Result<RuntimeStats> {
        let value = self
            .client
            .send(Method::GET, &format!("/v1/containers/{container}/stats"), None)
            .await?;
        Ok(serde_json::from_value(value)?)
    }

    async fn logs(&self, container: &str, tail: u32) -> Result<String> {
        let value = self
            .client
            .send(
                Method::GET,
                &format!("/v1/containers/{container}/logs?tail={tail}"),
                None,
            )
            .await?;
        Ok(value["logs"].as_str().unwrap_or_default().to_string())
    }

    async fn logs_since(&self, container: &str, since_unix: i64, tail: u32) -> Result<Vec<String>> {
        let value = self
            .client
            .send(
                Method::GET,
                &format!("/v1/containers/{container}/logs?since={since_unix}&tail={tail}"),
                None,
            )
            .await?;
        Ok(serde_json::from_value(value["lines"].clone()).unwrap_or_default())
    }

    async fn command(&self, container: &str, command: &str) -> Result<()> {
        self.post(
            &format!("/v1/containers/{container}/command"),
            json!({ "command": command }),
        )
        .await?;
        Ok(())
    }

    async fn exec(&self, container: &str, cmd: Vec<String>) -> Result<ExecResult> {
        let value = self
            .post(&format!("/v1/containers/{container}/exec"), json!({ "cmd": cmd }))
            .await?;
        Ok(serde_json::from_value(value)?)
    }

    async fn run_install(&self, spec: &InstallSpec) -> Result<String> {
        let value = self
            .client
            .send_with_timeout(
                Method::POST,
                "/v1/install",
                Some(&json!(spec)),
                Duration::from_secs(1900),
            )
            .await?;
        Ok(value["log"].as_str().unwrap_or_default().to_string())
    }

    async fn attach(&self, _container: &str) -> Result<AttachStream> {
        bail!("streaming console is only available on the local node; remote consoles use log polling")
    }
}
