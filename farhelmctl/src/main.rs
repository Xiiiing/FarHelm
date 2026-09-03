use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use farhelm_core::{PRODUCT_NAME, PRODUCT_VERSION};
use farhelm_protocol::{FARHELM_PROTOCOL, HealthResponse, HealthStatus};

#[derive(Debug, Parser)]
#[command(name = "farhelmctl", version, about = "FarHelm management CLI")]
struct Cli {
    #[command(subcommand)]
    command: CommandKind,
}

#[derive(Debug, Subcommand)]
enum CommandKind {
    /// Print the FarHelm product version.
    Version,
    /// Check the Hub health and protocol version.
    Health {
        #[arg(long, env = "FARHELM_HUB_URL", default_value = "http://127.0.0.1:8787")]
        hub: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        CommandKind::Version => {
            println!("{PRODUCT_NAME} {PRODUCT_VERSION}");
            Ok(())
        }
        CommandKind::Health { hub } => health(&hub).await,
    }
}

async fn health(hub: &str) -> Result<()> {
    let endpoint = health_url(hub);
    let response = reqwest::get(&endpoint)
        .await
        .with_context(|| format!("failed to connect to {endpoint}"))?
        .error_for_status()
        .with_context(|| format!("Hub health request failed at {endpoint}"))?;
    let health: HealthResponse = response
        .json()
        .await
        .context("Hub returned an invalid health response")?;

    ensure!(health.status == HealthStatus::Ok, "Hub is not healthy");
    ensure!(
        health.protocol == FARHELM_PROTOCOL,
        "unsupported Hub protocol: {}",
        health.protocol
    );
    println!(
        "{} {} is healthy ({})",
        health.service, health.version, health.protocol
    );
    Ok(())
}

fn health_url(hub: &str) -> String {
    format!("{}/api/v1/health", hub.trim_end_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_url_normalizes_trailing_slash() {
        assert_eq!(
            health_url("http://127.0.0.1:8787/"),
            "http://127.0.0.1:8787/api/v1/health"
        );
    }
}
