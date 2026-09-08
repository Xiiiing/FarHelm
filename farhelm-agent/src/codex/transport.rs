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
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::{Mutex as AsyncMutex, broadcast, oneshot},
};

const MAX_FRAME: u64 = 8 * 1024 * 1024;
type Reply = std::result::Result<Value, String>;
type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Reply>>>>;

pub struct Connection {
    stdin: Arc<AsyncMutex<ChildStdin>>,
    pending: Pending,
    next_id: AtomicU64,
    pub alive: Arc<AtomicBool>,
    pub events: broadcast::Sender<Value>,
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
        let stdin = Arc::new(AsyncMutex::new(
            child.stdin.take().context("codex_stdin_missing")?,
        ));
        let stdout = child.stdout.take().context("codex_stdout_missing")?;
        let pending: Pending = Arc::default();
        let alive = Arc::new(AtomicBool::new(true));
        let (events, _) = broadcast::channel(4096);
        let (input, replies, healthy, notices) = (
            stdin.clone(),
            pending.clone(),
            alive.clone(),
            events.clone(),
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
                        // FarHelm does not offer an approval UI. Never silently grant
                        // elevated permissions or let an unanswered approval hang a turn.
                        let result = match method {
                            "item/commandExecution/requestApproval"
                            | "item/fileChange/requestApproval" => {
                                Some(json!({"decision":"decline"}))
                            }
                            "item/permissions/requestApproval" => {
                                Some(json!({"permissions":{},"scope":"turn"}))
                            }
                            "item/tool/requestUserInput" => Some(json!({"answers":{}})),
                            _ => None,
                        };
                        let response = match result {
                            Some(result) => json!({"id":value["id"],"result":result}),
                            None => {
                                json!({"id":value["id"],"error":{"code":-32601,"message":"Unsupported FarHelm client request"}})
                            }
                        };
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
                            Err(format!("codex_request_rejected:{}", value["error"]["code"]))
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
            let _ = notices.send(json!({"method":"farhelm/disconnected"}));
        });
        let connection = Arc::new(Self {
            stdin,
            pending,
            next_id: AtomicU64::new(1),
            alive,
            events,
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
        let mut child = self.child.lock().await;
        let _ = child.start_kill();
        let _ = child.wait().await;
        self.reader.abort();
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

async fn write_json(stdin: &AsyncMutex<ChildStdin>, value: &Value) -> Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    ensure!(bytes.len() as u64 <= MAX_FRAME, "codex_request_too_large");
    bytes.push(b'\n');
    stdin
        .lock()
        .await
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
   printf '%s\n' '{"id":900,"method":"item/commandExecution/requestApproval","params":{"command":"untrusted"}}'
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
        assert_eq!(
            connection.request("approval", json!({})).await.unwrap()["declined"],
            true
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
