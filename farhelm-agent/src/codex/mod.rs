//! Local Codex is the transcript/authentication authority. No vendor body is a Hub record.
pub mod history;
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
    bin: Option<PathBuf>,
    connection: Mutex<Option<Arc<Connection>>>,
    index: RwLock<Index>,
    refresh: Mutex<()>,
    reads: Arc<Semaphore>,
    inflight: Mutex<HashMap<String, WeakShared<SharedRead>>>,
    status: watch::Sender<CodexStatus>,
    loaded: RwLock<HashMap<String, LoadedThread>>,
}
struct LoadedThread {
    thread: Value,
    mode: String,
    has_turns: bool,
}
#[derive(Clone)]
pub struct Codex {
    inner: Arc<Inner>,
}

impl Codex {
    pub fn new(bin: Option<PathBuf>) -> Self {
        Self {
            inner: Arc::new(Inner {
                bin,
                connection: Mutex::new(None),
                index: RwLock::new(Index::default()),
                refresh: Mutex::new(()),
                reads: Arc::new(Semaphore::new(4)),
                inflight: Mutex::new(HashMap::new()),
                loaded: RwLock::new(HashMap::new()),
                status: watch::channel(CodexStatus {
                    state: "starting".into(),
                    version: None,
                    reason: None,
                })
                .0,
            }),
        }
    }
    pub fn status(&self) -> CodexStatus {
        self.inner.status.borrow().clone()
    }
    pub fn subscribe_status(&self) -> watch::Receiver<CodexStatus> {
        self.inner.status.subscribe()
    }
    pub async fn warm(&self) -> Result<()> {
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
        if let Some(old) = current.take() {
            old.shutdown().await;
        }
        self.inner.loaded.write().await.clear();
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
                    loop {
                        let event = match events.recv().await {
                            Ok(e) => e,
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                if let Some(inner) = weak.upgrade() {
                                    inner.index.write().await.refreshed = None;
                                }
                                continue;
                            }
                            Err(_) => break,
                        };
                        let Some(inner) = weak.upgrade() else { break };
                        if event["method"] == "farhelm/disconnected" {
                            let mut status = inner.status.borrow().clone();
                            status.state = "unavailable".into();
                            status.reason = Some("codex_connection_closed".into());
                            inner.status.send_replace(status);
                            break;
                        }
                        let params = &event["params"];
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
                            source_kinds: ["cli", "vscode", "exec", "appServer", "unknown"],
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
        if !matches!(method, "codex.session.history" | "codex.session.display") {
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
                if let Some(loaded) = self.inner.loaded.read().await.get(&session)
                    && !loaded.has_turns
                {
                    return Ok(json!({"session_id":session,"turns":[],"next_cursor":null}));
                }
                let limit = params["limit"].as_u64().unwrap_or(20).clamp(1, 50) as usize;
                let legacy = self
                    .status()
                    .version
                    .as_deref()
                    .and_then(|v| semver::Version::parse(v).ok())
                    .is_some_and(|v| v < semver::Version::new(0, 153, 0));
                let (turns, next) = if legacy {
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
                    (
                        data.iter()
                            .rev()
                            .skip(offset)
                            .take(limit)
                            .cloned()
                            .collect::<Vec<_>>(),
                        next,
                    )
                } else {
                    let result: ThreadPage = serde_json::from_value(
                        connection
                            .request(
                                "thread/turns/list",
                                serde_json::to_value(ThreadTurnsListParams {
                                    thread_id: &session,
                                    cursor: upstream.as_deref(),
                                    limit: limit as u32,
                                    items_view: "full",
                                })?,
                            )
                            .await?,
                    )
                    .context("codex_history_invalid")?;
                    (result.data, result.next_cursor)
                };
                crate::runtime_tasks::blocking(move || {
                    history::bounded_page(
                        &session,
                        &turns,
                        upstream.as_deref(),
                        next.as_deref(),
                        &resume,
                    )
                })
                .await
            }
            "codex.session.start" | "codex.session.resume" => {
                let connection = self.connection().await?;
                let mode = params["mode"].as_str().unwrap_or("inspect");
                let sandbox = match mode {
                    "inspect" => "read-only",
                    "edit" => "workspace-write",
                    _ => bail!("invalid_session_mode"),
                };
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
                let mut request =
                    json!({"cwd":params["cwd"],"sandbox":sandbox,"approvalPolicy":"on-request"});
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
                if method == "codex.session.start" {
                    connection.request("thread/name/set",json!({"threadId":thread["id"],"name":history::formal_title(&thread["name"]).unwrap_or_else(||"Codex session".into())})).await?;
                }
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
                        },
                    );
                    self.inner.index.write().await.rows.insert(id, row);
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

    pub async fn turn<F, Fut>(
        &self,
        session: &str,
        prompt: &str,
        operation: &str,
        mut emit: F,
    ) -> Result<String>
    where
        F: FnMut(&'static str, Value) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
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
        let mut buffers = HashMap::<String, history::StreamText>::new();
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
            if event["method"] == "item/agentMessage/delta" {
                if let (Some(item), Some(delta)) = (data["itemId"].as_str(), data["delta"].as_str())
                {
                    let (offset, text) = buffers
                        .entry(item.to_owned())
                        .or_default()
                        .feed(delta, false);
                    if !text.is_empty() {
                        emit("codex.message.delta",json!({"session_id":session,"turn_id":turn,"item_id":item,"text_offset":offset,"delta":text})).await?;
                    }
                }
            } else if event["method"] == "turn/completed" {
                for (item, buffer) in &mut buffers {
                    let (offset, text) = buffer.feed("", true);
                    if !text.is_empty() {
                        emit("codex.message.delta",json!({"session_id":session,"turn_id":turn,"item_id":item,"text_offset":offset,"delta":text})).await?;
                    }
                }
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
        Some(json!({"session_id":id,"project_id":binding["project_id"],"display_label":if label.is_empty() {Value::Null} else {json!(label)},"updated_at_unix":updated}))
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
