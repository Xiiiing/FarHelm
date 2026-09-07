use std::{
    collections::BTreeMap,
    fs,
    io::{IsTerminal, Read},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Parser, Subcommand};
use farhelm_core::PRODUCT_VERSION;
use farhelm_protocol::{
    AgentEventAck, AgentEventBatch, AgentHeartbeat, AgentHeartbeatAck, AgentReadReportRequest,
    CommandAction, CommandClaimRequest, CommandClaimResponse, CommandState, CommandStatusResponse,
    FARHELM_PROTOCOL, ProbeResult,
};
use reqwest::{Client, Url};
use tokio::process::Command;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

mod codex;
mod command_store;
mod config;
mod experiment_store;
mod live;
mod management;
mod migrations;
mod runtime_tasks;

use command_store::CommandStore;
use config::{AgentFileConfig, AgentPaths};
use experiment_store::{
    AutoPrompt, ExperimentStore, ProjectMatchers, RemoteCommand, ScheduledPrompt, WatchRegistration,
};

#[derive(Parser)]
#[command(name = "farhelm-agent", version, about = "FarHelm host agent")]
struct Cli {
    #[command(subcommand)]
    command: CommandKind,
}

#[derive(Subcommand)]
enum CommandKind {
    /// Run the outbound Hub connection and local execution queues.
    Run {
        #[command(flatten)]
        connection: ConnectionArgs,
        #[arg(long, env = "FARHELM_HEARTBEAT_INTERVAL")]
        interval: Option<u64>,
        #[arg(long, env = "FARHELM_COMMAND_POLL_INTERVAL")]
        command_interval: Option<u64>,
        #[arg(long, env = "FARHELM_AGENT_DATABASE")]
        database: Option<PathBuf>,
    },
    /// Send one heartbeat and exit.
    Heartbeat {
        #[command(flatten)]
        connection: ConnectionArgs,
    },
    /// Claim and process at most one Hub command, then exit.
    CommandPoll {
        #[command(flatten)]
        connection: ConnectionArgs,
        #[arg(long, env = "FARHELM_AGENT_DATABASE")]
        database: Option<PathBuf>,
    },
    /// Register and inspect explicitly selected training processes.
    Experiment {
        #[command(subcommand)]
        command: ExperimentCommand,
    },
    /// Inspect Codex sessions available to an approved project.
    Codex {
        #[command(subcommand)]
        command: CodexCommand,
    },
    /// Install this executable, configuration, native Codex selection, and user service.
    Install {
        /// Install files without creating or starting a systemd user service.
        #[arg(long)]
        no_service: bool,
    },
    /// Start the installed user service.
    Start,
    /// Stop the installed user service.
    Stop,
    /// Restart the installed user service.
    Restart,
    /// Report service, Hub connectivity, and native Codex readiness.
    Status,
    /// Check the installed configuration and local prerequisites.
    Doctor {
        #[arg(long, env = "FARHELM_AGENT_CONFIG")]
        config: Option<PathBuf>,
    },
    /// Pair this host with an Agent entry created in the Console.
    Pair,
    /// Verify the local native Codex app-server protocol handshake.
    #[command(visible_alias = "worker-smoke")]
    CodexSmoke {
        #[arg(long)]
        bin: Option<PathBuf>,
    },
    /// Check for or install an immutable official Agent release.
    #[command(visible_alias = "upgrade")]
    Update {
        /// Only report whether an update is available.
        #[arg(long)]
        check: bool,
        /// Install one exact formal version, such as V0.8.0.
        #[arg(long)]
        version: Option<String>,
        /// Permit a user-approved first-number version change.
        #[arg(long)]
        allow_major: bool,
    },
    /// Atomically switch the Agent to its locally installed previous version.
    Rollback,
    /// Remove the Agent program and its managed service files.
    Uninstall {
        /// Keep the TOML configuration and SQLite data.
        #[arg(long)]
        keep_data: bool,
    },
}

#[derive(Subcommand)]
enum ExperimentCommand {
    /// Persist an explicit training result locally; the service delivers it to Hub.
    Report {
        #[arg(long, env = "FARHELM_AGENT_CONFIG")]
        config: Option<PathBuf>,
        #[arg(long)]
        project: String,
        #[arg(long)]
        run_id: Option<String>,
        #[arg(long)]
        name: String,
        #[arg(long, value_parser=["succeeded","failed","unknown"], conflicts_with="exit_code", required_unless_present="exit_code")]
        status: Option<String>,
        #[arg(long, conflicts_with = "status", allow_negative_numbers = true)]
        exit_code: Option<i32>,
        /// Use '-' to read the message from stdin.
        #[arg(long, default_value = "")]
        message: String,
        #[arg(long, requires = "on_success_prompt_file")]
        session: Option<String>,
        #[arg(long, requires = "session")]
        on_success_prompt_file: Option<String>,
    },
    /// Watch one existing PID. FarHelm never starts or stops the process.
    Watch {
        #[arg(long, env = "FARHELM_AGENT_CONFIG")]
        config: Option<PathBuf>,
        #[arg(long)]
        project: String,
        #[arg(long)]
        pid: u32,
        #[arg(long)]
        log: PathBuf,
        #[arg(long)]
        name: Option<String>,
        #[arg(
            long,
            conflicts_with = "new_session",
            required_unless_present = "new_session"
        )]
        session: Option<String>,
        #[arg(long, value_parser = ["inspect", "edit"], conflicts_with = "session", required_unless_present = "session")]
        new_session: Option<String>,
        /// Read the success prompt from this file, or from stdin when the value is '-'.
        #[arg(long)]
        on_success_prompt_file: Option<String>,
    },
    /// List local watches and their durable state.
    List {
        #[arg(long, env = "FARHELM_AGENT_CONFIG")]
        config: Option<PathBuf>,
    },
    /// Stop watching one registration without signaling its PID.
    Unwatch {
        watch_id: String,
        #[arg(long, env = "FARHELM_AGENT_CONFIG")]
        config: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum CodexCommand {
    /// Select an existing Codex installation without changing its login or settings.
    Configure {
        #[arg(long, env = "FARHELM_AGENT_CONFIG")]
        config: Option<PathBuf>,
        #[arg(long)]
        bin: Option<PathBuf>,
    },
    /// List resumable Codex threads whose cwd belongs to this project.
    Sessions {
        #[arg(long, env = "FARHELM_AGENT_CONFIG")]
        config: Option<PathBuf>,
        #[arg(long)]
        project: String,
    },
}

#[derive(Args)]
struct ConnectionArgs {
    #[arg(long, env = "FARHELM_AGENT_CONFIG")]
    config: Option<PathBuf>,
    #[arg(long, env = "FARHELM_HUB_URL")]
    hub: Option<String>,
    #[arg(long, env = "FARHELM_AGENT_TOKEN", hide_env_values = true)]
    token: Option<String>,
    #[arg(long, env = "FARHELM_AGENT_ID")]
    agent_id: Option<String>,
    #[arg(long, env = "FARHELM_AGENT_HOSTNAME")]
    hostname: Option<String>,
}

#[derive(Clone)]
struct HubArgs {
    link: live::Link,
    live_required: bool,
    hub: String,
    token: String,
    agent_id: String,
    hostname: Option<String>,
}

struct RuntimeArgs {
    hub: HubArgs,
    interval: u64,
    command_interval: u64,
    database: PathBuf,
    projects: BTreeMap<String, config::ProjectSection>,
    codex_bin: Option<PathBuf>,
}

#[derive(Debug)]
struct CodexTurnOrphaned(String);
impl std::fmt::Display for CodexTurnOrphaned {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}
impl std::error::Error for CodexTurnOrphaned {}
#[derive(Clone)]
struct CodexRuntime {
    tasks: runtime_tasks::RuntimeTasks,
    codex: codex::Codex,
    link: live::Link,
    agent_id: String,
    wake: Arc<tokio::sync::Notify>,
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
            connection,
            interval,
            command_interval,
            database,
        } => {
            let runtime = resolve_runtime(connection, interval, command_interval, database)?;
            run(
                runtime.hub,
                runtime.interval,
                runtime.command_interval,
                &runtime.database,
                &runtime.projects,
                runtime.codex_bin,
            )
            .await
        }
        CommandKind::Heartbeat { connection } => {
            let runtime = resolve_runtime(connection, None, None, None)?;
            heartbeat_once(&runtime.hub).await
        }
        CommandKind::CommandPoll {
            connection,
            database,
        } => {
            let runtime = resolve_runtime(connection, None, None, database)?;
            command_poll_once(&runtime).await
        }
        CommandKind::Experiment { command } => experiment_command(command),
        CommandKind::Codex { command } => codex_command(command).await,
        CommandKind::Install { no_service } => management::install(no_service).await,
        CommandKind::Start => management::service_action("start"),
        CommandKind::Stop => management::service_action("stop"),
        CommandKind::Restart => management::restart().await,
        CommandKind::Status => management::status(),
        CommandKind::Doctor { config } => management::doctor(config.as_deref()).await.map(|_| ()),
        CommandKind::Pair => management::pair().await,
        CommandKind::CodexSmoke { bin } => {
            let codex = codex::Codex::new(bin);
            codex.warm().await?;
            println!(
                "Codex handshake ok: {} ({})",
                codex.status().version.as_deref().unwrap_or("unknown"),
                codex.status().state
            );
            codex.shutdown().await;
            Ok(())
        }
        CommandKind::Update {
            check,
            version,
            allow_major,
        } => management::update(check, version.as_deref(), allow_major).await,
        CommandKind::Rollback => management::rollback().await,
        CommandKind::Uninstall { keep_data } => management::uninstall(keep_data),
    }
}

fn resolve_runtime(
    options: ConnectionArgs,
    interval: Option<u64>,
    command_interval: Option<u64>,
    database: Option<PathBuf>,
) -> Result<RuntimeArgs> {
    let paths = AgentPaths::discover().ok();
    let config_path = options.config.or_else(|| {
        paths
            .as_ref()
            .map(|value| value.config.clone())
            .filter(|path| path.is_file())
    });
    let config = config_path
        .as_deref()
        .map(AgentFileConfig::load)
        .transpose()?;
    let hub = options
        .hub
        .or_else(|| config.as_ref().map(|value| value.agent.hub_url.clone()))
        .context("Hub URL is missing; provide --config or FARHELM_HUB_URL")?;
    let token = options
        .token
        .or_else(|| config.as_ref().map(|value| value.agent.token.clone()))
        .context("Agent token is missing; provide --config or FARHELM_AGENT_TOKEN")?;
    let agent_id = options
        .agent_id
        .or_else(|| config.as_ref().map(|value| value.agent.id.clone()))
        .context("Agent ID is missing; provide --config or FARHELM_AGENT_ID")?;
    let hostname = options.hostname.or_else(|| {
        config
            .as_ref()
            .and_then(|value| value.agent.hostname.clone())
    });
    let codex_bin = config.as_ref().and_then(|value| value.codex.bin.clone());
    Ok(RuntimeArgs {
        hub: HubArgs {
            link: live::Link::default(),
            live_required: false,
            hub,
            token,
            agent_id,
            hostname,
        },
        interval: interval
            .or_else(|| config.as_ref().map(|value| value.agent.heartbeat_seconds))
            .unwrap_or(15),
        command_interval: command_interval
            .or_else(|| {
                config
                    .as_ref()
                    .map(|value| value.agent.command_poll_seconds)
            })
            .unwrap_or(2),
        database: database
            .or_else(|| config.as_ref().map(|value| value.agent.database.clone()))
            .unwrap_or_else(|| PathBuf::from("farhelm-agent.db")),
        projects: config.map(|value| value.projects).unwrap_or_default(),
        codex_bin,
    })
}

async fn run(
    mut hub: HubArgs,
    interval_secs: u64,
    command_interval_secs: u64,
    database: &Path,
    projects: &BTreeMap<String, config::ProjectSection>,
    codex_bin: Option<PathBuf>,
) -> Result<()> {
    ensure!(
        interval_secs >= 5,
        "heartbeat interval must be at least 5 seconds"
    );
    ensure!(
        command_interval_secs >= 1,
        "command poll interval must be at least 1 second"
    );
    hub.live_required = true;
    let (client, _, heartbeat) = heartbeat_client(&hub)?;
    let command_store = CommandStore::open(database)?;
    let experiment_store = ExperimentStore::open(database)?;
    experiment_store.import_config_projects(projects, unix_time())?;
    // Explicitly configured approvals remain visible even before Codex is ready.
    for (id, project) in experiment_store.approved_projects()? {
        let name = project
            .path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(&id);
        let (candidate, _) =
            experiment_store.upsert_discovered_project(&project.path, name, &id, 0, unix_time())?;
        experiment_store.enqueue_event(&format!("project:{}:configured:{}", candidate.candidate_id, unix_time()), "project.updated", &serde_json::json!({"candidate_id":candidate.candidate_id,"display_name":candidate.display_name,"suggested_project_id":candidate.suggested_project_id,"session_count":candidate.session_count,"state":candidate.state,"updated_at_unix":candidate.updated_at_unix}), unix_time())?;
    }

    let worker_runtime = CodexRuntime {
        tasks: runtime_tasks::RuntimeTasks::new(),
        codex: codex::Codex::new(codex_bin),
        link: hub.link.clone(),
        agent_id: hub.agent_id.clone(),
        wake: Arc::new(tokio::sync::Notify::new()),
    };
    experiment_store.recover_recorded_turns(unix_time())?;
    let orphaned = experiment_store.orphan_running_prompts(unix_time())?;
    if orphaned > 0 {
        warn!(
            orphaned,
            "Codex turns interrupted by the previous Agent exit were marked orphaned"
        );
    }
    let orphaned_remote = experiment_store.orphan_running_remote_commands(unix_time())?;
    if orphaned_remote > 0 {
        warn!(
            orphaned_remote,
            "remote Codex commands interrupted by the previous Agent exit were marked orphaned"
        );
    }
    experiment_store.orphan_running_schedules(unix_time())?;
    let command_store = Arc::new(command_store);
    let experiment_store = Arc::new(experiment_store);
    let (incoming_commands, mut commands_rx) = tokio::sync::mpsc::channel(32);
    let (incoming_reads, mut reads_rx) = tokio::sync::mpsc::channel(16);
    worker_runtime.tasks.spawn(live::run(
        hub.clone(),
        heartbeat,
        worker_runtime.codex.clone(),
        incoming_commands,
        incoming_reads,
        worker_runtime.wake.clone(),
    ));
    {
        let store = experiment_store.clone();
        let commands = command_store.clone();
        let wake = worker_runtime.wake.clone();
        let agent_id = hub.agent_id.clone();
        let receipt_hub = hub.clone();
        let receipt_client = client.clone();
        worker_runtime.tasks.spawn(async move {
            while let Some(command) = commands_rx.recv().await {
                if command.agent_id != agent_id || command.protocol != FARHELM_PROTOCOL {
                    receipt_hub.link.reset();
                    continue;
                }
                let incoming = command.clone();
                let store = store.clone();
                let receipts = store.clone();
                let commands = commands.clone();
                let result = runtime_tasks::blocking(move || {
                    if command.action == CommandAction::AgentProbe {
                        commands.receive(&command, unix_time()).map(|_| ())
                    } else {
                        store.receive_remote_command(&command, unix_time())
                    }
                })
                .await;
                match result {
                    Err(error) => {
                        warn!(%error, "Agent rejected an incoming command");
                        receipt_hub.link.reset();
                    }
                    Ok(()) if incoming.action != CommandAction::AgentProbe => {
                        let id = incoming.command_id.clone();
                        let Ok(saved) = receipts.background(move |s| s.remote_receipt(&id)).await
                        else {
                            receipt_hub.link.reset();
                            continue;
                        };
                        let state = saved.state;
                        let report = farhelm_protocol::CommandReportRequest {
                            protocol: FARHELM_PROTOCOL.into(),
                            agent_id: agent_id.clone(),
                            command_id: incoming.command_id.clone(),
                            state,
                            result: None,
                            data: saved.data,
                            detail: saved.detail,
                        };
                        if send_command_report(&receipt_client, &receipt_hub, &report)
                            .await
                            .is_ok()
                            && state == CommandState::Accepted
                        {
                            let id = incoming.command_id;
                            let _ = receipts
                                .background(move |s| {
                                    s.mark_remote_accepted_reported(&id, unix_time())
                                })
                                .await;
                        }
                    }
                    Ok(()) => {}
                }
                wake.notify_one();
            }
        });
    }
    {
        let store = experiment_store.clone();
        let native = worker_runtime.clone();
        let hub = hub.clone();
        worker_runtime.tasks.spawn(async move {
            let permits = Arc::new(tokio::sync::Semaphore::new(4));
            while let Some((request, expires)) = reads_rx.recv().await {
                let Ok(permit) = permits.clone().acquire_owned().await else {
                    break;
                };
                let store = store.clone();
                let worker = native.clone();
                let hub = hub.clone();
                native.tasks.spawn(async move {
                    let _permit = permit;
                    if expires <= unix_time() {
                        return;
                    }
                    let id = request.request_id.clone();
                    let outcome = tokio::time::timeout(
                        Duration::from_secs(expires.saturating_sub(unix_time()).min(20)),
                        read_request(&hub, &worker, &store, request),
                    )
                    .await;
                    let report = match outcome {
                        Ok(Ok(report)) => report,
                        _ => farhelm_protocol::AgentReadReportRequest {
                            protocol: FARHELM_PROTOCOL.to_owned(),
                            agent_id: hub.agent_id.clone(),
                            request_id: id,
                            ok: false,
                            data: None,
                            detail: Some("codex_read_failed".into()),
                        },
                    };
                    let _ = hub
                        .link
                        .request(
                            |request_id| farhelm_protocol::live::AgentFrame::ReadReport {
                                request_id,
                                report,
                            },
                        )
                        .await;
                });
            }
        });
    }
    {
        let mut link = hub.link.connected.subscribe();
        let mut native = worker_runtime.codex.subscribe_status();
        let status_path = database.with_extension("status.json");
        worker_runtime.tasks.spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(15));
            loop {
                let status = serde_json::json!({"agent_version":PRODUCT_VERSION,"pid":std::process::id(),"updated_at_unix":unix_time(),"hub_connected":*link.borrow(),"codex":native.borrow().clone()});
                let path = status_path.clone();
                let _ = runtime_tasks::blocking(move || farhelm_lifecycle::write_atomic(&path, &serde_json::to_vec(&status)?, 0o600)).await;
                tokio::select! { _ = link.changed() => {}, _ = native.changed() => {}, _ = tick.tick() => {} }
            }
        });
    }
    // A filesystem wake observes CLI commits as well as this service's outbox.
    worker_runtime.tasks.spawn(runtime_tasks::watch_database(
        database.to_owned(),
        worker_runtime.wake.clone(),
    ));
    let report_wake = Arc::new(tokio::sync::Notify::new());
    {
        let wake = report_wake.clone();
        let hub = hub.clone();
        let client = client.clone();
        let commands = command_store.clone();
        let experiments = experiment_store.clone();
        let runtime = worker_runtime.clone();
        worker_runtime.tasks.spawn(async move {
            loop {
                wake.notified().await;
                if !*hub.link.connected.borrow() {
                    continue;
                }
                let mut processed = 0;
                let _ = drain_local_work(&client, &hub, &commands, &mut processed).await;
                let _ = drain_remote_work(
                    &client,
                    &hub,
                    &experiments,
                    &BTreeMap::new(),
                    &runtime,
                    &mut processed,
                )
                .await;
                let _ = upload_events(&client, &hub, &experiments).await;
            }
        });
    }
    for lane in 0..4 {
        let report_wake = report_wake.clone();
        let experiments = experiment_store.clone();
        let worker = worker_runtime.clone();
        let database = database.to_owned();
        worker_runtime.tasks.spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(match lane {0=>10,1=>2,2=>30,_=>15}));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                if lane == 0 {tokio::select! {_=ticker.tick()=>{},_=worker.wake.notified()=>{}}} else {ticker.tick().await;}
                let result:Result<()> = async {
                    match lane {
                        0 => {
                            report_wake.notify_one();
                            dispatch_work(&experiments,&worker).await
                        },
                        1 => {
                            let store=experiments.clone();
                            runtime_tasks::blocking(move|| {
                                let matchers=approved_project_sections(&store)?.into_iter().map(|(id,p)|(id,ProjectMatchers {success:p.success_patterns,failure:p.failure_patterns})).collect();
                                store.inspect(&matchers,unix_time()).map(|watches| {for watch in watches {info!(watch_id=%watch.watch_id,state=?watch.state,"experiment finished");}})
                            }).await?;
                            worker.wake.notify_one();Ok(())
                        },
                        2 => {discover_projects(&database,&worker.codex).await?;worker.wake.notify_one();Ok(())},
                        _ => worker.codex.warm().await,
                    }
                }.await;
                if let Err(error)=result {warn!(lane,%error,"Agent service cycle failed; retrying");}
            }
        });
    }
    info!(version=PRODUCT_VERSION,agent_id=%hub.agent_id,"FarHelm Agent is running");
    shutdown_signal().await;
    worker_runtime.tasks.shutdown().await;
    worker_runtime.codex.shutdown().await;
    info!("FarHelm Agent stopped");
    Ok(())
}

fn approved_project_sections(
    store: &ExperimentStore,
) -> Result<BTreeMap<String, config::ProjectSection>> {
    Ok(store
        .approved_projects()?
        .into_iter()
        .map(|(id, project)| {
            (
                id,
                config::ProjectSection {
                    path: project.path,
                    success_patterns: project.success_patterns,
                    failure_patterns: project.failure_patterns,
                },
            )
        })
        .collect())
}

async fn discover_projects(database: &Path, codex: &codex::Codex) -> Result<()> {
    let value = codex
        .call("codex.projects.discover", serde_json::json!({}))
        .await?;
    let database = database.to_owned();
    runtime_tasks::blocking(move || {
    let projects = value
        .get("projects")
        .and_then(serde_json::Value::as_array)
        .context("Worker project discovery omitted projects")?;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is missing")?;
    let home = fs::canonicalize(home).context("failed to resolve HOME")?;
    let uid = unsafe { libc::geteuid() };
    let store = ExperimentStore::open(&database)?;
    for project in projects {
        let Some(raw_path) = project.get("cwd").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let path = match fs::canonicalize(raw_path) {
            Ok(path) => path,
            Err(_) => continue,
        };
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if !metadata.is_dir() || metadata.uid() != uid || path == Path::new("/") || path == home {
            continue;
        }
        let Some(display_name) = path
            .file_name()
            .and_then(|value| value.to_str())
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let suggested = suggested_project_id(display_name);
        let session_count = project
            .get("session_count")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let now = unix_time();
        let (candidate, changed) =
            store.upsert_discovered_project(&path, display_name, &suggested, session_count, now)?;
        if changed {
            let event_type = if candidate.state == "approved" {
                "project.updated"
            } else {
                "project.discovered"
            };
            store.enqueue_event(
                &format!("project:{}:{}:{}",candidate.candidate_id,candidate.state,candidate.updated_at_unix),
                event_type,
                &serde_json::json!({"candidate_id":candidate.candidate_id,"display_name":candidate.display_name,"suggested_project_id":candidate.suggested_project_id,"session_count":candidate.session_count,"state":candidate.state,"updated_at_unix":candidate.updated_at_unix}),
                now,
            )?;
        }
    }
    let approved = store
        .approved_projects()?
        .into_iter()
        .filter_map(|(id, project)| fs::canonicalize(project.path).ok().map(|path| (path, id)))
        .collect::<BTreeMap<_, _>>();
    for session in value
        .get("sessions")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(raw_path) = session.get("cwd").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let Ok(path) = fs::canonicalize(raw_path) else {
            continue;
        };
        let Some(project_id) = approved.get(&path) else {
            continue;
        };
        let Some(session_id) = session
            .get("session_id")
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        let updated = session
            .get("updated_at_unix")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_else(unix_time);
        store.discover_session(
            session_id,
            project_id,
            &path,
            &session["title"],
            session["archived"].as_bool().unwrap_or(false),
            updated,
        )?;
    }
    Ok(())
    }).await
}

fn suggested_project_id(name: &str) -> String {
    let mut value = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    value = value.trim_matches('-').chars().take(64).collect();
    if value.is_empty() {
        "project".to_owned()
    } else {
        value
    }
}

async fn sync_project_sessions(
    database: &Path,
    codex: &codex::Codex,
    project_id: &str,
    project_path: &Path,
) -> Result<()> {
    let value = codex
        .call(
            "codex.sessions.list",
            serde_json::json!({"project_path":project_path,"archived":"all"}),
        )
        .await?;
    let database = database.to_owned();
    let project_id = project_id.to_owned();
    let project_path = project_path.to_owned();
    runtime_tasks::blocking(move || {
        let sessions = value
            .get("sessions")
            .and_then(serde_json::Value::as_array)
            .context("Worker session list omitted sessions")?;
        let store = ExperimentStore::open(&database)?;
        for session in sessions {
            let session_id = session
                .get("session_id")
                .and_then(serde_json::Value::as_str)
                .context("Worker returned a session without ID")?;
            let updated = session
                .get("updated_at_unix")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_else(unix_time);
            store.discover_session(
                session_id,
                &project_id,
                &project_path,
                &session["title"],
                session["archived"].as_bool().unwrap_or(false),
                updated,
            )?;
        }
        Ok(())
    })
    .await
}

fn load_local_config(path: Option<PathBuf>) -> Result<AgentFileConfig> {
    let path = match path {
        Some(path) => path,
        None => AgentPaths::discover()?.config,
    };
    AgentFileConfig::load(&path)
}

fn experiment_command(command: ExperimentCommand) -> Result<()> {
    match command {
        ExperimentCommand::Report {
            config,
            project,
            run_id,
            name,
            status,
            exit_code,
            mut message,
            session,
            on_success_prompt_file,
        } => {
            let config = load_local_config(config)?;
            ensure!(
                !(on_success_prompt_file.as_deref() == Some("-") && message == "-"),
                "message and prompt cannot share stdin"
            );
            if message == "-" {
                message.clear();
                std::io::stdin().take(2049).read_to_string(&mut message)?;
            }
            let store = ExperimentStore::open(&config.agent.database)?;
            store.import_config_projects(&config.projects, unix_time())?;
            let report = experiment_store::ScriptReport {
                agent_id: config.agent.id.clone(),
                project_id: project,
                run_id,
                name,
                status: status.unwrap_or_else(|| {
                    if exit_code == Some(0) {
                        "succeeded"
                    } else {
                        "failed"
                    }
                    .to_owned()
                }),
                message,
                session_id: session,
                prompt: on_success_prompt_file
                    .as_deref()
                    .map(read_prompt)
                    .transpose()?,
            };
            println!(
                "{}",
                serde_json::to_string(&store.report_experiment(&report, unix_time())?)?
            );
            Ok(())
        }
        ExperimentCommand::Watch {
            config,
            project,
            pid,
            log,
            name,
            session,
            new_session,
            on_success_prompt_file,
        } => {
            let config = load_local_config(config)?;
            let store = ExperimentStore::open(&config.agent.database)?;
            store.import_config_projects(&config.projects, unix_time())?;
            let approved = store.approved_projects()?;
            let mut project_config = approved.get(&project).cloned().with_context(|| {
                format!("project {project:?} is not approved; import it in the Console first")
            })?;
            if project_config.success_patterns.is_empty()
                || project_config.failure_patterns.is_empty()
            {
                ensure!(
                    std::io::stdin().is_terminal(),
                    "project {project:?} has no experiment log rules; run this command interactively once to set them"
                );
                eprintln!(
                    "Project {project:?} needs reliable log markers before experiment monitoring can be enabled."
                );
                let success = prompt_regex("Success log regex: ")?;
                let failure = prompt_regex("Failure log regex: ")?;
                store.set_project_matchers(
                    &project,
                    std::slice::from_ref(&success),
                    std::slice::from_ref(&failure),
                    unix_time(),
                )?;
                project_config.success_patterns = vec![success];
                project_config.failure_patterns = vec![failure];
                eprintln!("Saved project-specific experiment log rules locally.");
            }
            let prompt = on_success_prompt_file
                .as_deref()
                .map(read_prompt)
                .transpose()?;
            let watch = store.register(
                &WatchRegistration {
                    project_id: project,
                    project_root: project_config.path.clone(),
                    name: name.unwrap_or_else(|| format!("PID {pid}")),
                    pid,
                    log_path: log,
                    session_id: session,
                    new_session_mode: new_session,
                    success_prompt: prompt,
                },
                unix_time(),
            )?;
            println!(
                "{}\t{}\t{}\t{}",
                watch.watch_id,
                watch.project_id,
                watch.pid,
                state_label(watch.state)
            );
            Ok(())
        }
        ExperimentCommand::List { config } => {
            let config = load_local_config(config)?;
            for watch in ExperimentStore::open(&config.agent.database)?.list()? {
                println!(
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    watch.watch_id,
                    watch.project_id,
                    watch.pid,
                    state_label(watch.state),
                    watch
                        .new_session_mode
                        .as_deref()
                        .or(watch.session_id.as_deref())
                        .unwrap_or("-"),
                    watch.updated_at_unix,
                    watch.detail.as_deref().unwrap_or(&watch.name),
                );
            }
            Ok(())
        }
        ExperimentCommand::Unwatch { watch_id, config } => {
            let config = load_local_config(config)?;
            ensure!(
                ExperimentStore::open(&config.agent.database)?.cancel(&watch_id, unix_time())?,
                "watch is not active or does not exist"
            );
            println!("Cancelled {watch_id}; PID was not signaled");
            Ok(())
        }
    }
}

async fn codex_command(command: CodexCommand) -> Result<()> {
    match command {
        CodexCommand::Configure { config, bin } => {
            let path = config.unwrap_or(AgentPaths::discover()?.config);
            let mut config = AgentFileConfig::load(&path)?;
            let bin = codex::transport::discover(bin.as_deref())?;
            let version = codex::transport::version(&bin).await?;
            let native = codex::Codex::new(Some(bin.clone()));
            native.warm().await?;
            let state = native.status().state;
            native.shutdown().await;
            config.codex.bin = Some(bin.clone());
            farhelm_lifecycle::write_atomic(&path, config.encode()?.as_bytes(), 0o600)?;
            println!(
                "Codex configured: {} ({version}, {state}). Restart the Agent service to apply.",
                bin.display()
            );
            Ok(())
        }
        CodexCommand::Sessions { config, project } => {
            let config = load_local_config(config)?;
            let store = ExperimentStore::open(&config.agent.database)?;
            store.import_config_projects(&config.projects, unix_time())?;
            let projects = approved_project_sections(&store)?;
            let path =
                fs::canonicalize(&projects.get(&project).context("unapproved project")?.path)?;
            let native = codex::Codex::new(config.codex.bin);
            let result = native
                .call(
                    "codex.sessions.list",
                    serde_json::json!({"project_path":path,"archived":"all"}),
                )
                .await;
            native.shutdown().await;
            println!("{}", serde_json::to_string_pretty(&result?)?);
            Ok(())
        }
    }
}

fn read_prompt(path: &str) -> Result<String> {
    let mut prompt = String::new();
    if path == "-" {
        std::io::stdin()
            .take(32 * 1024 + 1)
            .read_to_string(&mut prompt)?;
    } else {
        prompt = fs::read_to_string(path)
            .with_context(|| format!("failed to read prompt file {path}"))?;
    }
    ensure!(prompt.len() <= 32 * 1024, "prompt exceeds 32 KiB");
    ensure!(!prompt.trim().is_empty(), "prompt is empty");
    Ok(prompt)
}

fn prompt_regex(prompt: &str) -> Result<String> {
    eprint!("{prompt}");
    let mut value = String::new();
    std::io::stdin().read_line(&mut value)?;
    let value = value.trim().to_owned();
    ensure!(!value.is_empty(), "log regex is empty");
    regex::Regex::new(&value).context("log regex is invalid")?;
    Ok(value)
}

async fn upload_events(client: &Client, hub: &HubArgs, store: &ExperimentStore) -> Result<()> {
    let agent = hub.agent_id.clone();
    let events = store
        .background(move |s| s.pending_events(&agent, 100))
        .await?;
    if events.is_empty() {
        return Ok(());
    }
    if hub.live_required {
        let value = hub
            .link
            .request(|request_id| farhelm_protocol::live::AgentFrame::Events {
                request_id,
                batch: AgentEventBatch {
                    protocol: FARHELM_PROTOCOL.into(),
                    agent_id: hub.agent_id.clone(),
                    events,
                },
            })
            .await?;
        let ack: AgentEventAck = serde_json::from_value(value)?;
        ensure!(
            ack.protocol == FARHELM_PROTOCOL,
            "Hub event protocol mismatch"
        );
        return store
            .background(move |s| s.acknowledge_events(&ack.accepted_event_ids))
            .await;
    }
    let response = client
        .post(hub_endpoint(&hub.hub, "/api/v1/agent/events")?)
        .bearer_auth(&hub.token)
        .json(&AgentEventBatch {
            protocol: FARHELM_PROTOCOL.to_owned(),
            agent_id: hub.agent_id.clone(),
            events,
        })
        .send()
        .await
        .context("failed to upload events")?
        .error_for_status()
        .context("Hub rejected event batch")?;
    let ack: AgentEventAck = response
        .json()
        .await
        .context("Hub returned an invalid event acknowledgement")?;
    ensure!(
        ack.protocol == FARHELM_PROTOCOL,
        "Hub event protocol mismatch"
    );
    store
        .background(move |s| s.acknowledge_events(&ack.accepted_event_ids))
        .await
}

const fn state_label(state: farhelm_protocol::ExperimentState) -> &'static str {
    use farhelm_protocol::ExperimentState::*;
    match state {
        Watching => "watching",
        Succeeded => "succeeded",
        Failed => "failed",
        Unknown => "unknown",
        Cancelled => "cancelled",
    }
}

async fn command_poll_once(runtime: &RuntimeArgs) -> Result<()> {
    let (client, _, _) = heartbeat_client(&runtime.hub)?;
    let store = CommandStore::open(&runtime.database)?;
    let experiments = ExperimentStore::open(&runtime.database)?;
    experiments.import_config_projects(&runtime.projects, unix_time())?;
    let live_projects = approved_project_sections(&experiments)?;
    let worker_runtime = CodexRuntime {
        tasks: runtime_tasks::RuntimeTasks::new(),
        codex: codex::Codex::new(runtime.codex_bin.clone()),
        link: runtime.hub.link.clone(),
        agent_id: runtime.hub.agent_id.clone(),
        wake: Arc::new(tokio::sync::Notify::new()),
    };
    let processed = process_command_cycle(
        &client,
        &runtime.hub,
        &store,
        &experiments,
        &live_projects,
        &worker_runtime,
    )
    .await?;
    println!("Command poll completed: {processed} command(s) processed");
    Ok(())
}

async fn process_command_cycle(
    client: &Client,
    hub: &HubArgs,
    store: &CommandStore,
    experiments: &ExperimentStore,
    projects: &BTreeMap<String, config::ProjectSection>,
    worker_runtime: &CodexRuntime,
) -> Result<u64> {
    let mut processed = 0;
    drain_local_work(client, hub, store, &mut processed).await?;
    drain_remote_work(
        client,
        hub,
        experiments,
        projects,
        worker_runtime,
        &mut processed,
    )
    .await?;

    let claim_url = hub_endpoint(&hub.hub, "/api/v1/agent/commands/claim")?;
    let response = client
        .post(claim_url)
        .bearer_auth(&hub.token)
        .json(&CommandClaimRequest {
            protocol: FARHELM_PROTOCOL.to_owned(),
            agent_id: hub.agent_id.clone(),
            wait_secs: Some(1),
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
        if command.action == CommandAction::AgentProbe {
            store.receive(&command, unix_time())?;
        } else {
            experiments.receive_remote_command(&command, unix_time())?;
        }
        drain_local_work(client, hub, store, &mut processed).await?;
        drain_remote_work(
            client,
            hub,
            experiments,
            projects,
            worker_runtime,
            &mut processed,
        )
        .await?;
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
        let Some(pending) = store.background(|s| s.next_work()).await? else {
            return Ok(());
        };
        if pending.state == CommandState::Accepted && unix_time() >= pending.expires_at_unix {
            {
                let id = pending.command_id.clone();
                store
                    .background(move |s| s.expire(&id, unix_time()))
                    .await?
            };
            continue;
        }
        if pending.state == CommandState::Accepted && pending.reported {
            let id = pending.command_id.clone();
            let result = ProbeResult {
                agent_version: PRODUCT_VERSION.to_owned(),
                hostname: resolve_hostname(hub.hostname.as_deref(), &hub.agent_id),
            };
            store
                .background(move |s| s.complete_probe(&id, &result, unix_time()))
                .await?;
            *processed += 1;
            continue;
        }
        let report = store.report(&pending, &hub.agent_id);
        send_command_report(client, hub, &report).await?;
        store
            .background(move |s| s.mark_reported(&pending.command_id, pending.state, unix_time()))
            .await?;
    }
    bail!("local command work exceeded the bounded cycle limit")
}

async fn drain_remote_work(
    client: &Client,
    hub: &HubArgs,
    store: &ExperimentStore,
    projects: &BTreeMap<String, config::ProjectSection>,
    worker_runtime: &CodexRuntime,
    processed: &mut u64,
) -> Result<()> {
    let _ = (projects, worker_runtime, processed);
    for command in store.background(|s| s.pending_remote_commands()).await? {
        if unix_time() >= command.expires_at_unix {
            {
                let id = command.command_id.clone();
                store
                    .background(move |s| s.expire_remote_command(&id, unix_time()))
                    .await?
            };
            continue;
        }
        if !command.accepted_reported {
            let report = farhelm_protocol::CommandReportRequest {
                protocol: FARHELM_PROTOCOL.to_owned(),
                agent_id: hub.agent_id.clone(),
                command_id: command.command_id.clone(),
                state: CommandState::Accepted,
                result: None,
                detail: None,
                data: None,
            };
            send_command_report(client, hub, &report).await?;
            store
                .background(move |s| {
                    s.mark_remote_accepted_reported(&command.command_id, unix_time())
                })
                .await?;
        }
    }
    for terminal in store.background(|s| s.pending_remote_reports()).await? {
        send_command_report(
            client,
            hub,
            &farhelm_protocol::CommandReportRequest {
                protocol: FARHELM_PROTOCOL.to_owned(),
                agent_id: hub.agent_id.clone(),
                command_id: terminal.command_id.clone(),
                state: terminal.state,
                result: None,
                detail: terminal.detail,
                data: terminal.data,
            },
        )
        .await?;
        store
            .background(move |s| s.mark_remote_terminal_reported(&terminal.command_id, unix_time()))
            .await?;
    }
    Ok(())
}

async fn dispatch_work(store: &ExperimentStore, worker: &CodexRuntime) -> Result<()> {
    let projects = store.background(approved_project_sections).await?;
    for command in store
        .background(|s| s.runnable_remote_commands(unix_time()))
        .await?
    {
        if unix_time() >= command.expires_at_unix {
            {
                let id = command.command_id.clone();
                store
                    .background(move |s| s.expire_remote_command(&id, unix_time()))
                    .await?
            };
            continue;
        }
        let control = command.action != CommandAction::CodexTurnStart;
        if !control
            && let Some(session) = command
                .payload
                .get("session_id")
                .and_then(serde_json::Value::as_str)
            && {
                let session = session.to_owned();
                store
                    .background(move |s| s.remote_session_busy(&session))
                    .await?
            }
        {
            continue;
        }
        let Some(permit) = worker.tasks.permit(control) else {
            continue;
        };
        let id = command.command_id.clone();
        if store
            .background(move |s| s.claim_remote_command(&id, unix_time()))
            .await?
        {
            let database = store.path().to_owned();
            let projects = projects.clone();
            let runtime = worker.clone();
            worker.tasks.spawn(async move {
                let _permit = permit;
                if let Err(error) =
                    execute_remote_command(&database, &projects, &runtime, command).await
                {
                    warn!(%error,"Codex command failed");
                }
            });
        }
    }
    for prompt in store
        .background(|s| s.pending_auto_prompts(unix_time()))
        .await?
    {
        let Some(permit) = worker.tasks.permit(false) else {
            break;
        };
        let id = prompt.watch_id.clone();
        if store.background(move |s| s.claim_auto_prompt(&id)).await? {
            let database = store.path().to_owned();
            let runtime = worker.clone();
            worker.tasks.spawn(async move {
                let _permit = permit;
                if let Err(error) = run_auto_prompt(&database, &runtime, prompt).await {
                    warn!(%error,"automatic Codex prompt failed");
                }
            });
        }
    }
    for prompt in store.background(|s| s.due_schedules(unix_time())).await? {
        let Some(permit) = worker.tasks.permit(false) else {
            break;
        };
        let id = prompt.schedule_id.clone();
        if store
            .background(move |s| s.claim_schedule(&id, unix_time()))
            .await?
        {
            let database = store.path().to_owned();
            let runtime = worker.clone();
            let project = projects.get(&prompt.project_id).cloned();
            worker.tasks.spawn(async move {
                let _permit = permit;
                if let Err(error) = run_scheduled_prompt(&database, &runtime, project, prompt).await
                {
                    warn!(%error,"scheduled Codex prompt failed");
                }
            });
        }
    }
    Ok(())
}

async fn execute_remote_command(
    database: &Path,
    projects: &BTreeMap<String, config::ProjectSection>,
    worker_runtime: &CodexRuntime,
    command: RemoteCommand,
) -> Result<()> {
    let store = ExperimentStore::open_async(database).await?;
    let outcome = execute_remote_command_inner(&store, projects, worker_runtime, &command).await;
    let (state, data, detail) = match outcome {
        Ok(data) => (CommandState::Completed, Some(data), None),
        Err(error) => {
            warn!(command_id = %command.command_id, %error, "local Codex operation failed");
            let event_type = if error.downcast_ref::<CodexTurnOrphaned>().is_some() {
                "codex.turn.orphaned"
            } else {
                "codex.turn.failed"
            };
            let error_detail =
                "Codex operation failed; inspect the local Agent diagnostics".to_owned();
            let status = if event_type == "codex.turn.orphaned" {
                "orphaned"
            } else {
                "failed"
            };
            if command.action == CommandAction::CodexTurnStart
                && let Some(session) = command
                    .payload
                    .get("session_id")
                    .and_then(serde_json::Value::as_str)
                && let Some(binding) = store.session_binding(session)?
            {
                store.enqueue_event(
                    &format!("{}:session-failed", command.command_id),
                    "codex.session.updated",
                    &serde_json::json!({"session_id":session,"project_id":binding.project_id,"mode":binding.mode,"state":status,"title":null,"active_turn_id":null,"updated_at_unix":unix_time()}),
                    unix_time(),
                )?;
            }
            (
                CommandState::Failed,
                Some(serde_json::json!({"status":status})),
                Some(error_detail),
            )
        }
    };
    let id = command.command_id.clone();
    store
        .background(move |s| {
            s.finish_remote_command(&id, state, data.as_ref(), detail.as_deref(), unix_time())
        })
        .await?;
    worker_runtime.wake.notify_one();
    Ok(())
}

async fn execute_remote_command_inner(
    store: &ExperimentStore,
    projects: &BTreeMap<String, config::ProjectSection>,
    worker_runtime: &CodexRuntime,
    command: &RemoteCommand,
) -> Result<serde_json::Value> {
    if command.action == CommandAction::ProjectApprove {
        let candidate_ids = command
            .payload
            .get("candidate_ids")
            .and_then(serde_json::Value::as_array)
            .context("project approval omitted candidate_ids")?
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .context("project candidate ID must be a string")
            })
            .collect::<Result<Vec<_>>>()?;
        let now = unix_time();
        let approved = store.approve_candidates(&candidate_ids, now)?;
        for project in &approved {
            store.enqueue_event(
                &format!("project:{}:approved:{now}", project.candidate_id),
                "project.updated",
                &serde_json::json!({"candidate_id":project.candidate_id,"display_name":project.display_name,"suggested_project_id":project.suggested_project_id,"session_count":project.session_count,"state":"approved","updated_at_unix":now}),
                now,
            )?;
            sync_project_sessions(
                store.path(),
                &worker_runtime.codex,
                &project.suggested_project_id,
                &project.path,
            )
            .await?;
        }
        return Ok(
            serde_json::json!({"approved": approved.iter().map(|project| project.candidate_id.as_str()).collect::<Vec<_>>() }),
        );
    }
    let project_id = command
        .payload
        .get("project_id")
        .and_then(serde_json::Value::as_str)
        .context("command omitted project_id")?;
    let project = projects
        .get(project_id)
        .context("command references an unapproved project")?;
    let project_root =
        fs::canonicalize(&project.path).context("failed to resolve approved project path")?;
    let mode = command
        .payload
        .get("mode")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("inspect");
    match command.action {
        CommandAction::CodexSessionCreate | CommandAction::CodexSessionResume => {
            let method = if command.action == CommandAction::CodexSessionCreate {
                "codex.session.start"
            } else {
                "codex.session.resume"
            };
            let cwd = if command.action == CommandAction::CodexSessionResume {
                let session_id = command
                    .payload
                    .get("session_id")
                    .and_then(serde_json::Value::as_str)
                    .context("resume command omitted session_id")?;
                if let Some(binding) = store.session_binding(session_id)? {
                    ensure!(
                        binding.project_id == project_id && binding.mode == mode,
                        "session binding does not match the requested project and mode"
                    );
                    if mode == "inspect" {
                        ensure!(
                            binding.cwd == project_root,
                            "inspect session cwd is outside the approved project"
                        );
                    } else {
                        ensure!(
                            binding
                                .cwd
                                .starts_with(AgentPaths::discover()?.data.join("worktrees")),
                            "edit session cwd is outside managed worktrees"
                        );
                    }
                    binding.cwd
                } else if mode == "edit" {
                    create_isolated_worktree(&project_root, &command.command_id).await?
                } else {
                    project_root.clone()
                }
            } else if mode == "edit" {
                create_isolated_worktree(&project_root, &command.command_id).await?
            } else {
                project_root.clone()
            };
            let value = worker_runtime.codex.call(method, serde_json::json!({"session_id":command.payload.get("session_id"),"cwd":cwd.clone(),"mode":mode})).await?;
            let session_id = value
                .get("session_id")
                .and_then(serde_json::Value::as_str)
                .context("Worker omitted session_id")?;
            let session_cwd = value
                .get("cwd")
                .and_then(serde_json::Value::as_str)
                .map(PathBuf::from)
                .unwrap_or(cwd);
            store.bind_session(session_id, project_id, &session_cwd, mode, unix_time())?;
            store.enqueue_event(&format!("{}:session",command.command_id),"codex.session.updated",&serde_json::json!({"session_id":session_id,"project_id":project_id,"mode":mode,"state":"idle","title":value.get("title"),"active_turn_id":null,"updated_at_unix":unix_time()}),unix_time())?;
            Ok(serde_json::json!({"session_id":session_id}))
        }
        CommandAction::CodexTurnStart => {
            let session_id = command
                .payload
                .get("session_id")
                .and_then(serde_json::Value::as_str)
                .context("command omitted session_id")?
                .to_owned();
            let prompt = command
                .payload
                .get("prompt")
                .and_then(serde_json::Value::as_str)
                .context("command omitted prompt")?
                .to_owned();
            let job = AutoPrompt {
                watch_id: command.command_id.clone(),
                project_id: project_id.to_owned(),
                project_root,
                session_id: Some(session_id.clone()),
                new_session_mode: None,
                prompt,
                idempotency_key: command.command_id.clone(),
            };
            let (_, turn_id) = run_auto_prompt_inner(store, worker_runtime, &job).await?;
            Ok(serde_json::json!({"session_id":session_id,"turn_id":turn_id}))
        }
        CommandAction::CodexTurnSteer | CommandAction::CodexTurnInterrupt => {
            let method = if command.action == CommandAction::CodexTurnSteer {
                "codex.turn.steer"
            } else {
                "codex.turn.interrupt"
            };
            worker_runtime
                .codex
                .call(method, command.payload.clone())
                .await
        }
        CommandAction::CodexScheduleCreate => {
            let schedule_id = store.create_schedule(&command.payload, unix_time())?;
            Ok(serde_json::json!({"schedule_id":schedule_id}))
        }
        CommandAction::CodexScheduleCancel => {
            let schedule_id = command
                .payload
                .get("schedule_id")
                .and_then(serde_json::Value::as_str)
                .context("cancel command omitted schedule_id")?;
            store.cancel_schedule(schedule_id, unix_time())?;
            Ok(serde_json::json!({"schedule_id":schedule_id,"cancelled":true}))
        }
        CommandAction::AgentProbe => bail!("probe reached Codex executor"),
        CommandAction::ProjectApprove => unreachable!("project approval returned above"),
    }
}

async fn run_scheduled_prompt(
    database: &Path,
    worker: &CodexRuntime,
    project: Option<config::ProjectSection>,
    prompt: ScheduledPrompt,
) -> Result<()> {
    let store = ExperimentStore::open_async(database).await?;
    let result = async {
        let project = project.context("scheduled project is no longer approved")?;
        let root =
            fs::canonicalize(project.path).context("scheduled project path is unavailable")?;
        let job = AutoPrompt {
            watch_id: prompt.schedule_id.clone(),
            project_id: prompt.project_id.clone(),
            project_root: root,
            session_id: Some(prompt.session_id),
            new_session_mode: None,
            prompt: prompt.prompt,
            idempotency_key: prompt.schedule_id.clone(),
        };
        run_auto_prompt_inner(&store, worker, &job).await
    }
    .await;
    let state = match &result {
        Ok(_) => farhelm_protocol::CodexScheduleState::Completed,
        Err(error) if error.downcast_ref::<CodexTurnOrphaned>().is_some() => {
            farhelm_protocol::CodexScheduleState::Orphaned
        }
        Err(_) => farhelm_protocol::CodexScheduleState::Failed,
    };
    store.finish_schedule(&prompt.schedule_id, state, unix_time())?;
    result.map(|_| ())
}

async fn send_command_report(
    client: &Client,
    hub: &HubArgs,
    report: &farhelm_protocol::CommandReportRequest,
) -> Result<()> {
    if hub.live_required {
        let frame = report.clone();
        let value = hub
            .link
            .request(
                |request_id| farhelm_protocol::live::AgentFrame::CommandReport {
                    request_id,
                    report: frame,
                },
            )
            .await?;
        let status: CommandStatusResponse = serde_json::from_value(value)?;
        ensure!(
            status.command_id == report.command_id && status.state == report.state,
            "Hub command receipt mismatch"
        );
        return Ok(());
    }
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

async fn run_auto_prompt(
    database: &Path,
    worker_runtime: &CodexRuntime,
    job: AutoPrompt,
) -> Result<()> {
    let store = ExperimentStore::open_async(database).await?;
    let result = run_auto_prompt_inner(&store, worker_runtime, &job).await;
    let now = unix_time();
    match result {
        Ok((session_id, turn_id)) => store.finish_auto_prompt(
            &job.watch_id,
            "codex.turn.completed",
            &serde_json::json!({
                "watch_id":job.watch_id,"project_id":job.project_id,"session_id":session_id,
                "turn_id":turn_id,"idempotency_key":job.idempotency_key,"status":"completed"
            }),
            now,
        ),
        Err(error) => {
            let detail = error.to_string();
            let event_type = if error.downcast_ref::<CodexTurnOrphaned>().is_some() {
                "codex.turn.orphaned"
            } else {
                "codex.turn.failed"
            };
            store.finish_auto_prompt(
                &job.watch_id,
                event_type,
                &serde_json::json!({
                    "watch_id":job.watch_id,"project_id":job.project_id,"session_id":job.session_id,
                    "idempotency_key":job.idempotency_key,"detail":detail
                }),
                now,
            )?;
            Err(error)
        }
    }
}

async fn run_auto_prompt_inner(
    store: &ExperimentStore,
    worker_runtime: &CodexRuntime,
    job: &AutoPrompt,
) -> Result<(String, String)> {
    let (cwd, mode) = if let Some(session_id) = &job.session_id {
        if let Some(binding) = store.session_binding(session_id)? {
            ensure!(
                binding.project_id == job.project_id,
                "session belongs to another project"
            );
            if binding.mode == "inspect" {
                ensure!(
                    binding.cwd == job.project_root,
                    "inspect session cwd is outside the approved project"
                );
            } else {
                let worktrees = AgentPaths::discover()?.data.join("worktrees");
                ensure!(
                    binding.cwd.starts_with(worktrees),
                    "edit session cwd is outside managed worktrees"
                );
            }
            (binding.cwd, binding.mode)
        } else {
            (job.project_root.clone(), "inspect".to_owned())
        }
    } else {
        match job.new_session_mode.as_deref() {
            Some("edit") => (
                create_isolated_worktree(&job.project_root, &job.watch_id).await?,
                "edit".to_owned(),
            ),
            Some("inspect") | None => (job.project_root.clone(), "inspect".to_owned()),
            Some(_) => bail!("invalid new-session mode"),
        }
    };
    let value = worker_runtime
        .codex
        .call(
            if job.session_id.is_some() {
                "codex.session.resume"
            } else {
                "codex.session.start"
            },
            serde_json::json!({"session_id":job.session_id,"cwd":cwd,"mode":mode}),
        )
        .await?;
    let session_id = value["session_id"]
        .as_str()
        .context("Codex session ID missing")?
        .to_owned();
    let session_cwd = PathBuf::from(value["cwd"].as_str().context("Codex cwd missing")?);
    store.bind_session(
        &session_id,
        &job.project_id,
        &session_cwd,
        &mode,
        unix_time(),
    )?;
    if job.session_id.is_none() {
        store.link_watch_session(&job.watch_id, &session_id, unix_time())?;
    }
    enqueue_session_state(store, job, &session_id, &mode, "queued", "session-ready")?;
    let sequence = std::sync::atomic::AtomicU64::new(0);
    let result = worker_runtime.codex.turn(&session_id, &job.prompt, &job.idempotency_key, |event_type, data| {
        let event_number = sequence.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let payload = serde_json::json!({"operation_id":job.watch_id,"watch_id":job.watch_id,"project_id":job.project_id,"session_id":session_id,"data":data});
        let session_id = &session_id;
        let mode = &mode;
        async move {
            if event_type == "codex.message.delta" {
                worker_runtime.link.delta(farhelm_protocol::AgentEvent {protocol:FARHELM_PROTOCOL.into(),sequence:0,event_id:format!("{}:delta:{event_number}",job.watch_id),agent_id:worker_runtime.agent_id.clone(),event_type:event_type.into(),payload,created_at_unix:unix_time()});
                return Ok(());
            }
            store.enqueue_event(&format!("{}:native:{event_number}",job.watch_id), event_type, &payload, unix_time())?;
            worker_runtime.wake.notify_one();
            if event_type == "codex.turn.started" {
                store.enqueue_event(&format!("{}:session-running",job.watch_id), "codex.session.updated", &serde_json::json!({"session_id":session_id,"project_id":job.project_id,"mode":mode,"state":"running","title":null,"active_turn_id":data["turn_id"],"updated_at_unix":unix_time()}),unix_time())?;
            }
            Ok(())
        }
    }).await;
    match result {
        Ok(turn_id) => {
            enqueue_session_state(store, job, &session_id, &mode, "idle", "session-idle")?;
            Ok((session_id, turn_id))
        }
        Err(error) => {
            if let Some((event, payload)) = store.recorded_turn(&job.watch_id)?
                && event == "codex.turn.completed"
            {
                let turn = payload.get("data").unwrap_or(&payload)["turn_id"]
                    .as_str()
                    .context("completed receipt missing ID")?
                    .to_owned();
                enqueue_session_state(store, job, &session_id, &mode, "idle", "session-idle")?;
                return Ok((session_id, turn));
            }
            let state = if error.downcast_ref::<CodexTurnOrphaned>().is_some() {
                "orphaned"
            } else {
                "failed"
            };
            enqueue_session_state(store, job, &session_id, &mode, state, "session-terminal")?;
            Err(error)
        }
    }
}

fn enqueue_session_state(
    store: &ExperimentStore,
    job: &AutoPrompt,
    session_id: &str,
    mode: &str,
    state: &str,
    event_suffix: &str,
) -> Result<()> {
    store.enqueue_event(
        &format!("{}:{event_suffix}", job.watch_id),
        "codex.session.updated",
        &serde_json::json!({
            "session_id":session_id,"project_id":job.project_id,"mode":mode,"state":state,
            "title":null,"active_turn_id":null,"updated_at_unix":unix_time()
        }),
        unix_time(),
    )
}

async fn create_isolated_worktree(project_root: &Path, identifier: &str) -> Result<PathBuf> {
    ensure!(
        !identifier.is_empty()
            && identifier
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')),
        "worktree identifier is invalid"
    );
    let worktree_root = AgentPaths::discover()?
        .data
        .join("worktrees")
        .join(identifier);
    let parent = worktree_root
        .parent()
        .context("worktree path has no parent")?;
    tokio::fs::create_dir_all(parent).await?;
    let status = Command::new("git")
        .arg("-C")
        .arg(project_root)
        .arg("worktree")
        .arg("add")
        .arg("--detach")
        .arg(&worktree_root)
        .arg("HEAD")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .status()
        .await
        .context("failed to create isolated Codex worktree")?;
    ensure!(
        status.success(),
        "git worktree creation failed with {status}"
    );
    Ok(worktree_root)
}

async fn read_request(
    hub: &HubArgs,
    worker: &CodexRuntime,
    store: &ExperimentStore,
    request: farhelm_protocol::AgentReadRequest,
) -> Result<AgentReadReportRequest> {
    let outcome = if request.method == "codex.session.history" {
        let session_id = request
            .params
            .get("session_id")
            .and_then(serde_json::Value::as_str)
            .context("history read omitted session ID")?;
        ensure!(
            {
                let id = session_id.to_owned();
                store
                    .background(move |s| s.session_binding(&id))
                    .await?
                    .is_some()
            },
            "history read references an unapproved session"
        );
        worker
            .codex
            .call(&request.method, request.params)
            .await
            .and_then(|value| {
                let page: farhelm_protocol::CodexTranscriptPage = serde_json::from_value(value)?;
                Ok(serde_json::to_value(page)?)
            })
    } else if request.method == "codex.session.display" {
        let ids: Option<Vec<String>> = request
            .params
            .get("session_ids")
            .map(|value| serde_json::from_value(value.clone()))
            .transpose()?;
        let project = request
            .params
            .get("project_id")
            .and_then(serde_json::Value::as_str);
        let project = project.map(str::to_owned);
        let bindings = store
            .background(move |s| s.display_bindings(project.as_deref(), ids.as_deref()))
            .await?;
        let mut params = request.params;
        params["bindings"] = bindings;
        params["agent_id"] = serde_json::json!(hub.agent_id);
        tokio::time::timeout(
            Duration::from_secs(18),
            worker.codex.call(&request.method, params),
        )
        .await
        .context("display read timed out")?
    } else if request.method == "codex.schedule.detail" {
        let id = request
            .params
            .get("schedule_id")
            .and_then(serde_json::Value::as_str)
            .context("schedule detail omitted ID")?;
        let id = id.to_owned();
        store.background(move |s| s.schedule_detail(&id)).await
    } else {
        bail!("Hub requested unsupported transient read")
    };
    let report = match outcome {
        Ok(data) => AgentReadReportRequest {
            protocol: FARHELM_PROTOCOL.to_owned(),
            agent_id: hub.agent_id.clone(),
            request_id: request.request_id.clone(),
            ok: true,
            data: Some(data),
            detail: None,
        },
        Err(error) => AgentReadReportRequest {
            protocol: FARHELM_PROTOCOL.to_owned(),
            agent_id: hub.agent_id.clone(),
            request_id: request.request_id.clone(),
            ok: false,
            data: None,
            detail: Some(
                if error.to_string().contains("not_configured") {
                    "codex_not_configured"
                } else {
                    "codex_read_failed"
                }
                .into(),
            ),
        },
    };
    Ok(report)
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
