//! Panel ⇄ node transport.
//!
//! Requests are authenticated with a bearer token *and* an HMAC signature over
//! method, path, timestamp and body digest. Timestamps outside a five minute
//! window are rejected, which prevents replay of captured requests.

use crate::crypto::{constant_time_eq, hmac_sha256_hex, sha256_hex};
use crate::models::Node;
use anyhow::{anyhow, bail, Context, Result};
use reqwest::{Method, StatusCode};
use serde_json::Value;
use std::time::Duration;

pub const TS_HEADER: &str = "x-porgous-timestamp";
pub const SIG_HEADER: &str = "x-porgous-signature";
pub const TOKEN_HEADER: &str = "x-porgous-token";
pub const MAX_SKEW_SECS: i64 = 300;

/// Canonical string that is signed for every node request.
pub fn signing_payload(method: &str, path_and_query: &str, timestamp: i64, body: &[u8]) -> String {
    format!(
        "{}\n{}\n{}\n{}",
        method.to_uppercase(),
        path_and_query,
        timestamp,
        sha256_hex(body)
    )
}

pub fn sign(secret: &[u8], method: &str, path_and_query: &str, timestamp: i64, body: &[u8]) -> String {
    hmac_sha256_hex(
        secret,
        signing_payload(method, path_and_query, timestamp, body).as_bytes(),
    )
}

/// Constant-time verification used by the agent. Returns a human readable
/// error suitable for logs (never echoed to untrusted clients).
pub fn verify_signature(
    secret: &[u8],
    method: &str,
    path_and_query: &str,
    timestamp: i64,
    signature: &str,
    body: &[u8],
    now: i64,
) -> Result<()> {
    if (now - timestamp).abs() > MAX_SKEW_SECS {
        bail!("request timestamp outside the allowed window");
    }
    let expected = sign(secret, method, path_and_query, timestamp, body);
    if !constant_time_eq(expected.as_bytes(), signature.trim().as_bytes()) {
        bail!("invalid request signature");
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct NodeClient {
    pub node_id: String,
    pub base_url: String,
    token: String,
    secret: Vec<u8>,
    client: reqwest::Client,
}

impl NodeClient {
    pub fn new(node: &Node, token: String, secret: Vec<u8>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .connect_timeout(Duration::from_secs(10))
            .user_agent(concat!("porgous-panel/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            node_id: node.id.clone(),
            base_url: node.base_url(),
            token,
            secret,
            client,
        }
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.client
    }

    pub async fn send(&self, method: Method, path_and_query: &str, body: Option<&Value>) -> Result<Value> {
        self.send_with_timeout(method, path_and_query, body, Duration::from_secs(60))
            .await
    }

    pub async fn send_with_timeout(
        &self,
        method: Method,
        path_and_query: &str,
        body: Option<&Value>,
        timeout: Duration,
    ) -> Result<Value> {
        let body_bytes = body
            .map(|b| serde_json::to_vec(b))
            .transpose()?
            .unwrap_or_default();
        let response = self
            .request_inner(
                method,
                path_and_query,
                body_bytes,
                "application/json",
                Some(timeout),
            )
            .await?;
        let status = response.status();
        let bytes = response.bytes().await.unwrap_or_default();
        if !status.is_success() {
            let text = String::from_utf8_lossy(&bytes);
            return Err(anyhow!(
                "node request failed ({status}): {}",
                truncate(&text, 300)
            ));
        }
        if bytes.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&bytes).context("invalid JSON from node")
    }

    pub async fn request(
        &self,
        method: Method,
        path_and_query: &str,
        body: Vec<u8>,
        content_type: &str,
    ) -> Result<reqwest::Response> {
        self.request_inner(method, path_and_query, body, content_type, None)
            .await
    }

    async fn request_inner(
        &self,
        method: Method,
        path_and_query: &str,
        body: Vec<u8>,
        content_type: &str,
        timeout: Option<Duration>,
    ) -> Result<reqwest::Response> {
        let timestamp = chrono::Utc::now().timestamp();
        let signature = sign(&self.secret, method.as_str(), path_and_query, timestamp, &body);
        let url = format!("{}{}", self.base_url, path_and_query);
        let mut builder = self
            .client
            .request(method, &url)
            .header(TOKEN_HEADER, &self.token)
            .header(TS_HEADER, timestamp.to_string())
            .header(SIG_HEADER, signature)
            .header(reqwest::header::CONTENT_TYPE, content_type)
            .body(body);
        if let Some(timeout) = timeout {
            builder = builder.timeout(timeout);
        }
        let response = builder
            .send()
            .await
            .with_context(|| format!("contacting node {}", self.base_url))?;
        Ok(response)
    }

    /// Open a streaming GET (used for file downloads).
    pub async fn get_stream(&self, path_and_query: &str) -> Result<reqwest::Response> {
        let response = self
            .request(Method::GET, path_and_query, Vec::new(), "application/json")
            .await?;
        if response.status() == StatusCode::NOT_FOUND {
            bail!("file not found");
        }
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            bail!("node returned {status}: {}", truncate(&text, 300));
        }
        Ok(response)
    }

    pub async fn health(&self) -> Result<Value> {
        let response = self
            .client
            .get(format!("{}/v1/health", self.base_url))
            .send()
            .await?;
        if !response.status().is_success() {
            bail!("node health check failed with {}", response.status());
        }
        Ok(response.json().await?)
    }
}

fn truncate(text: &str, max: usize) -> String {
    let mut out: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        out.push('…');
    }
    out
}

/// Generate a fresh node token and secret. The token is shown once to the
/// administrator; the panel stores only its hash.
pub fn generate_node_credentials() -> (String, String) {
    (crate::crypto::random_token(32), crate::crypto::random_token(32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_roundtrip_and_replay_window() {
        let secret = b"secret";
        let ts = 1_700_000_000;
        let sig = sign(secret, "POST", "/v1/containers?x=1", ts, b"{}");
        assert!(verify_signature(secret, "POST", "/v1/containers?x=1", ts, &sig, b"{}", ts).is_ok());
        assert!(verify_signature(
            secret,
            "POST",
            "/v1/containers?x=1",
            ts,
            &sig,
            b"{}",
            ts + MAX_SKEW_SECS + 1
        )
        .is_err());
        assert!(verify_signature(secret, "POST", "/v1/containers?x=2", ts, &sig, b"{}", ts).is_err());
        assert!(verify_signature(secret, "POST", "/v1/containers?x=1", ts, &sig, b"{\"a\":1}", ts).is_err());
    }

    #[test]
    fn different_secret_rejected() {
        let ts = 1_700_000_000;
        let sig = sign(b"one", "GET", "/v1/health", ts, b"");
        assert!(verify_signature(b"two", "GET", "/v1/health", ts, &sig, b"", ts).is_err());
    }
}
