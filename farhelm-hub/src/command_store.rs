use std::{
    path::Path,
    sync::{Mutex, MutexGuard},
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use farhelm_protocol::{
    AgentCommand, CommandAction, CommandReportRequest, CommandState, CommandStatusResponse,
    FARHELM_PROTOCOL, ProbeResult,
};
use rusqlite::{Connection, OptionalExtension, Row, params};

#[derive(Debug)]
pub enum CreateCommandError {
    Conflict,
    Internal(anyhow::Error),
}

#[derive(Debug)]
pub enum ReportCommandError {
    NotFound,
    Conflict,
    Invalid,
    Internal(anyhow::Error),
}

pub struct CommandStore {
    connection: Mutex<Connection>,
}

impl CommandStore {
    pub fn open(path: &Path) -> Result<Self> {
        if path != Path::new(":memory:") {
            let parent = path
                .parent()
                .filter(|value| !value.as_os_str().is_empty())
                .context("FARHELM_HUB_DATABASE must have a parent directory")?;
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create Hub database directory {}",
                    parent.display()
                )
            })?;
        }
        let connection = Connection::open(path)
            .with_context(|| format!("failed to open Hub database {}", path.display()))?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        if path != Path::new(":memory:") {
            connection.pragma_update(None, "journal_mode", "WAL")?;
        }
        let schema_version: i64 =
            connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        anyhow::ensure!(
            schema_version <= 1,
            "Hub database schema is newer than this binary"
        );
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS commands (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                command_id TEXT NOT NULL UNIQUE,
                agent_id TEXT NOT NULL,
                action TEXT NOT NULL CHECK (action = 'agent.probe'),
                state TEXT NOT NULL CHECK (
                    state IN ('queued','delivered','accepted','completed','failed','expired','cancelled','unknown')
                ),
                idempotency_key TEXT NOT NULL UNIQUE,
                ttl_secs INTEGER NOT NULL,
                created_at_unix INTEGER NOT NULL,
                expires_at_unix INTEGER NOT NULL,
                updated_at_unix INTEGER NOT NULL,
                result_json TEXT,
                detail TEXT
            );
            CREATE INDEX IF NOT EXISTS commands_agent_delivery
                ON commands(agent_id, state, id);",
        )?;
        if schema_version == 0 {
            connection.pragma_update(None, "user_version", 1)?;
        }
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn create_probe(
        &self,
        agent_id: &str,
        idempotency_key: &str,
        ttl_secs: u64,
        now: u64,
    ) -> std::result::Result<CommandStatusResponse, CreateCommandError> {
        self.create_probe_inner(agent_id, idempotency_key, ttl_secs, now)
            .map_err(|error| match error.downcast_ref::<IdempotencyConflict>() {
                Some(_) => CreateCommandError::Conflict,
                None => CreateCommandError::Internal(error),
            })
    }

    fn create_probe_inner(
        &self,
        agent_id: &str,
        idempotency_key: &str,
        ttl_secs: u64,
        now: u64,
    ) -> Result<CommandStatusResponse> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let existing = transaction
            .query_row(
                "SELECT command_id, agent_id, action, state, created_at_unix, expires_at_unix,
                        updated_at_unix, result_json, detail, ttl_secs
                   FROM commands WHERE idempotency_key = ?1",
                [idempotency_key],
                command_from_row,
            )
            .optional()?;
        if let Some((command, existing_ttl)) = existing {
            if command.agent_id != agent_id
                || command.action != CommandAction::AgentProbe
                || existing_ttl != ttl_secs
            {
                return Err(IdempotencyConflict.into());
            }
            return Ok(command);
        }

        let expires_at = now
            .checked_add(ttl_secs)
            .context("command expiry overflowed")?;
        let temporary_id = format!("pending:{idempotency_key}");
        transaction.execute(
            "INSERT INTO commands (
                command_id, agent_id, action, state, idempotency_key, ttl_secs,
                created_at_unix, expires_at_unix, updated_at_unix
            ) VALUES (?1, ?2, 'agent.probe', 'queued', ?3, ?4, ?5, ?6, ?5)",
            params![
                temporary_id,
                agent_id,
                idempotency_key,
                as_i64(ttl_secs)?,
                as_i64(now)?,
                as_i64(expires_at)?
            ],
        )?;
        let command_id = format!("cmd_{:016x}", transaction.last_insert_rowid());
        transaction.execute(
            "UPDATE commands SET command_id = ?1 WHERE idempotency_key = ?2",
            params![command_id, idempotency_key],
        )?;
        transaction.commit()?;
        drop(connection);
        self.get(&command_id)?
            .context("newly created command was not found")
    }

    pub fn get(&self, command_id: &str) -> Result<Option<CommandStatusResponse>> {
        self.lock()?
            .query_row(
                "SELECT command_id, agent_id, action, state, created_at_unix, expires_at_unix,
                        updated_at_unix, result_json, detail, ttl_secs
                   FROM commands WHERE command_id = ?1",
                [command_id],
                command_from_row,
            )
            .optional()
            .map(|value| value.map(|(command, _)| command))
            .map_err(Into::into)
    }

    pub fn claim(&self, agent_id: &str, now: u64) -> Result<Option<AgentCommand>> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "UPDATE commands SET state = 'expired', updated_at_unix = ?1
              WHERE agent_id = ?2 AND state IN ('queued','delivered') AND expires_at_unix <= ?1",
            params![as_i64(now)?, agent_id],
        )?;
        let candidate = transaction
            .query_row(
                "SELECT command_id, agent_id, action, created_at_unix, expires_at_unix
                   FROM commands
                  WHERE agent_id = ?1 AND state IN ('queued','delivered') AND expires_at_unix > ?2
                  ORDER BY id LIMIT 1",
                params![agent_id, as_i64(now)?],
                |row| {
                    Ok(AgentCommand {
                        protocol: FARHELM_PROTOCOL.to_owned(),
                        command_id: row.get(0)?,
                        agent_id: row.get(1)?,
                        action: parse_action(&row.get::<_, String>(2)?)?,
                        created_at_unix: row_u64(row, 3)?,
                        expires_at_unix: row_u64(row, 4)?,
                    })
                },
            )
            .optional()?;
        if let Some(command) = &candidate {
            transaction.execute(
                "UPDATE commands SET state = 'delivered', updated_at_unix = ?1
                  WHERE command_id = ?2 AND state = 'queued'",
                params![as_i64(now)?, command.command_id],
            )?;
        }
        transaction.commit()?;
        Ok(candidate)
    }

    pub fn report(
        &self,
        report: &CommandReportRequest,
        now: u64,
    ) -> std::result::Result<CommandStatusResponse, ReportCommandError> {
        self.report_inner(report, now).map_err(|error| {
            if error.downcast_ref::<CommandNotFound>().is_some() {
                ReportCommandError::NotFound
            } else if error.downcast_ref::<TransitionConflict>().is_some() {
                ReportCommandError::Conflict
            } else if error.downcast_ref::<InvalidReport>().is_some() {
                ReportCommandError::Invalid
            } else {
                ReportCommandError::Internal(error)
            }
        })
    }

    fn report_inner(
        &self,
        report: &CommandReportRequest,
        now: u64,
    ) -> Result<CommandStatusResponse> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let current = transaction
            .query_row(
                "SELECT command_id, agent_id, action, state, created_at_unix, expires_at_unix,
                        updated_at_unix, result_json, detail, ttl_secs
                   FROM commands WHERE command_id = ?1",
                [&report.command_id],
                command_from_row,
            )
            .optional()?
            .ok_or(CommandNotFound)?
            .0;
        if current.agent_id != report.agent_id || current.action != CommandAction::AgentProbe {
            return Err(CommandNotFound.into());
        }

        validate_report(&current, report, now)?;
        if current.state == report.state {
            return Ok(current);
        }

        let result_json = report
            .result
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        transaction.execute(
            "UPDATE commands
                SET state = ?1, updated_at_unix = ?2, result_json = ?3, detail = ?4
              WHERE command_id = ?5",
            params![
                state_name(report.state),
                as_i64(now)?,
                result_json,
                report.detail,
                report.command_id
            ],
        )?;
        transaction.commit()?;
        drop(connection);
        self.get(&report.command_id)?
            .context("reported command was not found")
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>> {
        self.connection
            .lock()
            .map_err(|_| anyhow!("Hub command database lock was poisoned"))
    }
}

fn validate_report(
    current: &CommandStatusResponse,
    report: &CommandReportRequest,
    now: u64,
) -> Result<()> {
    if report.detail.as_ref().is_some_and(|value| {
        value.is_empty() || value.len() > 256 || value.chars().any(char::is_control)
    }) {
        return Err(InvalidReport.into());
    }
    let valid_shape = match report.state {
        CommandState::Accepted => {
            report.result.is_none() && report.detail.is_none() && now < current.expires_at_unix
        }
        CommandState::Completed => {
            report.result.as_ref().is_some_and(valid_probe_result) && report.detail.is_none()
        }
        CommandState::Failed => report.result.is_none() && report.detail.is_some(),
        CommandState::Expired => {
            report.result.is_none() && report.detail.is_none() && now >= current.expires_at_unix
        }
        _ => false,
    };
    if !valid_shape {
        return Err(InvalidReport.into());
    }
    if current.state == report.state {
        return Ok(());
    }
    let valid_transition = matches!(
        (current.state, report.state),
        (CommandState::Delivered, CommandState::Accepted)
            | (CommandState::Delivered, CommandState::Expired)
            | (CommandState::Accepted, CommandState::Completed)
            | (CommandState::Accepted, CommandState::Failed)
            | (CommandState::Accepted, CommandState::Expired)
    );
    if !valid_transition {
        return Err(TransitionConflict.into());
    }
    Ok(())
}

fn valid_probe_result(result: &ProbeResult) -> bool {
    !result.agent_version.is_empty()
        && result.agent_version.len() <= 32
        && !result.hostname.is_empty()
        && result.hostname.len() <= 255
        && !result.hostname.chars().any(char::is_control)
}

fn command_from_row(row: &Row<'_>) -> rusqlite::Result<(CommandStatusResponse, u64)> {
    let result_json: Option<String> = row.get(7)?;
    let result = result_json
        .map(|value| {
            serde_json::from_str::<ProbeResult>(&value).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    value.len(),
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })
        })
        .transpose()?;
    Ok((
        CommandStatusResponse {
            protocol: FARHELM_PROTOCOL.to_owned(),
            command_id: row.get(0)?,
            agent_id: row.get(1)?,
            action: parse_action(&row.get::<_, String>(2)?)?,
            state: parse_state(&row.get::<_, String>(3)?)?,
            created_at_unix: row_u64(row, 4)?,
            expires_at_unix: row_u64(row, 5)?,
            updated_at_unix: row_u64(row, 6)?,
            result,
            detail: row.get(8)?,
        },
        row_u64(row, 9)?,
    ))
}

fn as_i64(value: u64) -> Result<i64> {
    i64::try_from(value).context("timestamp or TTL exceeds SQLite INTEGER range")
}

fn row_u64(row: &Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

fn parse_action(value: &str) -> rusqlite::Result<CommandAction> {
    match value {
        "agent.probe" => Ok(CommandAction::AgentProbe),
        _ => Err(rusqlite::Error::InvalidQuery),
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
        "cancelled" => Ok(CommandState::Cancelled),
        "unknown" => Ok(CommandState::Unknown),
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
        CommandState::Cancelled => "cancelled",
        CommandState::Unknown => "unknown",
    }
}

#[derive(Debug, thiserror::Error)]
#[error("idempotency key conflicts with an existing request")]
struct IdempotencyConflict;

#[derive(Debug, thiserror::Error)]
#[error("command was not found")]
struct CommandNotFound;

#[derive(Debug, thiserror::Error)]
#[error("command state transition conflicts with current state")]
struct TransitionConflict;

#[derive(Debug, thiserror::Error)]
#[error("command report is invalid")]
struct InvalidReport;

#[cfg(test)]
mod tests {
    use super::*;

    fn report(command_id: &str, state: CommandState) -> CommandReportRequest {
        CommandReportRequest {
            protocol: FARHELM_PROTOCOL.to_owned(),
            agent_id: "gpu-a".to_owned(),
            command_id: command_id.to_owned(),
            state,
            result: (state == CommandState::Completed).then(|| ProbeResult {
                agent_version: "0.2.0".to_owned(),
                hostname: "trainer-a".to_owned(),
            }),
            detail: None,
        }
    }

    #[test]
    fn idempotency_and_state_transitions_are_durable() {
        let store = CommandStore::open(Path::new(":memory:")).unwrap();
        let created = store
            .create_probe("gpu-a", "request-key-0001", 60, 100)
            .unwrap();
        let duplicate = store
            .create_probe("gpu-a", "request-key-0001", 60, 101)
            .unwrap();
        assert_eq!(created.command_id, duplicate.command_id);
        assert!(matches!(
            store.create_probe("gpu-b", "request-key-0001", 60, 101),
            Err(CreateCommandError::Conflict)
        ));

        let claimed = store.claim("gpu-a", 102).unwrap().unwrap();
        assert_eq!(claimed.command_id, created.command_id);
        assert_eq!(
            store.get(&created.command_id).unwrap().unwrap().state,
            CommandState::Delivered
        );
        store
            .report(&report(&created.command_id, CommandState::Accepted), 103)
            .unwrap();
        let mut invalid = report(&created.command_id, CommandState::Completed);
        invalid.result.as_mut().unwrap().hostname = "trainer\nleak".to_owned();
        assert!(matches!(
            store.report(&invalid, 104),
            Err(ReportCommandError::Invalid)
        ));
        let completed = store
            .report(&report(&created.command_id, CommandState::Completed), 104)
            .unwrap();
        assert_eq!(completed.state, CommandState::Completed);
        let duplicate = store
            .report(&report(&created.command_id, CommandState::Completed), 105)
            .unwrap();
        assert_eq!(duplicate.state, CommandState::Completed);
        assert!(matches!(
            store.report(&report(&created.command_id, CommandState::Accepted), 106),
            Err(ReportCommandError::Conflict)
        ));
    }

    #[test]
    fn expired_command_is_never_claimed() {
        let store = CommandStore::open(Path::new(":memory:")).unwrap();
        let created = store
            .create_probe("gpu-a", "request-key-0002", 10, 100)
            .unwrap();
        assert!(store.claim("gpu-a", 110).unwrap().is_none());
        assert_eq!(
            store.get(&created.command_id).unwrap().unwrap().state,
            CommandState::Expired
        );
    }

    #[test]
    fn command_survives_store_reopen() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "farhelm-hub-command-{}-{nonce}.db",
            std::process::id()
        ));
        let command_id = {
            let store = CommandStore::open(&path).unwrap();
            store
                .create_probe("gpu-a", "request-key-reopen", 60, 100)
                .unwrap()
                .command_id
        };
        let reopened = CommandStore::open(&path).unwrap();
        assert_eq!(
            reopened.get(&command_id).unwrap().unwrap().state,
            CommandState::Queued
        );
        drop(reopened);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
    }
}
