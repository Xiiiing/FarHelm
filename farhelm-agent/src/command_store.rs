use std::{
    path::Path,
    sync::{Mutex, MutexGuard},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, ensure};
use farhelm_protocol::{
    AgentCommand, CommandReportRequest, CommandState, FARHELM_PROTOCOL, ProbeResult,
};
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Debug, Clone)]
pub struct PendingCommand {
    pub command_id: String,
    pub state: CommandState,
    pub expires_at_unix: u64,
    pub result: Option<ProbeResult>,
    pub detail: Option<String>,
    pub reported: bool,
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
                .unwrap_or_else(|| Path::new("."));
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create Agent database directory {}",
                    parent.display()
                )
            })?;
        }
        let connection = Connection::open(path)
            .with_context(|| format!("failed to open Agent database {}", path.display()))?;
        connection.busy_timeout(Duration::from_secs(5))?;
        if path != Path::new(":memory:") {
            connection.pragma_update(None, "journal_mode", "WAL")?;
        }
        let schema_version: i64 =
            connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        ensure!(
            schema_version <= 1,
            "Agent database schema is newer than this binary"
        );
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS processed_commands (
                command_id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                action TEXT NOT NULL CHECK (action = 'agent.probe'),
                expires_at_unix INTEGER NOT NULL,
                state TEXT NOT NULL CHECK (state IN ('accepted','completed','failed','expired')),
                result_json TEXT,
                detail TEXT,
                reported INTEGER NOT NULL CHECK (reported IN (0,1)),
                updated_at_unix INTEGER NOT NULL
            );",
        )?;
        if schema_version == 0 {
            connection.pragma_update(None, "user_version", 1)?;
        }
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn receive(&self, command: &AgentCommand, now: u64) -> Result<PendingCommand> {
        let connection = self.lock()?;
        if let Some(existing) = read_pending(&connection, &command.command_id)? {
            let identity: (String, String, u64) = connection.query_row(
                "SELECT agent_id, action, expires_at_unix FROM processed_commands WHERE command_id = ?1",
                [&command.command_id],
                |row| Ok((row.get(0)?, row.get(1)?, row_u64(row, 2)?)),
            )?;
            ensure!(
                identity.0 == command.agent_id
                    && identity.1 == "agent.probe"
                    && identity.2 == command.expires_at_unix,
                "duplicate command identity does not match persisted command"
            );
            return Ok(existing);
        }
        let state = if now >= command.expires_at_unix {
            CommandState::Expired
        } else {
            CommandState::Accepted
        };
        connection.execute(
            "INSERT INTO processed_commands (
                command_id, agent_id, action, expires_at_unix, state, reported, updated_at_unix
            ) VALUES (?1, ?2, 'agent.probe', ?3, ?4, 0, ?5)",
            params![
                command.command_id,
                command.agent_id,
                as_i64(command.expires_at_unix)?,
                state_name(state),
                as_i64(now)?
            ],
        )?;
        read_pending(&connection, &command.command_id)?
            .context("persisted Agent command was not found")
    }

    pub fn next_work(&self) -> Result<Option<PendingCommand>> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT command_id, state, expires_at_unix, result_json, detail, reported
                   FROM processed_commands
                  WHERE (state = 'accepted') OR (reported = 0)
                  ORDER BY updated_at_unix, command_id LIMIT 1",
                [],
                pending_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn mark_reported(&self, command_id: &str, state: CommandState, now: u64) -> Result<()> {
        let changed = self.lock()?.execute(
            "UPDATE processed_commands SET reported = 1, updated_at_unix = ?1
              WHERE command_id = ?2 AND state = ?3",
            params![as_i64(now)?, command_id, state_name(state)],
        )?;
        ensure!(
            changed == 1,
            "local command state changed before report acknowledgement"
        );
        Ok(())
    }

    pub fn expire(&self, command_id: &str, now: u64) -> Result<()> {
        let changed = self.lock()?.execute(
            "UPDATE processed_commands
                SET state = 'expired', result_json = NULL, detail = NULL,
                    reported = 0, updated_at_unix = ?1
              WHERE command_id = ?2 AND state = 'accepted' AND expires_at_unix <= ?1",
            params![as_i64(now)?, command_id],
        )?;
        ensure!(changed == 1, "command could not transition to expired");
        Ok(())
    }

    pub fn complete_probe(&self, command_id: &str, result: &ProbeResult, now: u64) -> Result<()> {
        let result_json = serde_json::to_string(result)?;
        let changed = self.lock()?.execute(
            "UPDATE processed_commands
                SET state = 'completed', result_json = ?1, detail = NULL,
                    reported = 0, updated_at_unix = ?2
              WHERE command_id = ?3 AND state = 'accepted' AND reported = 1",
            params![result_json, as_i64(now)?, command_id],
        )?;
        ensure!(
            changed == 1,
            "probe command was not durably accepted before execution"
        );
        Ok(())
    }

    pub fn report(&self, pending: &PendingCommand, agent_id: &str) -> CommandReportRequest {
        CommandReportRequest {
            protocol: FARHELM_PROTOCOL.to_owned(),
            agent_id: agent_id.to_owned(),
            command_id: pending.command_id.clone(),
            state: pending.state,
            result: pending.result.clone(),
            detail: pending.detail.clone(),
        }
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>> {
        self.connection
            .lock()
            .map_err(|_| anyhow!("Agent command database lock was poisoned"))
    }
}

fn read_pending(connection: &Connection, command_id: &str) -> Result<Option<PendingCommand>> {
    connection
        .query_row(
            "SELECT command_id, state, expires_at_unix, result_json, detail, reported
               FROM processed_commands WHERE command_id = ?1",
            [command_id],
            pending_from_row,
        )
        .optional()
        .map_err(Into::into)
}

fn as_i64(value: u64) -> Result<i64> {
    i64::try_from(value).context("timestamp exceeds SQLite INTEGER range")
}

fn row_u64(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

fn pending_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PendingCommand> {
    let result_json: Option<String> = row.get(3)?;
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
    Ok(PendingCommand {
        command_id: row.get(0)?,
        state: parse_state(&row.get::<_, String>(1)?)?,
        expires_at_unix: row_u64(row, 2)?,
        result,
        detail: row.get(4)?,
        reported: row.get::<_, i64>(5)? == 1,
    })
}

fn parse_state(value: &str) -> rusqlite::Result<CommandState> {
    match value {
        "accepted" => Ok(CommandState::Accepted),
        "completed" => Ok(CommandState::Completed),
        "failed" => Ok(CommandState::Failed),
        "expired" => Ok(CommandState::Expired),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

const fn state_name(state: CommandState) -> &'static str {
    match state {
        CommandState::Accepted => "accepted",
        CommandState::Completed => "completed",
        CommandState::Failed => "failed",
        CommandState::Expired => "expired",
        _ => "invalid",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use farhelm_protocol::CommandAction;

    fn command() -> AgentCommand {
        AgentCommand {
            protocol: FARHELM_PROTOCOL.to_owned(),
            command_id: "cmd_0000000000000001".to_owned(),
            agent_id: "gpu-a".to_owned(),
            action: CommandAction::AgentProbe,
            created_at_unix: 100,
            expires_at_unix: 160,
        }
    }

    #[test]
    fn duplicate_delivery_returns_cached_completion() {
        let store = CommandStore::open(Path::new(":memory:")).unwrap();
        let accepted = store.receive(&command(), 101).unwrap();
        assert_eq!(accepted.state, CommandState::Accepted);
        store
            .mark_reported(&accepted.command_id, CommandState::Accepted, 102)
            .unwrap();
        let result = ProbeResult {
            agent_version: "0.2.0".to_owned(),
            hostname: "trainer-a".to_owned(),
        };
        store
            .complete_probe(&accepted.command_id, &result, 103)
            .unwrap();
        let duplicate = store.receive(&command(), 104).unwrap();
        assert_eq!(duplicate.state, CommandState::Completed);
        assert_eq!(duplicate.result, Some(result));
    }

    #[test]
    fn command_identity_survives_database_reopen() {
        let path = std::env::temp_dir().join(format!(
            "farhelm-agent-command-{}-{}.db",
            std::process::id(),
            crate::unix_time()
        ));
        {
            let store = CommandStore::open(&path).unwrap();
            store.receive(&command(), 101).unwrap();
        }
        let reopened = CommandStore::open(&path).unwrap();
        assert_eq!(
            reopened.receive(&command(), 102).unwrap().state,
            CommandState::Accepted
        );
        drop(reopened);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
    }

    #[test]
    fn accepted_command_can_expire_before_execution() {
        let store = CommandStore::open(Path::new(":memory:")).unwrap();
        let accepted = store.receive(&command(), 101).unwrap();
        store.expire(&accepted.command_id, 160).unwrap();
        let expired = store.next_work().unwrap().unwrap();
        assert_eq!(expired.state, CommandState::Expired);
        assert!(!expired.reported);
    }
}
