use anyhow::{bail, Context, Result};
use porgous_core::agent::{self, AgentState};
use porgous_core::config::Config;
use porgous_core::runtime::LocalRuntime;
use porgous_core::state::node_token_hash;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

fn parse_config_arg(args: &[String]) -> Option<PathBuf> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--config" || arg == "-c" {
            return iter.next().map(PathBuf::from);
        }
        if let Some(value) = arg.strip_prefix("--config=") {
            return Some(PathBuf::from(value));
        }
    }
    None
}

#[tokio::main]
async fn main() -> Result<()> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();

    let args: Vec<String> = std::env::args().collect();
    let command = args.get(1).map(|s| s.as_str()).unwrap_or("serve");
    if matches!(command, "version" | "--version" | "-V") {
        println!("porgous-agent {}", porgous_core::VERSION);
        return Ok(());
    }
    if matches!(command, "help" | "--help" | "-h") {
        println!("porgous-agent [serve] [--config <file>]\n\nThe agent reads node.token, node.secret, node.bind and node.data_dir.");
        return Ok(());
    }
    if command != "serve" {
        bail!("unknown command '{command}'");
    }
    let config = Config::load(parse_config_arg(&args))?;
    if config.node.token.trim().is_empty() || config.node.secret.trim().is_empty() {
        bail!("node.token and node.secret must be configured (the panel shows them once when a node is created)");
    }
    if config.node.secret.len() < 24 {
        bail!("node.secret must be at least 24 characters");
    }
    let data_dir = config.node.data_dir.clone();
    std::fs::create_dir_all(data_dir.join("volumes"))?;
    std::fs::create_dir_all(data_dir.join("backups"))?;
    let runtime = Arc::new(
        LocalRuntime::connect(
            "agent",
            data_dir.join("volumes"),
            config.docker.network.clone(),
            config.docker.timezone.clone(),
            config.docker.default_image.clone(),
            config.docker.container_user.clone(),
            config.docker.host.clone(),
        )
        .await,
    );
    let state = AgentState {
        config: Arc::new(config.clone()),
        runtime,
        volumes: data_dir.join("volumes"),
        backups: data_dir.join("backups"),
        token_hash: node_token_hash(&config.node.token),
        secret: config.node.secret.clone().into_bytes(),
    };
    let addr: SocketAddr = config.node_bind_addr()?;
    let router = agent::router(state);
    if config.tls_enabled() {
        let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(
            config.tls.cert.clone().unwrap(),
            config.tls.key.clone().unwrap(),
        )
        .await
        .context("loading TLS certificate")?;
        tracing::info!(%addr, "agent listening with TLS");
        axum_server::bind_rustls(addr, tls)
            .serve(router.into_make_service_with_connect_info::<SocketAddr>())
            .await?;
    } else {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        tracing::warn!(%addr, "agent listening without TLS; terminate TLS with a reverse proxy");
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await?;
    }
    Ok(())
}
