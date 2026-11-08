//! File manager backend that transparently uses the local filesystem or a
//! remote node's agent.

use crate::error::{ApiError, ApiResult};
use crate::files::{FileEntry, LocalFiles};
use crate::models::{Node, Server};
use crate::nodes::NodeClient;
use crate::state::AppState;
use crate::util::urlencode;
use anyhow::Result;
use reqwest::Method;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

pub enum Download {
    Local(tokio::fs::File, u64),
    Remote(reqwest::Response),
}

enum Backend {
    Local(LocalFiles),
    Remote { client: Arc<NodeClient>, volume: String },
}

pub struct FileService {
    backend: Backend,
}

impl FileService {
    pub async fn for_server(state: &AppState, server: &Server) -> ApiResult<Self> {
        let node = crate::state::load_node(&state.db, &server.node_id)
            .await?
            .ok_or_else(|| ApiError::not_found("Node not found"))?;
        Self::for_node(state, &node, server)
    }

    pub fn for_node(state: &AppState, node: &Node, server: &Server) -> ApiResult<Self> {
        if node.is_local {
            let files = LocalFiles::new(state.config.volumes_dir().join(&server.uuid))
                .map_err(|e| ApiError::internal(e))?;
            Ok(Self {
                backend: Backend::Local(files),
            })
        } else {
            let client = state.node_client(node)?;
            Ok(Self {
                backend: Backend::Remote {
                    client: Arc::new(client),
                    volume: server.uuid.clone(),
                },
            })
        }
    }

    pub fn local_root(&self) -> Option<PathBuf> {
        match &self.backend {
            Backend::Local(files) => Some(files.root().to_path_buf()),
            _ => None,
        }
    }

    pub async fn list(&self, path: &str) -> ApiResult<Vec<FileEntry>> {
        match &self.backend {
            Backend::Local(files) => {
                let files = files.clone();
                let path = path.to_string();
                tokio::task::spawn_blocking(move || files.list(&path))
                    .await
                    .map_err(ApiError::internal)?
                    .map_err(|e| ApiError::bad_request(e.to_string()))
            }
            Backend::Remote { client, volume } => {
                let value = client
                    .send(
                        Method::GET,
                        &format!("/v1/files?volume={}&path={}", urlencode(volume), urlencode(path)),
                        None,
                    )
                    .await
                    .map_err(ApiError::internal)?;
                serde_json::from_value(value["entries"].clone()).map_err(ApiError::internal)
            }
        }
    }

    pub async fn read(&self, path: &str, max_bytes: u64) -> ApiResult<Vec<u8>> {
        match &self.backend {
            Backend::Local(files) => {
                let files = files.clone();
                let path = path.to_string();
                tokio::task::spawn_blocking(move || files.read(&path, max_bytes))
                    .await
                    .map_err(ApiError::internal)?
                    .map_err(|e| ApiError::bad_request(e.to_string()))
            }
            Backend::Remote { client, volume } => {
                let response = client
                    .request(
                        Method::GET,
                        &format!(
                            "/v1/files/download?volume={}&path={}",
                            urlencode(volume),
                            urlencode(path)
                        ),
                        Vec::new(),
                        "application/json",
                    )
                    .await
                    .map_err(ApiError::internal)?;
                if !response.status().is_success() {
                    return Err(ApiError::not_found("File not found on node"));
                }
                let bytes = response.bytes().await.map_err(ApiError::internal)?;
                if bytes.len() as u64 > max_bytes {
                    return Err(ApiError::bad_request("File is too large to open in the editor"));
                }
                Ok(bytes.to_vec())
            }
        }
    }

    pub async fn write(&self, path: &str, bytes: Vec<u8>) -> ApiResult<()> {
        match &self.backend {
            Backend::Local(files) => {
                let files = files.clone();
                let path = path.to_string();
                tokio::task::spawn_blocking(move || files.write(&path, &bytes))
                    .await
                    .map_err(ApiError::internal)?
                    .map_err(|e| ApiError::bad_request(e.to_string()))
            }
            Backend::Remote { client, volume } => {
                let response = client
                    .request(
                        Method::PUT,
                        &format!(
                            "/v1/files/write?volume={}&path={}",
                            urlencode(volume),
                            urlencode(path)
                        ),
                        bytes,
                        "application/octet-stream",
                    )
                    .await
                    .map_err(ApiError::internal)?;
                check(response).await
            }
        }
    }

    pub async fn mkdir(&self, path: &str) -> ApiResult<()> {
        match &self.backend {
            Backend::Local(files) => {
                let files = files.clone();
                let path = path.to_string();
                tokio::task::spawn_blocking(move || files.mkdir(&path))
                    .await
                    .map_err(ApiError::internal)?
                    .map_err(|e| ApiError::bad_request(e.to_string()))
            }
            Backend::Remote { client, volume } => {
                let value = client
                    .send(
                        Method::POST,
                        "/v1/files/mkdir",
                        Some(&json!({ "volume": volume, "path": path })),
                    )
                    .await
                    .map_err(ApiError::internal)?;
                let _ = value;
                Ok(())
            }
        }
    }

    pub async fn delete(&self, path: &str) -> ApiResult<()> {
        match &self.backend {
            Backend::Local(files) => {
                let files = files.clone();
                let path = path.to_string();
                tokio::task::spawn_blocking(move || files.delete(&path))
                    .await
                    .map_err(ApiError::internal)?
                    .map_err(|e| ApiError::bad_request(e.to_string()))
            }
            Backend::Remote { client, volume } => {
                let value = client
                    .send(
                        Method::POST,
                        "/v1/files/delete",
                        Some(&json!({ "volume": volume, "path": path })),
                    )
                    .await
                    .map_err(ApiError::internal)?;
                let _ = value;
                Ok(())
            }
        }
    }

    pub async fn rename(&self, from: &str, to: &str) -> ApiResult<()> {
        match &self.backend {
            Backend::Local(files) => {
                let files = files.clone();
                let from = from.to_string();
                let to = to.to_string();
                tokio::task::spawn_blocking(move || files.rename(&from, &to))
                    .await
                    .map_err(ApiError::internal)?
                    .map_err(|e| ApiError::bad_request(e.to_string()))
            }
            Backend::Remote { client, volume } => {
                client
                    .send(
                        Method::POST,
                        "/v1/files/rename",
                        Some(&json!({ "volume": volume, "from": from, "to": to })),
                    )
                    .await
                    .map_err(ApiError::internal)?;
                Ok(())
            }
        }
    }

    pub async fn chmod(&self, path: &str, mode: u32) -> ApiResult<()> {
        match &self.backend {
            Backend::Local(files) => {
                let files = files.clone();
                let path = path.to_string();
                tokio::task::spawn_blocking(move || files.chmod(&path, mode))
                    .await
                    .map_err(ApiError::internal)?
                    .map_err(|e| ApiError::bad_request(e.to_string()))
            }
            Backend::Remote { client, volume } => {
                client
                    .send(
                        Method::POST,
                        "/v1/files/chmod",
                        Some(&json!({ "volume": volume, "path": path, "mode": mode })),
                    )
                    .await
                    .map_err(ApiError::internal)?;
                Ok(())
            }
        }
    }

    pub async fn usage(&self) -> ApiResult<u64> {
        match &self.backend {
            Backend::Local(files) => {
                let files = files.clone();
                Ok(tokio::task::spawn_blocking(move || files.disk_usage())
                    .await
                    .unwrap_or(0))
            }
            Backend::Remote { client, volume } => {
                let value = client
                    .send(
                        Method::GET,
                        &format!("/v1/files/usage?volume={}", urlencode(volume)),
                        None,
                    )
                    .await
                    .map_err(ApiError::internal)?;
                Ok(value["bytes"].as_u64().unwrap_or(0))
            }
        }
    }

    pub async fn download(&self, path: &str) -> ApiResult<Download> {
        match &self.backend {
            Backend::Local(files) => {
                let files = files.clone();
                let path = path.to_string();
                let (file, size) = tokio::task::spawn_blocking(move || {
                    let (file, size) = files.open_for_read(&path)?;
                    Ok::<_, anyhow::Error>((file, size))
                })
                .await
                .map_err(ApiError::internal)?
                .map_err(|e| ApiError::bad_request(e.to_string()))?;
                let std_file = file;
                let tokio_file = tokio::fs::File::from_std(std_file);
                Ok(Download::Local(tokio_file, size))
            }
            Backend::Remote { client, volume } => {
                let response = client
                    .get_stream(&format!(
                        "/v1/files/download?volume={}&path={}",
                        urlencode(volume),
                        urlencode(path)
                    ))
                    .await
                    .map_err(|e| ApiError::bad_request(e.to_string()))?;
                Ok(Download::Remote(response))
            }
        }
    }
}

async fn check(response: reqwest::Response) -> ApiResult<()> {
    if response.status().is_success() {
        Ok(())
    } else {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        let text: String = text.chars().take(300).collect();
        Err(ApiError::new(
            axum::http::StatusCode::BAD_GATEWAY,
            "node_error",
            format!("node rejected file operation ({status}): {text}"),
        ))
    }
}

/// Resolve a file path from a request without allowing traversal.
pub fn safe_path(raw: &str) -> Result<String, ApiError> {
    let trimmed = raw.trim_start_matches('/');
    if trimmed.contains('\0') {
        return Err(ApiError::bad_request("Invalid path"));
    }
    for component in std::path::Path::new(trimmed).components() {
        if !matches!(
            component,
            std::path::Component::Normal(_) | std::path::Component::CurDir
        ) {
            return Err(ApiError::bad_request("Invalid path"));
        }
    }
    Ok(trimmed.to_string())
}
