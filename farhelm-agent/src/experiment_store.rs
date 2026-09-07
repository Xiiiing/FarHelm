use std::{
    collections::{BTreeMap, HashSet},
    fs,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use farhelm_protocol::{
    AgentCommand, AgentEvent, CodexScheduleState, CodexScheduleTrigger, CommandAction,
    ExperimentState, FARHELM_PROTOCOL,
};
use rand::RngCore;
use regex::Regex;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[path = "execution_store.rs"]
pub(crate) mod execution;
pub use execution::ScriptReport;

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
pub struct ScheduledPrompt {
    pub schedule_id: String,
    pub project_id: String,
    pub session_id: String,
    pub prompt: String,
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

#[derive(Debug, Clone)]
pub struct ApprovedProject {
    pub project_id: String,
    pub path: PathBuf,
    pub success_patterns: Vec<String>,
    pub failure_patterns: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ProjectCandidate {
    pub candidate_id: String,
    pub path: PathBuf,
    pub display_name: String,
    pub suggested_project_id: String,
    pub session_count: u64,
    pub state: String,
    pub updated_at_unix: u64,
}

#[derive(Clone)]
pub struct ExperimentStore {
    connection: Arc<Mutex<Connection>>,
    path: PathBuf,
}

impl ExperimentStore {
    pub async fn open_async(path: &Path) -> Result<Self> {
        let path = path.to_owned();
        crate::runtime_tasks::blocking(move || Self::open(&path)).await
    }
    pub async fn background<T: Send + 'static>(
        &self,
        work: impl FnOnce(&Self) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let store = self.clone();
        crate::runtime_tasks::blocking(move || work(&store)).await
    }

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
        crate::migrations::apply(&connection)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            path: path.to_owned(),
        })
    }

    pub fn import_config_projects(
        &self,
        projects: &BTreeMap<String, crate::config::ProjectSection>,
        now: u64,
    ) -> Result<()> {
        let connection = self.lock()?;
        let transaction = crate::migrations::write_transaction(&connection)?;
        for (project_id, project) in projects {
            let path = fs::canonicalize(&project.path).unwrap_or_else(|_| project.path.clone());
            transaction.execute(
                "INSERT INTO approved_projects (project_id,path,success_patterns_json,failure_patterns_json,updated_at_unix) VALUES (?1,?2,?3,?4,?5)
                 ON CONFLICT(project_id) DO UPDATE SET path=excluded.path,success_patterns_json=excluded.success_patterns_json,failure_patterns_json=excluded.failure_patterns_json,updated_at_unix=excluded.updated_at_unix",
                params![project_id,path.to_string_lossy(),serde_json::to_string(&project.success_patterns)?,serde_json::to_string(&project.failure_patterns)?,as_i64(now)?],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn approved_projects(&self) -> Result<BTreeMap<String, ApprovedProject>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare("SELECT project_id,path,success_patterns_json,failure_patterns_json FROM approved_projects ORDER BY project_id")?;
        let rows = statement.query_map([], |row| {
            let success: String = row.get(2)?;
            let failure: String = row.get(3)?;
            Ok(ApprovedProject {
                project_id: row.get(0)?,
                path: PathBuf::from(row.get::<_, String>(1)?),
                success_patterns: serde_json::from_str(&success).map_err(json_conversion(2))?,
                failure_patterns: serde_json::from_str(&failure).map_err(json_conversion(3))?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map(|rows| {
                rows.into_iter()
                    .map(|row| (row.project_id.clone(), row))
                    .collect()
            })
            .map_err(Into::into)
    }

    pub fn set_project_matchers(
        &self,
        project_id: &str,
        success: &[String],
        failure: &[String],
        now: u64,
    ) -> Result<()> {
        ensure!(
            !success.is_empty() && !failure.is_empty(),
            "both success and failure patterns are required"
        );
        for pattern in success.iter().chain(failure) {
            Regex::new(pattern)
                .with_context(|| format!("invalid experiment log regex {pattern:?}"))?;
        }
        ensure!(self.lock()?.execute(
            "UPDATE approved_projects SET success_patterns_json=?1,failure_patterns_json=?2,updated_at_unix=?3 WHERE project_id=?4",
            params![serde_json::to_string(success)?,serde_json::to_string(failure)?,as_i64(now)?,project_id],
        )? == 1, "approved project does not exist");
        Ok(())
    }

    pub fn upsert_discovered_project(
        &self,
        path: &Path,
        display_name: &str,
        suggested_project_id: &str,
        session_count: u64,
        now: u64,
    ) -> Result<(ProjectCandidate, bool)> {
        ensure!(
            path.is_absolute(),
            "discovered project path must be absolute"
        );
        let connection = self.lock()?;
        let existing: Option<(String,String,u64,String)> = connection.query_row(
            "SELECT candidate_id,suggested_project_id,session_count,state FROM discovered_projects WHERE path=?1",
            [path.to_string_lossy().as_ref()],
            |row| Ok((row.get(0)?,row.get(1)?,row_u64(row,2)?,row.get(3)?)),
        ).optional()?;
        let approved_id: Option<String> = connection
            .query_row(
                "SELECT project_id FROM approved_projects WHERE path=?1",
                [path.to_string_lossy().as_ref()],
                |row| row.get(0),
            )
            .optional()?;
        let desired_state = if approved_id.is_some() {
            "approved"
        } else {
            "discovered"
        };
        let (candidate_id, effective_id, changed) = match existing {
            Some((candidate_id, effective_id, previous_count, state)) => {
                let effective_id = approved_id.clone().unwrap_or(effective_id);
                (
                    candidate_id,
                    effective_id,
                    previous_count != session_count || state != desired_state,
                )
            }
            None => {
                let mut bytes = [0_u8; 16];
                rand::rng().fill_bytes(&mut bytes);
                let candidate_id = format!(
                    "prj_{}",
                    bytes
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<String>()
                );
                let effective_id = approved_id.clone().unwrap_or(available_project_id(
                    &connection,
                    suggested_project_id,
                    path,
                )?);
                (candidate_id, effective_id, true)
            }
        };
        connection.execute(
            "INSERT INTO discovered_projects (candidate_id,path,display_name,suggested_project_id,session_count,state,updated_at_unix) VALUES (?1,?2,?3,?4,?5,?6,?7)
             ON CONFLICT(path) DO UPDATE SET display_name=excluded.display_name,suggested_project_id=excluded.suggested_project_id,session_count=excluded.session_count,state=excluded.state,updated_at_unix=excluded.updated_at_unix",
            params![candidate_id,path.to_string_lossy(),display_name,effective_id,as_i64(session_count)?,desired_state,as_i64(now)?],
        )?;
        Ok((
            ProjectCandidate {
                candidate_id,
                path: path.to_owned(),
                display_name: display_name.to_owned(),
                suggested_project_id: effective_id,
                session_count,
                state: desired_state.to_owned(),
                updated_at_unix: now,
            },
            changed,
        ))
    }

    pub fn approve_candidates(
        &self,
        candidate_ids: &[String],
        now: u64,
    ) -> Result<Vec<ProjectCandidate>> {
        ensure!(
            !candidate_ids.is_empty() && candidate_ids.len() <= 100,
            "invalid candidate list"
        );
        let connection = self.lock()?;
        let transaction = crate::migrations::write_transaction(&connection)?;
        let mut approved = Vec::new();
        for candidate_id in candidate_ids {
            let candidate = transaction.query_row(
                "SELECT candidate_id,path,display_name,suggested_project_id,session_count,state,updated_at_unix FROM discovered_projects WHERE candidate_id=?1",
                [candidate_id], project_candidate_from_row,
            ).optional()?.with_context(|| format!("unknown project candidate {candidate_id}"))?;
            transaction.execute(
                "INSERT INTO approved_projects (project_id,path,updated_at_unix) VALUES (?1,?2,?3)
                 ON CONFLICT(project_id) DO UPDATE SET path=excluded.path,updated_at_unix=excluded.updated_at_unix",
                params![candidate.suggested_project_id,candidate.path.to_string_lossy(),as_i64(now)?],
            )?;
            transaction.execute("UPDATE discovered_projects SET state='approved',updated_at_unix=?1 WHERE candidate_id=?2", params![as_i64(now)?,candidate_id])?;
            approved.push(ProjectCandidate {
                state: "approved".to_owned(),
                updated_at_unix: now,
                ..candidate
            });
        }
        transaction.commit()?;
        Ok(approved)
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
        let transaction = crate::migrations::write_transaction(&connection)?;
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
        let transaction = crate::migrations::write_transaction(&connection)?;
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
            let transaction = crate::migrations::write_transaction(&connection)?;
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
                let automatic: bool = transaction.query_row(
                    "SELECT success_prompt IS NOT NULL FROM experiment_watches WHERE watch_id=?1",
                    [&watch.watch_id],
                    |r| r.get(0),
                )?;
                if state == ExperimentState::Succeeded && automatic {
                    execution::enqueue(
                        &transaction,
                        &watch.watch_id,
                        watch.session_id.as_deref().unwrap_or(&watch.watch_id),
                    )?;
                }
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
        let connection = self.lock()?;
        let tx = crate::migrations::write_transaction(&connection)?;
        let session:Option<Option<String>>=tx.query_row("SELECT session_id FROM experiment_watches WHERE watch_id=?1 AND auto_prompt_claimed=0 AND state='succeeded'",[watch_id],|r|r.get(0)).optional()?;
        let Some(session) = session else {
            return Ok(false);
        };
        execution::enqueue(&tx, watch_id, session.as_deref().unwrap_or(watch_id))?;
        if !execution::claim(&tx, watch_id)? {
            tx.commit()?;
            return Ok(false);
        }
        let changed=tx.execute("UPDATE experiment_watches SET auto_prompt_claimed=1 WHERE watch_id=?1 AND auto_prompt_claimed=0",[watch_id])?;
        tx.commit()?;
        Ok(changed == 1)
    }

    pub fn pending_auto_prompts(&self, now: u64) -> Result<Vec<AutoPrompt>> {
        let connection = self.lock()?;
        connection.execute("UPDATE execution_queue SET state='completed' WHERE state='queued' AND job_id IN (SELECT watch_id FROM experiment_watches WHERE auto_prompt_claimed=0 AND state='succeeded' AND updated_at_unix+86400<=?1)",[as_i64(now)?])?;
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
        let transaction = crate::migrations::write_transaction(&connection)?;
        let session_id = payload.get("session_id").and_then(Value::as_str);
        let changed = transaction.execute(
            "UPDATE experiment_watches
                SET auto_prompt_claimed=2,updated_at_unix=?1,session_id=COALESCE(?2,session_id)
              WHERE watch_id=?3 AND auto_prompt_claimed=1",
            params![as_i64(now)?, session_id, watch_id],
        )?;
        ensure!(changed == 1, "auto prompt was not running");
        execution::finish(&transaction, watch_id)?;
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
        let transaction = crate::migrations::write_transaction(&connection)?;
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
        let tx = crate::migrations::write_transaction(&connection)?;
        insert_event(&tx, event_id, event_type, payload, now)?;
        tx.commit()?;
        Ok(())
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

    pub fn discover_session(
        &self,
        session_id: &str,
        project_id: &str,
        cwd: &Path,
        title: &Value,
        archived: bool,
        updated: u64,
    ) -> Result<()> {
        let connection = self.lock()?;
        let transaction = crate::migrations::write_transaction(&connection)?;
        transaction.execute(
            "INSERT OR IGNORE INTO codex_session_bindings (session_id,project_id,cwd,mode,updated_at_unix) VALUES (?1,?2,?3,'inspect',?4)",
            params![session_id,project_id,cwd.to_string_lossy(),as_i64(updated)?],
        )?;
        let (bound_project, mode): (String, String) = transaction.query_row(
            "SELECT project_id,mode FROM codex_session_bindings WHERE session_id=?1",
            [session_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        ensure!(
            bound_project == project_id,
            "discovered session project changed"
        );
        let payload = json!({"session_id":session_id,"project_id":project_id,"mode":mode,
            "update_kind":"metadata","state":if archived {"archived"}else{"idle"},
            "title":title,"updated_at_unix":updated});
        let fingerprint = Sha256::digest(serde_json::to_vec(&payload)?)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        insert_event(
            &transaction,
            &format!("session-index:{session_id}:{fingerprint}"),
            "codex.session.updated",
            &payload,
            updated,
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn display_bindings(&self, project: Option<&str>, ids: Option<&[String]>) -> Result<Value> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT b.session_id,b.project_id,b.cwd FROM codex_session_bindings b JOIN approved_projects p ON p.project_id=b.project_id WHERE (?1 IS NULL OR b.project_id=?1)")?;
        let rows = statement.query_map([project], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut bindings = serde_json::Map::new();
        for row in rows {
            let (id, project_id, cwd) = row?;
            if ids.is_none_or(|ids| ids.contains(&id)) {
                bindings.insert(id, json!({"project_id":project_id,"cwd":cwd}));
            }
        }
        Ok(Value::Object(bindings))
    }

    pub fn link_watch_session(&self, watch_id: &str, session_id: &str, now: u64) -> Result<bool> {
        let connection = self.lock()?;
        let transaction = crate::migrations::write_transaction(&connection)?;
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
        let guard = self.lock()?;
        let connection = crate::migrations::write_transaction(&guard)?;
        if let Some((action, expires, encoded)) = connection.query_row(
            "SELECT action,expires_at_unix,payload_json FROM remote_codex_commands WHERE command_id=?1",
            [&command.command_id], |row| Ok((row.get::<_,String>(0)?,row_u64(row,1)?,row.get::<_,String>(2)?)),
        ).optional()? {
            ensure!(action == action_name(command.action) && expires == command.expires_at_unix && encoded == serde_json::to_string(payload)?, "duplicate command identity mismatch");
            connection.execute("UPDATE remote_codex_commands SET terminal_reported=0 WHERE command_id=?1 AND state IN ('completed','failed','expired')", [&command.command_id])?;
            connection.commit()?;
            return Ok(());
        }
        connection.execute(
            "INSERT INTO remote_codex_commands (command_id,action,expires_at_unix,payload_json,state,updated_at_unix) VALUES (?1,?2,?3,?4,?5,?6)",
            params![command.command_id,action_name(command.action),as_i64(command.expires_at_unix)?,serde_json::to_string(payload)?,if now>=command.expires_at_unix{"expired"}else{"accepted"},as_i64(now)?],
        )?;
        if command.action == CommandAction::CodexTurnStart {
            execution::enqueue(
                &connection,
                &command.command_id,
                required_payload_string(payload, "session_id")?,
            )?;
            if now >= command.expires_at_unix {
                execution::finish(&connection, &command.command_id)?;
            }
        }
        connection.commit()?;
        Ok(())
    }

    pub fn pending_remote_commands(&self) -> Result<Vec<RemoteCommand>> {
        self.remote_candidates(false, 0)
    }

    pub fn runnable_remote_commands(&self, now: u64) -> Result<Vec<RemoteCommand>> {
        self.remote_candidates(true, now)
    }

    fn remote_candidates(&self, runnable: bool, now: u64) -> Result<Vec<RemoteCommand>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT command_id,action,expires_at_unix,payload_json,accepted_reported FROM remote_codex_commands r WHERE state='accepted'
             AND (?1=0 OR expires_at_unix<=?2 OR (accepted_reported=1 AND (action!='codex.turn.start' OR EXISTS(
               SELECT 1 FROM execution_queue q WHERE q.job_id=r.command_id AND q.state='queued'
               AND NOT EXISTS(SELECT 1 FROM execution_queue older WHERE older.session_id=q.session_id AND (older.state='running' OR (older.state='queued' AND older.sequence<q.sequence)))))))
             ORDER BY accepted_reported,CASE WHEN ?1=1 AND action!='codex.turn.start' THEN 0 ELSE 1 END,updated_at_unix,command_id LIMIT 8",
        )?;
        let rows = statement.query_map(params![runnable, as_i64(now)?], |row| {
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
        Ok(self.lock()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM execution_queue WHERE session_id=?1 AND state='running')",
            [session_id],
            |r| r.get(0),
        )?)
    }

    pub fn mark_remote_accepted_reported(&self, command_id: &str, now: u64) -> Result<()> {
        self.lock()?.execute("UPDATE remote_codex_commands SET accepted_reported=1,updated_at_unix=?1 WHERE command_id=?2 AND state='accepted'",params![as_i64(now)?,command_id])?;
        Ok(())
    }

    pub fn claim_remote_command(&self, command_id: &str, now: u64) -> Result<bool> {
        let connection = self.lock()?;
        let tx = crate::migrations::write_transaction(&connection)?;
        if tx.execute("UPDATE remote_codex_commands SET state='expired',updated_at_unix=?2 WHERE command_id=?1 AND state='accepted' AND expires_at_unix<=?2",params![command_id,as_i64(now)?])?==1 {
            execution::finish(&tx,command_id)?;
            tx.commit()?;
            return Ok(false);
        }
        let row:Option<(String,String)>=tx.query_row("SELECT action,payload_json FROM remote_codex_commands WHERE command_id=?1 AND state='accepted' AND accepted_reported=1",[command_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let Some((action, encoded)) = row else {
            return Ok(false);
        };
        if action == "codex.turn.start" {
            let payload: Value = serde_json::from_str(&encoded)?;
            let session = required_payload_string(&payload, "session_id")?;
            execution::enqueue(&tx, command_id, session)?;
            if !execution::claim(&tx, command_id)? {
                tx.commit()?;
                return Ok(false);
            }
        }
        let changed=tx.execute("UPDATE remote_codex_commands SET state='running',updated_at_unix=?1 WHERE command_id=?2 AND state='accepted'",params![as_i64(now)?,command_id])?;
        tx.commit()?;
        Ok(changed == 1)
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
        let connection = self.lock()?;
        let tx = crate::migrations::write_transaction(&connection)?;
        ensure!(tx.execute(
            "UPDATE remote_codex_commands SET state=?1,data_json=?2,detail=?3,terminal_reported=0,updated_at_unix=?4 WHERE command_id=?5 AND state='running'",
            params![if state==farhelm_protocol::CommandState::Completed{"completed"}else{"failed"},data.map(serde_json::to_string).transpose()?,detail,as_i64(now)?,command_id]
        )?==1,"remote command was not running");
        let (action, encoded): (String, String) = tx.query_row(
            "SELECT action,payload_json FROM remote_codex_commands WHERE command_id=?1",
            [command_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if action == "codex.turn.start" {
            let payload: Value = serde_json::from_str(&encoded)?;
            let event = if state == farhelm_protocol::CommandState::Completed {
                "codex.turn.completed"
            } else if data.and_then(|v| v.get("status")).and_then(Value::as_str) == Some("orphaned")
            {
                "codex.turn.orphaned"
            } else {
                "codex.turn.failed"
            };
            insert_event(
                &tx,
                &format!("{command_id}:terminal"),
                event,
                &json!({"operation_id":command_id,"command_id":command_id,"session_id":payload.get("session_id"),"project_id":payload.get("project_id"),"data":data}),
                now,
            )?;
        }
        execution::finish(&tx, command_id)?;
        tx.commit()?;
        Ok(())
    }

    pub fn pending_remote_reports(&self) -> Result<Vec<RemoteCommandReport>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT command_id,state,data_json,detail FROM remote_codex_commands
              WHERE state IN ('completed','failed','expired') AND terminal_reported=0
              ORDER BY updated_at_unix,command_id LIMIT 8",
        )?;
        let rows = statement.query_map([], |row| {
            let state: String = row.get(1)?;
            let data: Option<String> = row.get(2)?;
            Ok(RemoteCommandReport {
                command_id: row.get(0)?,
                state: if state == "completed" {
                    farhelm_protocol::CommandState::Completed
                } else if state == "expired" {
                    farhelm_protocol::CommandState::Expired
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

    /// Retried deliveries return the actual persisted outcome, even after TTL.
    pub fn remote_receipt(&self, command_id: &str) -> Result<RemoteCommandReport> {
        self.lock()?
            .query_row(
                "SELECT state,data_json,detail FROM remote_codex_commands WHERE command_id=?1",
                [command_id],
                |row| {
                    let state: String = row.get(0)?;
                    let data: Option<String> = row.get(1)?;
                    Ok(RemoteCommandReport {
                        command_id: command_id.to_owned(),
                        state: match state.as_str() {
                            "completed" => farhelm_protocol::CommandState::Completed,
                            "failed" => farhelm_protocol::CommandState::Failed,
                            "expired" => farhelm_protocol::CommandState::Expired,
                            _ => farhelm_protocol::CommandState::Accepted,
                        },
                        data: data
                            .map(|value| serde_json::from_str(&value))
                            .transpose()
                            .map_err(json_conversion(1))?,
                        detail: row.get(2)?,
                    })
                },
            )
            .map_err(Into::into)
    }

    pub fn mark_remote_terminal_reported(&self, command_id: &str, now: u64) -> Result<()> {
        self.lock()?.execute(
            "UPDATE remote_codex_commands SET terminal_reported=1,updated_at_unix=?1
              WHERE command_id=?2 AND state IN ('completed','failed','expired')",
            params![as_i64(now)?, command_id],
        )?;
        Ok(())
    }

    pub fn orphan_running_remote_commands(&self, now: u64) -> Result<u64> {
        let connection = self.lock()?;
        let transaction = crate::migrations::write_transaction(&connection)?;
        let mut statement = transaction.prepare(
            "SELECT command_id,payload_json,action FROM remote_codex_commands WHERE state='running'",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        for (command_id, encoded, action) in &rows {
            let payload: Value = serde_json::from_str(encoded)?;
            transaction.execute(
                "UPDATE remote_codex_commands SET state='failed',detail='Agent restarted during Codex command; turn is orphaned',terminal_reported=0,updated_at_unix=?1 WHERE command_id=?2 AND state='running'",
                params![as_i64(now)?,command_id],
            )?;
            if action == "codex.turn.start" {
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
            }
            if action == "codex.turn.start"
                && let (Some(session_id), Some(project_id), Some(mode)) = (
                    payload.get("session_id").and_then(Value::as_str),
                    payload.get("project_id").and_then(Value::as_str),
                    payload.get("mode").and_then(Value::as_str),
                )
            {
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
        let connection = self.lock()?;
        let tx = crate::migrations::write_transaction(&connection)?;
        if tx.execute("UPDATE remote_codex_commands SET state='expired',updated_at_unix=?1 WHERE command_id=?2 AND state='accepted' AND expires_at_unix<=?1",params![as_i64(now)?,command_id])?==1 {
            execution::finish(&tx, command_id)?;
        }
        tx.commit()?;
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
        let transaction = crate::migrations::write_transaction(&connection)?;
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
    let payload = farhelm_protocol::public_event_payload(event_type, payload);
    let canonical = if matches!(
        event_type,
        "codex.turn.completed" | "codex.turn.failed" | "codex.turn.orphaned"
    ) {
        payload
            .get("operation_id")
            .or_else(|| payload.get("command_id"))
            .or_else(|| payload.get("watch_id"))
            .and_then(Value::as_str)
            .map(|id| (id, format!("execution:{id}:terminal")))
    } else {
        None
    };
    if let Some((id, _)) = &canonical {
        let inserted = connection.execute(
            "INSERT OR IGNORE INTO execution_results VALUES(?1,?2,?3)",
            params![id, event_type, serde_json::to_string(&payload)?],
        )?;
        if inserted == 0 {
            return Ok(());
        }
    }
    let event_id = canonical
        .as_ref()
        .map(|(_, key)| key.as_str())
        .unwrap_or(event_id);
    connection.execute(
        "INSERT OR IGNORE INTO event_outbox (event_id,event_type,payload_json,created_at_unix) VALUES (?1,?2,?3,?4)",
        params![event_id,event_type,serde_json::to_string(&payload)?,as_i64(now)?],
    )?;
    Ok(())
}

pub(crate) fn ensure_remote_command_columns(connection: &Connection) -> Result<()> {
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

fn project_candidate_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectCandidate> {
    Ok(ProjectCandidate {
        candidate_id: row.get(0)?,
        path: PathBuf::from(row.get::<_, String>(1)?),
        display_name: row.get(2)?,
        suggested_project_id: row.get(3)?,
        session_count: row_u64(row, 4)?,
        state: row.get(5)?,
        updated_at_unix: row_u64(row, 6)?,
    })
}

fn available_project_id(connection: &Connection, suggested: &str, path: &Path) -> Result<String> {
    let base = if suggested.is_empty() {
        "project"
    } else {
        suggested
    };
    for suffix in 1..=10_000_u32 {
        let candidate = if suffix == 1 {
            base.to_owned()
        } else {
            format!("{base}-{suffix}")
        };
        let collision: Option<String> = connection.query_row(
            "SELECT path FROM approved_projects WHERE project_id=?1 UNION SELECT path FROM discovered_projects WHERE suggested_project_id=?1 LIMIT 1",
            [&candidate],
            |row| row.get(0),
        ).optional()?;
        if collision
            .as_deref()
            .is_none_or(|existing| existing == path.to_string_lossy())
        {
            return Ok(candidate);
        }
    }
    bail!("could not allocate a unique project ID")
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
        CommandAction::CodexScheduleCreate => "codex.schedule.create",
        CommandAction::CodexScheduleCancel => "codex.schedule.cancel",
        CommandAction::ProjectApprove => "project.approve",
    }
}

fn parse_action(value: &str) -> rusqlite::Result<CommandAction> {
    match value {
        "codex.session.create" => Ok(CommandAction::CodexSessionCreate),
        "codex.session.resume" => Ok(CommandAction::CodexSessionResume),
        "codex.turn.start" => Ok(CommandAction::CodexTurnStart),
        "codex.turn.steer" => Ok(CommandAction::CodexTurnSteer),
        "codex.turn.interrupt" => Ok(CommandAction::CodexTurnInterrupt),
        "codex.schedule.create" => Ok(CommandAction::CodexScheduleCreate),
        "codex.schedule.cancel" => Ok(CommandAction::CodexScheduleCancel),
        "project.approve" => Ok(CommandAction::ProjectApprove),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn required_payload_string<'a>(payload: &'a Value, key: &str) -> Result<&'a str> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("command omitted {key}"))
}

const fn schedule_state_name(state: CodexScheduleState) -> &'static str {
    match state {
        CodexScheduleState::Pending => "pending",
        CodexScheduleState::Queued => "queued",
        CodexScheduleState::Running => "running",
        CodexScheduleState::Completed => "completed",
        CodexScheduleState::Cancelled => "cancelled",
        CodexScheduleState::Skipped => "skipped",
        CodexScheduleState::Missed => "missed",
        CodexScheduleState::Failed => "failed",
        CodexScheduleState::Orphaned => "orphaned",
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
        reopened
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
                200,
            )
            .unwrap();
        let receipt = reopened.remote_receipt("cmd_terminal").unwrap();
        assert_eq!(receipt.state, farhelm_protocol::CommandState::Completed);
        assert_eq!(receipt.data.unwrap()["turn_id"], "t");
        assert!(!reopened.claim_remote_command("cmd_terminal", 200).unwrap());
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
        store
            .discover_session(
                "session-a",
                "project-a",
                &directory.path().join("project"),
                &Value::Null,
                false,
                11,
            )
            .unwrap();
        store
            .discover_session(
                "session-a",
                "project-a",
                &directory.path().join("project"),
                &Value::Null,
                false,
                11,
            )
            .unwrap();
        let events = store.pending_events("agent-a", 100).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].payload["mode"], "edit");
        assert_eq!(events[0].payload["update_kind"], "metadata");
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

    #[test]
    fn discovered_projects_are_approved_without_exposing_paths_in_events() {
        let directory = tempfile::tempdir().unwrap();
        let project = directory.path().join("Work 831");
        fs::create_dir(&project).unwrap();
        let store = ExperimentStore::open(Path::new(":memory:")).unwrap();
        let (candidate, changed) = store
            .upsert_discovered_project(&project, "Work 831", "work-831", 14, 10)
            .unwrap();
        assert!(changed);
        assert_eq!(candidate.session_count, 14);
        let approved = store
            .approve_candidates(std::slice::from_ref(&candidate.candidate_id), 11)
            .unwrap();
        assert_eq!(approved[0].state, "approved");
        let projects = store.approved_projects().unwrap();
        assert_eq!(projects["work-831"].path, project);
        assert!(projects["work-831"].success_patterns.is_empty());
    }

    #[test]
    fn scheduled_prompt_is_due_once_and_prompt_is_erased_at_terminal_state() {
        let store = ExperimentStore::open(Path::new(":memory:")).unwrap();
        let payload = json!({
            "schedule_id":"sch_one","project_id":"project-a","session_id":"session-a",
            "prompt":"continue", "trigger":{"type":"at_time","run_at_unix":100}
        });
        store.create_schedule(&payload, 10).unwrap();
        assert!(store.due_schedules(99).unwrap().is_empty());
        let due = store.due_schedules(100).unwrap();
        assert_eq!(due.len(), 1);
        assert!(store.claim_schedule("sch_one", 100).unwrap());
        assert!(!store.claim_schedule("sch_one", 100).unwrap());
        store
            .finish_schedule("sch_one", CodexScheduleState::Completed, 101)
            .unwrap();
        assert_eq!(store.schedule_detail("sch_one").unwrap()["prompt"], "");
        assert!(
            store
                .pending_events("agent-a", 20)
                .unwrap()
                .iter()
                .any(|event| event.event_type == "codex.schedule.updated"
                    && event.payload["state"] == "completed")
        );
    }

    #[test]
    fn expired_schedule_is_missed_and_pending_schedule_can_be_cancelled() {
        let store = ExperimentStore::open(Path::new(":memory:")).unwrap();
        store.create_schedule(&json!({"schedule_id":"sch_old","project_id":"p","session_id":"s","prompt":"x","trigger":{"type":"at_time","run_at_unix":100}}), 10).unwrap();
        assert!(
            store
                .due_schedules(100 + 24 * 60 * 60 + 1)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store.schedule_detail("sch_old").unwrap()["summary"]["state"],
            "missed"
        );
        store.create_schedule(&json!({"schedule_id":"sch_cancel","project_id":"p","session_id":"s","prompt":"x","trigger":{"type":"at_time","run_at_unix":200}}), 10).unwrap();
        store.cancel_schedule("sch_cancel", 11).unwrap();
        assert_eq!(
            store.schedule_detail("sch_cancel").unwrap()["summary"]["state"],
            "cancelled"
        );
    }
}
