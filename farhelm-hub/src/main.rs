use std::{net::SocketAddr, str::FromStr};

use anyhow::{Context, Result};
use tracing::info;
use tracing_subscriber::EnvFilter;

const DEFAULT_BIND: &str = "127.0.0.1:8787";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let bind = std::env::var("FARHELM_HUB_BIND").unwrap_or_else(|_| DEFAULT_BIND.to_owned());
    let address = SocketAddr::from_str(&bind)
        .with_context(|| format!("invalid FARHELM_HUB_BIND value: {bind}"))?;
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .with_context(|| format!("failed to bind FarHelm Hub to {address}"))?;

    info!(%address, "FarHelm Hub is listening");
    axum::serve(listener, farhelm_hub::app())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("FarHelm Hub server failed")?;
    Ok(())
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::error!(%error, "failed to install Ctrl+C handler");
    }
}
