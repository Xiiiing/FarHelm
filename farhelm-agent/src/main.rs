use std::{
    collections::{BTreeMap, HashMap},
    fs,
    io::{IsTerminal, Read},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Parser, Subcommand};
use farhelm_core::PRODUCT_VERSION;
use farhelm_protocol::{
    AgentEventAck, AgentEventBatch, AgentHeartbeat, AgentHeartbeatAck, CommandAction,
    CommandClaimRequest, CommandClaimResponse, CommandState, CommandStatusResponse,
    FARHELM_PROTOCOL, ProbeResult, WORKER_PROTOCOL, WorkerHelloResult, WorkerRequest,
    WorkerResponse, read_frame, write_frame,
};
use reqwest::{Client, Url};
use tokio::{
    process::{ChildStdin, Command},
    sync::{Mutex as AsyncMutex, oneshot},
    time::timeout,
};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

mod command_store;
mod config;
mod experiment_store;
mod management;
mod resources;

use command_store::CommandStore;
use config::{AgentFileConfig, AgentPaths};
use experiment_store::{
    AutoPrompt, ExperimentStore, ProjectMatchers, RemoteCommand, WatchRegistration,
};

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
    /// Install this executable, configuration, Worker resources, and user service.
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
    /// Confirm that the installed user service is active.
    Status,
    /// Check the installed configuration and local prerequisites.
    Doctor {
        #[arg(long, env = "FARHELM_AGENT_CONFIG")]
        config: Option<PathBuf>,
    },
    /// Pair this host with an Agent entry created in the Console.
    Pair,
    /// Start the Python Worker and verify the framed protocol handshake.
    WorkerSmoke {
        #[arg(long, default_value = "python3")]
        python: String,
        #[arg(long, default_value = "farhelm-worker-codex")]
        worker_root: PathBuf,
    },
    /// Check for or install an immutable official Agent release.
    #[command(visible_alias = "upgrade")]
    Update {
        /// Only report whether an update is available.
        #[arg(long)]
        check: bool,
        /// Install one exact formal version, such as V0.5.0.
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
    worker_python: String,
    worker_root: PathBuf,
}

type WorkerRegistry = Arc<Mutex<HashMap<String, ActiveWorker>>>;
type WorkerResult = std::result::Result<serde_json::Value, String>;

#[derive(Clone)]
struct ActiveWorker {
    stdin: Arc<AsyncMutex<ChildStdin>>,
    waiters: Arc<Mutex<HashMap<String, oneshot::Sender<WorkerResult>>>>,
}

struct ActiveWorkerRegistration {
    registry: WorkerRegistry,
    session_id: String,
}

#[derive(Debug)]
struct WorkerTurnOrphaned(String);

impl std::fmt::Display for WorkerTurnOrphaned {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for WorkerTurnOrphaned {}

#[derive(Clone)]
struct WorkerRuntime {
    python: String,
    root: PathBuf,
    registry: WorkerRegistry,
}

impl Drop for ActiveWorkerRegistration {
    fn drop(&mut self) {
        if let Ok(mut registry) = self.registry.lock() {
            registry.remove(&self.session_id);
        }
    }
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
            let mut runtime = resolve_runtime(connection, interval, command_interval, database)?;
            if let Ok(paths) = AgentPaths::discover() {
                runtime.worker_python =
                    resources::ensure_worker_environment(&paths.worker, &runtime.worker_python)
                        .await?
                        .to_string_lossy()
                        .into_owned();
            }
            run(
                runtime.hub,
                runtime.interval,
                runtime.command_interval,
                &runtime.database,
                &runtime.projects,
                &runtime.worker_python,
                &runtime.worker_root,
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
        CommandKind::WorkerSmoke {
            python,
            worker_root,
        } => worker_smoke(&python, &worker_root).await,
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
    let worker_python = config
        .as_ref()
        .map_or_else(|| "python3".to_owned(), |value| value.worker.python.clone());
    let worker_root = paths.map_or_else(
        || PathBuf::from("farhelm-worker-codex"),
        |value| value.worker,
    );
    Ok(RuntimeArgs {
        hub: HubArgs {
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
        worker_python,
        worker_root,
    })
}

async fn run(
    hub: HubArgs,
    interval_secs: u64,
    command_interval_secs: u64,
    database: &Path,
    projects: &BTreeMap<String, config::ProjectSection>,
    worker_python: &str,
    worker_root: &Path,
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
    let experiment_store = ExperimentStore::open(database)?;
    experiment_store.import_config_projects(projects, unix_time())?;
    let worker_runtime = WorkerRuntime {
        python: worker_python.to_owned(),
        root: worker_root.to_owned(),
        registry: WorkerRegistry::default(),
    };
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
    let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut command_ticker = tokio::time::interval(Duration::from_secs(command_interval_secs));
    command_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut session_ticker = tokio::time::interval(Duration::from_secs(60));
    session_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
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
                let live_projects = approved_project_sections(&experiment_store)?;
                if let Err(error) = process_command_cycle(&client, &hub, &command_store, &experiment_store, &live_projects, &worker_runtime).await {
                    warn!(agent_id = %hub.agent_id, %error, "command cycle failed; retrying");
                }
                let project_matchers = live_projects.iter().map(|(id, project)| (id.clone(), ProjectMatchers { success: project.success_patterns.clone(), failure: project.failure_patterns.clone() })).collect::<BTreeMap<_, _>>();
                match experiment_store.inspect(&project_matchers, unix_time()) {
                    Ok(completed) => for watch in completed {
                        info!(watch_id = %watch.watch_id, state = ?watch.state, "experiment finished");
                    },
                    Err(error) => warn!(%error, "experiment inspection failed; retrying"),
                }
                for prompt in experiment_store.pending_auto_prompts(unix_time())? {
                    if let Some(session_id) = prompt.session_id.as_deref()
                        && experiment_store.remote_session_busy(session_id)? { continue; }
                    if experiment_store.claim_auto_prompt(&prompt.watch_id)? {
                        let database = database.to_owned();
                        let worker_runtime = worker_runtime.clone();
                        tokio::spawn(async move {
                            if let Err(error) = run_auto_prompt(&database, &worker_runtime, prompt).await {
                                warn!(%error, "automatic Codex prompt failed");
                            }
                        });
                    }
                }
                if let Err(error) = upload_events(&client, &hub, &experiment_store).await {
                    warn!(agent_id = %hub.agent_id, %error, "event outbox upload failed; retrying");
                }
            }
            _ = session_ticker.tick() => {
                if let Err(error) = discover_projects(database, &worker_runtime.python, &worker_runtime.root).await {
                    warn!(%error, "Codex project discovery failed");
                }
                for (project_id, project) in approved_project_sections(&experiment_store)? {
                    let database = database.to_owned();
                    let python = worker_runtime.python.clone();
                    let worker_root = worker_runtime.root.clone();
                    let project_id = project_id.clone();
                    let project_path = match fs::canonicalize(&project.path) {
                        Ok(path) => path,
                        Err(error) => {
                            warn!(%error, project_id, "approved project path is unavailable");
                            continue;
                        }
                    };
                    tokio::spawn(async move {
                        if let Err(error) = sync_project_sessions(&database,&python,&worker_root,&project_id,&project_path).await {
                            warn!(%error, project_id, "Codex session sync failed");
                        }
                    });
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

async fn discover_projects(database: &Path, python: &str, worker_root: &Path) -> Result<()> {
    let value = worker_call_once(
        python,
        worker_root,
        "codex.projects.discover",
        serde_json::json!({}),
    )
    .await?;
    let projects = value
        .get("projects")
        .and_then(serde_json::Value::as_array)
        .context("Worker project discovery omitted projects")?;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is missing")?;
    let home = fs::canonicalize(home).context("failed to resolve HOME")?;
    let uid = unsafe { libc::geteuid() };
    let store = ExperimentStore::open(database)?;
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
    Ok(())
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
    python: &str,
    worker_root: &Path,
    project_id: &str,
    project_path: &Path,
) -> Result<()> {
    let value = worker_call_once(
        python,
        worker_root,
        "codex.sessions.list",
        serde_json::json!({"project_path":project_path,"archived":"all"}),
    )
    .await?;
    let sessions = value
        .get("sessions")
        .and_then(serde_json::Value::as_array)
        .context("Worker session list omitted sessions")?;
    let store = ExperimentStore::open(database)?;
    for session in sessions {
        let session_id = session
            .get("session_id")
            .and_then(serde_json::Value::as_str)
            .context("Worker returned a session without ID")?;
        let updated = session
            .get("updated_at_unix")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_else(unix_time);
        store.bind_session(session_id, project_id, project_path, "inspect", updated)?;
        store.enqueue_event(
            &format!("session-sync:{session_id}:{updated}"),"codex.session.updated",
            &serde_json::json!({"session_id":session_id,"project_id":project_id,"mode":"inspect","state":if session.get("archived").and_then(serde_json::Value::as_bool).unwrap_or(false) { "archived" } else { "idle" },"title":session.get("title"),"active_turn_id":null,"updated_at_unix":updated}),unix_time(),
        )?;
    }
    Ok(())
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
        CodexCommand::Sessions { config, project } => {
            let config = load_local_config(config)?;
            let store = ExperimentStore::open(&config.agent.database)?;
            store.import_config_projects(&config.projects, unix_time())?;
            let projects = store.approved_projects()?;
            let project = projects
                .get(&project)
                .context("project is not approved; import it in the Console first")?;
            let project_path = fs::canonicalize(&project.path)
                .context("failed to resolve approved project path")?;
            let worker_root = AgentPaths::discover()?.worker;
            resources::materialize_worker(&worker_root)?;
            let value = worker_call_once(
                &config.worker.python,
                &worker_root,
                "codex.sessions.list",
                serde_json::json!({"project_path":project_path,"archived":"false"}),
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&value)?);
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
    let events = store.pending_events(&hub.agent_id, 100)?;
    if events.is_empty() {
        return Ok(());
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
    store.acknowledge_events(&ack.accepted_event_ids)
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
    let worker_runtime = WorkerRuntime {
        python: runtime.worker_python.clone(),
        root: runtime.worker_root.clone(),
        registry: WorkerRegistry::default(),
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
    worker_runtime: &WorkerRuntime,
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

async fn drain_remote_work(
    client: &Client,
    hub: &HubArgs,
    store: &ExperimentStore,
    projects: &BTreeMap<String, config::ProjectSection>,
    worker_runtime: &WorkerRuntime,
    processed: &mut u64,
) -> Result<()> {
    for terminal in store.pending_remote_reports()? {
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
        store.mark_remote_terminal_reported(&terminal.command_id, unix_time())?;
    }
    for command in store.pending_remote_commands()? {
        if command.action == CommandAction::CodexTurnStart
            && let Some(session_id) = command
                .payload
                .get("session_id")
                .and_then(serde_json::Value::as_str)
            && store.remote_session_busy(session_id)?
        {
            continue;
        }
        if unix_time() >= command.expires_at_unix {
            let report = farhelm_protocol::CommandReportRequest {
                protocol: FARHELM_PROTOCOL.to_owned(),
                agent_id: hub.agent_id.clone(),
                command_id: command.command_id.clone(),
                state: CommandState::Expired,
                result: None,
                detail: None,
                data: None,
            };
            send_command_report(client, hub, &report).await?;
            store.expire_remote_command(&command.command_id, unix_time())?;
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
            store.mark_remote_accepted_reported(&command.command_id, unix_time())?;
        }
        if store.claim_remote_command(&command.command_id, unix_time())? {
            let client = client.clone();
            let hub = hub.clone();
            let database = store_path_for_task(store)?;
            let projects = projects.clone();
            let worker_runtime = worker_runtime.clone();
            tokio::spawn(async move {
                if let Err(error) = execute_remote_command(
                    &client,
                    &hub,
                    &database,
                    &projects,
                    &worker_runtime,
                    command,
                )
                .await
                {
                    warn!(%error, "Codex command execution failed");
                }
            });
            *processed += 1;
        }
    }
    Ok(())
}

fn store_path_for_task(_store: &ExperimentStore) -> Result<PathBuf> {
    Ok(_store.path().to_owned())
}

async fn execute_remote_command(
    client: &Client,
    hub: &HubArgs,
    database: &Path,
    projects: &BTreeMap<String, config::ProjectSection>,
    worker_runtime: &WorkerRuntime,
    command: RemoteCommand,
) -> Result<()> {
    let store = ExperimentStore::open(database)?;
    let outcome = execute_remote_command_inner(&store, projects, worker_runtime, &command).await;
    let (state, data, detail) = match outcome {
        Ok(data) => (CommandState::Completed, Some(data), None),
        Err(error) => {
            let event_type = if error.downcast_ref::<WorkerTurnOrphaned>().is_some() {
                "codex.turn.orphaned"
            } else {
                "codex.turn.failed"
            };
            let error_detail: String = error.to_string().chars().take(512).collect();
            if command.action != CommandAction::ProjectApprove {
                store.enqueue_event(
                    &format!("{}:terminal", command.command_id),
                    event_type,
                    &serde_json::json!({
                        "command_id":command.command_id,"project_id":command.payload.get("project_id"),
                        "session_id":command.payload.get("session_id"),"detail":error_detail.clone()
                    }),
                    unix_time(),
                )?;
            }
            (CommandState::Failed, None, Some(error_detail))
        }
    };
    store.finish_remote_command(
        &command.command_id,
        state,
        data.as_ref(),
        detail.as_deref(),
        unix_time(),
    )?;
    send_command_report(
        client,
        hub,
        &farhelm_protocol::CommandReportRequest {
            protocol: FARHELM_PROTOCOL.to_owned(),
            agent_id: hub.agent_id.clone(),
            command_id: command.command_id.clone(),
            state,
            result: None,
            detail,
            data,
        },
    )
    .await?;
    store.mark_remote_terminal_reported(&command.command_id, unix_time())
}

async fn execute_remote_command_inner(
    store: &ExperimentStore,
    projects: &BTreeMap<String, config::ProjectSection>,
    worker_runtime: &WorkerRuntime,
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
                &worker_runtime.python,
                &worker_runtime.root,
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
            let value = worker_call_once(&worker_runtime.python, &worker_runtime.root, method, serde_json::json!({"session_id":command.payload.get("session_id"),"cwd":cwd.clone(),"mode":mode})).await?;
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
            Ok(value)
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
            active_worker_request(
                &worker_runtime.registry,
                command
                    .payload
                    .get("session_id")
                    .and_then(serde_json::Value::as_str)
                    .context("control command omitted session_id")?,
                &command.command_id,
                method,
                command.payload.clone(),
            )
            .await
        }
        CommandAction::AgentProbe => bail!("probe reached Codex executor"),
        CommandAction::ProjectApprove => unreachable!("project approval returned above"),
    }
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

async fn run_auto_prompt(
    database: &Path,
    worker_runtime: &WorkerRuntime,
    job: AutoPrompt,
) -> Result<()> {
    let store = ExperimentStore::open(database)?;
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
            let event_type = if error.downcast_ref::<WorkerTurnOrphaned>().is_some() {
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
    worker_runtime: &WorkerRuntime,
    job: &AutoPrompt,
) -> Result<(String, String)> {
    let source_root = worker_runtime
        .root
        .join("src")
        .canonicalize()
        .with_context(|| {
            format!(
                "worker source not found below {}",
                worker_runtime.root.display()
            )
        })?;
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
    let mut child = Command::new(&worker_runtime.python)
        .arg("-m")
        .arg("farhelm_worker_codex")
        .env("PYTHONPATH", source_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("failed to start Worker with `{}`", worker_runtime.python))?;
    let mut stdin = child.stdin.take().context("Worker stdin was not piped")?;
    let mut stdout = child.stdout.take().context("Worker stdout was not piped")?;
    let session_method = if job.session_id.is_some() {
        "codex.session.resume"
    } else {
        "codex.session.start"
    };
    let session_request = WorkerRequest {
        protocol: WORKER_PROTOCOL.to_owned(),
        kind: "request".to_owned(),
        request_id: format!("session:{}", job.watch_id),
        method: session_method.to_owned(),
        params: serde_json::json!({"session_id":job.session_id,"cwd":cwd.clone(),"mode":mode}),
    };
    write_frame(&mut stdin, &session_request).await?;
    let session_response: WorkerResponse = read_frame(&mut stdout).await?;
    ensure!(
        session_response.ok,
        "Worker failed to prepare session: {:?}",
        session_response.error
    );
    let session_id = session_response
        .result
        .as_ref()
        .and_then(|value| value.get("session_id"))
        .and_then(serde_json::Value::as_str)
        .context("Worker session response omitted session_id")?
        .to_owned();
    let session_cwd = session_response
        .result
        .as_ref()
        .and_then(|value| value.get("cwd"))
        .and_then(serde_json::Value::as_str)
        .map(PathBuf::from)
        .unwrap_or(cwd);
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
    let active_worker = ActiveWorker {
        stdin: Arc::new(AsyncMutex::new(stdin)),
        waiters: Arc::new(Mutex::new(HashMap::new())),
    };
    let _registration =
        register_active_worker(&worker_runtime.registry, &session_id, &active_worker)?;
    store.enqueue_event(
        &format!("{}:session-ready", job.watch_id), "codex.session.updated",
        &serde_json::json!({"session_id":session_id,"project_id":job.project_id,"mode":mode,"state":"queued","title":null,"active_turn_id":null,"updated_at_unix":unix_time()}), unix_time(),
    )?;

    let turn_request = WorkerRequest {
        protocol: WORKER_PROTOCOL.to_owned(),
        kind: "request".to_owned(),
        request_id: format!("turn:{}", job.watch_id),
        method: "codex.turn.start".to_owned(),
        params: serde_json::json!({"session_id":session_id,"prompt":job.prompt,"idempotency_key":job.idempotency_key}),
    };
    let turn_result: Result<String> = async {
        {
            let mut stdin = active_worker.stdin.lock().await;
            write_frame(&mut *stdin, &turn_request).await?;
        }
        let mut event_number = 0_u64;
        loop {
            let frame: serde_json::Value = read_frame(&mut stdout).await.map_err(|error| {
                anyhow::Error::new(WorkerTurnOrphaned(format!(
                    "Worker exited or violated framing during turn: {error}"
                )))
            })?;
            match frame.get("kind").and_then(serde_json::Value::as_str) {
                Some("event") => {
                    event_number += 1;
                    let event_type = frame
                        .get("event")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("codex.event");
                    let data = frame
                        .get("data")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null);
                    store.enqueue_event(
                        &format!("{}:worker:{event_number}", job.watch_id), event_type,
                        &serde_json::json!({"watch_id":job.watch_id,"project_id":job.project_id,"session_id":session_id,"data":data}), unix_time(),
                    )?;
                    if event_type == "codex.turn.started" {
                        let turn_id = data.get("turn_id").and_then(serde_json::Value::as_str);
                        store.enqueue_event(
                            &format!("{}:session-running", job.watch_id), "codex.session.updated",
                            &serde_json::json!({"session_id":session_id,"project_id":job.project_id,"mode":mode,"state":"running","title":null,"active_turn_id":turn_id,"updated_at_unix":unix_time()}), unix_time(),
                        )?;
                    }
                }
                Some("response") => {
                    let response: WorkerResponse = serde_json::from_value(frame)?;
                    if response.request_id != turn_request.request_id {
                        complete_worker_waiter(&active_worker, response)?;
                        continue;
                    }
                    ensure!(response.ok, "Worker turn failed: {:?}", response.error);
                    return response
                        .result
                        .as_ref()
                        .and_then(|value| value.get("turn_id"))
                        .and_then(serde_json::Value::as_str)
                        .map(ToOwned::to_owned)
                        .context("Worker turn response omitted turn_id");
                }
                _ => bail!("Worker emitted an invalid frame"),
            }
        }
    }
    .await;
    match turn_result {
        Ok(turn_id) => {
            enqueue_session_state(store, job, &session_id, &mode, "idle", "session-idle")?;
            Ok((session_id, turn_id))
        }
        Err(error) => {
            let state = if error.downcast_ref::<WorkerTurnOrphaned>().is_some() {
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

fn register_active_worker(
    registry: &WorkerRegistry,
    session_id: &str,
    worker: &ActiveWorker,
) -> Result<ActiveWorkerRegistration> {
    let mut workers = registry
        .lock()
        .map_err(|_| anyhow::anyhow!("active Worker registry lock was poisoned"))?;
    ensure!(
        !workers.contains_key(session_id),
        "session already has an active Worker"
    );
    workers.insert(session_id.to_owned(), worker.clone());
    Ok(ActiveWorkerRegistration {
        registry: registry.clone(),
        session_id: session_id.to_owned(),
    })
}

fn complete_worker_waiter(worker: &ActiveWorker, response: WorkerResponse) -> Result<()> {
    let sender = worker
        .waiters
        .lock()
        .map_err(|_| anyhow::anyhow!("Worker response registry lock was poisoned"))?
        .remove(&response.request_id)
        .context("Worker returned an unknown response ID")?;
    let result = if response.ok {
        Ok(response.result.unwrap_or(serde_json::Value::Null))
    } else {
        let error = response.error.map_or_else(
            || "Worker rejected request".to_owned(),
            |error| format!("Worker {}: {}", error.code, error.message),
        );
        Err(error)
    };
    let _ = sender.send(result);
    Ok(())
}

async fn active_worker_request(
    registry: &WorkerRegistry,
    session_id: &str,
    request_id: &str,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value> {
    let worker = registry
        .lock()
        .map_err(|_| anyhow::anyhow!("active Worker registry lock was poisoned"))?
        .get(session_id)
        .cloned()
        .context("session has no active Worker; the turn may be orphaned")?;
    let request = WorkerRequest {
        protocol: WORKER_PROTOCOL.to_owned(),
        kind: "request".to_owned(),
        request_id: request_id.to_owned(),
        method: method.to_owned(),
        params,
    };
    let (sender, receiver) = oneshot::channel();
    worker
        .waiters
        .lock()
        .map_err(|_| anyhow::anyhow!("Worker response registry lock was poisoned"))?
        .insert(request_id.to_owned(), sender);
    let write_result = {
        let mut stdin = worker.stdin.lock().await;
        write_frame(&mut *stdin, &request).await
    };
    if let Err(error) = write_result {
        if let Ok(mut waiters) = worker.waiters.lock() {
            waiters.remove(request_id);
        }
        return Err(error.into());
    }
    timeout(Duration::from_secs(30), receiver)
        .await
        .context("Worker control request timed out")?
        .context("active Worker exited before replying")?
        .map_err(anyhow::Error::msg)
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

async fn worker_call_once(
    python: &str,
    worker_root: &Path,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value> {
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
    let request = WorkerRequest {
        protocol: WORKER_PROTOCOL.to_owned(),
        kind: "request".to_owned(),
        request_id: format!("req_{:016x}", unix_time()),
        method: method.to_owned(),
        params,
    };
    write_frame(&mut stdin, &request).await?;
    let response: WorkerResponse = timeout(Duration::from_secs(30), read_frame(&mut stdout))
        .await
        .context("Worker request timed out")??;
    ensure!(
        response.request_id == request.request_id,
        "Worker response ID mismatch"
    );
    if !response.ok {
        let error = response
            .error
            .context("Worker rejected request without error")?;
        bail!("Worker {}: {}", error.code, error.message);
    }
    response.result.context("Worker response omitted result")
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
