use std::{
    collections::HashMap,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    sync::{Mutex, MutexGuard},
    time::Duration,
    time::Instant,
};

use anyhow::{Context, Result, anyhow, ensure};
use farhelm_protocol::{
    AgentCommand, CommandAction, CommandReportRequest, CommandState, CommandStatusResponse,
    FARHELM_PROTOCOL,
};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub struct TypedCommandStore {
    connection: Mutex<Connection>,
    bodies: Mutex<HashMap<String, (Instant, Value)>>,
    legacy_cleanup_pending: AtomicBool,
}

impl TypedCommandStore {
    pub fn open(path: &Path) -> Result<Self> {
        let connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        if path != Path::new(":memory:") {
            connection.pragma_update(None, "journal_mode", "WAL")?;
        }
        crate::migrations::apply(&connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
            bodies: Mutex::new(HashMap::new()),
            legacy_cleanup_pending: AtomicBool::new(false),
        })
    }

    pub fn create(
        &self,
        agent_id: &str,
        action: CommandAction,
        payload: &Value,
        idempotency_key: &str,
        ttl: u64,
        now: u64,
    ) -> Result<CommandStatusResponse> {
        ensure!(
            action != CommandAction::AgentProbe,
            "probe belongs to the legacy command store"
        );
        ensure!(
            !idempotency_key.is_empty() && idempotency_key.len() <= 192,
            "invalid idempotency key"
        );
        let expires = now.checked_add(ttl).context("command expiry overflowed")?;
        let payload_json = serde_json::to_string(payload)?;
        let ephemeral = payload.get("prompt").is_some();
        let stored_payload = if ephemeral {
            serde_json::to_string(
                &serde_json::json!({"redacted":true,"ephemeral":true,"bytes":payload_json.len(),"sha256":hex_digest(&payload_json),"session_id":payload.get("session_id"),"project_id":payload.get("project_id"),"schedule_id":payload.get("schedule_id"),"trigger":payload.get("trigger")}),
            )?
        } else {
            payload_json.clone()
        };
        let mut bodies = self
            .bodies
            .lock()
            .map_err(|_| anyhow!("body relay poisoned"))?;
        bodies.retain(|_, (time, _)| time.elapsed() < Duration::from_secs(19));
        ensure!(!ephemeral || bodies.len() < 256, "body relay is full");
        let connection = self.lock()?;
        let transaction = crate::migrations::write_transaction(&connection)?;
        if let Some((existing, existing_payload)) = transaction.query_row(
            "SELECT command_id,agent_id,action,state,created_at_unix,expires_at_unix,updated_at_unix,data_json,detail,payload_json FROM typed_commands WHERE idempotency_key=?1",
            [idempotency_key], |row| Ok((status_from_row(row)?, row.get::<_, String>(9)?)),
        ).optional()? {
            let payload_matches = existing_payload == payload_json || serde_json::from_str::<Value>(&existing_payload).ok().is_some_and(|redacted| {
                redacted.get("redacted").and_then(Value::as_bool)==Some(true)
                    && redacted.get("bytes").and_then(Value::as_u64)==Some(payload_json.len() as u64)
                    && redacted.get("sha256").and_then(Value::as_str)==Some(hex_digest(&payload_json).as_str())
            });
            ensure!(existing.agent_id == agent_id && existing.action == action && payload_matches, "idempotency key conflicts");
            if ephemeral && matches!(existing.state,CommandState::Queued|CommandState::Delivered){bodies.insert(existing.command_id.clone(),(Instant::now(),payload.clone()));}
            return Ok(existing);
        }
        transaction.execute(
            "INSERT INTO typed_commands (command_id,agent_id,action,payload_json,state,idempotency_key,created_at_unix,expires_at_unix,updated_at_unix)
             VALUES (?1,?2,?3,?4,'queued',?5,?6,?7,?6)",
            params![format!("pending:{idempotency_key}"),agent_id,action_name(action),stored_payload,idempotency_key,as_i64(now)?,as_i64(expires)?],
        )?;
        let command_id = format!("cmd_cdx_{:016x}", transaction.last_insert_rowid());
        transaction.execute(
            "UPDATE typed_commands SET command_id=?1 WHERE idempotency_key=?2",
            params![command_id, idempotency_key],
        )?;
        transaction.commit()?;
        if ephemeral {
            bodies.insert(command_id.clone(), (Instant::now(), payload.clone()));
        }
        drop(bodies);
        drop(connection);
        self.get(&command_id)?
            .context("created command disappeared")
    }

    pub fn purge_bodies(&self) {
        if let Ok(mut bodies) = self.bodies.lock() {
            bodies.retain(|_, (time, _)| time.elapsed() < Duration::from_secs(19));
        }
    }
    pub fn forget_body(&self, id: &str) {
        if let Ok(mut bodies) = self.bodies.lock() {
            bodies.remove(id);
        }
    }
    pub fn claim(&self, agent_id: &str, now: u64) -> Result<Option<AgentCommand>> {
        let mut bodies = self
            .bodies
            .lock()
            .map_err(|_| anyhow!("body relay poisoned"))?;
        bodies.retain(|_, (time, _)| time.elapsed() < Duration::from_secs(19));
        let connection = self.lock()?;
        let tx = crate::migrations::write_transaction(&connection)?;
        tx.execute("UPDATE typed_commands SET state='expired',updated_at_unix=?1 WHERE agent_id=?2 AND state IN ('queued','delivered') AND expires_at_unix<=?1",params![as_i64(now)?,agent_id])?;
        let mut stmt=tx.prepare("SELECT command_id,agent_id,action,created_at_unix,expires_at_unix,payload_json FROM typed_commands WHERE agent_id=?1 AND ((state IN ('queued','delivered') AND expires_at_unix>?2) OR (state='expired' AND json_type(payload_json,'$.prompt')='text')) ORDER BY id")?;
        let candidates = stmt
            .query_map(params![agent_id, as_i64(now)?], |r| {
                Ok(AgentCommand {
                    protocol: FARHELM_PROTOCOL.to_owned(),
                    command_id: r.get(0)?,
                    agent_id: r.get(1)?,
                    action: parse_action(&r.get::<_, String>(2)?)?,
                    created_at_unix: row_u64(r, 3)?,
                    expires_at_unix: row_u64(r, 4)?,
                    payload: Some(
                        serde_json::from_str(&r.get::<_, String>(5)?)
                            .map_err(json_conversion(5))?,
                    ),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        let mut selected = None;
        for mut command in candidates {
            if command
                .payload
                .as_ref()
                .is_some_and(|p| p.get("ephemeral") == Some(&Value::Bool(true)))
            {
                let Some((_, payload)) = bodies.get(&command.command_id) else {
                    continue;
                };
                command.payload = Some(payload.clone());
            }
            tx.execute("UPDATE typed_commands SET state='delivered',updated_at_unix=?2 WHERE command_id=?1 AND state='queued'",params![command.command_id,as_i64(now)?])?;
            selected = Some(command);
            break;
        }
        tx.commit()?;
        Ok(selected)
    }

    pub fn report(&self, report: &CommandReportRequest, now: u64) -> Result<CommandStatusResponse> {
        let connection = self.lock()?;
        let tx = crate::migrations::write_transaction(&connection)?;
        let current=tx.query_row("SELECT command_id,agent_id,action,state,created_at_unix,expires_at_unix,updated_at_unix,data_json,detail FROM typed_commands WHERE command_id=?1",[&report.command_id],status_from_row).optional()?.context("command not found")?;
        ensure!(
            current.agent_id == report.agent_id && current.action != CommandAction::AgentProbe,
            "command not found"
        );
        ensure!(
            report.result.is_none(),
            "typed command cannot include probe result"
        );
        ensure!(
            report
                .detail
                .as_ref()
                .is_none_or(|detail| !detail.is_empty() && detail.len() <= 512),
            "invalid detail"
        );
        let same = current.state == report.state;
        let valid = matches!(
            (current.state, report.state),
            (CommandState::Delivered, CommandState::Accepted)
                | (CommandState::Delivered, CommandState::Expired)
                | (CommandState::Accepted, CommandState::Completed)
                | (CommandState::Accepted, CommandState::Failed)
                | (CommandState::Accepted, CommandState::Expired)
        );
        ensure!(same || valid, "invalid command transition");
        if same && report.state != CommandState::Expired {
            return Ok(current);
        }
        let mut legacy_body = false;
        let payload_json = {
            let raw: String = tx.query_row(
                "SELECT payload_json FROM typed_commands WHERE command_id=?1",
                [&report.command_id],
                |row| row.get(0),
            )?;
            if matches!(
                report.state,
                CommandState::Accepted
                    | CommandState::Completed
                    | CommandState::Failed
                    | CommandState::Expired
            ) {
                if serde_json::from_str::<Value>(&raw)?.get("redacted") == Some(&Value::Bool(true))
                {
                    raw
                } else {
                    legacy_body = serde_json::from_str::<Value>(&raw)?.get("prompt").is_some();
                    serde_json::to_string(
                        &serde_json::json!({"redacted":true,"bytes":raw.len(),"sha256":hex_digest(&raw)}),
                    )?
                }
            } else {
                raw
            }
        };
        tx.execute(
            "UPDATE typed_commands SET state=?1,updated_at_unix=?2,data_json=?3,detail=?4,payload_json=?5
              WHERE command_id=?6",
            params![state_name(report.state),as_i64(now)?,report.data.as_ref().map(|data| serde_json::to_string(&public_result(data))).transpose()?,report.detail.as_deref().map(public_detail),payload_json,report.command_id],
        )?;
        tx.commit()?;
        if legacy_body {
            self.legacy_cleanup_pending.store(true, Ordering::Release);
        }
        drop(connection);
        self.forget_body(&report.command_id);
        if legacy_body && let Err(error) = self.maintain_legacy_privacy() {
            tracing::warn!(%error,"legacy body checkpoint will retry");
        }
        self.get(&report.command_id)?
            .context("reported command disappeared")
    }

    pub fn maintain_legacy_privacy(&self) -> Result<()> {
        if !self.legacy_cleanup_pending.load(Ordering::Acquire) {
            return Ok(());
        }
        let connection = self.lock()?;
        if crate::migrations::checkpoint(&connection)? {
            self.legacy_cleanup_pending.store(false, Ordering::Release);
        }
        Ok(())
    }

    pub fn get(&self, command_id: &str) -> Result<Option<CommandStatusResponse>> {
        self.lock()?.query_row(
            "SELECT command_id,agent_id,action,state,created_at_unix,expires_at_unix,updated_at_unix,data_json,detail FROM typed_commands WHERE command_id=?1",
            [command_id], status_from_row,
        ).optional().map_err(Into::into)
    }

    /// Verify a retry without retaining its body or requiring a new Agent heartbeat.
    pub fn saved_receipt(
        &self,
        agent_id: &str,
        action: CommandAction,
        payload: &Value,
        key: &str,
    ) -> Result<Option<CommandStatusResponse>> {
        let row = self.lock()?.query_row(
            "SELECT command_id,agent_id,action,state,created_at_unix,expires_at_unix,updated_at_unix,data_json,detail,payload_json FROM typed_commands WHERE idempotency_key=?1",
            [key], |r|Ok((status_from_row(r)?,r.get::<_,String>(9)?)),
        ).optional()?;
        let Some((status, encoded)) = row else {
            return Ok(None);
        };
        let body = serde_json::to_string(payload)?;
        let matches = encoded == body
            || serde_json::from_str::<Value>(&encoded)
                .ok()
                .is_some_and(|v| {
                    v.get("redacted").and_then(Value::as_bool) == Some(true)
                        && v.get("bytes").and_then(Value::as_u64) == Some(body.len() as u64)
                        && v.get("sha256").and_then(Value::as_str)
                            == Some(hex_digest(&body).as_str())
                });
        ensure!(
            status.agent_id == agent_id && status.action == action && matches,
            "idempotency key conflicts"
        );
        Ok(
            (!matches!(status.state, CommandState::Queued | CommandState::Delivered))
                .then_some(status),
        )
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>> {
        self.connection
            .lock()
            .map_err(|_| anyhow!("typed command database lock was poisoned"))
    }
}

fn hex_digest(value: &str) -> String {
    Sha256::digest(value.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn status_from_row(row: &Row<'_>) -> rusqlite::Result<CommandStatusResponse> {
    let data_json: Option<String> = row.get(7)?;
    Ok(CommandStatusResponse {
        protocol: FARHELM_PROTOCOL.to_owned(),
        command_id: row.get(0)?,
        agent_id: row.get(1)?,
        action: parse_action(&row.get::<_, String>(2)?)?,
        state: parse_state(&row.get::<_, String>(3)?)?,
        created_at_unix: row_u64(row, 4)?,
        expires_at_unix: row_u64(row, 5)?,
        updated_at_unix: row_u64(row, 6)?,
        result: None,
        data: data_json
            .map(|value| serde_json::from_str(&value).map_err(json_conversion(7)))
            .transpose()?,
        detail: row.get(8)?,
    })
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
        CommandAction::ProjectAdd => "project.add",
        CommandAction::ProjectSync => "project.sync",
        CommandAction::CodexSessionArchive => "codex.session.archive",
        CommandAction::CodexSessionUnarchive => "codex.session.unarchive",
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
        "project.add" => Ok(CommandAction::ProjectAdd),
        "project.sync" => Ok(CommandAction::ProjectSync),
        "codex.session.archive" => Ok(CommandAction::CodexSessionArchive),
        "codex.session.unarchive" => Ok(CommandAction::CodexSessionUnarchive),
        "project.approve" => Ok(CommandAction::ProjectApprove),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

pub(crate) fn ensure_action_schema(connection: &Connection) -> Result<()> {
    let schema: String = connection.query_row(
        "SELECT sql FROM sqlite_master WHERE type='table' AND name='typed_commands'",
        [],
        |row| row.get(0),
    )?;
    if schema.contains("project.add") {
        return Ok(());
    }
    connection.execute_batch(
        "ALTER TABLE typed_commands RENAME TO typed_commands_v041;
         CREATE TABLE typed_commands (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            command_id TEXT NOT NULL UNIQUE,
            agent_id TEXT NOT NULL,
            action TEXT NOT NULL CHECK (action IN ('codex.session.create','codex.session.resume','codex.turn.start','codex.turn.steer','codex.turn.interrupt','codex.schedule.create','codex.schedule.cancel','project.approve','project.add','project.sync','codex.session.archive','codex.session.unarchive')),
            payload_json TEXT NOT NULL,
            state TEXT NOT NULL CHECK (state IN ('queued','delivered','accepted','completed','failed','expired')),
            idempotency_key TEXT NOT NULL UNIQUE,
            created_at_unix INTEGER NOT NULL,
            expires_at_unix INTEGER NOT NULL,
            updated_at_unix INTEGER NOT NULL,
            data_json TEXT,
            detail TEXT
         );
         INSERT INTO typed_commands SELECT * FROM typed_commands_v041;
         DROP TABLE typed_commands_v041;
         CREATE INDEX IF NOT EXISTS typed_commands_delivery ON typed_commands(agent_id,state,id);",
    )?;
    Ok(())
}
const fn state_name(state: CommandState) -> &'static str {
    match state {
        CommandState::Queued => "queued",
        CommandState::Delivered => "delivered",
        CommandState::Accepted => "accepted",
        CommandState::Completed => "completed",
        CommandState::Failed => "failed",
        CommandState::Expired => "expired",
        CommandState::Cancelled | CommandState::Unknown => "failed",
    }
}
fn parse_state(value: &str) -> rusqlite::Result<CommandState> {
    match value {
        "queued" => Ok(CommandState::Queued),
        "delivered" => Ok(CommandState::Delivered),
        "accepted" => Ok(CommandState::Accepted),
        "completed" => Ok(CommandState::Completed),
        "failed" => Ok(CommandState::Failed),
        "expired" => Ok(CommandState::Expired),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}
fn as_i64(value: u64) -> Result<i64> {
    i64::try_from(value).context("value exceeds SQLite range")
}
fn row_u64(row: &Row<'_>, index: usize) -> rusqlite::Result<u64> {
    u64::try_from(row.get::<_, i64>(index)?).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
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

fn public_detail(detail: &str) -> &'static str {
    [
        "project_root_revoked",
        "project_root_is_container",
        "project_directory_expired",
        "project_directory_changed",
        "project_directory_unavailable",
        "project_directory_permission",
        "project_invalid_name",
        "project_name_conflict",
        "project_creation_unconfirmed",
        "project_candidate_missing",
        "project_history_sync_failed",
        "project_not_approved",
        "codex_session_archived",
        "codex_archive_busy",
        "codex_archive_unverified",
        "codex_archive_changed",
        "codex_archive_unapproved",
        "codex_archive_unsaved",
        "codex_session_in_use",
        "model_choice_unavailable",
        "codex_handoff_busy",
        "codex_handoff_unsaved",
        "codex_handoff_background",
        "codex_handoff_unconfirmed",
        "codex_handoff_unverified",
    ]
    .into_iter()
    .find(|code| *code == detail)
    .unwrap_or("Agent operation failed; inspect Agent diagnostics")
}

fn public_result(data: &Value) -> Value {
    let mut out = serde_json::Map::new();
    for key in [
        "session_id",
        "turn_id",
        "schedule_id",
        "cancelled",
        "approved",
        "project_id",
        "candidate_id",
        "status",
    ] {
        if let Some(value) = data.get(key) {
            if key == "approved" {
                if let Some(values) = value.as_array() {
                    out.insert(
                        key.to_owned(),
                        Value::Array(values.iter().filter(|v| v.is_string()).cloned().collect()),
                    );
                }
            } else if value.is_string() || (key == "cancelled" && value.is_boolean()) {
                out.insert(key.to_owned(), value.clone());
            }
        }
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_relay_survives_lost_receipts_without_durable_body() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("hub.db");
        let marker = "SYNTHETIC_PRIVATE_PROMPT_070";
        let payload = serde_json::json!({"project_id":"p","session_id":"s","prompt":marker});
        let store = TypedCommandStore::open(&path).unwrap();
        let first = store
            .create(
                "agent",
                CommandAction::CodexTurnStart,
                &payload,
                "operation-key",
                300,
                100,
            )
            .unwrap();
        drop(store);
        let store = TypedCommandStore::open(&path).unwrap();
        assert!(store.claim("agent", 101).unwrap().is_none());
        let retry = store
            .create(
                "agent",
                CommandAction::CodexTurnStart,
                &payload,
                "operation-key",
                300,
                101,
            )
            .unwrap();
        assert_eq!(first.command_id, retry.command_id);
        assert_eq!(
            store.claim("agent", 101).unwrap().unwrap().payload.unwrap()["prompt"],
            marker
        );
        let mut report = CommandReportRequest {
            protocol: FARHELM_PROTOCOL.into(),
            agent_id: "agent".into(),
            command_id: first.command_id.clone(),
            state: CommandState::Accepted,
            result: None,
            data: None,
            detail: None,
        };
        store.report(&report, 101).unwrap();
        assert!(store.claim("agent", 102).unwrap().is_none());
        assert_eq!(
            store
                .create(
                    "agent",
                    CommandAction::CodexTurnStart,
                    &payload,
                    "operation-key",
                    300,
                    102
                )
                .unwrap()
                .state,
            CommandState::Accepted
        );
        report.state = CommandState::Completed;
        report.data = Some(serde_json::json!({"session_id":"s","cwd":marker,"stdout":marker}));
        store.report(&report, 103).unwrap();
        report.state = CommandState::Failed;
        assert!(store.report(&report, 103).is_err());
        for entry in std::fs::read_dir(directory.path()).unwrap() {
            let bytes = std::fs::read(entry.unwrap().path()).unwrap();
            assert!(
                !bytes
                    .windows(marker.len())
                    .any(|window| window == marker.as_bytes()),
                "prompt leaked into database or WAL"
            );
        }
    }

    #[test]
    fn old_expired_body_is_retained_until_agent_receipt() {
        let store = TypedCommandStore::open(Path::new(":memory:")).unwrap();
        let created = store
            .create(
                "agent",
                CommandAction::CodexTurnStart,
                &serde_json::json!({"prompt":"legacy","session_id":"s"}),
                "legacy-key",
                10,
                100,
            )
            .unwrap();
        store
            .lock()
            .unwrap()
            .execute(
                "UPDATE typed_commands SET payload_json=?1 WHERE command_id=?2",
                params![
                    r#"{"prompt":"legacy","session_id":"s"}"#,
                    created.command_id
                ],
            )
            .unwrap();
        let command = store.claim("agent", 120).unwrap().unwrap();
        assert_eq!(command.payload.unwrap()["prompt"], "legacy");
        store
            .report(
                &CommandReportRequest {
                    protocol: FARHELM_PROTOCOL.into(),
                    agent_id: "agent".into(),
                    command_id: created.command_id.clone(),
                    state: CommandState::Expired,
                    result: None,
                    data: None,
                    detail: None,
                },
                120,
            )
            .unwrap();
        assert!(store.claim("agent", 121).unwrap().is_none());
        let value: String = store
            .lock()
            .unwrap()
            .query_row(
                "SELECT payload_json FROM typed_commands WHERE command_id=?1",
                [created.command_id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!value.contains("legacy"));
    }

    #[test]
    fn legacy_body_bytes_are_removed_after_receipt_even_if_checkpoint_was_busy() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("hub.db");
        let store = TypedCommandStore::open(&path).unwrap();
        let marker = "LEGACY_BODY_REQUIRING_PHYSICAL_CLEANUP";
        let payload = serde_json::json!({"prompt":marker.repeat(200),"session_id":"s"});
        let created = store
            .create(
                "agent",
                CommandAction::CodexTurnStart,
                &payload,
                "legacy-physical-001",
                100,
                100,
            )
            .unwrap();
        store
            .lock()
            .unwrap()
            .execute(
                "UPDATE typed_commands SET payload_json=?1 WHERE command_id=?2",
                params![payload.to_string(), created.command_id],
            )
            .unwrap();
        store.claim("agent", 101).unwrap().unwrap();
        let reader = Connection::open(&path).unwrap();
        reader.execute_batch("BEGIN;").unwrap();
        let before: String = reader
            .query_row("SELECT payload_json FROM typed_commands", [], |r| r.get(0))
            .unwrap();
        assert!(before.contains(marker));
        store
            .lock()
            .unwrap()
            .busy_timeout(Duration::from_millis(1))
            .unwrap();
        store
            .report(
                &CommandReportRequest {
                    protocol: FARHELM_PROTOCOL.into(),
                    agent_id: "agent".into(),
                    command_id: created.command_id,
                    state: CommandState::Accepted,
                    result: None,
                    data: None,
                    detail: None,
                },
                102,
            )
            .unwrap();
        assert!(store.legacy_cleanup_pending.load(Ordering::Acquire));
        reader.execute_batch("COMMIT;").unwrap();
        store.maintain_legacy_privacy().unwrap();
        assert!(!store.legacy_cleanup_pending.load(Ordering::Acquire));
        for suffix in ["hub.db", "hub.db-wal"] {
            let bytes = std::fs::read(directory.path().join(suffix)).unwrap();
            assert!(
                !bytes
                    .windows(marker.len())
                    .any(|window| window == marker.as_bytes()),
                "legacy bytes remain in {suffix}"
            );
        }
    }

    #[test]
    fn concurrent_terminal_reports_commit_exactly_one_outcome() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("hub.db");
        let first = TypedCommandStore::open(&path).unwrap();
        let second = TypedCommandStore::open(&path).unwrap();
        for n in 0..20 {
            let command = first
                .create(
                    "agent",
                    CommandAction::CodexSessionCreate,
                    &serde_json::json!({"project_id":"p"}),
                    &format!("terminal-race-{n}"),
                    300,
                    100,
                )
                .unwrap();
            first.claim("agent", 101).unwrap().unwrap();
            let mut report = CommandReportRequest {
                protocol: FARHELM_PROTOCOL.into(),
                agent_id: "agent".into(),
                command_id: command.command_id.clone(),
                state: CommandState::Accepted,
                result: None,
                data: None,
                detail: None,
            };
            first.report(&report, 101).unwrap();
            report.state = CommandState::Completed;
            let mut competing = report.clone();
            competing.state = CommandState::Failed;
            let barrier = std::sync::Barrier::new(2);
            std::thread::scope(|scope| {
                let left = scope.spawn(|| {
                    barrier.wait();
                    first.report(&report, 102).is_ok()
                });
                let right = scope.spawn(|| {
                    barrier.wait();
                    second.report(&competing, 102).is_ok()
                });
                assert_ne!(left.join().unwrap(), right.join().unwrap());
            });
            assert!(matches!(
                first.get(&command.command_id).unwrap().unwrap().state,
                CommandState::Completed | CommandState::Failed
            ));
        }
    }

    #[test]
    fn idempotency_key_requires_identical_payload() {
        let store = TypedCommandStore::open(Path::new(":memory:")).unwrap();
        let first = store
            .create(
                "gpu-a",
                CommandAction::CodexTurnStart,
                &serde_json::json!({"project_id":"p","session_id":"s","prompt":"one"}),
                "request-1",
                300,
                100,
            )
            .unwrap();
        let repeated = store
            .create(
                "gpu-a",
                CommandAction::CodexTurnStart,
                &serde_json::json!({"project_id":"p","session_id":"s","prompt":"one"}),
                "request-1",
                300,
                101,
            )
            .unwrap();
        assert_eq!(repeated.command_id, first.command_id);
        let _ = store.claim("gpu-a", 102).unwrap().unwrap();
        store
            .report(
                &CommandReportRequest {
                    protocol: FARHELM_PROTOCOL.into(),
                    agent_id: "gpu-a".into(),
                    command_id: first.command_id.clone(),
                    state: CommandState::Accepted,
                    result: None,
                    detail: None,
                    data: None,
                },
                103,
            )
            .unwrap();
        store
            .report(
                &CommandReportRequest {
                    protocol: FARHELM_PROTOCOL.into(),
                    agent_id: "gpu-a".into(),
                    command_id: first.command_id.clone(),
                    state: CommandState::Completed,
                    result: None,
                    detail: None,
                    data: None,
                },
                104,
            )
            .unwrap();
        let after_redaction = store
            .create(
                "gpu-a",
                CommandAction::CodexTurnStart,
                &serde_json::json!({"project_id":"p","session_id":"s","prompt":"one"}),
                "request-1",
                300,
                105,
            )
            .unwrap();
        assert_eq!(after_redaction.command_id, first.command_id);
        assert!(
            store
                .create(
                    "gpu-a",
                    CommandAction::CodexTurnStart,
                    &serde_json::json!({"project_id":"p","session_id":"s","prompt":"two"}),
                    "request-1",
                    300,
                    102,
                )
                .is_err()
        );
    }

    #[test]
    fn v041_command_table_adds_project_approval_without_losing_rows() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("hub.db");
        let connection = Connection::open(&database).unwrap();
        connection.execute_batch("CREATE TABLE typed_commands (
            id INTEGER PRIMARY KEY AUTOINCREMENT, command_id TEXT NOT NULL UNIQUE, agent_id TEXT NOT NULL,
            action TEXT NOT NULL CHECK (action IN ('codex.session.create','codex.session.resume','codex.turn.start','codex.turn.steer','codex.turn.interrupt')),
            payload_json TEXT NOT NULL, state TEXT NOT NULL CHECK (state IN ('queued','delivered','accepted','completed','failed','expired')),
            idempotency_key TEXT NOT NULL UNIQUE, created_at_unix INTEGER NOT NULL, expires_at_unix INTEGER NOT NULL,
            updated_at_unix INTEGER NOT NULL, data_json TEXT, detail TEXT);
            INSERT INTO typed_commands (command_id,agent_id,action,payload_json,state,idempotency_key,created_at_unix,expires_at_unix,updated_at_unix)
            VALUES ('cmd_cdx_0000000000000001','titan','codex.turn.start','{}','completed','old-command-key',1,2,2);").unwrap();
        drop(connection);
        let store = TypedCommandStore::open(&database).unwrap();
        assert!(store.get("cmd_cdx_0000000000000001").unwrap().is_some());
        let approval = store
            .create(
                "titan",
                CommandAction::ProjectApprove,
                &serde_json::json!({"candidate_ids":["candidate-a"]}),
                "project-import-key",
                60,
                10,
            )
            .unwrap();
        assert_eq!(approval.action, CommandAction::ProjectApprove);
    }
}
