//! Local Codex is the transcript/authentication authority. No vendor body is a Hub record.
mod archive;
mod context;
mod events;
mod temporary;
pub use events::DisplayEvent;
mod handoff;
pub mod history;
mod models;
mod protocol;
#[cfg(test)]
mod tests;
pub mod transport;
use anyhow::{Context, Result, bail, ensure};
use futures_util::{
    FutureExt,
    future::{BoxFuture, WeakShared},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    future::Future,
    path::PathBuf,
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, RwLock, Semaphore, watch};
type SharedRead = BoxFuture<'static, std::result::Result<Value, String>>;
use protocol::*;
use transport::Connection;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodexStatus {
    pub state: String,
    pub version: Option<String>,
    pub reason: Option<String>,
}

#[derive(Default)]
struct Index {
    rows: HashMap<String, Value>,
    refreshed: Option<Instant>,
    // A complete empty snapshot is distinct from an index not yet initialized.
    snapshot_generation: u64,
    revision: u64,
    changed: HashMap<String, u64>,
}
struct Inner {
    display: tokio::sync::broadcast::Sender<DisplayEvent>,
    live_activity: RwLock<HashMap<String, Value>>,
    event_generation: std::sync::atomic::AtomicU64,
    bin: Option<PathBuf>,
    connection: Mutex<Option<Arc<Connection>>>,
    index: RwLock<Index>,
    refresh: Mutex<()>,
    reads: Arc<Semaphore>,
    inflight: Mutex<HashMap<String, WeakShared<SharedRead>>>,
    status: watch::Sender<CodexStatus>,
    loaded: RwLock<HashMap<String, LoadedThread>>,
    models: Mutex<Option<(Instant, farhelm_protocol::CodexModelList)>>,
    skills: RwLock<HashMap<String, NativeSkill>>,
    image_resources: RwLock<HashMap<String, NativeImage>>,
    activity: RwLock<()>,
    lifecycle: Mutex<()>,
}
struct LoadedThread {
    thread: Value,
    mode: String,
    has_turns: bool,
    context: farhelm_protocol::CodexSessionContext,
}
#[derive(Clone)]
struct NativeSkill {
    name: String,
    path: PathBuf,
    cwd: PathBuf,
}
#[derive(Clone)]
struct NativeImage {
    session: String,
    path: PathBuf,
    mime: String,
}
#[derive(Clone)]
pub struct Codex {
    inner: Arc<Inner>,
}

impl Codex {
    pub fn new(bin: Option<PathBuf>) -> Self {
        Self {
            inner: Arc::new(Inner {
                display: tokio::sync::broadcast::channel(4096).0,
                live_activity: RwLock::new(HashMap::new()),
                event_generation: std::sync::atomic::AtomicU64::new(0),
                bin,
                connection: Mutex::new(None),
                index: RwLock::new(Index::default()),
                refresh: Mutex::new(()),
                reads: Arc::new(Semaphore::new(4)),
                inflight: Mutex::new(HashMap::new()),
                loaded: RwLock::new(HashMap::new()),
                models: Mutex::new(None),
                skills: RwLock::new(HashMap::new()),
                image_resources: RwLock::new(HashMap::new()),
                activity: RwLock::new(()),
                lifecycle: Mutex::new(()),
                status: watch::channel(CodexStatus {
                    state: "starting".into(),
                    version: None,
                    reason: None,
                })
                .0,
            }),
        }
    }
    pub fn subscribe_display(&self) -> tokio::sync::broadcast::Receiver<DisplayEvent> {
        self.inner.display.subscribe()
    }
    pub fn status(&self) -> CodexStatus {
        self.inner.status.borrow().clone()
    }
    pub fn subscribe_status(&self) -> watch::Receiver<CodexStatus> {
        self.inner.status.subscribe()
    }
    pub async fn warm(&self) -> Result<()> {
        let _activity = self.activity().await;
        let connection = self.connection().await?;
        self.refresh_account(&connection).await
    }
    async fn refresh_account(&self, connection: &Connection) -> Result<()> {
        let result = connection
            .request("account/read", json!({"refreshToken":false}))
            .await
            .and_then(|value| {
                serde_json::from_value::<GetAccountResponse>(value).map_err(Into::into)
            });
        let mut status = self.status();
        match &result {
            Ok(account) => {
                let missing = account.requires_openai_auth && account.account.is_none();
                status.state = if missing { "login_required" } else { "ready" }.into();
                status.reason = missing.then(|| "codex_login_required".into());
            }
            _ => {
                status.state = "unavailable".into();
                status.reason = Some("codex_account_read_failed".into());
            }
        }
        let ready = status.state != "unavailable";
        self.inner.status.send_if_modified(|current| {
            if current == &status {
                false
            } else {
                *current = status;
                true
            }
        });
        ensure!(ready, "codex_account_read_failed");
        Ok(())
    }
    pub async fn shutdown(&self) {
        if let Some(connection) = self.inner.connection.lock().await.take() {
            connection.shutdown().await;
        }
    }

    pub async fn connection(&self) -> Result<Arc<Connection>> {
        let mut current = self.inner.connection.lock().await;
        if let Some(connection) = current.as_ref()
            && connection.alive.load(Ordering::Acquire)
        {
            return Ok(connection.clone());
        }
        let generation = self.inner.event_generation.fetch_add(1, Ordering::AcqRel) + 1;
        if let Some(old) = current.take() {
            old.shutdown().await;
        }
        {
            let mut loaded = self.inner.loaded.write().await;
            for (session, thread) in loaded.iter() {
                if thread.thread["ephemeral"] == true {
                    let _ = self.inner.display.send(DisplayEvent {
                        kind: "codex.temporary.closed",
                        session: session.clone(),
                        data: json!({}),
                    });
                }
            }
            loaded.clear();
        }
        self.inner.live_activity.write().await.clear();
        self.inner.skills.write().await.clear();
        self.inner.image_resources.write().await.clear();
        let result: Result<(Arc<Connection>, String)> = async {
            let explicit = self.inner.bin.clone();
            let bin = tokio::task::spawn_blocking(move || transport::discover(explicit.as_deref()))
                .await??;
            let version = transport::version(&bin).await?;
            let connection = Connection::open(&bin).await?;
            Ok((connection, version))
        }
        .await;
        match result {
            Ok((connection, version)) => {
                self.inner.status.send_replace(CodexStatus {
                    state: "starting".into(),
                    version: Some(version),
                    reason: None,
                });
                *current = Some(connection.clone());
                let weak = Arc::downgrade(&self.inner);
                let mut events = connection.events.subscribe();
                tokio::spawn(async move {
                    let mut projector = events::Projector::default();
                    loop {
                        let event = match events.recv().await {
                            Ok(e) => e,
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                if let Some(inner) = weak.upgrade() {
                                    let mut index = inner.index.write().await;
                                    index.refreshed = None;
                                    for session in index.rows.keys() {
                                        let _ = inner.display.send(DisplayEvent {
                                            kind: "codex.native.changed",
                                            session: session.clone(),
                                            data: json!({}),
                                        });
                                    }
                                }
                                continue;
                            }
                            Err(_) => break,
                        };
                        let Some(inner) = weak.upgrade() else { break };
                        if inner.event_generation.load(Ordering::Acquire) != generation {
                            break;
                        }
                        if event["method"] == "farhelm/disconnected" {
                            for (session, loaded) in inner.loaded.read().await.iter() {
                                if loaded.thread["ephemeral"] == true {
                                    let _ = inner.display.send(DisplayEvent {
                                        kind: "codex.temporary.closed",
                                        session: session.clone(),
                                        data: json!({}),
                                    });
                                }
                            }
                            inner.live_activity.write().await.clear();
                            let mut status = inner.status.borrow().clone();
                            status.state = "unavailable".into();
                            status.reason = Some("codex_connection_closed".into());
                            inner.status.send_replace(status);
                            break;
                        }
                        events::observe_activity(&inner, &event).await;
                        temporary::observe(&inner, &event).await;
                        projector.accept(&event, &inner.display);
                        let params = &event["params"];
                        if matches!(
                            event["method"].as_str(),
                            Some(
                                "turn/started"
                                    | "item/started"
                                    | "item/agentMessage/delta"
                                    | "turn/completed"
                            )
                        ) && let Some(id) = params["threadId"].as_str()
                            && let Some(loaded) = inner.loaded.write().await.get_mut(id)
                        {
                            loaded.has_turns = true;
                        }
                        if event["method"] == "thread/settings/updated"
                            && let Some(id) = params["threadId"].as_str()
                            && let Some(loaded) = inner.loaded.write().await.get_mut(id)
                        {
                            loaded.context =
                                context::project(&params["threadSettings"], Some(&loaded.context));
                        }
                        let mut index = inner.index.write().await;
                        if event["method"] == "thread/started"
                            && let Some(id) = params["thread"]["id"].as_str()
                        {
                            let mut row = params["thread"].clone();
                            row["archived"] = json!(false);
                            index.rows.insert(id.to_owned(), row);
                            index.revision += 1;
                            let revision = index.revision;
                            index.changed.insert(id.to_owned(), revision);
                        } else if let Some(id) = params["threadId"].as_str() {
                            if let Some(row) = index.rows.get_mut(id) {
                                match event["method"].as_str() {
                                    Some("thread/name/updated") => {
                                        row["name"] = params["threadName"].clone();
                                    }
                                    Some("thread/archived") => row["archived"] = json!(true),
                                    Some("thread/unarchived") => row["archived"] = json!(false),
                                    Some("thread/status/changed") => {
                                        row["status"] = params["status"].clone()
                                    }
                                    Some("turn/completed") => index.refreshed = None,
                                    _ => {}
                                }
                                index.revision += 1;
                                let revision = index.revision;
                                index.changed.insert(id.to_owned(), revision);
                            } else if event["method"]
                                .as_str()
                                .is_some_and(|m| m.starts_with("thread/"))
                            {
                                index.refreshed = None;
                            }
                        }
                    }
                });
                self.refresh_account(&connection).await?;
                Ok(connection)
            }
            Err(error) => {
                let code = error.to_string();
                let reason = code
                    .split(':')
                    .next()
                    .filter(|s| {
                        s.len() <= 128 && s.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
                    })
                    .unwrap_or("codex_unavailable")
                    .to_owned();
                self.inner.status.send_replace(CodexStatus {
                    state: "unavailable".into(),
                    version: None,
                    reason: Some(reason),
                });
                Err(error)
            }
        }
    }

    async fn index(&self, force: bool) -> Result<Vec<Value>> {
        let generation = {
            let index = self.inner.index.read().await;
            if !force
                && index
                    .refreshed
                    .is_some_and(|time| time.elapsed() < Duration::from_secs(30))
            {
                return Ok(index.rows.values().cloned().collect());
            }
            index.snapshot_generation
        };
        let _refresh = match self.inner.refresh.try_lock() {
            Ok(guard) => guard,
            Err(_) => {
                if !force {
                    let index = self.inner.index.read().await;
                    if index.snapshot_generation > 0 {
                        // Names/search use the last complete projection while the
                        // existing discovery lane prepares an atomic replacement.
                        return Ok(index.rows.values().cloned().collect());
                    }
                }
                self.inner.refresh.lock().await
            }
        };
        {
            let index = self.inner.index.read().await;
            if index.snapshot_generation != generation {
                // Another caller completed the requested scan while we waited.
                return Ok(index.rows.values().cloned().collect());
            }
        }
        let connection = self.connection().await?;
        let revision = self.inner.index.read().await.revision;
        let mut rows = HashMap::new();
        for archived in [false, true] {
            let mut cursor = Value::Null;
            loop {
                let result = connection
                    .request(
                        "thread/list",
                        serde_json::to_value(ThreadListParams {
                            archived,
                            limit: 100,
                            cursor: cursor.as_str().map(str::to_owned),
                            source_kinds: THREAD_SOURCES,
                        })?,
                    )
                    .await?;
                for raw in result["data"].as_array().context("codex_index_invalid")? {
                    if let Some(id) = raw["id"].as_str() {
                        let mut row = raw.clone();
                        row["archived"] = json!(archived);
                        rows.insert(id.to_owned(), row);
                    }
                }
                let next = result["nextCursor"].clone();
                if next.is_null() || next == cursor {
                    break;
                }
                ensure!(rows.len() < 100_000, "codex_index_capacity");
                cursor = next;
            }
        }
        let mut index = self.inner.index.write().await;
        // Preserve native changes that arrived while paginated listing was in flight.
        for (id, changed) in &index.changed {
            if *changed > revision
                && let Some(current) = index.rows.get(id)
            {
                if let Some(row) = rows.get_mut(id) {
                    for field in ["name", "status", "archived"] {
                        if let Some(value) = current.get(field) {
                            row[field] = value.clone();
                        }
                    }
                } else {
                    rows.insert(id.clone(), current.clone());
                }
            }
        }
        index.rows = rows;
        index.refreshed = Some(Instant::now());
        index.snapshot_generation += 1;
        index.changed.retain(|_, changed| *changed > revision);
        Ok(index.rows.values().cloned().collect())
    }

    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let _activity = self.activity().await;
        if !matches!(
            method,
            "codex.session.history"
                | "codex.session.display"
                | "codex.models.list"
                | "codex.session.native"
        ) {
            return self.call_inner(method, params).await;
        }
        let key = serde_json::to_string(&(method, &params))?;
        let future = {
            let mut inflight = self.inner.inflight.lock().await;
            inflight.retain(|_, value| value.upgrade().is_some());
            if let Some(future) = inflight.get(&key).and_then(WeakShared::upgrade) {
                future
            } else {
                ensure!(inflight.len() < 32, "codex_read_capacity");
                let native = self.clone();
                let method = method.to_owned();
                let future: SharedRead = async move {
                    native
                        .call_inner(&method, params)
                        .await
                        .map_err(|error| error.to_string())
                }
                .boxed();
                let future = future.shared();
                inflight.insert(key.clone(), future.downgrade().expect("live read future"));
                future
            }
        };
        let result = future.await;
        self.inner.inflight.lock().await.remove(&key);
        result.map_err(anyhow::Error::msg)
    }
    async fn call_inner(&self, method: &str, params: Value) -> Result<Value> {
        let _permit = self.inner.reads.acquire().await?;
        match method {
            "codex.models.list" => Ok(serde_json::to_value(self.model_list().await?)?),
            "codex.session.native" => {
                let id = params["session_id"]
                    .as_str()
                    .context("session_id_missing")?;
                let value = self
                    .connection()
                    .await?
                    .request("thread/read", json!({"threadId":id,"includeTurns":false}))
                    .await?;
                let thread = &value["thread"];
                ensure!(thread["id"] == id, "session_identity_mismatch");
                let persisted = thread["ephemeral"] != true
                    && thread["path"]
                        .as_str()
                        .is_some_and(|p| std::path::Path::new(p).is_file());
                let native_name = thread["name"]
                    .as_str()
                    .filter(|s| s.chars().count() <= 128)
                    .map(history::redact_paths);
                Ok(serde_json::to_value(
                    farhelm_protocol::CodexNativeIdentity {
                        session_id: id.into(),
                        persisted,
                        held_by_agent: self.inner.loaded.read().await.contains_key(id),
                        native_name,
                        source: thread["source"]
                            .as_str()
                            .filter(|s| {
                                matches!(*s, "cli" | "vscode" | "exec" | "appServer" | "unknown")
                            })
                            .map(str::to_owned),
                        history_mode: thread["historyMode"]
                            .as_str()
                            .filter(|s| matches!(*s, "legacy" | "paginated"))
                            .map(str::to_owned),
                    },
                )?)
            }
            "codex.session.rename" => {
                let id = params["session_id"]
                    .as_str()
                    .context("session_id_missing")?;
                let name = params["name"].as_str().context("session_name_missing")?;
                ensure!(
                    farhelm_protocol::valid_session_name(name),
                    "invalid_session_name"
                );
                self.connection()
                    .await?
                    .request("thread/name/set", json!({"threadId":id,"name":name}))
                    .await?;
                let mut index = self.inner.index.write().await;
                index.revision += 1;
                let revision = index.revision;
                index.changed.insert(id.into(), revision);
                if let Some(row) = index.rows.get_mut(id) {
                    row["name"] = json!(name);
                }
                if let Some(loaded) = self.inner.loaded.write().await.get_mut(id) {
                    loaded.thread["name"] = json!(name);
                }
                Ok(json!({"session_id":id}))
            }
            "codex.native.read" => self.native_read(&params).await,
            "codex.projects.discover" => {
                let rows = self.index(true).await?;
                let mut projects = BTreeMap::<String, Value>::new();
                let mut sessions = Vec::new();
                for row in rows {
                    let Some(cwd) = row["cwd"]
                        .as_str()
                        .filter(|s| std::path::Path::new(s).is_absolute())
                    else {
                        continue;
                    };
                    let archived = row["archived"].as_bool().unwrap_or(false);
                    let updated = row["updatedAt"].as_u64().unwrap_or(0);
                    let project = projects.entry(cwd.into()).or_insert_with(
                        || json!({"cwd":cwd,"session_count":0,"updated_at_unix":0}),
                    );
                    project["session_count"] =
                        json!(project["session_count"].as_u64().unwrap_or(0) + 1);
                    project["updated_at_unix"] = json!(
                        project["updated_at_unix"]
                            .as_u64()
                            .unwrap_or(0)
                            .max(updated)
                    );
                    sessions.push(json!({"session_id":row["id"],"title":history::formal_title(&row["name"]),"cwd":cwd,"archived":archived,"updated_at_unix":updated}));
                }
                Ok(
                    json!({"projects":projects.into_values().collect::<Vec<_>>(),"sessions":sessions}),
                )
            }
            "codex.sessions.list" => {
                let rows = self.index(false).await?;
                let archived = params["archived"].as_str().unwrap_or("false");
                let sessions = rows
                    .iter()
                    .filter(|row| {
                        row["cwd"] == params["project_path"]
                            && (archived == "all"
                                || row["archived"].as_bool() == Some(archived == "true"))
                    })
                    .map(thread_result)
                    .collect::<Vec<_>>();
                Ok(json!({"sessions":sessions,"next_cursor":null}))
            }
            "codex.session.display" => Ok(display_page(&self.index(false).await?, &params)),
            "codex.session.history" => {
                let session = params["session_id"]
                    .as_str()
                    .context("history_session_missing")?
                    .to_owned();
                let (upstream, resume) =
                    history::decode_cursor(&session, params["cursor"].as_str())?;
                let connection = self.connection().await?;
                if let Some(page) = self.temporary_history(&session, &resume).await? {
                    return Ok(page);
                }
                if let Some(loaded) = self.inner.loaded.read().await.get(&session)
                    && !loaded.has_turns
                {
                    return Ok(
                        json!({"session_id":session,"turns":[],"next_cursor":null,"context":loaded.context}),
                    );
                }
                let limit = params["limit"].as_u64().unwrap_or(20).clamp(1, 50) as usize;
                let legacy = self
                    .status()
                    .version
                    .as_deref()
                    .and_then(|v| semver::Version::parse(v).ok())
                    .is_some_and(|v| v < semver::Version::new(0, 153, 0));
                let latest = params["cursor"].is_null();
                let (turns, next, settings) = if legacy {
                    let offset = upstream
                        .as_deref()
                        .map(|cursor| {
                            cursor
                                .strip_prefix("legacy:")
                                .context("invalid_history_cursor")?
                                .parse::<usize>()
                                .context("invalid_history_cursor")
                        })
                        .transpose()?
                        .unwrap_or(0);
                    let result = connection
                        .request(
                            "thread/read",
                            serde_json::to_value(ThreadReadParams {
                                thread_id: &session,
                                include_turns: true,
                            })?,
                        )
                        .await?;
                    let data = result["thread"]["turns"]
                        .as_array()
                        .context("codex_history_invalid")?;
                    ensure!(offset <= data.len(), "history_changed_reload");
                    let next =
                        (offset + limit < data.len()).then(|| format!("legacy:{}", offset + limit));
                    let loaded = self.inner.loaded.read().await;
                    let settings = latest.then(|| {
                        context::project(
                            &result["thread"],
                            loaded.get(&session).map(|thread| &thread.context),
                        )
                    });
                    (
                        data.iter()
                            .rev()
                            .skip(offset)
                            .take(limit)
                            .cloned()
                            .collect::<Vec<_>>(),
                        next,
                        settings,
                    )
                } else {
                    // The metadata RPC is independent of the history page and never resumes a thread.
                    // Continuations need no settings read and cannot overwrite the latest context.
                    let metadata = async {
                        if !latest {
                            return Ok(Value::Null);
                        }
                        tokio::time::timeout(
                            Duration::from_millis(250),
                            connection.request(
                                "thread/read",
                                serde_json::to_value(ThreadReadParams {
                                    thread_id: &session,
                                    include_turns: false,
                                })?,
                            ),
                        )
                        .await
                        .context("codex_context_timeout")?
                    };
                    let (result, metadata) = tokio::join!(
                        connection.request(
                            "thread/turns/list",
                            serde_json::to_value(ThreadTurnsListParams {
                                thread_id: &session,
                                cursor: upstream.as_deref(),
                                limit: limit as u32,
                                items_view: "full",
                            })?,
                        ),
                        metadata
                    );
                    let result: ThreadPage =
                        serde_json::from_value(result?).context("codex_history_invalid")?;
                    let loaded = self.inner.loaded.read().await;
                    let metadata = metadata.unwrap_or(Value::Null);
                    let settings = latest.then(|| {
                        context::project(
                            &metadata["thread"],
                            loaded.get(&session).map(|thread| &thread.context),
                        )
                    });
                    (result.data, result.next_cursor, settings)
                };
                let page_session = session.clone();
                let mut page = crate::runtime_tasks::blocking(move || {
                    history::bounded_page(
                        &page_session,
                        &turns,
                        upstream.as_deref(),
                        next.as_deref(),
                        &resume,
                    )
                })
                .await?;
                self.register_native_images(&session, &mut page).await?;
                if let Some(settings) = settings {
                    page["context"] = serde_json::to_value(settings)?;
                }
                Ok(page)
            }
            "codex.session.start" | "codex.session.resume" => {
                let connection = self.connection().await?;
                let mode = params["mode"].as_str().unwrap_or("inspect");
                ensure!(matches!(mode, "inspect" | "edit"), "invalid_session_mode");
                if method == "codex.session.resume"
                    && let Some(id) = params["session_id"].as_str()
                    && let Some(loaded) = self.inner.loaded.read().await.get(id)
                {
                    ensure!(
                        loaded.thread["cwd"] == params["cwd"] && loaded.mode == mode,
                        "session_project_mismatch"
                    );
                    return Ok(thread_result(&loaded.thread));
                }
                let mut request = json!({"cwd":params["cwd"]});
                if method == "codex.session.start" {
                    request["ephemeral"] = json!(params["ephemeral"] == true);
                }
                // Resuming belongs to Codex: explicit overrides here would erase the user's
                // persisted permission profile and approval policy. Legacy explicit creation
                // still supports inspect/edit; the Console opts into native project defaults.
                if method == "codex.session.start" && params["inherit_permissions"] != true {
                    request["sandbox"] = json!(if mode == "edit" {
                        "workspace-write"
                    } else {
                        "read-only"
                    });
                    request["approvalPolicy"] = json!("on-request");
                }
                if method == "codex.session.resume" {
                    request["threadId"] = params["session_id"].clone();
                    if self
                        .status()
                        .version
                        .as_deref()
                        .and_then(|v| semver::Version::parse(v).ok())
                        .is_some_and(|v| v >= semver::Version::new(0, 153, 0))
                    {
                        request["excludeTurns"] = json!(true);
                    }
                }
                let response = connection
                    .request(
                        if method == "codex.session.start" {
                            "thread/start"
                        } else {
                            "thread/resume"
                        },
                        request,
                    )
                    .await?;
                let thread = &response["thread"];
                ensure!(thread["cwd"] == params["cwd"], "session_project_mismatch");
                let mut row = thread.clone();
                row["archived"] = json!(false);
                if let Some(id) = row["id"].as_str() {
                    let id = id.to_owned();
                    self.inner.loaded.write().await.insert(
                        id.clone(),
                        LoadedThread {
                            thread: row.clone(),
                            mode: mode.to_owned(),
                            has_turns: method == "codex.session.resume",
                            context: context::project(&response, None),
                        },
                    );
                    let mut index = self.inner.index.write().await;
                    index.revision += 1;
                    let revision = index.revision;
                    index.changed.insert(id.clone(), revision);
                    index.rows.insert(id, row);
                }
                Ok(thread_result(thread))
            }
            "codex.turn.steer" => {
                self.connection()
                    .await?
                    .request(
                        "turn/steer",
                        serde_json::to_value(TurnSteerParams {
                            thread_id: params["session_id"]
                                .as_str()
                                .context("session_id_missing")?,
                            expected_turn_id: params["turn_id"]
                                .as_str()
                                .context("turn_id_missing")?,
                            input: [UserInput::Text {
                                text: params["prompt"].as_str().context("prompt_missing")?,
                            }],
                        })?,
                    )
                    .await
            }
            "codex.turn.interrupt" => {
                self.connection()
                    .await?
                    .request(
                        "turn/interrupt",
                        serde_json::to_value(TurnInterruptParams {
                            thread_id: params["session_id"]
                                .as_str()
                                .context("session_id_missing")?,
                            turn_id: params["turn_id"].as_str().context("turn_id_missing")?,
                        })?,
                    )
                    .await
            }
            _ => bail!("unsupported_codex_method"),
        }
    }

    pub async fn native_operation(
        &self,
        session: &str,
        operation: &farhelm_protocol::native::NativeOperation,
        attachments: &HashMap<String, PathBuf>,
        mode: &str,
    ) -> Result<Value> {
        use farhelm_protocol::native::{GoalStatus, NativeOperation, ReviewTarget};
        ensure!(operation.is_valid(), "invalid_native_operation");
        let _activity = self.activity().await;
        let _lifecycle = self.inner.lifecycle.lock().await;
        let connection = self.connection().await?;
        if matches!(
            operation,
            NativeOperation::Revert { .. }
                | NativeOperation::Compact
                | NativeOperation::Review { .. }
                | NativeOperation::GoalSet {
                    status: None | Some(GoalStatus::Active),
                    ..
                }
        ) {
            ensure!(
                connection.pending_server_requests(session).is_empty(),
                "codex_archive_busy"
            );
            let goal = connection
                .request("thread/goal/get", json!({"threadId":session}))
                .await?;
            ensure!(goal["goal"]["status"] != "active", "codex_archive_busy");
            let terminals = connection
                .request(
                    "thread/backgroundTerminals/list",
                    json!({"threadId":session,"limit":1}),
                )
                .await?;
            ensure!(
                terminals["data"].as_array().is_some_and(Vec::is_empty)
                    && terminals["nextCursor"].is_null(),
                "codex_archive_busy"
            );
            let live = connection
                .request(
                    "thread/read",
                    json!({"threadId":session,"includeTurns":false}),
                )
                .await?;
            ensure!(
                matches!(
                    live["thread"]["status"]["type"].as_str(),
                    Some("idle" | "notLoaded")
                ),
                "codex_archive_busy"
            );
            let queue = connection
                .request("thread/queue/list", json!({"threadId":session}))
                .await?;
            ensure!(
                queue["data"].as_array().is_some_and(Vec::is_empty),
                "codex_archive_busy"
            );
            if matches!(
                operation,
                NativeOperation::Revert { .. } | NativeOperation::GoalSet { .. }
            ) {
                ensure!(
                    live["thread"]["ephemeral"] == false
                        && live["thread"]["path"]
                            .as_str()
                            .is_some_and(|p| std::path::Path::new(p).is_file()),
                    "codex_archive_unsaved"
                );
            }
        }
        let revision_check = |value: &Value, expected: &str| -> Result<()> {
            ensure!(
                native_revision(value) == expected,
                "native_snapshot_conflict"
            );
            Ok(())
        };
        match operation {
            NativeOperation::QueueAdd { input, client_message_id } => {
                let input = self.native_inputs(session, input, attachments).await?;
                match connection.request("thread/queue/add", json!({"threadId":session,"input":input,"clientUserMessageId":client_message_id})).await {
                    Ok(_) => Ok(json!({"status":"accepted","client_message_id":client_message_id})),
                    Err(error) if error.to_string().contains("codex_request_rejected") || error.to_string().contains("codex_ephemeral_queue_unsupported") => Err(error),
                    Err(_) => bail!("native_queue_reconciling"),
                }
            }
            NativeOperation::QueueUpdate { submission_id, input, revision } => {
                let queue = connection.request("thread/queue/list", json!({"threadId":session})).await?;
                revision_check(&queue, revision)?;
                connection.request("thread/queue/update", json!({"threadId":session,"queuedSubmissionId":submission_id,"input":self.native_inputs(session, input, attachments).await?})).await
            }
            NativeOperation::QueueDelete { submission_id, revision } => {
                let queue = connection.request("thread/queue/list", json!({"threadId":session})).await?;
                revision_check(&queue, revision)?;
                connection.request("thread/queue/delete", json!({"threadId":session,"queuedSubmissionId":submission_id})).await
            }
            NativeOperation::QueueReorder { submission_ids, revision } => {
                let queue = connection.request("thread/queue/list", json!({"threadId":session})).await?;
                revision_check(&queue, revision)?;
                connection.request("thread/queue/reorder", json!({"threadId":session,"queuedSubmissionIds":submission_ids})).await
            }
            NativeOperation::QueueStart { submission_id, revision } => {
                let queue = connection.request("thread/queue/list", json!({"threadId":session})).await?;
                revision_check(&queue, revision)?;
                connection.request("thread/queue/start", json!({"threadId":session,"queuedSubmissionId":submission_id})).await
            }
            NativeOperation::SectionCreate { name } => connection.request("threadSection/create", json!({"name":name})).await,
            NativeOperation::SectionRename { section_id, name } => connection.request("threadSection/update", json!({"sectionId":section_id,"name":name})).await,
            NativeOperation::SectionDelete { section_id } => connection.request("threadSection/delete", json!({"sectionId":section_id})).await,
            NativeOperation::SectionMove { section_id, before_thread_id } => connection.request("thread/section/move", json!({"threadId":session,"sectionId":section_id,"beforeThreadId":before_thread_id})).await,
            NativeOperation::Fork { last_turn_id, ephemeral } => {
                if *ephemeral { self.check_temporary_capacity().await?; }
                let result = connection.request("thread/fork", json!({"threadId":session,"lastTurnId":last_turn_id,"ephemeral":ephemeral,"excludeTurns":true})).await?;
                if *ephemeral { self.register_temporary(&result["thread"], mode).await?; }
                Ok(result)
            },
            NativeOperation::Revert { before_turn_id } => connection.request("thread/revert", json!({"threadId":session,"beforeTurnId":before_turn_id})).await,
            NativeOperation::Delete { .. } => connection.request("thread/delete", json!({"threadId":session})).await,
            NativeOperation::TemporaryStart => bail!("temporary_start_misrouted"),
            NativeOperation::TemporaryEnd => {
                self.end_temporary(&connection, session).await
            }
            NativeOperation::Pin { pinned } => {
                let current = connection.request("thread/read", json!({"threadId":session,"includeTurns":false})).await?;
                ensure!(current["thread"]["isPinned"].is_boolean(), "codex_pin_upgrade_required");
                let result = connection.request("thread/metadata/update", json!({"threadId":session,"isPinned":pinned})).await?;
                ensure!(result["thread"]["isPinned"] == *pinned, "codex_pin_unverified");
                self.inner.index.write().await.refreshed = None;
                Ok(json!({"session_id":session,"is_pinned":pinned}))
            }
            NativeOperation::Compact => connection.request("thread/compact/start", json!({"threadId":session})).await,
            NativeOperation::GoalSet { objective, status, token_budget } => {
                let status = status.map(|s| match s { GoalStatus::Active=>"active",GoalStatus::Paused=>"paused",GoalStatus::Blocked=>"blocked",GoalStatus::UsageLimited=>"usageLimited",GoalStatus::BudgetLimited=>"budgetLimited",GoalStatus::Complete=>"complete" });
                connection.request("thread/goal/set", json!({"threadId":session,"objective":objective,"status":status,"tokenBudget":token_budget})).await
            }
            NativeOperation::GoalClear => connection.request("thread/goal/clear", json!({"threadId":session})).await,
            NativeOperation::ThreadSettings { settings } => connection.request("thread/settings/update", settings_params(session, None, settings)).await,
            NativeOperation::TurnSettings { turn_id, settings } => connection.request("turn/settings/update", settings_params(session, Some(turn_id), settings)).await,
            NativeOperation::Review { target } => {
                let target = match target { ReviewTarget::UncommittedChanges=>json!({"type":"uncommittedChanges"}),ReviewTarget::BaseBranch{branch}=>json!({"type":"baseBranch","branch":branch}),ReviewTarget::Commit{sha}=>json!({"type":"commit","sha":sha}) };
                connection.request("review/start", json!({"threadId":session,"target":target})).await
            }
            NativeOperation::InteractionAnswer { request_token, answer } => connection.answer_server_request(session, request_token, answer.clone()).await,
            NativeOperation::AttachmentBegin { .. } | NativeOperation::AttachmentChunk { .. } | NativeOperation::AttachmentFinish { .. } => bail!("attachment_operation_misrouted"),
        }
    }

    pub async fn reconcile_native_queue(&self, session: &str, message: &str) -> Result<bool> {
        let c = self.connection().await?;
        let queue = c
            .request("thread/queue/list", json!({"threadId":session}))
            .await?;
        let matches = queue["data"]
            .as_array()
            .context("native_queue_unverified")?
            .iter()
            .filter(|item| item["clientUserMessageId"] == message)
            .count();
        if matches > 0 {
            return Ok(matches == 1);
        }
        let mut cursor = Value::Null;
        for _ in 0..5 {
            let page = c
                .request(
                    "thread/turns/list",
                    json!({"threadId":session,"cursor":cursor,"limit":100,"itemsView":"full"}),
                )
                .await?;
            let turns = page["data"]
                .as_array()
                .context("native_history_unverified")?;
            if turns
                .iter()
                .filter_map(|turn| turn["items"].as_array())
                .flatten()
                .any(|item| item["type"] == "userMessage" && item["clientId"] == message)
            {
                return Ok(true);
            }
            cursor = page["nextCursor"].clone();
            if cursor.is_null() {
                break;
            }
        }
        Ok(false)
    }

    async fn native_read(&self, params: &Value) -> Result<Value> {
        let session = params["session_id"]
            .as_str()
            .context("session_id_missing")?;
        let connection = self.connection().await?;
        let value = match params["kind"]
            .as_str()
            .context("native_read_kind_missing")?
        {
            "activity" => {
                return Ok(
                    json!({"data":self.inner.live_activity.read().await.get(session).cloned().unwrap_or_else(|| json!({}))}),
                );
            }
            "queue" => {
                connection
                    .request("thread/queue/list", json!({"threadId":session}))
                    .await?
            }
            "sections" => {
                connection
                    .request("threadSection/list", json!({"limit":100}))
                    .await?
            }
            "goal" => {
                connection
                    .request("thread/goal/get", json!({"threadId":session}))
                    .await?
            }
            "skills" => return self.native_skills(&connection, params).await,
            "settings" => {
                let thread = connection
                    .request(
                        "thread/read",
                        json!({"threadId":session,"includeTurns":false}),
                    )
                    .await?;
                let modes = connection
                    .request("collaborationMode/list", json!({}))
                    .await?;
                let models = connection
                    .request("model/list", json!({"includeHidden":false}))
                    .await?;
                json!({"thread":thread,"modes":modes["data"],"models":models["data"]})
            }
            "permissions" => {
                connection
                    .request(
                        "permissionProfile/list",
                        json!({"limit":100,"cwd":params["cwd"]}),
                    )
                    .await?
            }
            "pending_interactions" => {
                return Ok(json!({"data":connection.pending_server_requests(session)}));
            }
            "image_resource" => return self.native_image_resource(session, params).await,
            _ => bail!("invalid_native_read_kind"),
        };
        Ok(json!({"revision":native_revision(&value),"data":value}))
    }

    async fn register_native_images(&self, session: &str, page: &mut Value) -> Result<()> {
        let cwd = self
            .inner
            .loaded
            .read()
            .await
            .get(session)
            .and_then(|loaded| loaded.thread["cwd"].as_str().map(PathBuf::from));
        let mut additions = Vec::new();
        for turn in page["turns"].as_array_mut().into_iter().flatten() {
            for item in turn["items"].as_array_mut().into_iter().flatten() {
                let Some(raw) = item
                    .as_object_mut()
                    .and_then(|map| map.remove("native_image_path"))
                else {
                    continue;
                };
                // The App Server path is private even when the session was evicted
                // between the native history read and this projection pass.
                let Some(cwd) = cwd.as_ref() else { continue };
                let Some(raw) = raw.as_str() else { continue };
                let Ok(path) = std::fs::canonicalize(raw) else {
                    continue;
                };
                if !path.starts_with(cwd)
                    || !path.is_file()
                    || std::fs::metadata(&path)?.len() > 20 * 1024 * 1024
                {
                    continue;
                }
                let mime = match path
                    .extension()
                    .and_then(|value| value.to_str())
                    .map(str::to_ascii_lowercase)
                    .as_deref()
                {
                    Some("png") => "image/png",
                    Some("jpg") | Some("jpeg") => "image/jpeg",
                    Some("webp") => "image/webp",
                    _ => continue,
                }
                .to_owned();
                let id = format!("img_{:032x}", rand::random::<u128>());
                item["image_resource_id"] = json!(id);
                item["mime_type"] = json!(mime);
                additions.push((
                    id,
                    NativeImage {
                        session: session.to_owned(),
                        path,
                        mime,
                    },
                ));
            }
        }
        let mut resources = self.inner.image_resources.write().await;
        if resources.len().saturating_add(additions.len()) > 4096 {
            resources.clear();
        }
        resources.extend(additions);
        Ok(())
    }

    async fn native_image_resource(&self, session: &str, params: &Value) -> Result<Value> {
        use base64::Engine;
        use std::io::{Read, Seek, SeekFrom};
        let id = params["resource_id"]
            .as_str()
            .context("image_resource_missing")?;
        let offset = params["offset"].as_u64().unwrap_or(0);
        let image = self
            .inner
            .image_resources
            .read()
            .await
            .get(id)
            .cloned()
            .context("image_resource_expired")?;
        ensure!(image.session == session, "image_resource_session_mismatch");
        let mut file = std::fs::File::open(&image.path)?;
        let size = file.metadata()?.len();
        ensure!(
            offset <= size && size <= 20 * 1024 * 1024,
            "image_resource_invalid"
        );
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = vec![0; 256 * 1024];
        let count = file.read(&mut bytes)?;
        bytes.truncate(count);
        Ok(
            json!({"resource_id":id,"offset":offset,"next_offset":offset+count as u64,"eof":offset+count as u64>=size,"mime_type":image.mime,"data_base64":base64::engine::general_purpose::STANDARD.encode(bytes)}),
        )
    }

    async fn native_skills(&self, connection: &Connection, params: &Value) -> Result<Value> {
        let cwd = PathBuf::from(params["cwd"].as_str().context("cwd_missing")?);
        let mut value = connection.request("skills/list", json!({"cwds":[cwd],"forceReload":params["force_reload"].as_bool().unwrap_or(false)})).await?;
        let mut resolved = Vec::new();
        for entry in value["data"]
            .as_array_mut()
            .context("skills_list_invalid")?
        {
            for skill in entry["skills"]
                .as_array_mut()
                .context("skills_list_invalid")?
            {
                let path = PathBuf::from(skill["path"].as_str().context("skill_path_missing")?);
                let name = skill["name"]
                    .as_str()
                    .context("skill_name_missing")?
                    .to_owned();
                let id = format!("skill_{:032x}", rand::random::<u128>());
                skill
                    .as_object_mut()
                    .context("skill_invalid")?
                    .remove("path");
                skill["skillId"] = json!(id);
                resolved.push((
                    id,
                    NativeSkill {
                        name,
                        path,
                        cwd: cwd.clone(),
                    },
                ));
            }
        }
        let mut skills = self.inner.skills.write().await;
        if skills.len().saturating_add(resolved.len()) > 4096 {
            skills.clear();
        }
        skills.extend(resolved);
        Ok(json!({"revision":native_revision(&value),"data":value}))
    }

    async fn native_inputs(
        &self,
        session: &str,
        input: &[farhelm_protocol::native::NativeInput],
        attachments: &HashMap<String, PathBuf>,
    ) -> Result<Vec<Value>> {
        use farhelm_protocol::native::NativeInput;
        let cwd = self
            .inner
            .loaded
            .read()
            .await
            .get(session)
            .and_then(|loaded| loaded.thread["cwd"].as_str().map(PathBuf::from))
            .context("session_not_loaded")?;
        let skills = self.inner.skills.read().await;
        input.iter().map(|item| match item {
            NativeInput::Text { text } => Ok(json!({"type":"text","text":text})),
            NativeInput::Skill { skill_id } => {
                let skill = skills.get(skill_id).context("skill_not_resolved")?;
                ensure!(skill.cwd == cwd, "skill_project_mismatch");
                Ok(json!({"type":"skill","name":skill.name,"path":skill.path}))
            }
            NativeInput::Image { attachment_id } => Ok(json!({"type":"localImage","path":attachments.get(attachment_id).context("attachment_not_resolved")?})),
        }).collect()
    }

    #[cfg(test)]
    pub async fn turn<F, Fut>(
        &self,
        session: &str,
        prompt: &str,
        operation: &str,
        emit: F,
    ) -> Result<String>
    where
        F: FnMut(&'static str, Value) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        self.turn_with_model(session, prompt, operation, None, emit)
            .await
    }

    pub async fn model_list(&self) -> Result<farhelm_protocol::CodexModelList> {
        let mut cached = self.inner.models.lock().await;
        if let Some((time, models)) = &*cached
            && time.elapsed() < Duration::from_secs(30)
        {
            return Ok(models.clone());
        }
        let connection = self.connection().await?;
        let models = tokio::time::timeout(Duration::from_secs(15), models::list(&connection))
            .await
            .context("codex_models_timeout")??;
        *cached = Some((Instant::now(), models.clone()));
        Ok(models)
    }

    pub async fn turn_with_model<F, Fut>(
        &self,
        session: &str,
        prompt: &str,
        operation: &str,
        choice: Option<&farhelm_protocol::CodexModelChoice>,
        mut emit: F,
    ) -> Result<String>
    where
        F: FnMut(&'static str, Value) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        let _activity = self.activity().await;
        if let Some(choice) = choice {
            ensure!(choice.is_valid(), "invalid_model_choice");
            let models = self.model_list().await?;
            ensure!(
                models.models.iter().any(|m| m.model == choice.model
                    && m.reasoning_efforts.contains(&choice.reasoning_effort)),
                "model_choice_unavailable"
            );
        }
        let connection = self.connection().await?;
        let mut events = connection.events.subscribe();
        if let Some(loaded) = self.inner.loaded.write().await.get_mut(session) {
            loaded.has_turns = true;
        }
        let response = connection
            .request(
                "turn/start",
                serde_json::to_value(TurnStartParams {
                    thread_id: session,
                    input: [UserInput::Text { text: prompt }],
                    client_user_message_id: operation,
                    model: choice.map(|c| c.model.as_str()),
                    effort: choice.map(|c| c.reasoning_effort.as_str()),
                })?,
            )
            .await
            .map_err(|error| anyhow::Error::new(crate::CodexTurnOrphaned(error.to_string())))?;
        let turn = response["turn"]["id"]
            .as_str()
            .context("turn_id_missing")?
            .to_owned();
        emit(
            "codex.turn.started",
            json!({"session_id":session,"turn_id":turn}),
        )
        .await?;
        loop {
            let event = events.recv().await.map_err(|_| {
                anyhow::Error::new(crate::CodexTurnOrphaned("codex_event_stream_lost".into()))
            })?;
            if event["method"] == "farhelm/disconnected" {
                return Err(crate::CodexTurnOrphaned("codex_connection_closed".into()).into());
            }
            let data = &event["params"];
            if data["threadId"] != session
                || data["turnId"]
                    .as_str()
                    .or_else(|| data["turn"]["id"].as_str())
                    != Some(turn.as_str())
            {
                continue;
            }
            if event["method"] == "turn/completed" {
                let status = data["turn"]["status"].as_str().unwrap_or("unknown");
                emit(
                    if status == "completed" {
                        "codex.turn.completed"
                    } else {
                        "codex.turn.failed"
                    },
                    json!({"session_id":session,"turn_id":turn,"status":status}),
                )
                .await?;
                ensure!(status == "completed", "codex_turn_failed");
                return Ok(turn);
            }
        }
    }
}

fn thread_result(row: &Value) -> Value {
    json!({"session_id":row["id"],"cwd":row["cwd"],"title":history::formal_title(&row["name"]),"archived":row["archived"],"updated_at_unix":row["updatedAt"],"created_at_unix":row["createdAt"]})
}

fn native_revision(value: &Value) -> String {
    Sha256::digest(serde_json::to_vec(value).unwrap_or_default())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn settings_params(
    session: &str,
    turn: Option<&String>,
    settings: &farhelm_protocol::native::NativeSettings,
) -> Value {
    let mut value = json!({"threadId":session});
    if let Some(turn) = turn {
        value["turnId"] = json!(turn);
    }
    if let Some(model) = &settings.model {
        value["model"] = json!(model);
    }
    if let Some(effort) = &settings.reasoning_effort {
        value["effort"] = json!(effort);
    }
    if turn.is_none() {
        if let Some(policy) = &settings.approval_policy {
            value["approvalPolicy"] = json!(policy);
        }
        if let Some(profile) = &settings.permissions {
            value["permissions"] = json!(profile);
        }
        if let Some(mode) = &settings.collaboration_mode {
            value["collaborationMode"] = json!({"mode":mode,"settings":{"model":settings.model.as_deref().unwrap_or("default"),"reasoning_effort":settings.reasoning_effort,"developer_instructions":null}});
        }
    }
    if let Some(reviewer) = &settings.approvals_reviewer {
        value["approvalsReviewer"] = json!(reviewer);
    }
    if let Some(tier) = &settings.service_tier {
        value["serviceTier"] = json!(tier);
    }
    value
}

fn display_page(rows: &[Value], params: &Value) -> Value {
    let query = params["query"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    let archived = params["archived"].as_str().unwrap_or("false");
    let agent = params["agent_id"].as_str().unwrap_or_default();
    let after = &params["after"];
    let mut matches=rows.iter().filter_map(|row|{
        let id=row["id"].as_str()?;let binding=&params["bindings"][id];
        if binding.is_null() || row["cwd"]!=binding["cwd"] || (archived!="all" && row["archived"].as_bool()!=Some(archived=="true")) {return None;}
        let title=history::formal_title(&row["name"]);let preview=row["preview"].as_str().unwrap_or_default();
        if !query.is_empty() && !title.as_deref().unwrap_or_default().to_lowercase().contains(&query) && !preview.to_lowercase().contains(&query) {return None;}
        let updated=row["updatedAt"].as_u64().unwrap_or(0);
        if let Some(at)=after["updated_at_unix"].as_u64() && (std::cmp::Reverse(updated),agent,id)<=(std::cmp::Reverse(at),after["agent_id"].as_str().unwrap_or_default(),after["session_id"].as_str().unwrap_or_default()) {return None;}
        let label=title.unwrap_or_else(||history::redact_paths(preview).split_whitespace().collect::<Vec<_>>().join(" ").chars().take(80).collect());
        Some(json!({"session_id":id,"project_id":binding["project_id"],"display_label":if label.is_empty() {Value::Null} else {json!(label)},"is_pinned":row["isPinned"],"section_id":row["section"]["id"],"section_name":row["section"]["name"],"updated_at_unix":updated}))
    }).collect::<Vec<_>>();
    matches.sort_by(|a, b| {
        b["updated_at_unix"]
            .as_u64()
            .cmp(&a["updated_at_unix"].as_u64())
            .then_with(|| a["session_id"].as_str().cmp(&b["session_id"].as_str()))
    });
    let more = matches.len() > 50;
    matches.truncate(51);
    json!({"sessions":matches,"has_more":more})
}
