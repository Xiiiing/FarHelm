use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use farhelm_core::{PRODUCT_NAME, PRODUCT_VERSION};
use farhelm_protocol::{FARHELM_PROTOCOL, HealthResponse, HealthStatus};
use farhelm_updater::{Role, Updater};

const HUB_ROLLBACK: &str = "/opt/farhelm-hub/rollback.sh";

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
    /// Check for or install an immutable official Hub release.
    Upgrade {
        /// Only report whether an update is available.
        #[arg(long)]
        check: bool,
        /// Install one exact formal version, such as V0.2.0.
        #[arg(long)]
        version: Option<String>,
        /// Permit a user-approved first-number version change.
        #[arg(long)]
        allow_major: bool,
    },
    /// Atomically switch the Hub to its locally installed previous version.
    Rollback,
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        CommandKind::Version => {
            println!("{PRODUCT_NAME} {PRODUCT_VERSION}");
            Ok(())
        }
        CommandKind::Health { hub } => health(&hub).await,
        CommandKind::Upgrade {
            check,
            version,
            allow_major,
        } => upgrade(check, version.as_deref(), allow_major).await,
        CommandKind::Rollback => run_rollback(HUB_ROLLBACK).await,
    }
}

async fn upgrade(check_only: bool, requested: Option<&str>, allow_major: bool) -> Result<()> {
    let updater = Updater::new()?;
    let Some(candidate) = updater
        .check(Role::Hub, PRODUCT_VERSION, requested, allow_major)
        .await?
    else {
        println!("FarHelm Hub {PRODUCT_VERSION} is up to date.");
        return Ok(());
    };
    if check_only {
        println!(
            "FarHelm Hub {} is available (current {}).",
            candidate.version, PRODUCT_VERSION
        );
        return Ok(());
    }
    ensure!(is_root(), "Hub upgrade must be run with sudo/root");
    println!(
        "Downloading verified {} for immutable release {}...",
        candidate.archive_name(),
        candidate.tag
    );
    let bundle = updater.download(Role::Hub, &candidate).await?;
    updater.install(&bundle, None).await?;
    println!("FarHelm Hub upgraded to {}.", candidate.version);
    Ok(())
}

async fn run_rollback(script: &str) -> Result<()> {
    ensure!(is_root(), "Hub rollback must be run with sudo/root");
    let status = tokio::process::Command::new("bash")
        .arg(script)
        .status()
        .await
        .with_context(|| format!("failed to start {script}"))?;
    ensure!(status.success(), "Hub rollback failed");
    Ok(())
}

fn is_root() -> bool {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .is_ok_and(|output| output.status.success() && output.stdout == b"0\n")
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
