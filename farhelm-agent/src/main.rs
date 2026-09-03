use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, Subcommand};
use farhelm_core::PRODUCT_VERSION;
use farhelm_protocol::{
    WORKER_PROTOCOL, WorkerHelloResult, WorkerRequest, WorkerResponse, read_frame, write_frame,
};
use tokio::{process::Command, time::timeout};
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "farhelm-agent", version, about = "FarHelm host agent")]
struct Cli {
    #[command(subcommand)]
    command: CommandKind,
}

#[derive(Debug, Subcommand)]
enum CommandKind {
    /// Run the long-lived Agent skeleton until interrupted.
    Run,
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

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    match Cli::parse().command {
        CommandKind::Run => run().await,
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

async fn run() -> Result<()> {
    info!(
        version = PRODUCT_VERSION,
        "FarHelm Agent skeleton is running"
    );
    tokio::signal::ctrl_c()
        .await
        .context("failed to wait for Ctrl+C")?;
    info!("FarHelm Agent stopped");
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
