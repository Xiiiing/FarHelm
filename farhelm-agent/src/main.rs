use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Parser, Subcommand};
use farhelm_core::PRODUCT_VERSION;
use farhelm_protocol::{
    AgentHeartbeat, AgentHeartbeatAck, CommandAction, CommandClaimRequest, CommandClaimResponse,
    CommandState, CommandStatusResponse, FARHELM_PROTOCOL, ProbeResult, WORKER_PROTOCOL,
    WorkerHelloResult, WorkerRequest, WorkerResponse, read_frame, write_frame,
};
use reqwest::{Client, Url};
use tokio::{process::Command, time::timeout};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

mod command_store;

use command_store::CommandStore;

#[derive(Parser)]
#[command(name = "farhelm-agent", version, about = "FarHelm host agent")]
struct Cli {
    #[command(subcommand)]
    command: CommandKind,
}

#[derive(Subcommand)]
enum CommandKind {
    /// Send heartbeats until interrupted.
    Run {
        #[command(flatten)]
        hub: HubArgs,
        #[arg(long, env = "FARHELM_HEARTBEAT_INTERVAL", default_value_t = 15)]
        interval: u64,
        #[arg(long, env = "FARHELM_COMMAND_POLL_INTERVAL", default_value_t = 2)]
        command_interval: u64,
        #[arg(
            long,
            env = "FARHELM_AGENT_DATABASE",
            default_value = "farhelm-agent.db"
        )]
        database: PathBuf,
    },
    /// Send one heartbeat and exit.
    Heartbeat {
        #[command(flatten)]
        hub: HubArgs,
    },
    /// Claim and process at most one Hub command, then exit.
    CommandPoll {
        #[command(flatten)]
        hub: HubArgs,
        #[arg(
            long,
            env = "FARHELM_AGENT_DATABASE",
            default_value = "farhelm-agent.db"
        )]
        database: PathBuf,
    },
    /// Check local prerequisites without changing the host.
    Doctor {
        #[arg(long, default_value = "python3")]
        python: String,
        #[arg(long, default_value = "farhelm-worker-codex")]
        worker_root: PathBuf,
    },
    /// Start the Python Worker and verify the framed protocol handshake.
    WorkerSmoke {
        #[arg(long, default_value = "python3")]
        python: String,
        #[arg(long, default_value = "farhelm-worker-codex")]
        worker_root: PathBuf,
    },
}

#[derive(Args)]
struct HubArgs {
    #[arg(long, env = "FARHELM_HUB_URL")]
    hub: String,
    #[arg(long, env = "FARHELM_AGENT_TOKEN", hide_env_values = true)]
    token: String,
    #[arg(long, env = "FARHELM_AGENT_ID")]
    agent_id: String,
    #[arg(long, env = "FARHELM_AGENT_HOSTNAME")]
    hostname: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    match Cli::parse().command {
        CommandKind::Run {
            hub,
            interval,
            command_interval,
            database,
        } => run(hub, interval, command_interval, &database).await,
        CommandKind::Heartbeat { hub } => heartbeat_once(&hub).await,
        CommandKind::CommandPoll { hub, database } => command_poll_once(&hub, &database).await,
        CommandKind::Doctor {
            python,
            worker_root,
        } => doctor(&python, &worker_root),
        CommandKind::WorkerSmoke {
            python,
            worker_root,
        } => worker_smoke(&python, &worker_root).await,
    }
}

async fn run(
    hub: HubArgs,
    interval_secs: u64,
    command_interval_secs: u64,
    database: &Path,
) -> Result<()> {
    ensure!(
        interval_secs >= 5,
        "heartbeat interval must be at least 5 seconds"
    );
    ensure!(
        command_interval_secs >= 1,
        "command poll interval must be at least 1 second"
    );
    let (client, endpoint, heartbeat) = heartbeat_client(&hub)?;
    let command_store = CommandStore::open(database)?;
    let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut command_ticker = tokio::time::interval(Duration::from_secs(command_interval_secs));
    command_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);

    info!(
        version = PRODUCT_VERSION,
        agent_id = %hub.agent_id,
        interval_secs,
        command_interval_secs,
        "FarHelm Agent is running"
    );
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                match send_heartbeat(&client, endpoint.clone(), &hub.token, &heartbeat).await {
                    Ok(()) => info!(agent_id = %hub.agent_id, "heartbeat accepted"),
                    Err(error) => warn!(agent_id = %hub.agent_id, %error, "heartbeat failed; retrying"),
                }
            }
            _ = command_ticker.tick() => {
                if let Err(error) = process_command_cycle(&client, &hub, &command_store).await {
                    warn!(agent_id = %hub.agent_id, %error, "command cycle failed; retrying");
                }
            }
            () = &mut shutdown => {
                break;
            }
        }
    }
    info!("FarHelm Agent stopped");
    Ok(())
}

async fn command_poll_once(hub: &HubArgs, database: &Path) -> Result<()> {
    let (client, _, _) = heartbeat_client(hub)?;
    let store = CommandStore::open(database)?;
    let processed = process_command_cycle(&client, hub, &store).await?;
    println!("Command poll completed: {processed} command(s) processed");
    Ok(())
}

async fn process_command_cycle(
    client: &Client,
    hub: &HubArgs,
    store: &CommandStore,
) -> Result<u64> {
    let mut processed = 0;
    drain_local_work(client, hub, store, &mut processed).await?;

    let claim_url = hub_endpoint(&hub.hub, "/api/v1/agent/commands/claim")?;
    let response = client
        .post(claim_url)
        .bearer_auth(&hub.token)
        .json(&CommandClaimRequest {
            protocol: FARHELM_PROTOCOL.to_owned(),
            agent_id: hub.agent_id.clone(),
        })
        .send()
        .await
        .context("failed to claim Hub command")?
        .error_for_status()
        .context("Hub rejected command claim")?;
    let claim: CommandClaimResponse = response
        .json()
        .await
        .context("Hub returned an invalid command claim")?;
    ensure!(
        claim.protocol == FARHELM_PROTOCOL,
        "Hub command protocol mismatch"
    );
    if let Some(command) = claim.command {
        ensure!(
            command.protocol == FARHELM_PROTOCOL,
            "command protocol mismatch"
        );
        ensure!(
            command.agent_id == hub.agent_id,
            "Hub delivered a command for another Agent"
        );
        ensure!(
            command.action == CommandAction::AgentProbe,
            "unsupported command action"
        );
        store.receive(&command, unix_time())?;
        drain_local_work(client, hub, store, &mut processed).await?;
    }
    Ok(processed)
}

async fn drain_local_work(
    client: &Client,
    hub: &HubArgs,
    store: &CommandStore,
    processed: &mut u64,
) -> Result<()> {
    for _ in 0..8 {
        let Some(pending) = store.next_work()? else {
            return Ok(());
        };
        if pending.state == CommandState::Accepted && unix_time() >= pending.expires_at_unix {
            store.expire(&pending.command_id, unix_time())?;
            continue;
        }
        if pending.state == CommandState::Accepted && pending.reported {
            store.complete_probe(
                &pending.command_id,
                &ProbeResult {
                    agent_version: PRODUCT_VERSION.to_owned(),
                    hostname: resolve_hostname(hub.hostname.as_deref(), &hub.agent_id),
                },
                unix_time(),
            )?;
            *processed += 1;
            continue;
        }
        let report = store.report(&pending, &hub.agent_id);
        send_command_report(client, hub, &report).await?;
        store.mark_reported(&pending.command_id, pending.state, unix_time())?;
    }
    bail!("local command work exceeded the bounded cycle limit")
}

async fn send_command_report(
    client: &Client,
    hub: &HubArgs,
    report: &farhelm_protocol::CommandReportRequest,
) -> Result<()> {
    let report_url = hub_endpoint(&hub.hub, "/api/v1/agent/commands/report")?;
    let response = client
        .post(report_url)
        .bearer_auth(&hub.token)
        .json(report)
        .send()
        .await
        .context("failed to report Hub command")?
        .error_for_status()
        .context("Hub rejected command report")?;
    let status: CommandStatusResponse = response
        .json()
        .await
        .context("Hub returned an invalid command status")?;
    ensure!(
        status.command_id == report.command_id,
        "Hub acknowledged another command"
    );
    ensure!(
        status.state == report.state,
        "Hub acknowledged an unexpected command state"
    );
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            warn!(%error, "failed to install Ctrl+C handler");
        }
    };
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => warn!(%error, "failed to install SIGTERM handler"),
        }
    };
    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
}

async fn heartbeat_once(hub: &HubArgs) -> Result<()> {
    let (client, endpoint, heartbeat) = heartbeat_client(hub)?;
    send_heartbeat(&client, endpoint, &hub.token, &heartbeat).await?;
    println!(
        "Heartbeat accepted for {} ({FARHELM_PROTOCOL})",
        hub.agent_id
    );
    Ok(())
}

fn heartbeat_client(hub: &HubArgs) -> Result<(Client, Url, AgentHeartbeat)> {
    ensure!(
        hub.token.len() >= 32,
        "Agent token must contain at least 32 characters"
    );
    let endpoint = heartbeat_url(&hub.hub)?;
    let hostname = resolve_hostname(hub.hostname.as_deref(), &hub.agent_id);
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent(format!("farhelm-agent/{PRODUCT_VERSION}"))
        .build()
        .context("failed to build Hub HTTP client")?;
    Ok((
        client,
        endpoint,
        AgentHeartbeat::new(&hub.agent_id, hostname, PRODUCT_VERSION),
    ))
}

fn heartbeat_url(hub: &str) -> Result<Url> {
    hub_endpoint(hub, "/api/v1/agents/heartbeat")
}

fn hub_endpoint(hub: &str, path: &str) -> Result<Url> {
    let mut url = Url::parse(hub).context("FARHELM_HUB_URL is not a valid URL")?;
    let local_http = url.scheme() == "http"
        && url
            .host_str()
            .is_some_and(|host| matches!(host, "127.0.0.1" | "::1" | "localhost"));
    ensure!(
        url.scheme() == "https" || local_http,
        "Hub URL must use HTTPS (HTTP is allowed only for loopback testing)"
    );
    url.set_path(path);
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn resolve_hostname(configured: Option<&str>, agent_id: &str) -> String {
    configured
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| agent_id.to_owned())
}

async fn send_heartbeat(
    client: &Client,
    endpoint: Url,
    token: &str,
    heartbeat: &AgentHeartbeat,
) -> Result<()> {
    let response = client
        .post(endpoint)
        .bearer_auth(token)
        .json(heartbeat)
        .send()
        .await
        .context("failed to reach Hub")?
        .error_for_status()
        .context("Hub rejected heartbeat")?;
    let ack: AgentHeartbeatAck = response
        .json()
        .await
        .context("Hub returned an invalid heartbeat acknowledgement")?;
    ensure!(ack.accepted, "Hub did not accept heartbeat");
    ensure!(
        ack.protocol == FARHELM_PROTOCOL,
        "Hub protocol mismatch: {}",
        ack.protocol
    );
    Ok(())
}

fn doctor(python: &str, worker_root: &Path) -> Result<()> {
    let python_ok = std::process::Command::new(python)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    let worker_src = worker_root.join("src/farhelm_worker_codex");
    let nvidia_available = std::process::Command::new("nvidia-smi")
        .arg("--query-gpu=name")
        .arg("--format=csv,noheader")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());

    println!("FarHelm Agent doctor");
    println!("  Python ({python}): {}", state(python_ok));
    println!("  Worker source: {}", state(worker_src.is_dir()));
    println!(
        "  NVIDIA tools: {} (optional for skeleton)",
        state(nvidia_available)
    );

    ensure!(python_ok, "Python command `{python}` is unavailable");
    ensure!(
        worker_src.is_dir(),
        "Worker source not found at {}",
        worker_src.display()
    );
    Ok(())
}

const fn state(value: bool) -> &'static str {
    if value { "ok" } else { "missing" }
}

async fn worker_smoke(python: &str, worker_root: &Path) -> Result<()> {
    let source_root = worker_root
        .join("src")
        .canonicalize()
        .with_context(|| format!("worker source not found below {}", worker_root.display()))?;

    let mut child = Command::new(python)
        .arg("-m")
        .arg("farhelm_worker_codex")
        .env("PYTHONPATH", source_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("failed to start Worker with `{python}`"))?;

    let mut stdin = child.stdin.take().context("Worker stdin was not piped")?;
    let mut stdout = child.stdout.take().context("Worker stdout was not piped")?;
    let request = WorkerRequest::hello("req_worker_smoke", PRODUCT_VERSION);

    write_frame(&mut stdin, &request)
        .await
        .context("failed to send Worker hello")?;
    let response: WorkerResponse = timeout(Duration::from_secs(5), read_frame(&mut stdout))
        .await
        .context("Worker hello timed out")??;

    ensure!(
        response.protocol == WORKER_PROTOCOL,
        "Worker protocol mismatch"
    );
    ensure!(
        response.request_id == request.request_id,
        "request ID mismatch"
    );
    if !response.ok {
        bail!("Worker rejected hello: {:?}", response.error);
    }
    let result: WorkerHelloResult =
        serde_json::from_value(response.result.context("Worker hello omitted its result")?)
            .context("Worker hello result was invalid")?;
    ensure!(
        result
            .capabilities
            .iter()
            .any(|item| item == "worker.hello"),
        "Worker did not advertise worker.hello"
    );

    drop(stdin);
    let status = timeout(Duration::from_secs(5), child.wait())
        .await
        .context("Worker did not stop after stdin closed")??;
    ensure!(status.success(), "Worker exited with {status}");

    println!(
        "Worker handshake ok: {} {} ({})",
        result.worker, result.version, response.protocol
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heartbeat_endpoint_uses_versioned_path() {
        let url = heartbeat_url("https://farhelm.example.com/base?ignored=yes").unwrap();
        assert_eq!(
            url.as_str(),
            "https://farhelm.example.com/api/v1/agents/heartbeat"
        );
    }

    #[test]
    fn public_plaintext_hub_is_rejected() {
        assert!(heartbeat_url("http://farhelm.example.com").is_err());
        assert!(heartbeat_url("http://127.0.0.1:8787").is_ok());
    }

    #[test]
    fn configured_hostname_wins() {
        assert_eq!(resolve_hostname(Some(" trainer-a "), "gpu-a"), "trainer-a");
    }
}
