use std::{ffi::OsString, net::SocketAddr, path::PathBuf, str::FromStr};

use anyhow::{Context, Result, ensure};
use farhelm_core::PRODUCT_VERSION;
use farhelm_hub::{AppState, HubConfig};
use tracing::info;
use tracing_subscriber::EnvFilter;

const DEFAULT_BIND: &str = "127.0.0.1:8787";

#[tokio::main]
async fn main() -> Result<()> {
    if requests_version(std::env::args_os().skip(1)) {
        println!("farhelm-hub {PRODUCT_VERSION}");
        return Ok(());
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let bind = std::env::var("FARHELM_HUB_BIND").unwrap_or_else(|_| DEFAULT_BIND.to_owned());
    let address = SocketAddr::from_str(&bind)
        .with_context(|| format!("invalid FARHELM_HUB_BIND value: {bind}"))?;
    ensure!(
        address.ip().is_loopback(),
        "FARHELM_HUB_BIND must use a loopback address; terminate public TLS with a reverse proxy"
    );
    let config = config_from_env()?;
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .with_context(|| format!("failed to bind FarHelm Hub to {address}"))?;

    info!(%address, "FarHelm Hub is listening");
    axum::serve(listener, farhelm_hub::app(AppState::new(config)?))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("FarHelm Hub server failed")?;
    Ok(())
}

fn requests_version(mut arguments: impl Iterator<Item = OsString>) -> bool {
    let argument = arguments.next();
    arguments.next().is_none()
        && argument
            .as_deref()
            .is_some_and(|value| value == "--version" || value == "-V")
}

fn config_from_env() -> Result<HubConfig> {
    let admin_user = required_env("FARHELM_ADMIN_USER")?;
    let admin_password = required_env("FARHELM_ADMIN_PASSWORD")?;
    let agent_token = required_env("FARHELM_AGENT_TOKEN")?;
    let console_dir = PathBuf::from(required_env("FARHELM_CONSOLE_DIR")?);
    let database_path = PathBuf::from(
        std::env::var("FARHELM_HUB_DATABASE")
            .unwrap_or_else(|_| "/var/lib/farhelm-hub/farhelm.db".to_owned()),
    );

    let config = HubConfig {
        admin_user,
        admin_password,
        agent_token,
        console_dir,
        database_path,
    };
    config.validate()?;
    Ok(config)
}

fn required_env(name: &str) -> Result<String> {
    let value =
        std::env::var(name).with_context(|| format!("required setting {name} is missing"))?;
    ensure!(!value.is_empty(), "required setting {name} is empty");
    Ok(value)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_flag_is_handled_without_runtime_configuration() {
        assert!(requests_version([OsString::from("--version")].into_iter()));
        assert!(requests_version([OsString::from("-V")].into_iter()));
        assert!(!requests_version(std::iter::empty()));
        assert!(!requests_version(
            [OsString::from("--version"), OsString::from("extra")].into_iter()
        ));
    }
}
