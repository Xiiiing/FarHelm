use std::{
    collections::HashSet,
    fs,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
    sync::{Mutex, MutexGuard},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, ensure};
use farhelm_protocol::{
    AgentCommand, AgentEvent, CommandAction, ExperimentState, FARHELM_PROTOCOL,
};
use regex::Regex;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const LOG_TAIL_LIMIT: u64 = 1024 * 1024;
const PROMPT_LIMIT: usize = 32 * 1024;

#[derive(Debug, Clone)]
pub struct WatchRegistration {
    pub project_id: String,
    pub project_root: PathBuf,
    pub name: String,
    pub pid: u32,
    pub log_path: PathBuf,
    pub session_id: Option<String>,
    pub new_session_mode: Option<String>,
    pub success_prompt: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProjectMatchers {
    pub success: Vec<String>,
    pub failure: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct WatchRecord {
    pub watch_id: String,
    pub project_id: String,
    pub name: String,
    pub pid: u32,
    pub proc_start_time: u64,
    pub uid: u32,
    pub log_path: PathBuf,
    pub session_id: Option<String>,
    pub new_session_mode: Option<String>,
    pub state: ExperimentState,
    pub detail: Option<String>,
    pub updated_at_unix: u64,
}

#[derive(Debug, Clone)]
pub struct CompletedWatch {
    pub watch_id: String,
    pub state: ExperimentState,
}

#[derive(Debug, Clone)]
pub struct AutoPrompt {
    pub watch_id: String,
    pub project_id: String,
    pub project_root: PathBuf,
    pub session_id: Option<String>,
    pub new_session_mode: Option<String>,
    pub prompt: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone)]
pub struct RemoteCommand {
    pub command_id: String,
    pub action: CommandAction,
    pub expires_at_unix: u64,
    pub payload: Value,
    pub accepted_reported: bool,
}

#[derive(Debug, Clone)]
pub struct RemoteCommandReport {
    pub command_id: String,
    pub state: farhelm_protocol::CommandState,
    pub data: Option<Value>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SessionBinding {
    pub project_id: String,
    pub cwd: PathBuf,
    pub mode: String,
}

pub struct ExperimentStore {
    connection: Mutex<Connection>,
    path: PathBuf,
}

impl ExperimentStore {
    pub fn open(path: &Path) -> Result<Self> {
        if path != Path::new(":memory:") {
            let parent = path
                .parent()
                .filter(|part| !part.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let connection = Connection::open(path)
            .with_context(|| format!("failed to open Agent database {}", path.display()))?;
        connection.busy_timeout(Duration::from_secs(5))?;
        if path != Path::new(":memory:") {
            connection.pragma_update(None, "journal_mode", "WAL")?;
        }
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS experiment_watches (
                watch_id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                project_root TEXT NOT NULL,
                name TEXT NOT NULL,
                pid INTEGER NOT NULL,
                proc_start_time INTEGER NOT NULL,
                uid INTEGER NOT NULL,
                log_path TEXT NOT NULL,
                session_id TEXT,
                new_session_mode TEXT CHECK (new_session_mode IS NULL OR new_session_mode IN ('inspect','edit')),
                success_prompt TEXT,
                state TEXT NOT NULL CHECK (state IN ('watching','succeeded','failed','unknown','cancelled')),
                detail TEXT,
                auto_prompt_claimed INTEGER NOT NULL DEFAULT 0 CHECK (auto_prompt_claimed IN (0,1,2)),
                created_at_unix INTEGER NOT NULL,
                updated_at_unix INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS event_outbox (
                sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                event_id TEXT NOT NULL UNIQUE,
                event_type TEXT NOT NULL,
                payload_json TEXT NOT NULL,
                created_at_unix INTEGER NOT NULL,
                acknowledged INTEGER NOT NULL DEFAULT 0 CHECK (acknowledged IN (0,1))
            );
            CREATE TABLE IF NOT EXISTS remote_codex_commands (
                command_id TEXT PRIMARY KEY,
                action TEXT NOT NULL,
                expires_at_unix INTEGER NOT NULL,
                payload_json TEXT NOT NULL,
                state TEXT NOT NULL CHECK (state IN ('accepted','running','completed','failed','expired','orphaned')),
                accepted_reported INTEGER NOT NULL DEFAULT 0 CHECK (accepted_reported IN (0,1)),
                terminal_reported INTEGER NOT NULL DEFAULT 0 CHECK (terminal_reported IN (0,1)),
                data_json TEXT,
                detail TEXT,
                updated_at_unix INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS codex_session_bindings (
                session_id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                cwd TEXT NOT NULL,
                mode TEXT NOT NULL CHECK (mode IN ('inspect','edit')),
                updated_at_unix INTEGER NOT NULL
            );",
        )?;
        ensure_remote_command_columns(&connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
            path: path.to_owned(),
        })
    }

    pub fn register(&self, registration: &WatchRegistration, now: u64) -> Result<WatchRecord> {
        ensure!(registration.pid > 0, "PID must be positive");
        ensure!(
            registration.project_root.is_absolute(),
            "project path must be absolute"
        );
        validate_relative_path(&registration.log_path)?;
        if let Some(prompt) = &registration.success_prompt {
            ensure!(prompt.len() <= PROMPT_LIMIT, "prompt exceeds 32 KiB");
            ensure!(!prompt.trim().is_empty(), "prompt is empty");
        }
        ensure!(
            registration.session_id.is_some() ^ registration.new_session_mode.is_some(),
            "choose exactly one of an existing session or a new session"
        );
        let process = read_process_identity(registration.pid)?;
        let project_root = fs::canonicalize(&registration.project_root).with_context(|| {
            format!(
                "failed to resolve project {}",
                registration.project_root.display()
            )
        })?;
        let log_path = fs::canonicalize(project_root.join(&registration.log_path))
            .context("failed to resolve experiment log path")?;
        ensure!(
            log_path.starts_with(&project_root),
            "log path resolves outside the approved project"
        );
        ensure!(
            process.cwd.starts_with(&project_root),
            "PID cwd is outside the approved project"
        );
        ensure!(
            process.uid == unsafe { libc::geteuid() },
            "PID is owned by another user"
        );

        let mut hasher = Sha256::new();
        hasher.update(registration.project_id.as_bytes());
        hasher.update(registration.pid.to_be_bytes());
        hasher.update(process.start_time.to_be_bytes());
        hasher.update(now.to_be_bytes());
        let digest = hasher.finalize();
        let watch_id = format!(
            "watch_{}",
            digest
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        let payload = watch_payload(
            &watch_id,
            registration,
            ExperimentState::Watching,
            None,
            now,
        );
        let connection = self.lock()?;
        let transaction = connection.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO experiment_watches (
                watch_id, project_id, project_root, name, pid, proc_start_time, uid, log_path,
                session_id, new_session_mode, success_prompt, state, created_at_unix, updated_at_unix
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'watching',?12,?12)",
            params![
                watch_id,
                registration.project_id,
                project_root.to_string_lossy(),
                registration.name,
                i64::from(registration.pid),
                as_i64(process.start_time)?,
                i64::from(process.uid),
                log_path.to_string_lossy(),
                registration.session_id,
                registration.new_session_mode,
                registration.success_prompt,
                as_i64(now)?,
            ],
        )?;
        insert_event(
            &transaction,
            &format!("{watch_id}:watching"),
            "experiment.updated",
            &payload,
            now,
        )?;
        transaction.commit()?;
        drop(connection);
        self.get(&watch_id)?.context("registered watch disappeared")
    }

    pub fn list(&self) -> Result<Vec<WatchRecord>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT watch_id, project_id, name, pid, proc_start_time, uid, log_path,
                    session_id, new_session_mode, state, detail, updated_at_unix
               FROM experiment_watches ORDER BY created_at_unix DESC, watch_id DESC",
        )?;
        let rows = statement.query_map([], watch_from_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn cancel(&self, watch_id: &str, now: u64) -> Result<bool> {
        let connection = self.lock()?;
        let transaction = connection.unchecked_transaction()?;
        let changed = transaction.execute(
            "UPDATE experiment_watches SET state='cancelled', detail='monitoring cancelled', updated_at_unix=?1
              WHERE watch_id=?2 AND state='watching'",
            params![as_i64(now)?, watch_id],
        )?;
        if changed == 1 {
            let payload = json!({"watch_id":watch_id,"state":"cancelled","detail":"monitoring cancelled","updated_at_unix":now});
            insert_event(
                &transaction,
                &format!("{watch_id}:cancelled"),
                "experiment.updated",
                &payload,
                now,
            )?;
        }
        transaction.commit()?;
        Ok(changed == 1)
    }

    pub fn inspect(
        &self,
        matchers: &std::collections::BTreeMap<String, ProjectMatchers>,
        now: u64,
    ) -> Result<Vec<CompletedWatch>> {
        let watches = self
            .list()?
            .into_iter()
            .filter(|watch| watch.state == ExperimentState::Watching)
            .collect::<Vec<_>>();
        let mut completed = Vec::new();
        for watch in watches {
            let process_ended = match read_process_identity(watch.pid) {
                Ok(identity) => {
                    identity.start_time != watch.proc_start_time || identity.uid != watch.uid
                }
                Err(_) => true,
            };
            if !process_ended {
                continue;
            }
            let patterns = matchers
                .get(&watch.project_id)
                .context("watch references an unknown project")?;
            let (state, detail) = classify_log(&watch.log_path, patterns)?;
            let connection = self.lock()?;
            let transaction = connection.unchecked_transaction()?;
            let changed = transaction.execute(
                "UPDATE experiment_watches SET state=?1, detail=?2, updated_at_unix=?3
                  WHERE watch_id=?4 AND state='watching' AND proc_start_time=?5",
                params![
                    state_name(state),
                    detail,
                    as_i64(now)?,
                    watch.watch_id,
                    as_i64(watch.proc_start_time)?
                ],
            )?;
            if changed == 1 {
                let payload = json!({
                    "watch_id":watch.watch_id,"agent_id":"","project_id":watch.project_id,
                    "name":watch.name,"pid":watch.pid,"state":state,"session_id":watch.session_id,
                    "detail":detail,"updated_at_unix":now
                });
                insert_event(
                    &transaction,
                    &format!("{}:{}", watch.watch_id, state_name(state)),
                    "experiment.updated",
                    &payload,
                    now,
                )?;
                completed.push(CompletedWatch {
                    watch_id: watch.watch_id.clone(),
                    state,
                });
            }
            transaction.commit()?;
        }
        Ok(completed)
    }

    pub fn claim_auto_prompt(&self, watch_id: &str) -> Result<bool> {
        let changed = self.lock()?.execute(
            "UPDATE experiment_watches SET auto_prompt_claimed=1 WHERE watch_id=?1 AND state='succeeded' AND success_prompt IS NOT NULL AND auto_prompt_claimed=0",
            [watch_id],
        )?;
        Ok(changed == 1)
    }

    pub fn pending_auto_prompts(&self, now: u64) -> Result<Vec<AutoPrompt>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT watch_id,project_id,project_root,session_id,new_session_mode,success_prompt,proc_start_time
               FROM experiment_watches
              WHERE state='succeeded' AND success_prompt IS NOT NULL AND auto_prompt_claimed=0 AND updated_at_unix+86400>?1
              ORDER BY updated_at_unix,watch_id LIMIT 8",
        )?;
        let rows = statement.query_map([as_i64(now)?], |row| {
            let watch_id: String = row.get(0)?;
            let start_time = row_u64(row, 6)?;
            Ok(AutoPrompt {
                idempotency_key: format!("{watch_id}:{start_time}"),
                watch_id,
                project_id: row.get(1)?,
                project_root: PathBuf::from(row.get::<_, String>(2)?),
                session_id: row.get(3)?,
                new_session_mode: row.get(4)?,
                prompt: row.get(5)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn finish_auto_prompt(
        &self,
        watch_id: &str,
        event_type: &str,
        payload: &Value,
        now: u64,
    ) -> Result<()> {
        let connection = self.lock()?;
        let transaction = connection.unchecked_transaction()?;
        let session_id = payload.get("session_id").and_then(Value::as_str);
        let changed = transaction.execute(
            "UPDATE experiment_watches
                SET auto_prompt_claimed=2,updated_at_unix=?1,session_id=COALESCE(?2,session_id)
              WHERE watch_id=?3 AND auto_prompt_claimed=1",
            params![as_i64(now)?, session_id, watch_id],
        )?;
        ensure!(changed == 1, "auto prompt was not running");
        insert_event(
            &transaction,
            &format!("{watch_id}:{event_type}"),
            event_type,
            payload,
            now,
        )?;
        if let Some(session_id) = session_id {
            let (project_id, name, pid, state, detail): (
                String,
                String,
                u32,
                ExperimentState,
                Option<String>,
            ) = transaction.query_row(
                "SELECT project_id,name,pid,state,detail FROM experiment_watches WHERE watch_id=?1",
                [watch_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        u32::try_from(row.get::<_, i64>(2)?).map_err(conversion(2))?,
                        parse_state(&row.get::<_, String>(3)?)?,
                        row.get(4)?,
                    ))
                },
            )?;
            insert_event(
                &transaction,
                &format!("{watch_id}:session:{session_id}"),
                "experiment.updated",
                &json!({
                    "watch_id":watch_id,"agent_id":"","project_id":project_id,"name":name,
                    "pid":pid,"state":state,"session_id":session_id,"detail":detail,
                    "updated_at_unix":now
                }),
                now,
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn orphan_running_prompts(&self, now: u64) -> Result<u64> {
        let connection = self.lock()?;
        let transaction = connection.unchecked_transaction()?;
        let mut statement = transaction.prepare(
            "SELECT w.watch_id,w.session_id,w.project_id,COALESCE(b.mode,w.new_session_mode,'inspect')
               FROM experiment_watches w
               LEFT JOIN codex_session_bindings b ON b.session_id=w.session_id
              WHERE w.auto_prompt_claimed=1",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        for (watch_id, session_id, project_id, mode) in &rows {
            transaction.execute(
                "UPDATE experiment_watches SET auto_prompt_claimed=2 WHERE watch_id=?1",
                [watch_id],
            )?;
            insert_event(
                &transaction,
                &format!("{watch_id}:codex.orphaned"),
                "codex.turn.orphaned",
                &json!({"watch_id":watch_id,"session_id":session_id,"detail":"Agent restarted during Codex turn"}),
                now,
            )?;
            if let Some(session_id) = session_id {
                insert_event(
                    &transaction,
                    &format!("{watch_id}:session-orphaned"),
                    "codex.session.updated",
                    &json!({
                        "session_id":session_id,"project_id":project_id,"mode":mode,
                        "state":"orphaned","title":null,"active_turn_id":null,
                        "updated_at_unix":now
                    }),
                    now,
                )?;
            }
        }
        transaction.commit()?;
        Ok(rows.len() as u64)
    }

    pub fn enqueue_event(
        &self,
        event_id: &str,
        event_type: &str,
        payload: &Value,
        now: u64,
    ) -> Result<()> {
        let connection = self.lock()?;
        insert_event(&connection, event_id, event_type, payload, now)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn bind_session(
        &self,
        session_id: &str,
        project_id: &str,
        cwd: &Path,
        mode: &str,
        now: u64,
    ) -> Result<()> {
        ensure!(!session_id.is_empty(), "session ID is empty");
        ensure!(cwd.is_absolute(), "session cwd must be absolute");
        ensure!(
            matches!(mode, "inspect" | "edit"),
            "session mode is invalid"
        );
        self.lock()?.execute(
            "INSERT INTO codex_session_bindings (session_id,project_id,cwd,mode,updated_at_unix)
             VALUES (?1,?2,?3,?4,?5)
             ON CONFLICT(session_id) DO UPDATE SET project_id=excluded.project_id,cwd=excluded.cwd,mode=excluded.mode,updated_at_unix=excluded.updated_at_unix",
            params![session_id,project_id,cwd.to_string_lossy(),mode,as_i64(now)?],
        )?;
        Ok(())
    }

    pub fn link_watch_session(&self, watch_id: &str, session_id: &str, now: u64) -> Result<bool> {
        let connection = self.lock()?;
        let transaction = connection.unchecked_transaction()?;
        let changed = transaction.execute(
            "UPDATE experiment_watches SET session_id=?1,updated_at_unix=?2
              WHERE watch_id=?3 AND state='succeeded' AND session_id IS NULL",
            params![session_id, as_i64(now)?, watch_id],
        )?;
        if changed == 1 {
            let (project_id, name, pid, state, detail): (
                String,
                String,
                u32,
                ExperimentState,
                Option<String>,
            ) = transaction.query_row(
                "SELECT project_id,name,pid,state,detail FROM experiment_watches WHERE watch_id=?1",
                [watch_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        u32::try_from(row.get::<_, i64>(2)?).map_err(conversion(2))?,
                        parse_state(&row.get::<_, String>(3)?)?,
                        row.get(4)?,
                    ))
                },
            )?;
            insert_event(
                &transaction,
                &format!("{watch_id}:session:{session_id}"),
                "experiment.updated",
                &json!({
                    "watch_id":watch_id,"agent_id":"","project_id":project_id,"name":name,
                    "pid":pid,"state":state,"session_id":session_id,"detail":detail,
                    "updated_at_unix":now
                }),
                now,
            )?;
        }
        transaction.commit()?;
        Ok(changed == 1)
    }

    pub fn session_binding(&self, session_id: &str) -> Result<Option<SessionBinding>> {
        self.lock()?
            .query_row(
                "SELECT project_id,cwd,mode FROM codex_session_bindings WHERE session_id=?1",
                [session_id],
                |row| {
                    Ok(SessionBinding {
                        project_id: row.get(0)?,
                        cwd: PathBuf::from(row.get::<_, String>(1)?),
                        mode: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn receive_remote_command(&self, command: &AgentCommand, now: u64) -> Result<()> {
        ensure!(
            command.action != CommandAction::AgentProbe,
            "probe cannot enter Codex inbox"
        );
        let payload = command
            .payload
            .as_ref()
            .context("Codex command omitted payload")?;
        let connection = self.lock()?;
        if let Some((action, expires, encoded)) = connection.query_row(
            "SELECT action,expires_at_unix,payload_json FROM remote_codex_commands WHERE command_id=?1",
            [&command.command_id], |row| Ok((row.get::<_,String>(0)?,row_u64(row,1)?,row.get::<_,String>(2)?)),
        ).optional()? {
            ensure!(action == action_name(command.action) && expires == command.expires_at_unix && encoded == serde_json::to_string(payload)?, "duplicate command identity mismatch");
            return Ok(());
        }
        connection.execute(
            "INSERT INTO remote_codex_commands (command_id,action,expires_at_unix,payload_json,state,updated_at_unix) VALUES (?1,?2,?3,?4,?5,?6)",
            params![command.command_id,action_name(command.action),as_i64(command.expires_at_unix)?,serde_json::to_string(payload)?,if now>=command.expires_at_unix{"expired"}else{"accepted"},as_i64(now)?],
        )?;
        Ok(())
    }

    pub fn pending_remote_commands(&self) -> Result<Vec<RemoteCommand>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT command_id,action,expires_at_unix,payload_json,accepted_reported FROM remote_codex_commands WHERE state='accepted' ORDER BY updated_at_unix,command_id LIMIT 8",
        )?;
        let rows = statement.query_map([], |row| {
            let encoded: String = row.get(3)?;
            Ok(RemoteCommand {
                command_id: row.get(0)?,
                action: parse_action(&row.get::<_, String>(1)?)?,
                expires_at_unix: row_u64(row, 2)?,
                payload: serde_json::from_str(&encoded).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        3,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?,
                accepted_reported: row.get::<_, i64>(4)? == 1,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn remote_session_busy(&self, session_id: &str) -> Result<bool> {
        let connection = self.lock()?;
        let auto_busy: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM experiment_watches WHERE auto_prompt_claimed=1 AND session_id=?1)",
            [session_id], |row| row.get(0),
        )?;
        if auto_busy {
            return Ok(true);
        }
        let mut statement = connection
            .prepare("SELECT payload_json FROM remote_codex_commands WHERE state='running'")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        for encoded in rows {
            let payload: Value = serde_json::from_str(&encoded?)?;
            if payload.get("session_id").and_then(Value::as_str) == Some(session_id) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn mark_remote_accepted_reported(&self, command_id: &str, now: u64) -> Result<()> {
        self.lock()?.execute("UPDATE remote_codex_commands SET accepted_reported=1,updated_at_unix=?1 WHERE command_id=?2 AND state='accepted'",params![as_i64(now)?,command_id])?;
        Ok(())
    }

    pub fn claim_remote_command(&self, command_id: &str, now: u64) -> Result<bool> {
        Ok(self.lock()?.execute("UPDATE remote_codex_commands SET state='running',updated_at_unix=?1 WHERE command_id=?2 AND state='accepted' AND accepted_reported=1",params![as_i64(now)?,command_id])?==1)
    }

    pub fn finish_remote_command(
        &self,
        command_id: &str,
        state: farhelm_protocol::CommandState,
        data: Option<&Value>,
        detail: Option<&str>,
        now: u64,
    ) -> Result<()> {
        ensure!(
            matches!(
                state,
                farhelm_protocol::CommandState::Completed | farhelm_protocol::CommandState::Failed
            ),
            "remote command terminal state is invalid"
        );
        ensure!(self.lock()?.execute(
            "UPDATE remote_codex_commands SET state=?1,data_json=?2,detail=?3,terminal_reported=0,updated_at_unix=?4 WHERE command_id=?5 AND state='running'",
            params![if state==farhelm_protocol::CommandState::Completed{"completed"}else{"failed"},data.map(serde_json::to_string).transpose()?,detail,as_i64(now)?,command_id]
        )?==1,"remote command was not running");
        Ok(())
    }

    pub fn pending_remote_reports(&self) -> Result<Vec<RemoteCommandReport>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT command_id,state,data_json,detail FROM remote_codex_commands
              WHERE state IN ('completed','failed') AND terminal_reported=0
              ORDER BY updated_at_unix,command_id LIMIT 8",
        )?;
        let rows = statement.query_map([], |row| {
            let state: String = row.get(1)?;
            let data: Option<String> = row.get(2)?;
            Ok(RemoteCommandReport {
                command_id: row.get(0)?,
                state: if state == "completed" {
                    farhelm_protocol::CommandState::Completed
                } else {
                    farhelm_protocol::CommandState::Failed
                },
                data: data
                    .map(|value| serde_json::from_str(&value).map_err(json_conversion(2)))
                    .transpose()?,
                detail: row.get(3)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn mark_remote_terminal_reported(&self, command_id: &str, now: u64) -> Result<()> {
        self.lock()?.execute(
            "UPDATE remote_codex_commands SET terminal_reported=1,updated_at_unix=?1
              WHERE command_id=?2 AND state IN ('completed','failed')",
            params![as_i64(now)?, command_id],
        )?;
        Ok(())
    }

    pub fn orphan_running_remote_commands(&self, now: u64) -> Result<u64> {
        let connection = self.lock()?;
        let transaction = connection.unchecked_transaction()?;
        let mut statement = transaction.prepare(
            "SELECT command_id,payload_json FROM remote_codex_commands WHERE state='running'",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        for (command_id, encoded) in &rows {
            let payload: Value = serde_json::from_str(encoded)?;
            transaction.execute(
                "UPDATE remote_codex_commands SET state='failed',detail='Agent restarted during Codex command; turn is orphaned',terminal_reported=0,updated_at_unix=?1 WHERE command_id=?2 AND state='running'",
                params![as_i64(now)?,command_id],
            )?;
            insert_event(
                &transaction,
                &format!("{command_id}:orphaned"),
                "codex.turn.orphaned",
                &json!({
                    "command_id":command_id,
                    "session_id":payload.get("session_id"),
                    "project_id":payload.get("project_id"),
                    "detail":"Agent restarted during Codex command"
                }),
                now,
            )?;
            if let (Some(session_id), Some(project_id), Some(mode)) = (
                payload.get("session_id").and_then(Value::as_str),
                payload.get("project_id").and_then(Value::as_str),
                payload.get("mode").and_then(Value::as_str),
            ) {
                insert_event(
                    &transaction,
                    &format!("{command_id}:session-orphaned"),
                    "codex.session.updated",
                    &json!({
                        "session_id":session_id,"project_id":project_id,"mode":mode,
                        "state":"orphaned","title":null,"active_turn_id":null,
                        "updated_at_unix":now
                    }),
                    now,
                )?;
            }
        }
        transaction.commit()?;
        Ok(rows.len() as u64)
    }

    pub fn expire_remote_command(&self, command_id: &str, now: u64) -> Result<()> {
        self.lock()?.execute("UPDATE remote_codex_commands SET state='expired',updated_at_unix=?1 WHERE command_id=?2 AND state='accepted'",params![as_i64(now)?,command_id])?;
        Ok(())
    }

    pub fn pending_events(&self, agent_id: &str, limit: usize) -> Result<Vec<AgentEvent>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT sequence,event_id,event_type,payload_json,created_at_unix FROM event_outbox
              WHERE acknowledged=0 ORDER BY sequence LIMIT ?1",
        )?;
        let rows = statement.query_map([i64::try_from(limit)?], |row| {
            let payload_json: String = row.get(3)?;
            let mut payload: Value = serde_json::from_str(&payload_json).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    3,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?;
            if let Value::Object(ref mut map) = payload {
                map.insert("agent_id".to_owned(), Value::String(agent_id.to_owned()));
            }
            Ok(AgentEvent {
                protocol: FARHELM_PROTOCOL.to_owned(),
                sequence: row_u64(row, 0)?,
                event_id: row.get(1)?,
                agent_id: agent_id.to_owned(),
                event_type: row.get(2)?,
                payload,
                created_at_unix: row_u64(row, 4)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn acknowledge_events(&self, event_ids: &[String]) -> Result<()> {
        let connection = self.lock()?;
        let transaction = connection.unchecked_transaction()?;
        for event_id in event_ids {
            transaction.execute(
                "UPDATE event_outbox SET acknowledged=1 WHERE event_id=?1",
                [event_id],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    fn get(&self, watch_id: &str) -> Result<Option<WatchRecord>> {
        self.lock()?
            .query_row(
                "SELECT watch_id, project_id, name, pid, proc_start_time, uid, log_path,
                    session_id, new_session_mode, state, detail, updated_at_unix
               FROM experiment_watches WHERE watch_id=?1",
                [watch_id],
                watch_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>> {
        self.connection
            .lock()
            .map_err(|_| anyhow!("Agent experiment database lock was poisoned"))
    }
}

#[derive(Debug)]
struct ProcessIdentity {
    start_time: u64,
    uid: u32,
    cwd: PathBuf,
}

fn read_process_identity(pid: u32) -> Result<ProcessIdentity> {
    let root = PathBuf::from(format!("/proc/{pid}"));
    let stat = fs::read_to_string(root.join("stat")).context("PID is not running")?;
    let after_name = stat.rsplit_once(')').context("invalid /proc stat")?.1;
    let fields = after_name.split_whitespace().collect::<Vec<_>>();
    let start_time = fields
        .get(19)
        .context("/proc stat has no start time")?
        .parse::<u64>()?;
    let uid = fs::metadata(&root)?.uid();
    let cwd = fs::canonicalize(root.join("cwd")).context("failed to resolve PID cwd")?;
    Ok(ProcessIdentity {
        start_time,
        uid,
        cwd,
    })
}

fn classify_log(path: &Path, patterns: &ProjectMatchers) -> Result<(ExperimentState, String)> {
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((ExperimentState::Unknown, "log file is missing".to_owned()));
        }
        Err(error) => {
            return Err(error).with_context(|| format!("failed to open {}", path.display()));
        }
    };
    use std::io::{Read, Seek, SeekFrom};
    let length = file.metadata()?.len();
    file.seek(SeekFrom::Start(length.saturating_sub(LOG_TAIL_LIMIT)))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let tail = String::from_utf8_lossy(&bytes);
    for pattern in &patterns.failure {
        if Regex::new(pattern)
            .with_context(|| format!("invalid failure pattern {pattern}"))?
            .is_match(&tail)
        {
            return Ok((
                ExperimentState::Failed,
                format!("matched failure marker: {pattern}"),
            ));
        }
    }
    for pattern in &patterns.success {
        if Regex::new(pattern)
            .with_context(|| format!("invalid success pattern {pattern}"))?
            .is_match(&tail)
        {
            return Ok((
                ExperimentState::Succeeded,
                format!("matched success marker: {pattern}"),
            ));
        }
    }
    Ok((
        ExperimentState::Unknown,
        "no configured completion marker matched".to_owned(),
    ))
}

fn validate_relative_path(path: &Path) -> Result<()> {
    ensure!(
        !path.as_os_str().is_empty() && !path.is_absolute(),
        "log path must be relative"
    );
    ensure!(
        path.components()
            .all(|part| matches!(part, Component::Normal(_))),
        "log path must not contain traversal"
    );
    Ok(())
}

fn watch_payload(
    watch_id: &str,
    registration: &WatchRegistration,
    state: ExperimentState,
    detail: Option<&str>,
    now: u64,
) -> Value {
    json!({"watch_id":watch_id,"project_id":registration.project_id,"name":registration.name,"pid":registration.pid,"state":state,"session_id":registration.session_id,"detail":detail,"updated_at_unix":now})
}

fn insert_event(
    connection: &Connection,
    event_id: &str,
    event_type: &str,
    payload: &Value,
    now: u64,
) -> Result<()> {
    connection.execute(
        "INSERT OR IGNORE INTO event_outbox (event_id,event_type,payload_json,created_at_unix) VALUES (?1,?2,?3,?4)",
        params![event_id,event_type,serde_json::to_string(payload)?,as_i64(now)?],
    )?;
    Ok(())
}

fn ensure_remote_command_columns(connection: &Connection) -> Result<()> {
    let mut statement = connection.prepare("PRAGMA table_info(remote_codex_commands)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<HashSet<_>>>()?;
    drop(statement);
    for (name, declaration) in [
        (
            "terminal_reported",
            "INTEGER NOT NULL DEFAULT 0 CHECK (terminal_reported IN (0,1))",
        ),
        ("data_json", "TEXT"),
        ("detail", "TEXT"),
    ] {
        if !columns.contains(name) {
            connection.execute_batch(&format!(
                "ALTER TABLE remote_codex_commands ADD COLUMN {name} {declaration}"
            ))?;
        }
    }
    Ok(())
}

fn json_conversion(index: usize) -> impl FnOnce(serde_json::Error) -> rusqlite::Error {
    move |error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    }
}

fn watch_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WatchRecord> {
    Ok(WatchRecord {
        watch_id: row.get(0)?,
        project_id: row.get(1)?,
        name: row.get(2)?,
        pid: u32::try_from(row.get::<_, i64>(3)?).map_err(conversion(3))?,
        proc_start_time: row_u64(row, 4)?,
        uid: u32::try_from(row.get::<_, i64>(5)?).map_err(conversion(5))?,
        log_path: PathBuf::from(row.get::<_, String>(6)?),
        session_id: row.get(7)?,
        new_session_mode: row.get(8)?,
        state: parse_state(&row.get::<_, String>(9)?)?,
        detail: row.get(10)?,
        updated_at_unix: row_u64(row, 11)?,
    })
}

fn conversion(index: usize) -> impl FnOnce(std::num::TryFromIntError) -> rusqlite::Error {
    move |error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    }
}

fn parse_state(value: &str) -> rusqlite::Result<ExperimentState> {
    match value {
        "watching" => Ok(ExperimentState::Watching),
        "succeeded" => Ok(ExperimentState::Succeeded),
        "failed" => Ok(ExperimentState::Failed),
        "unknown" => Ok(ExperimentState::Unknown),
        "cancelled" => Ok(ExperimentState::Cancelled),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

const fn state_name(state: ExperimentState) -> &'static str {
    match state {
        ExperimentState::Watching => "watching",
        ExperimentState::Succeeded => "succeeded",
        ExperimentState::Failed => "failed",
        ExperimentState::Unknown => "unknown",
        ExperimentState::Cancelled => "cancelled",
    }
}

const fn action_name(action: CommandAction) -> &'static str {
    match action {
        CommandAction::AgentProbe => "agent.probe",
        CommandAction::CodexSessionCreate => "codex.session.create",
        CommandAction::CodexSessionResume => "codex.session.resume",
        CommandAction::CodexTurnStart => "codex.turn.start",
        CommandAction::CodexTurnSteer => "codex.turn.steer",
        CommandAction::CodexTurnInterrupt => "codex.turn.interrupt",
    }
}

fn parse_action(value: &str) -> rusqlite::Result<CommandAction> {
    match value {
        "codex.session.create" => Ok(CommandAction::CodexSessionCreate),
        "codex.session.resume" => Ok(CommandAction::CodexSessionResume),
        "codex.turn.start" => Ok(CommandAction::CodexTurnStart),
        "codex.turn.steer" => Ok(CommandAction::CodexTurnSteer),
        "codex.turn.interrupt" => Ok(CommandAction::CodexTurnInterrupt),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn as_i64(value: u64) -> Result<i64> {
    i64::try_from(value).context("value exceeds SQLite INTEGER range")
}
fn row_u64(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    u64::try_from(row.get::<_, i64>(index)?).map_err(conversion(index))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn failure_markers_win_over_success_markers() {
        let directory = tempfile::tempdir().unwrap();
        let log = directory.path().join("train.log");
        writeln!(
            fs::File::create(&log).unwrap(),
            "TRAINING COMPLETE\nCUDA ERROR"
        )
        .unwrap();
        let result = classify_log(
            &log,
            &ProjectMatchers {
                success: vec!["TRAINING COMPLETE".into()],
                failure: vec!["CUDA ERROR".into()],
            },
        )
        .unwrap();
        assert_eq!(result.0, ExperimentState::Failed);
    }

    #[test]
    fn missing_log_is_unknown() {
        let result = classify_log(
            Path::new("/definitely/missing/farhelm.log"),
            &ProjectMatchers {
                success: vec!["done".into()],
                failure: vec!["fail".into()],
            },
        )
        .unwrap();
        assert_eq!(result.0, ExperimentState::Unknown);
    }

    #[test]
    fn exited_pid_queues_success_prompt_exactly_once() {
        let directory = tempfile::tempdir().unwrap();
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .current_dir(directory.path())
            .spawn()
            .unwrap();
        fs::write(directory.path().join("train.log"), "TRAINING COMPLETE\n").unwrap();
        let database = directory.path().join("agent.db");
        let store = ExperimentStore::open(&database).unwrap();
        let watch = store
            .register(
                &WatchRegistration {
                    project_id: "test".into(),
                    project_root: directory.path().to_owned(),
                    name: "short test".into(),
                    pid: child.id(),
                    log_path: PathBuf::from("train.log"),
                    session_id: None,
                    new_session_mode: Some("inspect".into()),
                    success_prompt: Some("inspect results".into()),
                },
                100,
            )
            .unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        let matchers = std::collections::BTreeMap::from([(
            "test".into(),
            ProjectMatchers {
                success: vec!["TRAINING COMPLETE".into()],
                failure: vec!["FAILED".into()],
            },
        )]);
        assert_eq!(store.inspect(&matchers, 101).unwrap().len(), 1);
        assert_eq!(store.inspect(&matchers, 102).unwrap().len(), 0);
        let pending = store.pending_auto_prompts(101).unwrap();
        assert_eq!(pending.len(), 1);
        assert!(store.claim_auto_prompt(&watch.watch_id).unwrap());
        assert!(!store.claim_auto_prompt(&watch.watch_id).unwrap());
        assert!(
            store
                .link_watch_session(&watch.watch_id, "ses_new", 102)
                .unwrap()
        );
        store
            .finish_auto_prompt(
                &watch.watch_id,
                "codex.turn.completed",
                &json!({"session_id":"ses_new","turn_id":"turn_1"}),
                103,
            )
            .unwrap();
        assert_eq!(
            store
                .get(&watch.watch_id)
                .unwrap()
                .unwrap()
                .session_id
                .as_deref(),
            Some("ses_new")
        );
        assert!(
            store
                .pending_events("agent-a", 20)
                .unwrap()
                .iter()
                .any(|event| {
                    event.event_type == "experiment.updated"
                        && event.payload.get("session_id").and_then(Value::as_str)
                            == Some("ses_new")
                })
        );
        drop(store);
        assert!(
            ExperimentStore::open(&database)
                .unwrap()
                .pending_auto_prompts(102)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn terminal_remote_report_survives_database_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("agent.db");
        let store = ExperimentStore::open(&database).unwrap();
        store
            .receive_remote_command(
                &AgentCommand {
                    protocol: FARHELM_PROTOCOL.into(),
                    command_id: "cmd_terminal".into(),
                    agent_id: "agent-a".into(),
                    action: CommandAction::CodexTurnStart,
                    created_at_unix: 10,
                    expires_at_unix: 100,
                    payload: Some(json!({"project_id":"p","session_id":"s","prompt":"go"})),
                },
                10,
            )
            .unwrap();
        store
            .mark_remote_accepted_reported("cmd_terminal", 11)
            .unwrap();
        assert!(store.claim_remote_command("cmd_terminal", 12).unwrap());
        store
            .finish_remote_command(
                "cmd_terminal",
                farhelm_protocol::CommandState::Completed,
                Some(&json!({"turn_id":"t"})),
                None,
                13,
            )
            .unwrap();
        drop(store);

        let reopened = ExperimentStore::open(&database).unwrap();
        let reports = reopened.pending_remote_reports().unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].command_id, "cmd_terminal");
        assert_eq!(reports[0].data.as_ref().unwrap()["turn_id"], "t");
        reopened
            .mark_remote_terminal_reported("cmd_terminal", 14)
            .unwrap();
        assert!(reopened.pending_remote_reports().unwrap().is_empty());
    }

    #[test]
    fn running_remote_turn_becomes_orphaned_without_replay_after_restart() {
        let store = ExperimentStore::open(Path::new(":memory:")).unwrap();
        store
            .receive_remote_command(
                &AgentCommand {
                    protocol: FARHELM_PROTOCOL.into(),
                    command_id: "cmd_orphan".into(),
                    agent_id: "agent-a".into(),
                    action: CommandAction::CodexTurnStart,
                    created_at_unix: 10,
                    expires_at_unix: 100,
                    payload: Some(json!({
                        "project_id":"p","session_id":"s","mode":"inspect","prompt":"go"
                    })),
                },
                10,
            )
            .unwrap();
        store
            .mark_remote_accepted_reported("cmd_orphan", 11)
            .unwrap();
        assert!(store.claim_remote_command("cmd_orphan", 12).unwrap());

        assert_eq!(store.orphan_running_remote_commands(13).unwrap(), 1);
        assert!(store.pending_remote_commands().unwrap().is_empty());
        let report = store.pending_remote_reports().unwrap().pop().unwrap();
        assert_eq!(report.command_id, "cmd_orphan");
        assert_eq!(report.state, farhelm_protocol::CommandState::Failed);
        let events = store.pending_events("agent-a", 20).unwrap();
        assert!(
            events
                .iter()
                .any(|event| event.event_type == "codex.turn.orphaned")
        );
        assert!(events.iter().any(|event| {
            event.event_type == "codex.session.updated"
                && event.payload.get("state").and_then(Value::as_str) == Some("orphaned")
        }));
    }

    #[test]
    fn edit_session_binding_preserves_isolated_cwd() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("agent.db");
        let cwd = directory.path().join("worktrees/session-a");
        let store = ExperimentStore::open(&database).unwrap();
        store
            .bind_session("session-a", "project-a", &cwd, "edit", 10)
            .unwrap();
        drop(store);
        let binding = ExperimentStore::open(&database)
            .unwrap()
            .session_binding("session-a")
            .unwrap()
            .unwrap();
        assert_eq!(binding.project_id, "project-a");
        assert_eq!(binding.cwd, cwd);
        assert_eq!(binding.mode, "edit");
    }
}
