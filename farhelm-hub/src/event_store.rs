use std::{
    path::Path,
    sync::{Mutex, MutexGuard},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, ensure};
use farhelm_protocol::{
    AgentEvent, CodexSessionListResponse, CodexSessionMode, CodexSessionState, CodexSessionSummary,
    ExperimentListResponse, ExperimentState, ExperimentSummary, FARHELM_PROTOCOL,
};
use rusqlite::{Connection, Row, params};
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct StoredEvent {
    pub sequence: u64,
    pub event_id: String,
    pub event_type: String,
    pub payload: Value,
}

#[derive(Debug, Clone)]
pub struct PushDelivery {
    pub event_sequence: u64,
    pub event_id: String,
    pub event_type: String,
    pub payload: Value,
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
    pub attempts: u32,
}

pub struct EventStore {
    connection: Mutex<Connection>,
}

impl EventStore {
    pub fn open(path: &Path) -> Result<Self> {
        let connection = Connection::open(path)
            .with_context(|| format!("failed to open Hub event database {}", path.display()))?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        if path != Path::new(":memory:") {
            connection.pragma_update(None, "journal_mode", "WAL")?;
        }
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS agent_events (
                sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                event_id TEXT NOT NULL UNIQUE,
                agent_id TEXT NOT NULL,
                agent_sequence INTEGER NOT NULL,
                event_type TEXT NOT NULL,
                payload_json TEXT NOT NULL,
                created_at_unix INTEGER NOT NULL,
                UNIQUE(agent_id, agent_sequence)
            );
            CREATE TABLE IF NOT EXISTS experiments (
                watch_id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                project_id TEXT NOT NULL,
                name TEXT NOT NULL,
                pid INTEGER NOT NULL,
                state TEXT NOT NULL CHECK (state IN ('watching','succeeded','failed','unknown','cancelled')),
                session_id TEXT,
                detail TEXT,
                updated_at_unix INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS codex_sessions (
                session_id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                project_id TEXT NOT NULL,
                mode TEXT NOT NULL CHECK (mode IN ('inspect','edit')),
                state TEXT NOT NULL CHECK (state IN ('creating','idle','queued','running','interrupting','failed','orphaned','archived')),
                title TEXT,
                active_turn_id TEXT,
                updated_at_unix INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS push_subscriptions (
                endpoint TEXT PRIMARY KEY,
                p256dh TEXT NOT NULL,
                auth TEXT NOT NULL,
                created_at_unix INTEGER NOT NULL,
                updated_at_unix INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS push_deliveries (
                event_sequence INTEGER NOT NULL,
                endpoint TEXT NOT NULL,
                attempts INTEGER NOT NULL DEFAULT 0,
                next_attempt_unix INTEGER NOT NULL,
                state TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending','failed')),
                last_error TEXT,
                PRIMARY KEY(event_sequence,endpoint),
                FOREIGN KEY(event_sequence) REFERENCES agent_events(sequence) ON DELETE CASCADE,
                FOREIGN KEY(endpoint) REFERENCES push_subscriptions(endpoint) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS auth_recovery_codes (
                hash TEXT PRIMARY KEY,
                consumed INTEGER NOT NULL DEFAULT 0 CHECK (consumed IN (0,1))
            );",
        )?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn ingest(&self, agent_id: &str, events: &[AgentEvent]) -> Result<Vec<StoredEvent>> {
        ensure!(events.len() <= 100, "event batch exceeds 100 items");
        let connection = self.lock()?;
        let transaction = connection.unchecked_transaction()?;
        let mut inserted = Vec::new();
        for event in events {
            ensure!(
                event.protocol == FARHELM_PROTOCOL,
                "event protocol mismatch"
            );
            ensure!(event.agent_id == agent_id, "event belongs to another Agent");
            ensure!(
                !event.event_id.is_empty() && event.event_id.len() <= 192,
                "invalid event ID"
            );
            let changed = transaction.execute(
                "INSERT OR IGNORE INTO agent_events (event_id,agent_id,agent_sequence,event_type,payload_json,created_at_unix)
                 VALUES (?1,?2,?3,?4,?5,?6)",
                params![event.event_id,agent_id,as_i64(event.sequence)?,event.event_type,serde_json::to_string(&event.payload)?,as_i64(event.created_at_unix)?],
            )?;
            if changed == 1 {
                let sequence = u64::try_from(transaction.last_insert_rowid())
                    .context("invalid event sequence")?;
                apply_materialized_view(&transaction, agent_id, event)?;
                if pushworthy(event) {
                    transaction.execute(
                        "INSERT OR IGNORE INTO push_deliveries (event_sequence,endpoint,next_attempt_unix)
                         SELECT ?1,endpoint,?2 FROM push_subscriptions",
                        params![as_i64(sequence)?, as_i64(event.created_at_unix)?],
                    )?;
                }
                inserted.push(StoredEvent {
                    sequence,
                    event_id: event.event_id.clone(),
                    event_type: event.event_type.clone(),
                    payload: event.payload.clone(),
                });
            }
        }
        transaction.commit()?;
        Ok(inserted)
    }

    pub fn experiments(&self) -> Result<ExperimentListResponse> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT watch_id,agent_id,project_id,name,pid,state,session_id,detail,updated_at_unix FROM experiments ORDER BY updated_at_unix DESC,watch_id DESC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(ExperimentSummary {
                watch_id: row.get(0)?,
                agent_id: row.get(1)?,
                project_id: row.get(2)?,
                name: row.get(3)?,
                pid: u32::try_from(row.get::<_, i64>(4)?).map_err(conversion(4))?,
                state: parse_experiment_state(&row.get::<_, String>(5)?)?,
                session_id: row.get(6)?,
                detail: row.get(7)?,
                updated_at_unix: row_u64(row, 8)?,
            })
        })?;
        Ok(ExperimentListResponse {
            protocol: FARHELM_PROTOCOL.to_owned(),
            experiments: rows.collect::<rusqlite::Result<Vec<_>>>()?,
        })
    }

    pub fn sessions(&self, project: Option<&str>) -> Result<CodexSessionListResponse> {
        let connection = self.lock()?;
        let sql = "SELECT session_id,agent_id,project_id,mode,state,title,active_turn_id,updated_at_unix FROM codex_sessions WHERE (?1 IS NULL OR project_id=?1) ORDER BY updated_at_unix DESC,session_id DESC";
        let mut statement = connection.prepare(sql)?;
        let rows = statement.query_map([project], |row| {
            Ok(CodexSessionSummary {
                session_id: row.get(0)?,
                agent_id: row.get(1)?,
                project_id: row.get(2)?,
                mode: parse_mode(&row.get::<_, String>(3)?)?,
                state: parse_session_state(&row.get::<_, String>(4)?)?,
                title: row.get(5)?,
                active_turn_id: row.get(6)?,
                updated_at_unix: row_u64(row, 7)?,
            })
        })?;
        Ok(CodexSessionListResponse {
            protocol: FARHELM_PROTOCOL.to_owned(),
            sessions: rows.collect::<rusqlite::Result<Vec<_>>>()?,
        })
    }

    pub fn session(&self, session_id: &str) -> Result<Option<CodexSessionSummary>> {
        use rusqlite::OptionalExtension;
        self.lock()?.query_row(
            "SELECT session_id,agent_id,project_id,mode,state,title,active_turn_id,updated_at_unix FROM codex_sessions WHERE session_id=?1",
            [session_id],
            |row| Ok(CodexSessionSummary {
                session_id: row.get(0)?, agent_id: row.get(1)?, project_id: row.get(2)?, mode: parse_mode(&row.get::<_,String>(3)?)?,
                state: parse_session_state(&row.get::<_,String>(4)?)?, title: row.get(5)?, active_turn_id: row.get(6)?, updated_at_unix: row_u64(row,7)?,
            }),
        ).optional().map_err(Into::into)
    }

    pub fn replay(&self, after: u64, limit: usize) -> Result<Vec<StoredEvent>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare("SELECT sequence,event_id,event_type,payload_json FROM agent_events WHERE sequence>?1 ORDER BY sequence LIMIT ?2")?;
        let rows = statement.query_map(
            params![as_i64(after)?, i64::try_from(limit)?],
            event_from_row,
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn save_push_subscription(
        &self,
        endpoint: &str,
        p256dh: &str,
        auth: &str,
        now: u64,
    ) -> Result<()> {
        ensure!(
            endpoint.starts_with("https://") && endpoint.len() <= 2048,
            "invalid push endpoint"
        );
        ensure!(
            (16..=256).contains(&p256dh.len()) && (8..=128).contains(&auth.len()),
            "invalid push keys"
        );
        self.lock()?.execute(
            "INSERT INTO push_subscriptions (endpoint,p256dh,auth,created_at_unix,updated_at_unix) VALUES (?1,?2,?3,?4,?4)
             ON CONFLICT(endpoint) DO UPDATE SET p256dh=excluded.p256dh,auth=excluded.auth,updated_at_unix=excluded.updated_at_unix",
            params![endpoint,p256dh,auth,as_i64(now)?],
        )?;
        Ok(())
    }

    pub fn delete_push_subscription(&self, endpoint: &str) -> Result<bool> {
        let connection = self.lock()?;
        let transaction = connection.unchecked_transaction()?;
        transaction.execute("DELETE FROM push_deliveries WHERE endpoint=?1", [endpoint])?;
        let changed = transaction.execute(
            "DELETE FROM push_subscriptions WHERE endpoint=?1",
            [endpoint],
        )?;
        transaction.commit()?;
        Ok(changed == 1)
    }

    pub fn pending_push_deliveries(&self, now: u64, limit: usize) -> Result<Vec<PushDelivery>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT d.event_sequence,e.event_id,e.event_type,e.payload_json,d.endpoint,s.p256dh,s.auth,d.attempts
               FROM push_deliveries d
               JOIN agent_events e ON e.sequence=d.event_sequence
               JOIN push_subscriptions s ON s.endpoint=d.endpoint
              WHERE d.state='pending' AND d.next_attempt_unix<=?1
              ORDER BY d.next_attempt_unix,d.event_sequence LIMIT ?2",
        )?;
        let rows = statement.query_map(params![as_i64(now)?, i64::try_from(limit)?], |row| {
            let encoded: String = row.get(3)?;
            Ok(PushDelivery {
                event_sequence: row_u64(row, 0)?,
                event_id: row.get(1)?,
                event_type: row.get(2)?,
                payload: serde_json::from_str(&encoded).map_err(json_conversion(3))?,
                endpoint: row.get(4)?,
                p256dh: row.get(5)?,
                auth: row.get(6)?,
                attempts: u32::try_from(row.get::<_, i64>(7)?).map_err(conversion(7))?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn mark_push_sent(&self, delivery: &PushDelivery) -> Result<()> {
        self.lock()?.execute(
            "DELETE FROM push_deliveries WHERE event_sequence=?1 AND endpoint=?2",
            params![as_i64(delivery.event_sequence)?, delivery.endpoint],
        )?;
        Ok(())
    }

    pub fn mark_push_failed(
        &self,
        delivery: &PushDelivery,
        permanent: bool,
        detail: &str,
        now: u64,
    ) -> Result<()> {
        let connection = self.lock()?;
        let transaction = connection.unchecked_transaction()?;
        if permanent {
            transaction.execute(
                "DELETE FROM push_deliveries WHERE endpoint=?1",
                [&delivery.endpoint],
            )?;
            transaction.execute(
                "DELETE FROM push_subscriptions WHERE endpoint=?1",
                [&delivery.endpoint],
            )?;
        } else {
            let attempts = delivery.attempts.saturating_add(1);
            let exhausted = attempts >= 8;
            let delay = 30_u64.saturating_mul(1_u64 << attempts.min(7)).min(3600);
            transaction.execute(
                "UPDATE push_deliveries SET attempts=?1,next_attempt_unix=?2,state=?3,last_error=?4
                  WHERE event_sequence=?5 AND endpoint=?6",
                params![
                    i64::from(attempts),
                    as_i64(now.saturating_add(delay))?,
                    if exhausted { "failed" } else { "pending" },
                    detail.chars().take(200).collect::<String>(),
                    as_i64(delivery.event_sequence)?,
                    delivery.endpoint
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn sync_recovery_codes(&self, hashes: &[String]) -> Result<Vec<String>> {
        let connection = self.lock()?;
        let transaction = connection.unchecked_transaction()?;
        for hash in hashes {
            transaction.execute(
                "INSERT OR IGNORE INTO auth_recovery_codes (hash) VALUES (?1)",
                [hash],
            )?;
        }
        let mut statement = transaction
            .prepare("SELECT hash FROM auth_recovery_codes WHERE consumed=0 ORDER BY rowid")?;
        let available = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        transaction.commit()?;
        Ok(available)
    }

    pub fn consume_recovery_code(&self, hash: &str) -> Result<bool> {
        Ok(self.lock()?.execute(
            "UPDATE auth_recovery_codes SET consumed=1 WHERE hash=?1 AND consumed=0",
            [hash],
        )? == 1)
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>> {
        self.connection
            .lock()
            .map_err(|_| anyhow!("Hub event database lock was poisoned"))
    }
}

fn pushworthy(event: &AgentEvent) -> bool {
    match event.event_type.as_str() {
        "experiment.updated" => event
            .payload
            .get("state")
            .and_then(Value::as_str)
            .is_some_and(|state| matches!(state, "succeeded" | "failed" | "unknown")),
        "codex.turn.completed" | "codex.turn.failed" | "codex.turn.orphaned" => true,
        _ => false,
    }
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

fn apply_materialized_view(
    connection: &Connection,
    agent_id: &str,
    event: &AgentEvent,
) -> Result<()> {
    match event.event_type.as_str() {
        "experiment.updated" => {
            let mut value = event.payload.clone();
            if let Value::Object(ref mut map) = value {
                map.insert("agent_id".to_owned(), Value::String(agent_id.to_owned()));
            }
            let experiment: ExperimentSummary =
                serde_json::from_value(value).context("invalid experiment event")?;
            connection.execute(
                "INSERT INTO experiments (watch_id,agent_id,project_id,name,pid,state,session_id,detail,updated_at_unix)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
                 ON CONFLICT(watch_id) DO UPDATE SET state=excluded.state,session_id=excluded.session_id,detail=excluded.detail,updated_at_unix=excluded.updated_at_unix
                 WHERE excluded.updated_at_unix>=experiments.updated_at_unix",
                params![experiment.watch_id,agent_id,experiment.project_id,experiment.name,i64::from(experiment.pid),experiment_state_name(experiment.state),experiment.session_id,experiment.detail,as_i64(experiment.updated_at_unix)?],
            )?;
        }
        "codex.session.updated" => {
            let mut value = event.payload.clone();
            if let Value::Object(ref mut map) = value {
                map.insert("agent_id".to_owned(), Value::String(agent_id.to_owned()));
            }
            let session: CodexSessionSummary =
                serde_json::from_value(value).context("invalid Codex session event")?;
            connection.execute(
                "INSERT INTO codex_sessions (session_id,agent_id,project_id,mode,state,title,active_turn_id,updated_at_unix)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
                 ON CONFLICT(session_id) DO UPDATE SET state=excluded.state,title=excluded.title,active_turn_id=excluded.active_turn_id,updated_at_unix=excluded.updated_at_unix
                 WHERE excluded.updated_at_unix>=codex_sessions.updated_at_unix",
                params![session.session_id,agent_id,session.project_id,mode_name(session.mode),session_state_name(session.state),session.title,session.active_turn_id,as_i64(session.updated_at_unix)?],
            )?;
        }
        _ => {}
    }
    Ok(())
}

fn event_from_row(row: &Row<'_>) -> rusqlite::Result<StoredEvent> {
    let encoded: String = row.get(3)?;
    Ok(StoredEvent {
        sequence: row_u64(row, 0)?,
        event_id: row.get(1)?,
        event_type: row.get(2)?,
        payload: serde_json::from_str(&encoded).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                3,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
    })
}

const fn experiment_state_name(state: ExperimentState) -> &'static str {
    match state {
        ExperimentState::Watching => "watching",
        ExperimentState::Succeeded => "succeeded",
        ExperimentState::Failed => "failed",
        ExperimentState::Unknown => "unknown",
        ExperimentState::Cancelled => "cancelled",
    }
}
fn parse_experiment_state(value: &str) -> rusqlite::Result<ExperimentState> {
    match value {
        "watching" => Ok(ExperimentState::Watching),
        "succeeded" => Ok(ExperimentState::Succeeded),
        "failed" => Ok(ExperimentState::Failed),
        "unknown" => Ok(ExperimentState::Unknown),
        "cancelled" => Ok(ExperimentState::Cancelled),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}
const fn mode_name(mode: CodexSessionMode) -> &'static str {
    match mode {
        CodexSessionMode::Inspect => "inspect",
        CodexSessionMode::Edit => "edit",
    }
}
fn parse_mode(value: &str) -> rusqlite::Result<CodexSessionMode> {
    match value {
        "inspect" => Ok(CodexSessionMode::Inspect),
        "edit" => Ok(CodexSessionMode::Edit),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}
const fn session_state_name(state: CodexSessionState) -> &'static str {
    match state {
        CodexSessionState::Creating => "creating",
        CodexSessionState::Idle => "idle",
        CodexSessionState::Queued => "queued",
        CodexSessionState::Running => "running",
        CodexSessionState::Interrupting => "interrupting",
        CodexSessionState::Failed => "failed",
        CodexSessionState::Orphaned => "orphaned",
        CodexSessionState::Archived => "archived",
    }
}
fn parse_session_state(value: &str) -> rusqlite::Result<CodexSessionState> {
    match value {
        "creating" => Ok(CodexSessionState::Creating),
        "idle" => Ok(CodexSessionState::Idle),
        "queued" => Ok(CodexSessionState::Queued),
        "running" => Ok(CodexSessionState::Running),
        "interrupting" => Ok(CodexSessionState::Interrupting),
        "failed" => Ok(CodexSessionState::Failed),
        "orphaned" => Ok(CodexSessionState::Orphaned),
        "archived" => Ok(CodexSessionState::Archived),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}
fn as_i64(value: u64) -> Result<i64> {
    i64::try_from(value).context("value exceeds SQLite INTEGER range")
}
fn row_u64(row: &Row<'_>, index: usize) -> rusqlite::Result<u64> {
    u64::try_from(row.get::<_, i64>(index)?).map_err(conversion(index))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_event_push_retries_are_durable_and_bounded() {
        let store = EventStore::open(Path::new(":memory:")).unwrap();
        store
            .save_push_subscription(
                "https://push.example.test/id",
                &"a".repeat(88),
                &"b".repeat(22),
                10,
            )
            .unwrap();
        let event = AgentEvent {
            protocol: FARHELM_PROTOCOL.into(),
            sequence: 1,
            event_id: "event-terminal".into(),
            agent_id: "agent-a".into(),
            event_type: "experiment.updated".into(),
            payload: serde_json::json!({
                "watch_id":"watch-a","agent_id":"agent-a","project_id":"p","name":"run",
                "pid":42,"state":"succeeded","updated_at_unix":10
            }),
            created_at_unix: 10,
        };
        store.ingest("agent-a", &[event]).unwrap();
        let mut now = 10;
        for attempt in 1..=8 {
            let delivery = store.pending_push_deliveries(now, 1).unwrap().remove(0);
            assert_eq!(delivery.attempts, attempt - 1);
            store
                .mark_push_failed(&delivery, false, "temporary", now)
                .unwrap();
            now += 4000;
        }
        assert!(store.pending_push_deliveries(now, 1).unwrap().is_empty());
    }

    #[test]
    fn consumed_recovery_code_stays_consumed() {
        let store = EventStore::open(Path::new(":memory:")).unwrap();
        assert_eq!(
            store.sync_recovery_codes(&["hash-a".into()]).unwrap(),
            ["hash-a"]
        );
        assert!(store.consume_recovery_code("hash-a").unwrap());
        assert!(!store.consume_recovery_code("hash-a").unwrap());
        assert!(
            store
                .sync_recovery_codes(&["hash-a".into()])
                .unwrap()
                .is_empty()
        );
    }
}
