#[path = "notification_store.rs"]
pub mod notifications;
use std::{
    path::Path,
    sync::{Mutex, MutexGuard},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, ensure};
use farhelm_protocol::{
    AgentEvent, CodexScheduleListResponse, CodexScheduleState, CodexScheduleSummary,
    CodexSessionListResponse, CodexSessionMode, CodexSessionState, CodexSessionSummary,
    ExperimentListResponse, ExperimentState, ExperimentSummary, FARHELM_PROTOCOL,
    ProjectCandidateState, ProjectCandidateSummary, ProjectListResponse,
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

#[derive(Debug, Clone)]
pub struct StoredBrowserSession {
    pub user: String,
    pub csrf_token: String,
    pub expires_at_unix: u64,
}

#[derive(Debug, Clone)]
pub struct PairingRecord {
    pub pairing_id: String,
    pub agent_id: String,
    pub attempts: u32,
    pub expires_at_unix: u64,
    pub consumed: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum ArchiveFilter {
    Current,
    Archived,
    All,
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
        crate::migrations::apply(&connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn save_browser_session(
        &self,
        token_hash: &str,
        user: &str,
        csrf_token: &str,
        created_at_unix: u64,
        expires_at_unix: u64,
    ) -> Result<()> {
        self.lock()?.execute(
            "INSERT INTO browser_sessions (token_hash,user,csrf_token,created_at_unix,expires_at_unix) VALUES (?1,?2,?3,?4,?5)",
            params![token_hash,user,csrf_token,as_i64(created_at_unix)?,as_i64(expires_at_unix)?],
        )?;
        Ok(())
    }

    pub fn browser_session(
        &self,
        token_hash: &str,
        now: u64,
    ) -> Result<Option<StoredBrowserSession>> {
        use rusqlite::OptionalExtension;
        let connection = self.lock()?;
        connection.execute(
            "DELETE FROM browser_sessions WHERE expires_at_unix<=?1",
            [as_i64(now)?],
        )?;
        connection
            .query_row(
                "SELECT user,csrf_token,expires_at_unix FROM browser_sessions WHERE token_hash=?1",
                [token_hash],
                |row| {
                    Ok(StoredBrowserSession {
                        user: row.get(0)?,
                        csrf_token: row.get(1)?,
                        expires_at_unix: row_u64(row, 2)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn delete_browser_session(&self, token_hash: &str) -> Result<bool> {
        Ok(self.lock()?.execute(
            "DELETE FROM browser_sessions WHERE token_hash=?1",
            [token_hash],
        )? == 1)
    }

    pub fn revoke_browser_sessions(&self) -> Result<()> {
        self.lock()?.execute("DELETE FROM browser_sessions", [])?;
        Ok(())
    }

    pub fn login_failure_count(&self, now: u64, window_secs: u64) -> Result<u64> {
        let cutoff = now.saturating_sub(window_secs);
        let connection = self.lock()?;
        connection.execute(
            "DELETE FROM login_failures WHERE occurred_at_unix<?1",
            [as_i64(cutoff)?],
        )?;
        let count: i64 =
            connection.query_row("SELECT COUNT(*) FROM login_failures", [], |row| row.get(0))?;
        u64::try_from(count).context("invalid login failure count")
    }

    pub fn record_login_failure(&self, now: u64) -> Result<()> {
        self.lock()?.execute(
            "INSERT INTO login_failures (occurred_at_unix) VALUES (?1)",
            [as_i64(now)?],
        )?;
        Ok(())
    }

    pub fn clear_login_failures(&self) -> Result<()> {
        self.lock()?.execute("DELETE FROM login_failures", [])?;
        Ok(())
    }

    pub fn import_agent_credential(
        &self,
        agent_id: &str,
        token_hash: &str,
        now: u64,
    ) -> Result<()> {
        self.lock()?.execute(
            "INSERT OR IGNORE INTO agent_credentials (agent_id,token_hash,created_at_unix,updated_at_unix) VALUES (?1,?2,?3,?3)",
            params![agent_id,token_hash,as_i64(now)?],
        )?;
        Ok(())
    }

    pub fn agent_for_token_hash(&self, token_hash: &str) -> Result<Option<String>> {
        use rusqlite::OptionalExtension;
        self.lock()?
            .query_row(
                "SELECT agent_id FROM agent_credentials WHERE token_hash=?1",
                [token_hash],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn create_pairing_code(
        &self,
        pairing_id: &str,
        agent_id: &str,
        code_hash: &str,
        now: u64,
        expires_at_unix: u64,
    ) -> Result<()> {
        let connection = self.lock()?;
        let transaction = crate::migrations::write_transaction(&connection)?;
        transaction.execute(
            "DELETE FROM pairing_codes WHERE agent_id=?1 AND consumed=0",
            [agent_id],
        )?;
        transaction.execute(
            "INSERT INTO pairing_codes (pairing_id,agent_id,code_hash,expires_at_unix,created_at_unix) VALUES (?1,?2,?3,?4,?5)",
            params![pairing_id,agent_id,code_hash,as_i64(expires_at_unix)?,as_i64(now)?],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn pairing_by_hash(&self, code_hash: &str) -> Result<Option<PairingRecord>> {
        use rusqlite::OptionalExtension;
        self.lock()?.query_row(
            "SELECT pairing_id,agent_id,attempts,expires_at_unix,consumed FROM pairing_codes WHERE code_hash=?1",
            [code_hash],
            |row| Ok(PairingRecord { pairing_id: row.get(0)?, agent_id: row.get(1)?, attempts: u32::try_from(row.get::<_,i64>(2)?).map_err(conversion(2))?, expires_at_unix: row_u64(row,3)?, consumed: row.get::<_,i64>(4)? != 0 }),
        ).optional().map_err(Into::into)
    }

    pub fn consume_pairing_and_set_credential(
        &self,
        pairing_id: &str,
        agent_id: &str,
        token_hash: &str,
        now: u64,
    ) -> Result<bool> {
        let connection = self.lock()?;
        let transaction = crate::migrations::write_transaction(&connection)?;
        let consumed = transaction.execute(
            "UPDATE pairing_codes SET consumed=1 WHERE pairing_id=?1 AND agent_id=?2 AND consumed=0 AND attempts<5 AND expires_at_unix>?3",
            params![pairing_id,agent_id,as_i64(now)?],
        )? == 1;
        if consumed {
            transaction.execute(
                "INSERT INTO agent_credentials (agent_id,token_hash,created_at_unix,updated_at_unix) VALUES (?1,?2,?3,?3)
                 ON CONFLICT(agent_id) DO UPDATE SET token_hash=excluded.token_hash,updated_at_unix=excluded.updated_at_unix",
                params![agent_id,token_hash,as_i64(now)?],
            )?;
        }
        transaction.commit()?;
        Ok(consumed)
    }

    pub fn pairing_failure_count(&self, now: u64, window_secs: u64) -> Result<u64> {
        let cutoff = now.saturating_sub(window_secs);
        let connection = self.lock()?;
        connection.execute(
            "DELETE FROM pairing_failures WHERE occurred_at_unix<?1",
            [as_i64(cutoff)?],
        )?;
        let count: i64 =
            connection.query_row("SELECT COUNT(*) FROM pairing_failures", [], |row| {
                row.get(0)
            })?;
        u64::try_from(count).context("invalid pairing failure count")
    }

    pub fn record_pairing_failure(&self, now: u64) -> Result<()> {
        self.lock()?.execute(
            "INSERT INTO pairing_failures (occurred_at_unix) VALUES (?1)",
            [as_i64(now)?],
        )?;
        Ok(())
    }

    pub fn clear_pairing_failures(&self) -> Result<()> {
        self.lock()?.execute("DELETE FROM pairing_failures", [])?;
        Ok(())
    }

    pub fn delete_pairing_code(&self, pairing_id: &str) -> Result<bool> {
        Ok(self.lock()?.execute(
            "DELETE FROM pairing_codes WHERE pairing_id=?1",
            [pairing_id],
        )? == 1)
    }

    pub fn projects(&self) -> Result<ProjectListResponse> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT candidate_id,agent_id,display_name,suggested_project_id,session_count,state,updated_at_unix FROM project_candidates ORDER BY state,updated_at_unix DESC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(ProjectCandidateSummary {
                candidate_id: row.get(0)?,
                agent_id: row.get(1)?,
                display_name: row.get(2)?,
                suggested_project_id: row.get(3)?,
                session_count: row_u64(row, 4)?,
                state: parse_project_state(&row.get::<_, String>(5)?)?,
                updated_at_unix: row_u64(row, 6)?,
            })
        })?;
        Ok(ProjectListResponse {
            protocol: FARHELM_PROTOCOL.to_owned(),
            projects: rows.collect::<rusqlite::Result<Vec<_>>>()?,
        })
    }

    pub fn project_candidate_agent(&self, candidate_id: &str) -> Result<Option<String>> {
        use rusqlite::OptionalExtension;
        self.lock()?
            .query_row(
                "SELECT agent_id FROM project_candidates WHERE candidate_id=?1",
                [candidate_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn ingest(&self, agent_id: &str, events: &[AgentEvent]) -> Result<Vec<StoredEvent>> {
        ensure!(events.len() <= 100, "event batch exceeds 100 items");
        let connection = self.lock()?;
        let transaction = crate::migrations::write_transaction(&connection)?;
        let mut inserted = Vec::new();
        for original in events {
            let mut sanitized = original.clone();
            sanitized.payload =
                farhelm_protocol::public_event_payload(&original.event_type, &original.payload);
            let event = &sanitized;
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
                let notified = notifications::ingest(&transaction, agent_id, event)?;
                if !notified
                    && pushworthy(event)
                    && !event.event_type.starts_with("codex.")
                    && event.event_type != "experiment.updated"
                {
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

    pub fn sessions(
        &self,
        project: Option<&str>,
        archived: ArchiveFilter,
        agent: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> Result<CodexSessionListResponse> {
        let connection = self.lock()?;
        let archive_clause = match archived {
            ArchiveFilter::Current => " AND state!='archived'",
            ArchiveFilter::Archived => " AND state='archived'",
            ArchiveFilter::All => "",
        };
        let sql = format!(
            "SELECT session_id,agent_id,project_id,mode,state,title,active_turn_id,updated_at_unix,COALESCE((SELECT MAX(revision) FROM projection_revisions WHERE entity_id IN ('session:'||agent_id||':'||session_id||':metadata','session:'||agent_id||':'||session_id||':execution','session:'||agent_id||':'||session_id||':title')),0) FROM codex_sessions WHERE (?1 IS NULL OR project_id=?1) AND (?2 IS NULL OR agent_id=?2){archive_clause} ORDER BY updated_at_unix DESC,session_id DESC LIMIT ?3 OFFSET ?4"
        );
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map(
            params![
                project,
                agent,
                i64::try_from(limit + 1)?,
                i64::try_from(offset)?
            ],
            |row| {
                Ok(CodexSessionSummary {
                    session_id: row.get(0)?,
                    agent_id: row.get(1)?,
                    project_id: row.get(2)?,
                    mode: parse_mode(&row.get::<_, String>(3)?)?,
                    state: parse_session_state(&row.get::<_, String>(4)?)?,
                    title: row.get(5)?,
                    active_turn_id: row.get(6)?,
                    updated_at_unix: row_u64(row, 7)?,
                    revision: row_u64(row, 8)?,
                })
            },
        )?;
        let mut sessions = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        let next_cursor = (sessions.len() > limit).then(|| (offset + limit).to_string());
        sessions.truncate(limit);
        Ok(CodexSessionListResponse {
            protocol: FARHELM_PROTOCOL.to_owned(),
            sessions,
            next_cursor,
        })
    }

    pub fn session_agents(
        &self,
        agent: Option<&str>,
        project: Option<&str>,
    ) -> Result<Vec<String>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare("SELECT DISTINCT agent_id FROM codex_sessions WHERE (?1 IS NULL OR agent_id=?1) AND (?2 IS NULL OR project_id=?2) ORDER BY agent_id")?;
        Ok(statement
            .query_map(params![agent, project], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn session(&self, session_id: &str) -> Result<Option<CodexSessionSummary>> {
        use rusqlite::OptionalExtension;
        self.lock()?.query_row(
            "SELECT session_id,agent_id,project_id,mode,state,title,active_turn_id,updated_at_unix,COALESCE((SELECT MAX(revision) FROM projection_revisions WHERE entity_id IN ('session:'||agent_id||':'||session_id||':metadata','session:'||agent_id||':'||session_id||':execution','session:'||agent_id||':'||session_id||':title')),0) FROM codex_sessions WHERE session_id=?1",
            [session_id],
            |row| Ok(CodexSessionSummary {
                session_id: row.get(0)?, agent_id: row.get(1)?, project_id: row.get(2)?, mode: parse_mode(&row.get::<_,String>(3)?)?,
                state: parse_session_state(&row.get::<_,String>(4)?)?, title: row.get(5)?, active_turn_id: row.get(6)?, updated_at_unix: row_u64(row,7)?, revision: row_u64(row,8)?,
            }),
        ).optional().map_err(Into::into)
    }

    pub fn schedules(&self, session_id: Option<&str>) -> Result<CodexScheduleListResponse> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT schedule_id,agent_id,session_id,project_id,trigger_json,state,created_at_unix,updated_at_unix
             FROM codex_schedules WHERE (?1 IS NULL OR session_id=?1) ORDER BY updated_at_unix DESC,schedule_id DESC",
        )?;
        let rows = statement.query_map([session_id], |row| {
            let trigger: String = row.get(4)?;
            Ok(CodexScheduleSummary {
                schedule_id: row.get(0)?,
                agent_id: row.get(1)?,
                session_id: row.get(2)?,
                project_id: row.get(3)?,
                trigger: serde_json::from_str(&trigger).map_err(json_conversion(4))?,
                state: parse_schedule_state(&row.get::<_, String>(5)?)?,
                created_at_unix: row_u64(row, 6)?,
                updated_at_unix: row_u64(row, 7)?,
            })
        })?;
        Ok(CodexScheduleListResponse {
            protocol: FARHELM_PROTOCOL.to_owned(),
            schedules: rows.collect::<rusqlite::Result<Vec<_>>>()?,
        })
    }

    pub fn schedule(&self, schedule_id: &str) -> Result<Option<CodexScheduleSummary>> {
        use rusqlite::OptionalExtension;
        self.lock()?.query_row(
            "SELECT schedule_id,agent_id,session_id,project_id,trigger_json,state,created_at_unix,updated_at_unix FROM codex_schedules WHERE schedule_id=?1",
            [schedule_id], |row| {
                let trigger: String = row.get(4)?;
                Ok(CodexScheduleSummary { schedule_id:row.get(0)?,agent_id:row.get(1)?,session_id:row.get(2)?,project_id:row.get(3)?,trigger:serde_json::from_str(&trigger).map_err(json_conversion(4))?,state:parse_schedule_state(&row.get::<_,String>(5)?)?,created_at_unix:row_u64(row,6)?,updated_at_unix:row_u64(row,7)? })
            }
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
        let transaction = crate::migrations::write_transaction(&connection)?;
        transaction.execute("UPDATE notification_deliveries SET state='failed',last_error='subscription revoked' WHERE endpoint=?1 AND state='pending'",[endpoint])?;
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
        let transaction = crate::migrations::write_transaction(&connection)?;
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
            let metadata =
                event.payload.get("update_kind").and_then(Value::as_str) == Some("metadata");
            let dimension = if metadata { "metadata" } else { "execution" };
            let revision_key = format!("session:{agent_id}:{}:{dimension}", session.session_id);
            if connection.execute("INSERT INTO projection_revisions VALUES(?1,?2) ON CONFLICT(entity_id) DO UPDATE SET revision=excluded.revision WHERE excluded.revision>projection_revisions.revision", params![revision_key,as_i64(event.sequence)?])? == 0 {
                return Ok(());
            }
            let title = session
                .title
                .filter(|title| farhelm_protocol::valid_session_title(title));
            connection.execute(
                "INSERT INTO codex_sessions (session_id,agent_id,project_id,mode,state,title,active_turn_id,updated_at_unix)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
                 ON CONFLICT(session_id) DO UPDATE SET
                 mode=CASE WHEN ?9 THEN codex_sessions.mode ELSE excluded.mode END,
                 state=CASE WHEN NOT ?9 THEN excluded.state
                   WHEN codex_sessions.state IN ('creating','queued','running','interrupting') THEN codex_sessions.state
                   WHEN excluded.state='archived' THEN 'archived'
                   WHEN codex_sessions.state='archived' THEN 'idle' ELSE codex_sessions.state END,
                 active_turn_id=CASE WHEN ?9 THEN codex_sessions.active_turn_id ELSE excluded.active_turn_id END,
                 updated_at_unix=MAX(codex_sessions.updated_at_unix,excluded.updated_at_unix)
                 WHERE codex_sessions.agent_id=excluded.agent_id",
                params![session.session_id,agent_id,session.project_id,mode_name(session.mode),session_state_name(session.state),title,session.active_turn_id,as_i64(session.updated_at_unix)?,metadata],
            )?;
            if let Some(title) = title {
                let key = format!("session:{agent_id}:{}:title", session.session_id);
                if connection.execute("INSERT INTO projection_revisions VALUES(?1,?2) ON CONFLICT(entity_id) DO UPDATE SET revision=excluded.revision WHERE excluded.revision>projection_revisions.revision", params![key,as_i64(event.sequence)?])? > 0 {
                    connection.execute("UPDATE codex_sessions SET title=?1 WHERE session_id=?2 AND agent_id=?3", params![title, session.session_id,agent_id])?;
                }
            }
        }
        "codex.schedule.updated" => {
            let id = format!(
                "{agent_id}:{}",
                required_string(&event.payload, "schedule_id")?
            );
            if let Some(revision) = event.payload.get("revision").and_then(Value::as_u64) {
                let changed=connection.execute("INSERT INTO projection_revisions VALUES(?1,?2) ON CONFLICT(entity_id) DO UPDATE SET revision=excluded.revision WHERE excluded.revision>projection_revisions.revision",params![id,as_i64(revision)?])?;
                if changed == 0 {
                    return Ok(());
                }
            } else if connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM projection_revisions WHERE entity_id=?1)",
                [id],
                |r| r.get::<_, bool>(0),
            )? {
                return Ok(());
            }
            let mut value = event.payload.clone();
            if let Value::Object(ref mut map) = value {
                map.insert("agent_id".to_owned(), Value::String(agent_id.to_owned()));
            }
            let schedule: CodexScheduleSummary =
                serde_json::from_value(value).context("invalid Codex schedule event")?;
            connection.execute(
                "INSERT INTO codex_schedules (schedule_id,agent_id,session_id,project_id,trigger_json,state,created_at_unix,updated_at_unix)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
                 ON CONFLICT(schedule_id) DO UPDATE SET state=excluded.state,updated_at_unix=excluded.updated_at_unix
                 WHERE excluded.updated_at_unix>=codex_schedules.updated_at_unix",
                params![schedule.schedule_id,agent_id,schedule.session_id,schedule.project_id,serde_json::to_string(&schedule.trigger)?,schedule_state_name(schedule.state),as_i64(schedule.created_at_unix)?,as_i64(schedule.updated_at_unix)?],
            )?;
        }
        "project.discovered" | "project.updated" => {
            if event.event_type == "project.updated" {
                notifications::audit(
                    connection,
                    "project.authorization",
                    required_string(&event.payload, "candidate_id")?,
                    "approved",
                    event.created_at_unix,
                )?;
            }
            let candidate_id = required_string(&event.payload, "candidate_id")?;
            let display_name = required_string(&event.payload, "display_name")?;
            let suggested_project_id = required_string(&event.payload, "suggested_project_id")?;
            ensure!(
                candidate_id.len() <= 128
                    && candidate_id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric()
                            || matches!(byte, b'-' | b'_' | b'.')),
                "invalid project candidate ID"
            );
            ensure!(
                display_name.len() <= 255 && !display_name.chars().any(char::is_control),
                "invalid project display name"
            );
            ensure!(
                suggested_project_id.len() <= 64
                    && suggested_project_id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric()
                            || matches!(byte, b'-' | b'_' | b'.')),
                "invalid suggested project ID"
            );
            let session_count = event
                .payload
                .get("session_count")
                .and_then(Value::as_u64)
                .context("project event missing session_count")?;
            let state = event
                .payload
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or("discovered");
            ensure!(
                matches!(state, "discovered" | "approved"),
                "invalid project state"
            );
            let updated_at_unix = event
                .payload
                .get("updated_at_unix")
                .and_then(Value::as_u64)
                .unwrap_or(event.created_at_unix);
            connection.execute(
                "INSERT INTO project_candidates (candidate_id,agent_id,display_name,suggested_project_id,session_count,state,updated_at_unix)
                 VALUES (?1,?2,?3,?4,?5,?6,?7)
                 ON CONFLICT(candidate_id) DO UPDATE SET display_name=excluded.display_name,suggested_project_id=excluded.suggested_project_id,session_count=excluded.session_count,state=excluded.state,updated_at_unix=excluded.updated_at_unix
                 WHERE project_candidates.agent_id=excluded.agent_id AND excluded.updated_at_unix>=project_candidates.updated_at_unix",
                params![candidate_id,agent_id,display_name,suggested_project_id,as_i64(session_count)?,state,as_i64(updated_at_unix)?],
            )?;
        }
        _ => {}
    }
    Ok(())
}

fn schedule_state_name(state: CodexScheduleState) -> &'static str {
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

fn parse_schedule_state(value: &str) -> rusqlite::Result<CodexScheduleState> {
    match value {
        "pending" => Ok(CodexScheduleState::Pending),
        "queued" => Ok(CodexScheduleState::Queued),
        "running" => Ok(CodexScheduleState::Running),
        "completed" => Ok(CodexScheduleState::Completed),
        "cancelled" => Ok(CodexScheduleState::Cancelled),
        "skipped" => Ok(CodexScheduleState::Skipped),
        "missed" => Ok(CodexScheduleState::Missed),
        "failed" => Ok(CodexScheduleState::Failed),
        "orphaned" => Ok(CodexScheduleState::Orphaned),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn required_string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("event missing {key}"))
}

fn parse_project_state(value: &str) -> rusqlite::Result<ProjectCandidateState> {
    match value {
        "discovered" => Ok(ProjectCandidateState::Discovered),
        "approved" => Ok(ProjectCandidateState::Approved),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
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
            let delivery = store.pending_notifications(now).unwrap().remove(0);
            assert_eq!(delivery.attempts, attempt - 1);
            store
                .finish_notification_delivery(&delivery, Some((false, "temporary", None)), now)
                .unwrap();
            now += (30_u64 * (1 << attempt.min(7))).min(3600);
        }
        assert!(store.pending_notifications(now).unwrap().is_empty());
    }

    #[test]
    fn browser_sessions_and_pairing_are_durable_and_one_time() {
        let store = EventStore::open(Path::new(":memory:")).unwrap();
        store
            .save_browser_session("token-hash", "admin", "csrf", 10, 100)
            .unwrap();
        assert_eq!(
            store
                .browser_session("token-hash", 11)
                .unwrap()
                .unwrap()
                .user,
            "admin"
        );
        store
            .create_pairing_code("pair-a", "agent-a", "code-hash", 10, 100)
            .unwrap();
        let pairing = store.pairing_by_hash("code-hash").unwrap().unwrap();
        assert_eq!(pairing.agent_id, "agent-a");
        assert!(
            store
                .consume_pairing_and_set_credential("pair-a", "agent-a", "agent-token-hash", 11)
                .unwrap()
        );
        assert!(
            !store
                .consume_pairing_and_set_credential("pair-a", "agent-a", "other-token-hash", 12)
                .unwrap()
        );
        assert_eq!(
            store
                .agent_for_token_hash("agent-token-hash")
                .unwrap()
                .as_deref(),
            Some("agent-a")
        );
        store
            .import_agent_credential("agent-a", "old-config-hash", 13)
            .unwrap();
        assert!(
            store
                .agent_for_token_hash("old-config-hash")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn project_candidates_materialize_without_a_path() {
        let store = EventStore::open(Path::new(":memory:")).unwrap();
        store.ingest("titan", &[AgentEvent {
            protocol: FARHELM_PROTOCOL.into(), sequence: 1, event_id: "project-1".into(), agent_id: "titan".into(),
            event_type: "project.discovered".into(), created_at_unix: 10,
            payload: serde_json::json!({"candidate_id":"candidate-a","display_name":"work831","suggested_project_id":"work831","session_count":14,"state":"discovered","updated_at_unix":10}),
        }]).unwrap();
        let projects = store.projects().unwrap();
        assert_eq!(projects.projects.len(), 1);
        assert_eq!(projects.projects[0].session_count, 14);
        assert!(!serde_json::to_string(&projects).unwrap().contains("/work/"));
    }
}
