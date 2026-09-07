//! Materialized notifications, script reports and device delivery receipts.
use super::*;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    pub cursor: Option<i64>,
    pub limit: Option<u32>,
    pub agent: Option<String>,
    pub category: Option<String>,
    pub state: Option<String>,
    pub unread: Option<bool>,
    pub id: Option<String>,
    pub source: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub id: i64,
    pub event_id: String,
    pub agent_id: String,
    pub category: String,
    pub state: String,
    pub target_id: String,
    pub title: String,
    pub message: String,
    pub created_at_unix: u64,
    pub read_at_unix: Option<u64>,
}
#[derive(Debug, Clone)]
pub struct Delivery {
    pub notification: Notification,
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
    pub attempts: u32,
}

pub(crate) fn migrate(connection: &Connection) -> Result<()> {
    connection.execute_batch("
      CREATE TABLE IF NOT EXISTS notification_preferences(id INTEGER PRIMARY KEY CHECK(id=1),experiments INTEGER NOT NULL DEFAULT 1,codex INTEGER NOT NULL DEFAULT 1);
      INSERT OR IGNORE INTO notification_preferences(id) VALUES(1);
      CREATE TABLE IF NOT EXISTS projection_revisions(entity_id TEXT PRIMARY KEY,revision INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS experiment_reports(report_id TEXT PRIMARY KEY,agent_id TEXT NOT NULL,payload_json TEXT NOT NULL,created_at_unix INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS notifications(id INTEGER PRIMARY KEY AUTOINCREMENT,event_id TEXT NOT NULL UNIQUE,business_key TEXT NOT NULL UNIQUE,agent_id TEXT NOT NULL,category TEXT NOT NULL,state TEXT NOT NULL,target_id TEXT NOT NULL,title TEXT NOT NULL,message TEXT NOT NULL,created_at_unix INTEGER NOT NULL,read_at_unix INTEGER);
      CREATE INDEX IF NOT EXISTS notification_filter ON notifications(agent_id,category,id);
      CREATE TABLE IF NOT EXISTS notification_deliveries(notification_id INTEGER NOT NULL,endpoint TEXT NOT NULL,attempts INTEGER NOT NULL DEFAULT 0,state TEXT NOT NULL DEFAULT 'pending',next_attempt_unix INTEGER NOT NULL,last_error TEXT,PRIMARY KEY(notification_id,endpoint));
      CREATE TABLE IF NOT EXISTS device_settings(endpoint TEXT PRIMARY KEY,device_id TEXT NOT NULL UNIQUE,name TEXT NOT NULL,experiments INTEGER NOT NULL DEFAULT 1,codex INTEGER NOT NULL DEFAULT 1);
      CREATE TABLE IF NOT EXISTS audit_entries(id INTEGER PRIMARY KEY AUTOINCREMENT,action TEXT NOT NULL,target TEXT NOT NULL,outcome TEXT NOT NULL,created_at_unix INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS browser_operations(operation_key TEXT PRIMARY KEY,fingerprint TEXT NOT NULL,response_json TEXT NOT NULL,created_at_unix INTEGER NOT NULL);
")?;
    Ok(())
}

pub(crate) fn migrate_history(connection: &Connection) -> Result<()> {
    let mut cursor = 0_i64;
    loop {
        let rows = {
            let mut stmt=connection.prepare("SELECT sequence,event_id,agent_id,agent_sequence,event_type,payload_json,created_at_unix FROM agent_events WHERE sequence>?1 ORDER BY sequence LIMIT 100")?;
            stmt.query_map([cursor], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    AgentEvent {
                        protocol: FARHELM_PROTOCOL.into(),
                        event_id: r.get(1)?,
                        agent_id: r.get(2)?,
                        sequence: row_u64(r, 3)?,
                        event_type: r.get(4)?,
                        payload: serde_json::from_str(&r.get::<_, String>(5)?).map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                5,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        })?,
                        created_at_unix: row_u64(r, 6)?,
                    },
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
        };
        if rows.is_empty() {
            break;
        }
        for (sequence, mut event) in rows {
            event.payload =
                farhelm_protocol::public_event_payload(&event.event_type, &event.payload);
            // Old versions could use an SDK preview as a session title.
            if event.event_type == "codex.session.updated" {
                event.payload["title"] = Value::Null;
            }
            connection.execute(
                "UPDATE agent_events SET payload_json=?2 WHERE sequence=?1",
                params![sequence, serde_json::to_string(&event.payload)?],
            )?;
            ingest(connection, &event.agent_id, &event)?;
            cursor = sequence;
        }
    }
    connection.execute_batch("UPDATE codex_sessions SET title=NULL;
        UPDATE notification_deliveries SET state='expired',last_error='historical import';
        UPDATE notification_deliveries SET (state,attempts,next_attempt_unix,last_error)=(SELECT d.state,d.attempts,d.next_attempt_unix,d.last_error FROM push_deliveries d JOIN agent_events e ON e.sequence=d.event_sequence JOIN notifications n ON n.event_id=e.event_id WHERE n.id=notification_deliveries.notification_id AND d.endpoint=notification_deliveries.endpoint) WHERE EXISTS(SELECT 1 FROM push_deliveries d JOIN agent_events e ON e.sequence=d.event_sequence JOIN notifications n ON n.event_id=e.event_id WHERE n.id=notification_deliveries.notification_id AND d.endpoint=notification_deliveries.endpoint);
        DELETE FROM push_deliveries;")?;
    Ok(())
}
fn string(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

pub(super) fn ingest(connection: &Connection, agent: &str, event: &AgentEvent) -> Result<bool> {
    if event.event_type == "experiment.reported" {
        let p = &event.payload;
        let report = required_string(p, "report_id")?;
        let state = required_string(p, "state")?;
        ensure!(
            matches!(state, "succeeded" | "failed" | "unknown"),
            "invalid report status"
        );
        ensure!(
            string(p, "name").chars().count() <= 128 && string(p, "message").len() <= 2048,
            "invalid report size"
        );
        connection.execute(
            "INSERT OR IGNORE INTO experiment_reports VALUES(?1,?2,?3,?4)",
            params![
                report,
                agent,
                serde_json::to_string(p)?,
                as_i64(event.created_at_unix)?
            ],
        )?;
    }
    let p = &event.payload;
    let data = p.get("data").unwrap_or(p);
    let (category, state, target, title, message, identity) = match event.event_type.as_str() {
        "experiment.updated" | "experiment.reported" => {
            let state = string(p, "state");
            if !matches!(state.as_str(), "succeeded" | "failed" | "unknown") {
                return Ok(false);
            }
            let target = if event.event_type == "experiment.reported" {
                string(p, "report_id")
            } else {
                string(p, "watch_id")
            };
            (
                "experiment",
                state,
                target.clone(),
                string(p, "name"),
                string(p, "message"),
                target,
            )
        }
        "codex.turn.completed" | "codex.turn.failed" | "codex.turn.orphaned" => {
            let session = if p.get("session_id").is_some() {
                string(p, "session_id")
            } else {
                string(data, "session_id")
            };
            if session.is_empty() {
                return Ok(false);
            }
            let state = match event.event_type.as_str() {
                "codex.turn.completed" => "succeeded",
                "codex.turn.failed" => "failed",
                _ => "unknown",
            };
            let identity = [
                string(p, "operation_id"),
                string(p, "command_id"),
                string(p, "watch_id"),
                string(data, "turn_id"),
                event.event_id.clone(),
            ]
            .into_iter()
            .find(|v| !v.is_empty())
            .unwrap();
            (
                "codex",
                state.to_owned(),
                session.clone(),
                format!("Codex {}", session.chars().take(12).collect::<String>()),
                String::new(),
                identity,
            )
        }
        _ => return Ok(false),
    };
    let key = format!("{agent}:{category}:{identity}");
    let changed=connection.execute("INSERT OR IGNORE INTO notifications(event_id,business_key,agent_id,category,state,target_id,title,message,created_at_unix) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![event.event_id,key,agent,category,state,target,title,message,as_i64(event.created_at_unix)?])?;
    if changed == 1 {
        let id = connection.last_insert_rowid();
        connection.execute("INSERT OR IGNORE INTO notification_deliveries(notification_id,endpoint,next_attempt_unix) SELECT ?1,s.endpoint,?2 FROM push_subscriptions s LEFT JOIN device_settings d ON d.endpoint=s.endpoint WHERE CASE WHEN ?3='codex' THEN COALESCE(d.codex,1) ELSE COALESCE(d.experiments,1) END=1",params![id,as_i64(event.created_at_unix)?,category])?;
        audit(
            connection,
            &event.event_type,
            &target,
            &state,
            event.created_at_unix,
        )?;
    }
    Ok(changed == 1)
}
fn notification_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Notification> {
    Ok(Notification {
        id: r.get(0)?,
        event_id: r.get(1)?,
        agent_id: r.get(2)?,
        category: r.get(3)?,
        state: r.get(4)?,
        target_id: r.get(5)?,
        title: r.get(6)?,
        message: r.get(7)?,
        created_at_unix: row_u64(r, 8)?,
        read_at_unix: r.get::<_, Option<i64>>(9)?.map(|v| v as u64),
    })
}
const COLUMNS: &str =
    "id,event_id,agent_id,category,state,target_id,title,message,created_at_unix,read_at_unix";
pub(super) fn audit(
    connection: &Connection,
    action: &str,
    target: &str,
    outcome: &str,
    now: u64,
) -> Result<()> {
    connection.execute(
        "INSERT INTO audit_entries(action,target,outcome,created_at_unix) VALUES(?1,?2,?3,?4)",
        params![action, target, outcome, as_i64(now)?],
    )?;
    Ok(())
}
impl EventStore {
    pub fn notification_write(
        &self,
        key: &str,
        action: &str,
        target: &str,
        body: &Value,
        now: u64,
    ) -> Result<Value> {
        let fingerprint =
            crate::secret_hash(&serde_json::to_string(&json!([action, target, body]))?);
        let connection = self.lock()?;
        let tx = crate::migrations::write_transaction(&connection)?;
        let previous: Option<(String, String)> = tx
            .query_row(
                "SELECT fingerprint,response_json FROM browser_operations WHERE operation_key=?1",
                [key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((saved, response)) = previous {
            ensure!(saved == fingerprint, "idempotency conflict");
            return Ok(serde_json::from_str(&response)?);
        }
        match action {
            "notification.preferences" => {
                let experiments = body
                    .get("experiments")
                    .and_then(Value::as_bool)
                    .context("experiments required")?;
                let codex = body
                    .get("codex")
                    .and_then(Value::as_bool)
                    .context("codex required")?;
                tx.execute(
                    "UPDATE notification_preferences SET experiments=?1,codex=?2 WHERE id=1",
                    params![experiments, codex],
                )?;
            }
            "notification.browser-test" => {
                let event = format!("browser-test:{key}");
                tx.execute("INSERT INTO notifications(event_id,business_key,agent_id,category,state,target_id,title,message,created_at_unix) VALUES(?1,?1,'Hub','test','succeeded','','页面测试通知','页面通知已连接',?2)",params![event,as_i64(now)?])?;
            }
            "notification.read" => {
                tx.execute(
                    "UPDATE notifications SET read_at_unix=COALESCE(read_at_unix,?2) WHERE id=?1",
                    params![target, as_i64(now)?],
                )?;
            }
            "notification.read-all" => {
                let through = body
                    .get("through_id")
                    .and_then(Value::as_i64)
                    .context("through_id required")?;
                ensure!(through >= 0, "invalid through_id");
                tx.execute("UPDATE notifications SET read_at_unix=?1 WHERE read_at_unix IS NULL AND id<=?2",params![as_i64(now)?,through])?;
            }
            "notification.settings" | "notification.revoke" | "notification.test" => {
                let endpoint = {
                    let mut stmt = tx.prepare("SELECT endpoint FROM push_subscriptions")?;
                    stmt.query_map([], |r| r.get::<_, String>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()?
                        .into_iter()
                        .find(|s| device_id(s) == target)
                };
                if action != "notification.revoke" {
                    ensure!(endpoint.is_some(), "device not found");
                }
                if let Some(endpoint) = endpoint {
                    match action {
                        "notification.settings" => {
                            let name = body
                                .get("name")
                                .and_then(Value::as_str)
                                .context("name required")?;
                            ensure!(
                                !name.trim().is_empty()
                                    && name.chars().count() <= 64
                                    && !name.chars().any(char::is_control),
                                "invalid device name"
                            );
                            let experiments = body
                                .get("experiments")
                                .and_then(Value::as_bool)
                                .context("experiments required")?;
                            let codex = body
                                .get("codex")
                                .and_then(Value::as_bool)
                                .context("codex required")?;
                            tx.execute("INSERT INTO device_settings VALUES(?1,?2,?3,?4,?5) ON CONFLICT(endpoint) DO UPDATE SET name=excluded.name,experiments=excluded.experiments,codex=excluded.codex",params![endpoint,target,name,experiments,codex])?;
                            tx.execute("UPDATE notification_deliveries SET state='expired',last_error='category disabled' WHERE endpoint=?1 AND state='pending' AND notification_id IN (SELECT id FROM notifications WHERE (category='experiment' AND ?2=0) OR (category='codex' AND ?3=0))",params![endpoint,experiments,codex])?;
                        }
                        "notification.revoke" => {
                            tx.execute("UPDATE notification_deliveries SET state='failed',last_error='subscription revoked' WHERE endpoint=?1 AND state='pending'",[&endpoint])?;
                            tx.execute(
                                "DELETE FROM push_deliveries WHERE endpoint=?1",
                                [&endpoint],
                            )?;
                            tx.execute(
                                "DELETE FROM push_subscriptions WHERE endpoint=?1",
                                [&endpoint],
                            )?;
                        }
                        _ => {
                            let event = format!("test:{target}:{key}");
                            tx.execute("INSERT INTO notifications(event_id,business_key,agent_id,category,state,target_id,title,message,created_at_unix) VALUES(?1,?1,'Hub','test','succeeded','','测试通知','',?2)",params![event,as_i64(now)?])?;
                            tx.execute("INSERT INTO notification_deliveries(notification_id,endpoint,next_attempt_unix) VALUES(?1,?2,?3)",params![tx.last_insert_rowid(),endpoint,as_i64(now)?])?;
                        }
                    }
                }
            }
            _ => anyhow::bail!("unsupported notification operation"),
        }
        let response = json!({"ok":true,"queued":action=="notification.test"});
        audit(&tx, action, target, "accepted", now)?;
        tx.execute(
            "INSERT INTO browser_operations VALUES(?1,?2,?3,?4)",
            params![
                key,
                fingerprint,
                serde_json::to_string(&response)?,
                as_i64(now)?
            ],
        )?;
        tx.commit()?;
        Ok(response)
    }
    pub fn notification_preferences(&self) -> Result<Value> {
        Ok(self.lock()?.query_row(
            "SELECT experiments,codex FROM notification_preferences WHERE id=1",
            [],
            |r| Ok(json!({"experiments":r.get::<_,bool>(0)?,"codex":r.get::<_,bool>(1)?})),
        )?)
    }
    pub fn overview_counts(&self) -> Result<Value> {
        let c = self.lock()?;
        let active: i64 = c.query_row(
            "SELECT count(*) FROM experiments WHERE state='watching'",
            [],
            |r| r.get(0),
        )?;
        let sessions: i64 = c.query_row(
            "SELECT count(*) FROM codex_sessions WHERE state IN ('running','queued','creating')",
            [],
            |r| r.get(0),
        )?;
        let unread: i64 = c.query_row(
            "SELECT count(*) FROM notifications WHERE read_at_unix IS NULL",
            [],
            |r| r.get(0),
        )?;
        let failures: i64 = c.query_row(
            "SELECT count(*) FROM notifications WHERE state IN ('failed','unknown')",
            [],
            |r| r.get(0),
        )?;
        Ok(
            serde_json::json!({"protocol":FARHELM_PROTOCOL,"active_experiments":active,"active_sessions":sessions,"unread_notifications":unread,"failed_tasks":failures}),
        )
    }

    pub fn notifications(&self, q: &ListQuery) -> Result<Value> {
        let connection = self.lock()?;
        let limit = q.limit.unwrap_or(30).clamp(1, 100);
        let mut stmt=connection.prepare(&format!("SELECT {COLUMNS} FROM notifications WHERE (?1 IS NULL OR id<?1) AND (?2 IS NULL OR agent_id=?2) AND (?3 IS NULL OR category=?3) AND (?4 IS NULL OR state=?4) AND (?5=0 OR read_at_unix IS NULL) ORDER BY id DESC LIMIT ?6"))?;
        let mut rows = stmt
            .query_map(
                params![
                    q.cursor,
                    q.agent,
                    q.category,
                    q.state,
                    q.unread.unwrap_or(false),
                    limit + 1
                ],
                notification_row,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let more = rows.len() > limit as usize;
        rows.truncate(limit as usize);
        let next = if more {
            rows.last().map(|n| n.id)
        } else {
            None
        };
        let unread: i64 = connection.query_row(
            "SELECT count(*) FROM notifications WHERE read_at_unix IS NULL",
            [],
            |r| r.get(0),
        )?;
        let latest: i64 =
            connection.query_row("SELECT COALESCE(MAX(id),0) FROM notifications", [], |r| {
                r.get(0)
            })?;
        let preferences = connection.query_row(
            "SELECT experiments,codex FROM notification_preferences WHERE id=1",
            [],
            |r| Ok(json!({"experiments":r.get::<_,bool>(0)?,"codex":r.get::<_,bool>(1)?})),
        )?;
        Ok(
            json!({"protocol":FARHELM_PROTOCOL,"notifications":rows,"next_cursor":next,"unread_count":unread,"latest_id":latest,"preferences":preferences}),
        )
    }
    /// Newly committed business events carry their existing projection to live browsers.
    pub fn notifications_for_events(&self, ids: &[String]) -> Result<Vec<Notification>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(&format!(
            "SELECT {COLUMNS} FROM notifications WHERE event_id=?1"
        ))?;
        let mut rows = Vec::new();
        for id in ids {
            if let Some(row) = statement.query_row([id], notification_row).optional()? {
                rows.push(row);
            }
        }
        Ok(rows)
    }
    pub fn notification(&self, id: i64) -> Result<Option<Notification>> {
        self.lock()?
            .query_row(
                &format!("SELECT {COLUMNS} FROM notifications WHERE id=?1"),
                [id],
                notification_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn record_audit(&self, action: &str, target: &str, outcome: &str, now: u64) -> Result<()> {
        audit(&*self.lock()?, action, target, outcome, now)
    }
    pub fn audits(&self, q: &ListQuery) -> Result<Value> {
        let connection = self.lock()?;
        let limit = q.limit.unwrap_or(30).clamp(1, 100);
        let mut stmt=connection.prepare("SELECT id,action,target,outcome,created_at_unix FROM audit_entries WHERE (?1 IS NULL OR id<?1) ORDER BY id DESC LIMIT ?2")?;
        let mut rows=stmt.query_map(params![q.cursor,limit+1],|r|Ok(serde_json::json!({"id":r.get::<_,i64>(0)?,"action":r.get::<_,String>(1)?,"target":r.get::<_,String>(2)?,"outcome":r.get::<_,String>(3)?,"created_at_unix":row_u64(r,4)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let more = rows.len() > limit as usize;
        rows.truncate(limit as usize);
        let next = if more {
            rows.last().and_then(|r| r.get("id")).cloned()
        } else {
            None
        };
        Ok(serde_json::json!({"protocol":FARHELM_PROTOCOL,"entries":rows,"next_cursor":next}))
    }
    pub fn experiment_runs(&self, q: &ListQuery) -> Result<Value> {
        let connection = self.lock()?;
        let limit = q.limit.unwrap_or(30).clamp(1, 100);
        // The event sequence gives both source types one stable pagination order.
        let mut stmt=connection.prepare("SELECT MAX(sequence),event_type,payload_json,agent_id FROM agent_events WHERE event_type IN ('experiment.updated','experiment.reported') GROUP BY agent_id,COALESCE(json_extract(payload_json,'$.report_id'),json_extract(payload_json,'$.watch_id')) HAVING (?1 IS NULL OR MAX(sequence)<?1) AND (?2 IS NULL OR agent_id=?2) AND (?3 IS NULL OR json_extract(payload_json,'$.state')=?3) AND (?5 IS NULL OR COALESCE(json_extract(payload_json,'$.report_id'),json_extract(payload_json,'$.watch_id'))=?5) AND (?6 IS NULL OR event_type=CASE ?6 WHEN 'script_report' THEN 'experiment.reported' ELSE 'experiment.updated' END) ORDER BY MAX(sequence) DESC LIMIT ?4")?;
        let mut rows = stmt
            .query_map(
                params![q.cursor, q.agent, q.state, limit + 1, q.id, q.source],
                |r| {
                    let encoded: String = r.get(2)?;
                    let mut p: Value =
                        serde_json::from_str(&encoded).map_err(json_conversion(2))?;
                    p["sequence"] = r.get::<_, i64>(0)?.into();
                    p["agent_id"] = r.get::<_, String>(3)?.into();
                    p["source"] = if r.get::<_, String>(1)? == "experiment.reported" {
                        "script_report"
                    } else {
                        "pid_watch"
                    }
                    .into();
                    p["id"] = p
                        .get("report_id")
                        .or_else(|| p.get("watch_id"))
                        .cloned()
                        .unwrap_or(Value::Null);
                    Ok(p)
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let more = rows.len() > limit as usize;
        rows.truncate(limit as usize);
        let next = if more {
            rows.last().and_then(|r| r.get("sequence")).cloned()
        } else {
            None
        };
        Ok(serde_json::json!({"protocol":FARHELM_PROTOCOL,"experiments":rows,"next_cursor":next}))
    }
    pub fn devices(&self) -> Result<Value> {
        let connection = self.lock()?;
        let mut stmt=connection.prepare("SELECT s.endpoint,COALESCE(d.device_id,''),COALESCE(d.name,'Browser'),COALESCE(d.experiments,1),COALESCE(d.codex,1) FROM push_subscriptions s LEFT JOIN device_settings d ON d.endpoint=s.endpoint")?;
        let rows=stmt.query_map([],|r|{let endpoint:String=r.get(0)?;let id:String=r.get(1)?;Ok(serde_json::json!({"id":if id.is_empty(){device_id(&endpoint)}else{id},"name":r.get::<_,String>(2)?,"experiments":r.get::<_,bool>(3)?,"codex":r.get::<_,bool>(4)?}))})?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(serde_json::json!({"protocol":FARHELM_PROTOCOL,"devices":rows}))
    }

    pub fn notification_deliveries(&self, id: i64) -> Result<Value> {
        let connection = self.lock()?;
        let mut stmt=connection.prepare("SELECT endpoint,state,attempts,last_error FROM notification_deliveries WHERE notification_id=?1")?;
        let rows=stmt.query_map([id],|r|Ok(serde_json::json!({"device_id":device_id(&r.get::<_,String>(0)?),"state":r.get::<_,String>(1)?,"attempts":r.get::<_,u32>(2)?,"last_error":r.get::<_,Option<String>>(3)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(serde_json::json!(rows))
    }
    pub fn pending_notifications(&self, now: u64) -> Result<Vec<Delivery>> {
        let connection = self.lock()?;
        connection.execute("UPDATE notification_deliveries SET state='expired' WHERE state='pending' AND notification_id IN (SELECT id FROM notifications WHERE created_at_unix+86400<=?1)",[as_i64(now)?])?;
        let mut stmt=connection.prepare("SELECT n.id,n.event_id,n.agent_id,n.category,n.state,n.target_id,n.title,n.message,n.created_at_unix,n.read_at_unix,d.endpoint,s.p256dh,s.auth,d.attempts FROM notification_deliveries d JOIN notifications n ON n.id=d.notification_id JOIN push_subscriptions s ON s.endpoint=d.endpoint WHERE d.state='pending' AND d.next_attempt_unix<=?1 ORDER BY n.id LIMIT 32")?;
        Ok(stmt
            .query_map([as_i64(now)?], |r| {
                Ok(Delivery {
                    notification: notification_row(r)?,
                    endpoint: r.get(10)?,
                    p256dh: r.get(11)?,
                    auth: r.get(12)?,
                    attempts: r.get(13)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn finish_notification_delivery(
        &self,
        d: &Delivery,
        error: Option<(bool, &str, Option<u64>)>,
        now: u64,
    ) -> Result<()> {
        let connection = self.lock()?;
        let tx = crate::migrations::write_transaction(&connection)?;
        match error {
            None => {
                tx.execute("UPDATE notification_deliveries SET state='accepted',attempts=attempts+1,last_error=NULL WHERE notification_id=?1 AND endpoint=?2",params![d.notification.id,d.endpoint])?;
            }
            Some((permanent, detail, retry)) => {
                let attempts = d.attempts + 1;
                let delay = (30_u64.saturating_mul(1 << attempts.min(7)))
                    .min(3600)
                    .max(retry.unwrap_or(0));
                tx.execute("UPDATE notification_deliveries SET state=?3,attempts=?4,last_error=?5,next_attempt_unix=?6 WHERE notification_id=?1 AND endpoint=?2",params![d.notification.id,d.endpoint,if permanent||attempts>=8{"failed"}else{"pending"},attempts,detail,as_i64(now.saturating_add(delay))?])?;
                if permanent {
                    tx.execute("UPDATE notification_deliveries SET state='failed',last_error='subscription revoked' WHERE endpoint=?1 AND state='pending'",[&d.endpoint])?;
                    tx.execute(
                        "DELETE FROM push_deliveries WHERE endpoint=?1",
                        [&d.endpoint],
                    )?;
                    tx.execute(
                        "DELETE FROM push_subscriptions WHERE endpoint=?1",
                        [&d.endpoint],
                    )?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }
}
fn device_id(endpoint: &str) -> String {
    crate::secret_hash(endpoint)
}

#[cfg(test)]
#[path = "notification_tests.rs"]
mod tests;
