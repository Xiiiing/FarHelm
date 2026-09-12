//! The official app-server JSONL protocol, over a single private stdio process.
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::{Mutex as AsyncMutex, broadcast, oneshot},
};

const MAX_FRAME: u64 = 8 * 1024 * 1024;
type Reply = std::result::Result<Value, String>;
type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Reply>>>>;
#[derive(Clone)]
struct PendingServerRequest {
    wire_id: Value,
    request: Value,
    generation: String,
    thread_id: String,
    turn_id: String,
    item_id: String,
    approval_id: Value,
    expires: Instant,
    expires_at_unix: u64,
}
type ServerRequests = Arc<Mutex<HashMap<String, PendingServerRequest>>>;

pub struct Connection {
    stdin: Arc<AsyncMutex<Option<ChildStdin>>>,
    pending: Pending,
    next_id: AtomicU64,
    pub alive: Arc<AtomicBool>,
    pub events: broadcast::Sender<Value>,
    server_requests: ServerRequests,
    generation: String,
    child: AsyncMutex<Child>,
    reader: tokio::task::AbortHandle,
}

impl Connection {
    pub async fn open(bin: &Path) -> Result<Arc<Self>> {
        let mut command = command(bin)?;
        let mut child = command
            .args(["app-server", "--listen", "stdio://"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("codex_start_failed")?;
        let stdin = Arc::new(AsyncMutex::new(Some(
            child.stdin.take().context("codex_stdin_missing")?,
        )));
        let stdout = child.stdout.take().context("codex_stdout_missing")?;
        let pending: Pending = Arc::default();
        let alive = Arc::new(AtomicBool::new(true));
        let (events, _) = broadcast::channel(4096);
        let server_requests: ServerRequests = Arc::default();
        let generation = format!("{:032x}", rand::random::<u128>());
        let reader_generation = generation.clone();
        let (input, replies, healthy, notices, requests) = (
            stdin.clone(),
            pending.clone(),
            alive.clone(),
            events.clone(),
            server_requests.clone(),
        );
        let reader = tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = Vec::new();
                match (&mut reader)
                    .take(MAX_FRAME + 1)
                    .read_until(b'\n', &mut line)
                    .await
                {
                    Ok(0) | Err(_) => break,
                    Ok(_) if line.len() as u64 > MAX_FRAME => break,
                    Ok(_) => {}
                }
                let Ok(value) = serde_json::from_slice::<Value>(&line) else {
                    break;
                };
                if let Some(method) = value["method"].as_str() {
                    if !value["id"].is_null() {
                        if matches!(
                            method,
                            "item/commandExecution/requestApproval"
                                | "item/fileChange/requestApproval"
                                | "item/permissions/requestApproval"
                                | "item/tool/requestUserInput"
                        ) {
                            if requests.lock().is_ok_and(|pending| {
                                pending
                                    .values()
                                    .any(|request| request.wire_id == value["id"])
                            }) {
                                continue;
                            }
                            let token = format!("req_{:032x}", rand::random::<u128>());
                            let inserted = requests.lock().ok().is_some_and(|mut pending| {
                                if pending.len() >= 64
                                    || ["threadId", "turnId", "itemId"].iter().any(|key| {
                                        value["params"][key].as_str().is_none_or(str::is_empty)
                                    })
                                {
                                    return false;
                                }
                                pending.insert(
                                    token.clone(),
                                    PendingServerRequest {
                                        wire_id: value["id"].clone(),
                                        request: value.clone(),
                                        generation: reader_generation.clone(),
                                        thread_id: value["params"]["threadId"]
                                            .as_str()
                                            .unwrap_or_default()
                                            .into(),
                                        turn_id: value["params"]["turnId"]
                                            .as_str()
                                            .unwrap_or_default()
                                            .into(),
                                        item_id: value["params"]["itemId"]
                                            .as_str()
                                            .unwrap_or_default()
                                            .into(),
                                        approval_id: value["params"]["approvalId"].clone(),
                                        expires: Instant::now() + Duration::from_secs(900),
                                        expires_at_unix: crate::unix_time() + 900,
                                    },
                                );
                                true
                            });
                            if inserted {
                                let expiry_requests = Arc::downgrade(&requests);
                                let expiry_input = Arc::downgrade(&input);
                                let expiry_notices = notices.clone();
                                let expiry_token = token.clone();
                                tokio::spawn(async move {
                                    tokio::time::sleep(Duration::from_secs(900)).await;
                                    let Some(requests) = expiry_requests.upgrade() else {
                                        return;
                                    };
                                    let expired = requests
                                        .lock()
                                        .ok()
                                        .and_then(|mut p| p.remove(&expiry_token));
                                    if let Some(item) = expired {
                                        let _ = expiry_notices.send(json!({"method":"farhelm/serverRequestResolved","params":{"threadId":item.thread_id,"requestToken":expiry_token,"state":"expired"}}));
                                        if let Some(input) = expiry_input.upgrade() {
                                            let _ = tokio::time::timeout(Duration::from_secs(5), write_json(&input, &json!({"id":item.wire_id,"error":{"code":-32000,"message":"FarHelm interaction expired"}}))).await;
                                        }
                                    }
                                });
                                let _ = notices.send(json!({"method":"farhelm/serverRequest","params":{"requestToken":token,"threadId":value["params"]["threadId"],"turnId":value["params"]["turnId"],"requestMethod":method}}));
                                continue;
                            }
                        }
                        let response = json!({"id":value["id"],"error":{"code":-32601,"message":"Unsupported or saturated FarHelm client request"}});
                        if !matches!(
                            tokio::time::timeout(
                                Duration::from_secs(5),
                                write_json(&input, &response)
                            )
                            .await,
                            Ok(Ok(()))
                        ) {
                            break;
                        }
                    } else {
                        if method == "serverRequest/resolved" {
                            let request_id = &value["params"]["requestId"];
                            if let Ok(mut pending) = requests.lock() {
                                pending.retain(|_, item| &item.wire_id != request_id);
                            }
                        }
                        if method == "turn/completed"
                            && let Ok(mut pending) = requests.lock()
                        {
                            pending.retain(|_, item| {
                                !(value["params"]["threadId"] == item.thread_id
                                    && value["params"]["turn"]["id"] == item.turn_id)
                            });
                        }
                        let _ = notices.send(value);
                    }
                } else if let Some(id) = value["id"].as_u64() {
                    let sender = replies
                        .lock()
                        .expect("Codex reply map poisoned")
                        .remove(&id);
                    if let Some(sender) = sender {
                        let result = if value.get("error").is_some() {
                            // Vendor error text may contain prompts, paths or credentials.
                            if value["error"]["message"]
                                .as_str()
                                .is_some_and(|s| s.contains("already has an active writer"))
                            {
                                Err("codex_session_in_use".into())
                            } else if value["error"]["message"].as_str().is_some_and(|s| {
                                s.contains("ephemeral thread does not support queued submissions")
                            }) {
                                Err("codex_ephemeral_queue_unsupported".into())
                            } else {
                                Err(format!("codex_request_rejected:{}", value["error"]["code"]))
                            }
                        } else {
                            Ok(value["result"].clone())
                        };
                        let _ = sender.send(result);
                    }
                } else {
                    break;
                }
            }
            healthy.store(false, Ordering::Release);
            for (_, sender) in
                std::mem::take(&mut *replies.lock().expect("Codex reply map poisoned"))
            {
                let _ = sender.send(Err("codex_connection_closed".to_owned()));
            }
            if let Ok(mut pending) = requests.lock() {
                pending.clear();
            }
            let _ = notices.send(json!({"method":"farhelm/disconnected"}));
        });
        let connection = Arc::new(Self {
            stdin,
            pending,
            next_id: AtomicU64::new(1),
            alive,
            events,
            server_requests,
            generation,
            child: AsyncMutex::new(child),
            reader: reader.abort_handle(),
        });
        let initialized = connection
            .request(
                "initialize",
                serde_json::to_value(super::protocol::InitializeParams {
                    client_info: super::protocol::ClientInfo {
                        name: "farhelm",
                        title: "FarHelm",
                        version: farhelm_core::PRODUCT_VERSION,
                    },
                    capabilities: super::protocol::Capabilities {
                        experimental_api: true,
                    },
                })?,
            )
            .await;
        if let Err(error) = initialized {
            connection.shutdown().await;
            return Err(error);
        }
        write_json(&connection.stdin, &json!({"method":"initialized"})).await?;
        Ok(connection)
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        ensure!(
            self.alive.load(Ordering::Acquire),
            "codex_connection_closed"
        );
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = self.pending.lock().expect("Codex reply map poisoned");
            ensure!(pending.len() < 64, "codex_request_capacity");
            pending.insert(id, sender);
        }
        let _guard = PendingGuard {
            pending: self.pending.clone(),
            id,
        };
        let result = tokio::time::timeout(Duration::from_secs(18), async {
            write_json(
                &self.stdin,
                &json!({"id":id,"method":method,"params":params}),
            )
            .await?;
            receiver
                .await
                .context("codex_connection_closed")?
                .map_err(anyhow::Error::msg)
        })
        .await
        .context("codex_request_timeout")?;
        result.map_err(|error| anyhow::anyhow!("{method}: {error}"))
    }

    pub async fn shutdown(&self) {
        self.alive.store(false, Ordering::Release);
        self.server_requests
            .lock()
            .expect("Codex request map poisoned")
            .clear();
        let _ = self.events.send(json!({"method":"farhelm/disconnected"}));
        let mut child = self.child.lock().await;
        let _ = child.start_kill();
        let _ = child.wait().await;
        self.reader.abort();
    }

    pub fn pending_server_requests(&self, session: &str) -> Vec<Value> {
        let Ok(pending) = self.server_requests.lock() else {
            return Vec::new();
        };
        pending
            .iter()
            .filter(|(_, item)| {
                item.thread_id == session
                    && item.expires > Instant::now()
                    && item.generation == self.generation
                    && self.alive.load(Ordering::Acquire)
            })
            .map(|(token, item)| {
                let mut value = item.request.clone();
                if let Some(map) = value.as_object_mut() {
                    map.remove("id");
                    map.insert("requestToken".into(), json!(token));
                    map.insert("generation".into(), json!(item.generation));
                    map.insert("expiresAtUnix".into(), json!(item.expires_at_unix));
                    map.insert("itemId".into(), json!(item.item_id));
                    map.insert("approvalId".into(), item.approval_id.clone());
                }
                value
            })
            .collect()
    }

    pub async fn answer_server_request(
        &self,
        session: &str,
        token: &str,
        answer: Value,
    ) -> Result<Value> {
        ensure!(
            self.alive.load(Ordering::Acquire),
            "codex_connection_closed"
        );
        let pending = {
            let mut requests = self
                .server_requests
                .lock()
                .map_err(|_| anyhow::anyhow!("codex_server_request_poisoned"))?;
            let pending = requests
                .get(token)
                .context("codex_server_request_resolved")?;
            ensure!(
                pending.thread_id == session && pending.generation == self.generation,
                "codex_server_request_scope_mismatch"
            );
            ensure!(
                pending.expires > Instant::now(),
                "codex_server_request_expired"
            );
            validate_server_answer(&pending.request, &answer)?;
            requests.remove(token).expect("validated request exists")
        };
        // A partial write is ambiguous. Never reinsert a consumed approval token.
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            write_json(&self.stdin, &json!({"id":pending.wire_id,"result":answer})),
        )
        .await;
        let answered = matches!(result, Ok(Ok(())));
        let _ = self.events.send(json!({"method":"farhelm/serverRequestResolved","params":{"threadId":session,"turnId":pending.turn_id,"requestToken":token,"state":if answered {"answered"} else {"unknown"}}}));
        ensure!(answered, "codex_server_answer_unknown");
        Ok(json!({"request_token":token,"state":"answered"}))
    }

    /// Only called after the owner verifies that every loaded thread is idle.
    /// Closing stdin lets Codex flush and release its writers without a forced kill.
    pub async fn close_idle(&self) -> Result<()> {
        ensure!(
            self.server_requests
                .lock()
                .expect("Codex request map poisoned")
                .is_empty(),
            "codex_handoff_pending_interaction"
        );
        self.stdin.lock().await.take();
        tokio::time::timeout(Duration::from_secs(5), self.child.lock().await.wait())
            .await
            .context("codex_handoff_unconfirmed")??;
        self.alive.store(false, Ordering::Release);
        self.reader.abort();
        Ok(())
    }
}

fn validate_server_answer(request: &Value, answer: &Value) -> Result<()> {
    let method = request["method"]
        .as_str()
        .context("invalid_server_request")?;
    match method {
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
            let decision = answer["decision"]
                .as_str()
                .context("invalid_approval_answer")?;
            ensure!(
                matches!(
                    decision,
                    "accept" | "acceptForSession" | "decline" | "cancel"
                ),
                "invalid_approval_answer"
            );
            if let Some(available) = request["params"]["availableDecisions"].as_array() {
                ensure!(
                    available.iter().any(|v| v.as_str() == Some(decision)),
                    "approval_decision_unavailable"
                );
            }
        }
        "item/permissions/requestApproval" => {
            ensure!(
                answer["permissions"].is_object(),
                "invalid_permission_answer"
            );
            ensure!(
                matches!(
                    answer["scope"].as_str().unwrap_or("turn"),
                    "turn" | "session"
                ),
                "invalid_permission_scope"
            );
            ensure!(
                permission_subset(&answer["permissions"], &request["params"]["permissions"]),
                "permission_grant_expands_request"
            );
            if let Some(strict) = answer.get("strictAutoReview") {
                ensure!(strict.is_boolean(), "invalid_strict_auto_review");
            }
        }
        "item/tool/requestUserInput" => {
            let answers = answer["answers"]
                .as_object()
                .context("invalid_user_input_answer")?;
            let questions = request["params"]["questions"]
                .as_array()
                .context("invalid_user_input_request")?;
            ensure!(
                answers
                    .keys()
                    .all(|id| questions.iter().any(|q| q["id"] == *id)),
                "unknown_user_input_question"
            );
            ensure!(
                answers.values().all(|value| value["answers"]
                    .as_array()
                    .is_some_and(|values| values.iter().all(Value::is_string))),
                "invalid_user_input_answer"
            );
        }
        _ => bail!("unsupported_server_request"),
    }
    Ok(())
}

fn permission_subset(granted: &Value, requested: &Value) -> bool {
    match (granted, requested) {
        (Value::Object(granted), Value::Object(requested)) => granted.iter().all(|(key, value)| {
            requested
                .get(key)
                .is_some_and(|expected| permission_subset(value, expected))
        }),
        (Value::Array(granted), Value::Array(requested)) => granted
            .iter()
            .all(|value| requested.iter().any(|expected| value == expected)),
        (Value::Bool(false), Value::Bool(_)) => true,
        _ => granted == requested,
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.reader.abort();
    }
}
struct PendingGuard {
    pending: Pending,
    id: u64,
}
impl Drop for PendingGuard {
    fn drop(&mut self) {
        if let Ok(mut map) = self.pending.lock() {
            map.remove(&self.id);
        }
    }
}

async fn write_json(stdin: &AsyncMutex<Option<ChildStdin>>, value: &Value) -> Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    ensure!(bytes.len() as u64 <= MAX_FRAME, "codex_request_too_large");
    bytes.push(b'\n');
    stdin
        .lock()
        .await
        .as_mut()
        .context("codex_connection_closed")?
        .write_all(&bytes)
        .await
        .context("codex_write_failed")
}

pub fn command(bin: &Path) -> Result<Command> {
    let mut command = Command::new(bin);
    for key in [
        "FARHELM_AGENT_TOKEN",
        "FARHELM_HUB_TOKEN",
        "FARHELM_PAIRING_CODE",
        "FARHELM_ADMIN_PASSWORD",
    ] {
        command.env_remove(key);
    }
    let mut paths = vec![bin.parent().context("codex_bin_parent_missing")?.to_owned()];
    if let Some(path) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&path));
    }
    command.env("PATH", std::env::join_paths(paths)?);
    Ok(command)
}

pub async fn version(bin: &Path) -> Result<String> {
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        command(bin)?.arg("--version").kill_on_drop(true).output(),
    )
    .await
    .context("codex_version_timeout")??;
    ensure!(output.status.success(), "codex_version_failed");
    let text = std::str::from_utf8(&output.stdout)?;
    let version = text
        .trim()
        .strip_prefix("codex-cli ")
        .context("codex_version_unrecognized")?;
    let parsed = semver::Version::parse(version).context("codex_version_unrecognized")?;
    ensure!(
        parsed >= semver::Version::new(0, 147, 0),
        "codex_upgrade_required"
    );
    Ok(version.to_owned())
}

pub fn discover(explicit: Option<&Path>) -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let valid = |path: &Path| {
        path.is_absolute()
            && std::fs::metadata(path)
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    if let Some(path) = explicit {
        ensure!(valid(path), "codex_configured_binary_unavailable");
        return Ok(path.to_owned());
    }
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|p| p.join("codex")));
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        candidates.push(home.join(".local/bin/codex"));
        candidates.push(home.join(".npm-global/bin/codex"));
        if let Ok(entries) = std::fs::read_dir(home.join(".nvm/versions/node")) {
            candidates.extend(entries.flatten().map(|e| e.path().join("bin/codex")));
        }
    }
    let mut unique = HashMap::new();
    for path in candidates {
        if valid(&path) {
            unique.entry(std::fs::canonicalize(&path)?).or_insert(path);
        }
    }
    match unique.len() {
        0 => bail!(
            "codex_not_configured: install and log in to Codex, then run farhelm-agent codex configure --bin PATH"
        ),
        1 => Ok(unique.into_values().next().expect("one binary")),
        _ => bail!(
            "codex_multiple_binaries: select one with farhelm-agent codex configure --bin PATH"
        ),
    }
}

#[cfg(test)]
pub(super) fn write_fixture_executable(path: &Path, script: &str) {
    use std::io::Write;
    // Other test threads can fork between a parent-side write and close,
    // inheriting a writable fd until exec and causing ETXTBSY. Keep that fd
    // entirely inside a writer process and wait for it to exit before exec.
    let mut writer = std::process::Command::new("/bin/sh")
        .args(["-c", "umask 077; cat > \"$1\" && chmod 700 \"$1\"", "--"])
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    writer
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    assert!(writer.wait().unwrap().success());
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn permission_answers_cannot_expand_native_request() {
        let request = json!({"method":"item/permissions/requestApproval","params":{"permissions":{"network":{"enabled":true},"fileSystem":{"read":["/a"]}}}});
        assert!(
            validate_server_answer(
                &request,
                &json!({"permissions":{"network":{"enabled":true}},"scope":"turn"})
            )
            .is_ok()
        );
        assert!(
            validate_server_answer(
                &request,
                &json!({"permissions":{"fileSystem":{"read":["/b"]}},"scope":"turn"})
            )
            .is_err()
        );
    }
    fn executable(directory: &Path, name: &str, script: &str) -> PathBuf {
        let path = directory.join(name);
        write_fixture_executable(&path, script);
        path
    }
    #[tokio::test]
    async fn one_process_multiplexes_notifications_reordered_replies_and_cancelled_reads() {
        let directory = tempfile::tempdir().unwrap();
        let bin = executable(
            directory.path(),
            "codex",
            r#"#!/bin/sh
read -r line
printf '%s\n' '{"id":1,"result":{"userAgent":"fixture"}}'
read -r line
while IFS= read -r line; do
 id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
 case "$line" in
  *'"method":"left"'*) left=$id ;;
  *'"method":"right"'*)
   printf '%s\n' '{"method":"thread/status/changed","params":{"threadId":"fixture","status":{"type":"idle"}}}'
   printf '{"id":%s,"result":{"side":"right"}}\n' "$id"
   printf '{"id":%s,"result":{"side":"left"}}\n' "$left" ;;
  *'"method":"cancel"'*) ;;
  *'"method":"approval"'*)
   printf '%s\n' '{"id":900,"method":"item/commandExecution/requestApproval","params":{"threadId":"fixture","turnId":"t1","itemId":"i1","command":"untrusted"}}'
   IFS= read -r reply
   case "$reply" in *'"decision":"decline"'*) printf '{"id":%s,"result":{"declined":true}}\n' "$id" ;; *) exit 1 ;; esac ;;
  *) printf '{"id":%s,"result":{}}\n' "$id" ;;
 esac
done
"#,
        );
        let connection = Connection::open(&bin).await.unwrap();
        let mut events = connection.events.subscribe();
        let (left, right) = tokio::join!(
            connection.request("left", json!({})),
            connection.request("right", json!({}))
        );
        assert_eq!(left.unwrap()["side"], "left");
        assert_eq!(right.unwrap()["side"], "right");
        assert_eq!(
            events.recv().await.unwrap()["method"],
            "thread/status/changed"
        );
        assert!(
            tokio::time::timeout(
                Duration::from_millis(10),
                connection.request("cancel", json!({}))
            )
            .await
            .is_err()
        );
        assert!(connection.pending.lock().unwrap().is_empty());
        let approval_connection = connection.clone();
        let approval =
            tokio::spawn(async move { approval_connection.request("approval", json!({})).await });
        let request_event = events.recv().await.unwrap();
        assert_eq!(request_event["method"], "farhelm/serverRequest");
        let token = request_event["params"]["requestToken"].as_str().unwrap();
        assert_eq!(connection.pending_server_requests("fixture").len(), 1);
        {
            let mut pending = connection.server_requests.lock().unwrap();
            pending.get_mut(token).unwrap().expires = Instant::now() - Duration::from_secs(1);
        }
        assert!(connection.pending_server_requests("fixture").is_empty());
        assert!(
            connection
                .answer_server_request("fixture", token, json!({"decision":"accept"}))
                .await
                .unwrap_err()
                .to_string()
                .contains("expired")
        );
        {
            let mut pending = connection.server_requests.lock().unwrap();
            let request = pending.get_mut(token).unwrap();
            request.expires = Instant::now() + Duration::from_secs(900);
            request.generation = "previous-connection".into();
        }
        assert!(
            connection
                .answer_server_request("fixture", token, json!({"decision":"accept"}))
                .await
                .unwrap_err()
                .to_string()
                .contains("scope_mismatch")
        );
        connection
            .server_requests
            .lock()
            .unwrap()
            .get_mut(token)
            .unwrap()
            .generation = connection.generation.clone();
        assert!(connection.pending_server_requests("other").is_empty());
        assert!(
            connection
                .answer_server_request("other", token, json!({"decision":"accept"}))
                .await
                .is_err()
        );
        assert!(
            connection
                .answer_server_request("fixture", token, json!({"decision":"invalid"}))
                .await
                .is_err()
        );
        assert_eq!(connection.pending_server_requests("fixture").len(), 1);
        connection
            .answer_server_request("fixture", token, json!({"decision":"decline"}))
            .await
            .unwrap();
        assert_eq!(approval.await.unwrap().unwrap()["declined"], true);
        assert!(
            connection
                .answer_server_request("fixture", token, json!({"decision":"accept"}))
                .await
                .is_err()
        );
        connection.shutdown().await;
        assert!(!connection.alive.load(Ordering::Acquire));
    }
    #[tokio::test]
    async fn malformed_frames_fail_waiters_and_binary_parent_resolves_npm_interpreters() {
        let directory = tempfile::tempdir().unwrap();
        let bin = executable(
            directory.path(),
            "codex",
            "#!/usr/bin/env farhelm-fixture-node\n",
        );
        executable(
            directory.path(),
            "farhelm-fixture-node",
            "#!/bin/sh\nprintf 'codex-cli 0.153.4\\n'\n",
        );
        assert_eq!(version(&bin).await.unwrap(), "0.153.4");
        executable(
            directory.path(),
            "farhelm-fixture-node",
            "#!/bin/sh\nprintf 'codex-cli 0.147.0\\n'\n",
        );
        assert_eq!(version(&bin).await.unwrap(), "0.147.0");
        let malformed = executable(
            directory.path(),
            "malformed",
            "#!/bin/sh\nread -r line\nprintf '%s\\n' '{\"id\":1,\"result\":{}}'\nread -r line\nread -r line\nprintf 'broken-json\\n'\n",
        );
        let connection = Connection::open(&malformed).await.unwrap();
        assert!(
            connection
                .request("read", json!({}))
                .await
                .unwrap_err()
                .to_string()
                .contains("codex_connection_closed")
        );
        assert!(!connection.alive.load(Ordering::Acquire));
        connection.shutdown().await;
    }
    #[tokio::test]
    async fn oversized_native_frame_closes_connection_without_retaining_waiters() {
        let directory = tempfile::tempdir().unwrap();
        let bin = executable(
            directory.path(),
            "oversized",
            "#!/bin/sh\nread -r line\nprintf '%s\\n' '{\"id\":1,\"result\":{}}'\nread -r line\nread -r line\nhead -c 8388609 /dev/zero | tr '\\000' 'x'\n",
        );
        let connection = Connection::open(&bin).await.unwrap();
        assert!(connection.request("read", json!({})).await.is_err());
        assert!(!connection.alive.load(Ordering::Acquire));
        assert!(connection.pending.lock().unwrap().is_empty());
        connection.shutdown().await;
    }
}
