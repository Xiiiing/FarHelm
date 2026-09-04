use std::{net::SocketAddr, path::PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use farhelm_core::PRODUCT_VERSION;
use farhelm_hub::AppState;
use tracing::info;
use tracing_subscriber::EnvFilter;

mod config;
mod management;

use config::{DEFAULT_CONFIG_PATH, HubFileConfig};

#[derive(Debug, Parser)]
#[command(name = "farhelm-hub", version, about = "FarHelm Control Hub")]
struct Cli {
    #[command(subcommand)]
    command: CommandKind,
}

#[derive(Debug, Subcommand)]
enum CommandKind {
    /// Run the Hub in the foreground.
    Serve {
        #[arg(long, env = "FARHELM_HUB_CONFIG", default_value = DEFAULT_CONFIG_PATH)]
        config: PathBuf,
    },
    /// Install this executable, configuration, and the system service.
    Install {
        /// Install files without enabling or starting the service.
        #[arg(long)]
        no_start: bool,
    },
    /// Start the installed system service.
    Start,
    /// Stop the installed system service.
    Stop,
    /// Restart the installed system service.
    Restart,
    /// Confirm that the installed system service is active.
    Status,
    /// Validate configuration and local runtime prerequisites.
    Doctor {
        #[arg(long, env = "FARHELM_HUB_CONFIG", default_value = DEFAULT_CONFIG_PATH)]
        config: PathBuf,
    },
    /// Check for or install an immutable official Hub update.
    #[command(visible_alias = "upgrade")]
    Update {
        #[arg(long)]
        check: bool,
        #[arg(long)]
        version: Option<String>,
        #[arg(long)]
        allow_major: bool,
    },
    /// Atomically switch to the locally preserved previous binary.
    Rollback,
    /// Remove the Hub program and its managed service files.
    Uninstall {
        /// Keep the TOML configuration and SQLite data.
        #[arg(long)]
        keep_data: bool,
    },
    /// Check a running Hub health endpoint.
    Health {
        #[arg(long, default_value = "http://127.0.0.1:8787")]
        hub: String,
    },
    /// Local administrator account management.
    Admin {
        #[command(subcommand)]
        command: AdminCommand,
    },
}

#[derive(Debug, Subcommand)]
enum AdminCommand {
    /// Set a new password and revoke every browser session.
    ResetPassword,
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        CommandKind::Serve { config } => serve(config).await,
        CommandKind::Install { no_start } => management::install(no_start).await,
        CommandKind::Start => management::service_action("start"),
        CommandKind::Stop => management::service_action("stop"),
        CommandKind::Restart => management::restart().await,
        CommandKind::Status => management::status(),
        CommandKind::Doctor { config } => management::doctor(&config),
        CommandKind::Update {
            check,
            version,
            allow_major,
        } => management::update(check, version.as_deref(), allow_major).await,
        CommandKind::Rollback => management::rollback().await,
        CommandKind::Uninstall { keep_data } => management::uninstall(keep_data),
        CommandKind::Health { hub } => management::health(&hub).await,
        CommandKind::Admin {
            command: AdminCommand::ResetPassword,
        } => management::reset_password().await,
    }
}

async fn serve(path: PathBuf) -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let file = HubFileConfig::load(&path)?;
    let address: SocketAddr = file
        .hub
        .bind
        .parse()
        .with_context(|| format!("invalid hub.bind value: {}", file.hub.bind))?;
    let config = file.runtime();
    config.validate()?;
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .with_context(|| format!("failed to bind FarHelm Hub to {address}"))?;

    info!(version = PRODUCT_VERSION, %address, "FarHelm Hub is listening");
    let state = AppState::new(config)?;
    state.spawn_background_tasks();
    axum::serve(listener, farhelm_hub::app(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("FarHelm Hub server failed")?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(%error, "failed to install Ctrl+C handler");
        }
    };
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => tracing::error!(%error, "failed to install SIGTERM handler"),
        }
    };
    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
}
