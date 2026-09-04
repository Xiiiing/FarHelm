use std::{
    path::Path,
    sync::{Mutex, MutexGuard},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, ensure};
use farhelm_protocol::{
    AgentCommand, CommandAction, CommandReportRequest, CommandState, CommandStatusResponse,
    FARHELM_PROTOCOL,
};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde_json::Value;

pub struct TypedCommandStore {
    connection: Mutex<Connection>,
}

impl TypedCommandStore {
    pub fn open(path: &Path) -> Result<Self> {
        let connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        if path != Path::new(":memory:") {
            connection.pragma_update(None, "journal_mode", "WAL")?;
        }
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS typed_commands (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                command_id TEXT NOT NULL UNIQUE,
                agent_id TEXT NOT NULL,
                action TEXT NOT NULL CHECK (action IN ('codex.session.create','codex.session.resume','codex.turn.start','codex.turn.steer','codex.turn.interrupt')),
                payload_json TEXT NOT NULL,
                state TEXT NOT NULL CHECK (state IN ('queued','delivered','accepted','completed','failed','expired')),
                idempotency_key TEXT NOT NULL UNIQUE,
                created_at_unix INTEGER NOT NULL,
                expires_at_unix INTEGER NOT NULL,
                updated_at_unix INTEGER NOT NULL,
                data_json TEXT,
                detail TEXT
            );
            CREATE INDEX IF NOT EXISTS typed_commands_delivery ON typed_commands(agent_id,state,id);",
        )?;
        Ok(Self {
            connection: Mutex::new(connection),
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
        let connection = self.lock()?;
        let transaction = connection.unchecked_transaction()?;
        if let Some((existing, existing_payload)) = transaction.query_row(
            "SELECT command_id,agent_id,action,state,created_at_unix,expires_at_unix,updated_at_unix,data_json,detail,payload_json FROM typed_commands WHERE idempotency_key=?1",
            [idempotency_key], |row| Ok((status_from_row(row)?, row.get::<_, String>(9)?)),
        ).optional()? {
            ensure!(existing.agent_id == agent_id && existing.action == action && existing_payload == payload_json, "idempotency key conflicts");
            return Ok(existing);
        }
        transaction.execute(
            "INSERT INTO typed_commands (command_id,agent_id,action,payload_json,state,idempotency_key,created_at_unix,expires_at_unix,updated_at_unix)
             VALUES (?1,?2,?3,?4,'queued',?5,?6,?7,?6)",
            params![format!("pending:{idempotency_key}"),agent_id,action_name(action),payload_json,idempotency_key,as_i64(now)?,as_i64(expires)?],
        )?;
        let command_id = format!("cmd_cdx_{:016x}", transaction.last_insert_rowid());
        transaction.execute(
            "UPDATE typed_commands SET command_id=?1 WHERE idempotency_key=?2",
            params![command_id, idempotency_key],
        )?;
        transaction.commit()?;
        drop(connection);
        self.get(&command_id)?
            .context("created command disappeared")
    }

    pub fn claim(&self, agent_id: &str, now: u64) -> Result<Option<AgentCommand>> {
        let connection = self.lock()?;
        let transaction = connection.unchecked_transaction()?;
        transaction.execute("UPDATE typed_commands SET state='expired',payload_json='{}',updated_at_unix=?1 WHERE agent_id=?2 AND state IN ('queued','delivered') AND expires_at_unix<=?1", params![as_i64(now)?,agent_id])?;
        let command = transaction.query_row(
            "SELECT command_id,agent_id,action,created_at_unix,expires_at_unix,payload_json FROM typed_commands WHERE agent_id=?1 AND state IN ('queued','delivered') AND expires_at_unix>?2 ORDER BY id LIMIT 1",
            params![agent_id,as_i64(now)?], |row| {
                let encoded: String = row.get(5)?;
                Ok(AgentCommand { protocol:FARHELM_PROTOCOL.to_owned(),command_id:row.get(0)?,agent_id:row.get(1)?,action:parse_action(&row.get::<_,String>(2)?)?,created_at_unix:row_u64(row,3)?,expires_at_unix:row_u64(row,4)?,payload:Some(serde_json::from_str(&encoded).map_err(json_conversion(5))?) })
            },
        ).optional()?;
        if let Some(command) = &command {
            transaction.execute("UPDATE typed_commands SET state='delivered',updated_at_unix=?1 WHERE command_id=?2 AND state='queued'", params![as_i64(now)?,command.command_id])?;
        }
        transaction.commit()?;
        Ok(command)
    }

    pub fn report(&self, report: &CommandReportRequest, now: u64) -> Result<CommandStatusResponse> {
        let current = self.get(&report.command_id)?.context("command not found")?;
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
        if same {
            return Ok(current);
        }
        self.lock()?.execute(
            "UPDATE typed_commands SET state=?1,updated_at_unix=?2,data_json=?3,detail=?4,
                    payload_json=CASE WHEN ?1 IN ('completed','failed','expired') THEN '{}' ELSE payload_json END
              WHERE command_id=?5",
            params![state_name(report.state),as_i64(now)?,report.data.as_ref().map(serde_json::to_string).transpose()?,report.detail,report.command_id],
        )?;
        self.get(&report.command_id)?
            .context("reported command disappeared")
    }

    pub fn get(&self, command_id: &str) -> Result<Option<CommandStatusResponse>> {
        self.lock()?.query_row(
            "SELECT command_id,agent_id,action,state,created_at_unix,expires_at_unix,updated_at_unix,data_json,detail FROM typed_commands WHERE command_id=?1",
            [command_id], status_from_row,
        ).optional().map_err(Into::into)
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>> {
        self.connection
            .lock()
            .map_err(|_| anyhow!("typed command database lock was poisoned"))
    }
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
