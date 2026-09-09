//! Local execution identity, scheduling and script reports share one transaction boundary.
use super::*;

pub(crate) fn migrate(connection: &Connection) -> Result<()> {
    connection.execute_batch("CREATE TABLE IF NOT EXISTS execution_results(job_id TEXT PRIMARY KEY,event_type TEXT NOT NULL,payload_json TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS execution_queue (
            sequence INTEGER PRIMARY KEY AUTOINCREMENT, job_id TEXT NOT NULL UNIQUE,
            session_id TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'queued'
            CHECK(state IN ('queued','running','completed','orphaned')));
        CREATE UNIQUE INDEX IF NOT EXISTS one_running_session ON execution_queue(session_id) WHERE state='running';
        CREATE TABLE IF NOT EXISTS script_reports (
            identity TEXT PRIMARY KEY, fingerprint TEXT NOT NULL, payload_json TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS schedule_revisions(schedule_id TEXT PRIMARY KEY,revision INTEGER NOT NULL);
        INSERT OR IGNORE INTO execution_queue(job_id,session_id,state) SELECT command_id,json_extract(payload_json,'$.session_id'),CASE WHEN state='running' THEN 'orphaned' ELSE 'queued' END FROM remote_codex_commands WHERE action='codex.turn.start' AND state IN ('accepted','running') ORDER BY updated_at_unix,command_id;
        INSERT OR IGNORE INTO execution_queue(job_id,session_id,state) SELECT schedule_id,session_id,CASE WHEN state='running' THEN 'orphaned' ELSE 'queued' END FROM codex_prompt_schedules WHERE state IN ('queued','running') ORDER BY created_at_unix,schedule_id;")?;
    Ok(())
}

pub(super) fn enqueue(connection: &Connection, job: &str, session: &str) -> Result<()> {
    lifecycle::writable(connection, session)?;
    connection.execute(
        "INSERT OR IGNORE INTO execution_queue(job_id,session_id) VALUES(?1,?2)",
        params![job, session],
    )?;
    Ok(())
}

pub(super) fn claim(connection: &Connection, job: &str) -> Result<bool> {
    Ok(connection.execute("UPDATE execution_queue SET state='running' WHERE job_id=?1 AND state='queued'
        AND NOT EXISTS(SELECT 1 FROM execution_queue other WHERE other.session_id=execution_queue.session_id
          AND (other.state='running' OR (other.state='queued' AND other.sequence<execution_queue.sequence)))",[job])?==1)
}

pub(super) fn finish(connection: &Connection, job: &str) -> Result<()> {
    connection.execute("UPDATE execution_queue SET state='completed' WHERE job_id=?1 AND state IN ('queued','running')",[job])?;
    Ok(())
}

fn schedule_event(connection: &Connection, id: &str, now: u64) -> Result<()> {
    connection.execute("INSERT INTO schedule_revisions VALUES(?1,1) ON CONFLICT(schedule_id) DO UPDATE SET revision=revision+1",[id])?;
    let revision: i64 = connection.query_row(
        "SELECT revision FROM schedule_revisions WHERE schedule_id=?1",
        [id],
        |r| r.get(0),
    )?;
    let payload:Value=connection.query_row("SELECT project_id,session_id,trigger_json,state,created_at_unix FROM codex_prompt_schedules WHERE schedule_id=?1",[id],|r|{
        let trigger:String=r.get(2)?;
        Ok(json!({"schedule_id":id,"project_id":r.get::<_,String>(0)?,"session_id":r.get::<_,String>(1)?,"trigger":serde_json::from_str::<Value>(&trigger).map_err(json_conversion(2))?,"state":r.get::<_,String>(3)?,"created_at_unix":row_u64(r,4)?,"updated_at_unix":now,"revision":revision}))
    })?;
    insert_event(
        connection,
        &format!("schedule:{id}:{revision}"),
        "codex.schedule.updated",
        &payload,
        now,
    )
}

fn insert_schedule(connection: &Connection, payload: &Value, now: u64) -> Result<String> {
    let id = required_payload_string(payload, "schedule_id")?;
    let project = required_payload_string(payload, "project_id")?;
    let session = required_payload_string(payload, "session_id")?;
    lifecycle::writable(connection, session)?;
    let prompt = required_payload_string(payload, "prompt")?;
    ensure!(
        prompt.len() <= PROMPT_LIMIT,
        "scheduled prompt exceeds 32 KiB"
    );
    let trigger: CodexScheduleTrigger =
        serde_json::from_value(payload.get("trigger").cloned().context("missing trigger")?)?;
    let grace = match &trigger {
        CodexScheduleTrigger::AtTime { run_at_unix } => Some(run_at_unix.saturating_add(86400)),
        CodexScheduleTrigger::ExperimentSucceeded { watch_id } => {
            let valid:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM experiment_watches WHERE watch_id=?1 AND project_id=?2 AND state='watching')",params![watch_id,project],|r|r.get(0))?;
            ensure!(valid, "scheduled experiment is not watching");
            None
        }
    };
    connection.execute("INSERT INTO codex_prompt_schedules(schedule_id,project_id,session_id,trigger_json,prompt,state,grace_expires_at_unix,created_at_unix,updated_at_unix) VALUES(?1,?2,?3,?4,?5,'pending',?6,?7,?7)",params![id,project,session,serde_json::to_string(&trigger)?,prompt,grace.map(as_i64).transpose()?,as_i64(now)?])?;
    schedule_event(connection, id, now)?;
    Ok(id.to_owned())
}

#[derive(Debug)]
pub struct ScriptReport {
    pub agent_id: String,
    pub project_id: String,
    pub run_id: Option<String>,
    pub name: String,
    pub status: String,
    pub message: String,
    pub session_id: Option<String>,
    pub prompt: Option<String>,
}

impl ExperimentStore {
    pub fn recorded_turn(&self, job: &str) -> Result<Option<(String, Value)>> {
        Ok(self
            .lock()?
            .query_row(
                "SELECT event_type,payload_json FROM execution_results WHERE job_id=?1",
                [job],
                |r| {
                    Ok((
                        r.get(0)?,
                        serde_json::from_str(&r.get::<_, String>(1)?)
                            .map_err(json_conversion(1))?,
                    ))
                },
            )
            .optional()?)
    }

    /// A terminal Worker receipt survives a lost response or a crash before origin finalization.
    pub fn recover_recorded_turns(&self, now: u64) -> Result<()> {
        let pending = {
            let connection = self.lock()?;
            let mut stmt = connection.prepare("SELECT r.job_id,r.event_type,r.payload_json FROM execution_results r JOIN execution_queue q ON q.job_id=r.job_id WHERE q.state='running'")?;
            stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
        };
        for (job, event, encoded) in pending {
            let payload: Value = serde_json::from_str(&encoded)?;
            let kind = {
                let c = self.lock()?;
                c.query_row("SELECT CASE WHEN EXISTS(SELECT 1 FROM remote_codex_commands WHERE command_id=?1 AND state='running') THEN 'remote' WHEN EXISTS(SELECT 1 FROM codex_prompt_schedules WHERE schedule_id=?1 AND state='running') THEN 'schedule' ELSE 'watch' END",[&job],|r|r.get::<_,String>(0))?
            };
            let succeeded = event == "codex.turn.completed";
            match kind.as_str() {
                "remote" => self.finish_remote_command(
                    &job,
                    if succeeded {
                        farhelm_protocol::CommandState::Completed
                    } else {
                        farhelm_protocol::CommandState::Failed
                    },
                    Some(payload.get("data").unwrap_or(&payload)),
                    None,
                    now,
                )?,
                "schedule" => self.finish_schedule(
                    &job,
                    if succeeded {
                        CodexScheduleState::Completed
                    } else if event == "codex.turn.orphaned" {
                        CodexScheduleState::Orphaned
                    } else {
                        CodexScheduleState::Failed
                    },
                    now,
                )?,
                _ => self.finish_auto_prompt(&job, &event, &payload, now)?,
            }
        }
        Ok(())
    }

    pub fn report_experiment(&self, report: &ScriptReport, now: u64) -> Result<Value> {
        ensure!(
            matches!(report.status.as_str(), "succeeded" | "failed" | "unknown"),
            "invalid experiment status"
        );
        ensure!(
            !report.name.trim().is_empty()
                && report.name.chars().count() <= 128
                && !report.name.chars().any(char::is_control),
            "invalid experiment name"
        );
        ensure!(report.message.len() <= 2048, "message exceeds 2 KiB");
        ensure!(
            report.session_id.is_some() == report.prompt.is_some(),
            "session and success prompt must be supplied together"
        );
        if let Some(prompt) = &report.prompt {
            ensure!(
                !prompt.trim().is_empty() && prompt.len() <= PROMPT_LIMIT,
                "invalid success prompt"
            );
        }
        let generated = format!("run_{:032x}", rand::random::<u128>());
        let run = report.run_id.as_deref().unwrap_or(&generated);
        ensure!(
            !run.is_empty()
                && run.len() <= 128
                && run
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)),
            "invalid run ID"
        );
        let identity = Sha256::digest(serde_json::to_vec(&json!([
            report.agent_id,
            report.project_id,
            run
        ]))?)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
        let fingerprint = Sha256::digest(serde_json::to_vec(&json!([
            report.name,
            report.status,
            report.message,
            report.session_id,
            report.prompt
        ]))?)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
        let connection = self.lock()?;
        let tx = crate::migrations::write_transaction(&connection)?;
        let approved: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM approved_projects WHERE project_id=?1)",
            [&report.project_id],
            |r| r.get(0),
        )?;
        ensure!(approved, "project is not approved");
        if let Some(session) = &report.session_id {
            let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM codex_session_bindings WHERE session_id=?1 AND project_id=?2)",params![session,report.project_id],|r|r.get(0))?;
            ensure!(valid, "session is not bound to this project");
        }
        if let Some((old, encoded)) = tx
            .query_row(
                "SELECT fingerprint,payload_json FROM script_reports WHERE identity=?1",
                [&identity],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?
        {
            ensure!(
                old == fingerprint,
                "run ID conflicts with a different report"
            );
            return Ok(
                json!({"run_id":run,"event_id":format!("report:{identity}"),"stored_locally":true,"report":serde_json::from_str::<Value>(&encoded)?}),
            );
        }
        let followup = if report.status == "succeeded" {
            report
                .session_id
                .as_ref()
                .map(|_| format!("report_{identity}"))
        } else {
            None
        };
        let payload = json!({"run_id":run,"report_id":identity,"project_id":report.project_id,"name":report.name,"state":report.status,"message":report.message,"session_id":report.session_id,"followup_schedule_id":followup,"source":"script_report","updated_at_unix":now});
        tx.execute(
            "INSERT INTO script_reports VALUES(?1,?2,?3)",
            params![identity, fingerprint, serde_json::to_string(&payload)?],
        )?;
        insert_event(
            &tx,
            &format!("report:{identity}"),
            "experiment.reported",
            &payload,
            now,
        )?;
        if let Some(id) = followup {
            insert_schedule(
                &tx,
                &json!({"schedule_id":id,"project_id":report.project_id,"session_id":report.session_id,"prompt":report.prompt,"trigger":{"type":"at_time","run_at_unix":now}}),
                now,
            )?;
        }
        tx.commit()?;
        Ok(
            json!({"run_id":run,"event_id":format!("report:{identity}"),"stored_locally":true,"report":payload}),
        )
    }

    pub fn create_schedule(&self, payload: &Value, now: u64) -> Result<String> {
        let connection = self.lock()?;
        let tx = crate::migrations::write_transaction(&connection)?;
        let id = insert_schedule(&tx, payload, now)?;
        tx.commit()?;
        Ok(id)
    }
    pub fn cancel_schedule(&self, id: &str, now: u64) -> Result<()> {
        let connection = self.lock()?;
        let tx = crate::migrations::write_transaction(&connection)?;
        ensure!(tx.execute("UPDATE codex_prompt_schedules SET state='cancelled',prompt='',updated_at_unix=?2 WHERE schedule_id=?1 AND state IN ('pending','queued')",params![id,as_i64(now)?])?==1,"schedule is not cancellable");
        finish(&tx, id)?;
        schedule_event(&tx, id, now)?;
        tx.commit()?;
        Ok(())
    }
    pub fn schedule_detail(&self, id: &str) -> Result<Value> {
        self.lock()?.query_row("SELECT project_id,session_id,trigger_json,prompt,state,created_at_unix,updated_at_unix FROM codex_prompt_schedules WHERE schedule_id=?1",[id],|r|{
            let trigger:String=r.get(2)?;
            Ok(json!({"summary":{"schedule_id":id,"project_id":r.get::<_,String>(0)?,"session_id":r.get::<_,String>(1)?,"trigger":serde_json::from_str::<Value>(&trigger).map_err(json_conversion(2))?,"state":r.get::<_,String>(4)?,"created_at_unix":row_u64(r,5)?,"updated_at_unix":row_u64(r,6)?},"prompt":r.get::<_,String>(3)?}))
        }).optional()?.context("schedule not found")
    }
    pub fn due_schedules(&self, now: u64) -> Result<Vec<ScheduledPrompt>> {
        let connection = self.lock()?;
        let tx = crate::migrations::write_transaction(&connection)?;
        let rows = {
            let mut stmt=tx.prepare("SELECT schedule_id,project_id,session_id,trigger_json,prompt,grace_expires_at_unix,state FROM codex_prompt_schedules WHERE state IN ('pending','queued') ORDER BY created_at_unix,rowid")?;
            stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, Option<i64>>(5)?.map(|v| v as u64),
                    r.get::<_, String>(6)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut due = Vec::new();
        for (id, project, session, encoded, prompt, mut grace, state) in rows {
            let trigger: CodexScheduleTrigger = serde_json::from_str(&encoded)?;
            let next = match trigger {
                CodexScheduleTrigger::AtTime { run_at_unix } if now >= run_at_unix => {
                    Some("queued")
                }
                CodexScheduleTrigger::ExperimentSucceeded { watch_id } => {
                    let watch:Option<(String,u64)>=tx.query_row("SELECT state,updated_at_unix FROM experiment_watches WHERE watch_id=?1",[watch_id],|r|Ok((r.get(0)?,row_u64(r,1)?))).optional()?;
                    match watch {
                        Some((s, t)) if s == "succeeded" => {
                            grace = Some(t.saturating_add(86400));
                            Some("queued")
                        }
                        Some((s, _)) if s == "watching" => None,
                        _ => Some("skipped"),
                    }
                }
                _ => None,
            };
            let Some(mut next) = next else { continue };
            if next == "queued" && grace.is_some_and(|v| now > v) {
                next = "missed";
            }
            if state != next {
                tx.execute("UPDATE codex_prompt_schedules SET state=?2,updated_at_unix=?3,grace_expires_at_unix=?4,prompt=CASE WHEN ?2='queued' THEN prompt ELSE '' END WHERE schedule_id=?1",params![id,next,as_i64(now)?,grace.map(as_i64).transpose()?])?;
                schedule_event(&tx, &id, now)?;
            }
            if next == "queued" {
                enqueue(&tx, &id, &session)?;
                due.push(ScheduledPrompt {
                    schedule_id: id,
                    project_id: project,
                    session_id: session,
                    prompt,
                });
            } else {
                finish(&tx, &id)?;
            }
        }
        tx.commit()?;
        Ok(due)
    }
    pub fn claim_schedule(&self, id: &str, now: u64) -> Result<bool> {
        let connection = self.lock()?;
        let tx = crate::migrations::write_transaction(&connection)?;
        if tx.execute("UPDATE codex_prompt_schedules SET state='missed',prompt='',updated_at_unix=?2 WHERE schedule_id=?1 AND state='queued' AND grace_expires_at_unix<?2",params![id,as_i64(now)?])?==1 {
            finish(&tx,id)?;
            schedule_event(&tx,id,now)?;
            tx.commit()?;
            return Ok(false);
        }
        if !claim(&tx, id)? {
            return Ok(false);
        }
        ensure!(tx.execute("UPDATE codex_prompt_schedules SET state='running',updated_at_unix=?2 WHERE schedule_id=?1 AND state='queued'",params![id,as_i64(now)?])?==1,"schedule is not queued");
        schedule_event(&tx, id, now)?;
        tx.commit()?;
        Ok(true)
    }
    pub fn finish_schedule(&self, id: &str, state: CodexScheduleState, now: u64) -> Result<()> {
        ensure!(
            matches!(
                state,
                CodexScheduleState::Completed
                    | CodexScheduleState::Failed
                    | CodexScheduleState::Orphaned
            ),
            "invalid terminal state"
        );
        let connection = self.lock()?;
        let tx = crate::migrations::write_transaction(&connection)?;
        ensure!(tx.execute("UPDATE codex_prompt_schedules SET state=?2,prompt='',updated_at_unix=?3 WHERE schedule_id=?1 AND state='running'",params![id,schedule_state_name(state),as_i64(now)?])?==1,"schedule is not running");
        finish(&tx, id)?;
        schedule_event(&tx, id, now)?;
        {
            let (session, project): (String, String) = tx.query_row(
                "SELECT session_id,project_id FROM codex_prompt_schedules WHERE schedule_id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let event = if state == CodexScheduleState::Completed {
                "codex.turn.completed"
            } else if state == CodexScheduleState::Orphaned {
                "codex.turn.orphaned"
            } else {
                "codex.turn.failed"
            };
            insert_event(
                &tx,
                &format!("{id}:terminal"),
                event,
                &json!({"operation_id":id,"session_id":session,"project_id":project}),
                now,
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn orphan_running_schedules(&self, now: u64) -> Result<()> {
        let ids = {
            let connection = self.lock()?;
            let mut stmt = connection
                .prepare("SELECT schedule_id FROM codex_prompt_schedules WHERE state='running'")?;
            stmt.query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        for id in ids {
            self.finish_schedule(&id, CodexScheduleState::Orphaned, now)?;
        }
        self.lock()?.execute(
            "UPDATE execution_queue SET state='orphaned' WHERE state='running'",
            [],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(path: &Path, root: &Path) -> ExperimentStore {
        let store = ExperimentStore::open(path).unwrap();
        store
            .import_config_projects(
                &BTreeMap::from([(
                    "p".into(),
                    crate::config::ProjectSection {
                        path: root.into(),
                        success_patterns: vec![],
                        failure_patterns: vec![],
                    },
                )]),
                100,
            )
            .unwrap();
        store.bind_session("s", "p", root, "inspect", 100).unwrap();
        store
    }
    fn report(status: &str) -> ScriptReport {
        ScriptReport {
            agent_id: "agent".into(),
            project_id: "p".into(),
            run_id: Some("batch-8".into()),
            name: "8 轮训练".into(),
            status: status.into(),
            message: "全部完成".into(),
            session_id: Some("s".into()),
            prompt: Some("LOCAL_PROMPT_SENTINEL".into()),
        }
    }

    #[test]
    fn report_receipt_outbox_and_followup_survive_offline_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.db");
        let store = setup(&path, dir.path());
        let receipt = store.report_experiment(&report("succeeded"), 100).unwrap();
        assert_eq!(
            receipt,
            store.report_experiment(&report("succeeded"), 101).unwrap()
        );
        assert!(store.report_experiment(&report("failed"), 101).is_err());
        let events = store.pending_events("agent", 100).unwrap();
        assert!(
            !serde_json::to_string(&events)
                .unwrap()
                .contains("LOCAL_PROMPT_SENTINEL")
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| e.event_type == "experiment.reported")
                .count(),
            1
        );
        drop(store);
        let store = ExperimentStore::open(&path).unwrap();
        assert_eq!(
            receipt,
            store.report_experiment(&report("succeeded"), 102).unwrap()
        );
        let due = store.due_schedules(102).unwrap();
        assert_eq!(due.len(), 1);
        assert!(store.claim_schedule(&due[0].schedule_id, 102).unwrap());
        assert!(!store.claim_schedule(&due[0].schedule_id, 102).unwrap());
        store.orphan_running_schedules(103).unwrap();
        assert!(store.due_schedules(104).unwrap().is_empty());
        let before = store.pending_events("agent", 100).unwrap();
        store
            .acknowledge_events(
                &before
                    .iter()
                    .map(|e| e.event_id.clone())
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        assert!(store.pending_events("agent", 100).unwrap().is_empty());
    }

    #[test]
    fn failed_unknown_and_expired_reports_never_start_followups() {
        for status in ["failed", "unknown", "succeeded"] {
            let dir = tempfile::tempdir().unwrap();
            let store = setup(&dir.path().join("a.db"), dir.path());
            store.report_experiment(&report(status), 100).unwrap();
            assert!(store.due_schedules(100 + 86401).unwrap().is_empty());
        }
    }

    #[test]
    fn same_second_revisions_queue_fairness_and_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        let store = setup(&dir.path().join("a.db"), dir.path());
        for id in ["a", "b"] {
            store.create_schedule(&json!({"schedule_id":id,"project_id":"p","session_id":"s","prompt":"check","trigger":{"type":"at_time","run_at_unix":100}}),100).unwrap();
        }
        assert_eq!(store.due_schedules(100).unwrap().len(), 2);
        assert!(!store.claim_schedule("b", 100).unwrap());
        store.cancel_schedule("a", 100).unwrap();
        assert!(!store.claim_schedule("a", 100).unwrap());
        assert!(store.claim_schedule("b", 100).unwrap());
        assert!(store.cancel_schedule("b", 100).is_err());
        store
            .finish_schedule("b", CodexScheduleState::Completed, 100)
            .unwrap();
        let events = store.pending_events("agent", 100).unwrap();
        let ids = events.iter().map(|e| &e.event_id).collect::<HashSet<_>>();
        assert_eq!(ids.len(), events.len());
        assert!(events.len() >= 7);
    }

    #[test]
    fn completed_receipt_wins_over_restart_orphaning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.db");
        let store = setup(&path, dir.path());
        store.create_schedule(&json!({"schedule_id":"job","project_id":"p","session_id":"s","prompt":"check","trigger":{"type":"at_time","run_at_unix":100}}),100).unwrap();
        store.due_schedules(100).unwrap();
        store.claim_schedule("job", 100).unwrap();
        store.enqueue_event("worker-terminal","codex.turn.completed",&json!({"operation_id":"job","session_id":"s","data":{"turn_id":"t","status":"completed"}}),100).unwrap();
        drop(store);
        let store = ExperimentStore::open(&path).unwrap();
        store.recover_recorded_turns(101).unwrap();
        store.orphan_running_schedules(101).unwrap();
        assert!(store.due_schedules(101).unwrap().is_empty());
        let events = store.pending_events("agent", 100).unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|e| e.event_type == "codex.turn.completed")
                .count(),
            1
        );
        assert!(!events.iter().any(|e| e.event_type == "codex.turn.orphaned"));
        assert_eq!(
            store.schedule_detail("job").unwrap()["summary"]["state"],
            "completed"
        );
    }

    #[test]
    fn independent_connections_serialize_claim_cancel_and_report_retries() {
        use std::sync::{Arc, Barrier};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.db");
        let store = setup(&path, dir.path());
        for round in 0..20 {
            let id = format!("race-{round}");
            store.create_schedule(&json!({"schedule_id":id,"project_id":"p","session_id":"s","prompt":"check","trigger":{"type":"at_time","run_at_unix":100}}),100).unwrap();
            store.due_schedules(100).unwrap();
            let other = ExperimentStore::open(&path).unwrap();
            let barrier = Arc::new(Barrier::new(2));
            let ready = barrier.clone();
            let candidate = id.clone();
            let claimant = std::thread::spawn(move || {
                ready.wait();
                other.claim_schedule(&candidate, 100).unwrap()
            });
            barrier.wait();
            let cancelled = store.cancel_schedule(&id, 100).is_ok();
            let claimed = claimant.join().unwrap();
            assert_ne!(cancelled, claimed, "exactly one outcome must commit");
            if claimed {
                store
                    .finish_schedule(&id, CodexScheduleState::Completed, 100)
                    .unwrap();
            }
        }
        let mut handles = Vec::new();
        let barrier = Arc::new(Barrier::new(8));
        for _ in 0..8 {
            let other = ExperimentStore::open(&path).unwrap();
            let ready = barrier.clone();
            handles.push(std::thread::spawn(move || {
                ready.wait();
                other.report_experiment(&report("succeeded"), 100).unwrap()
            }));
        }
        let receipts: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert!(receipts.iter().all(|r| r == &receipts[0]));
        assert_eq!(store.due_schedules(100).unwrap().len(), 1);
    }

    #[test]
    fn expired_receipts_and_late_claims_do_not_block_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let store = setup(&dir.path().join("a.db"), dir.path());
        for (id, expiry) in [("already-expired", 99), ("expires-in-queue", 101)] {
            store
                .receive_remote_command(
                    &AgentCommand {
                        protocol: FARHELM_PROTOCOL.into(),
                        command_id: id.into(),
                        agent_id: "agent".into(),
                        action: CommandAction::CodexTurnStart,
                        created_at_unix: 90,
                        expires_at_unix: expiry,
                        payload: Some(json!({"session_id":"s","project_id":"p","prompt":"go"})),
                    },
                    100,
                )
                .unwrap();
            store.mark_remote_accepted_reported(id, 100).unwrap();
            assert!(!store.claim_remote_command(id, 102).unwrap());
        }
        store.create_schedule(&json!({"schedule_id":"late","project_id":"p","session_id":"s","prompt":"check","trigger":{"type":"at_time","run_at_unix":100}}),100).unwrap();
        store.due_schedules(100).unwrap();
        assert!(!store.claim_schedule("late", 86501).unwrap());
        assert_eq!(
            store.schedule_detail("late").unwrap()["summary"]["state"],
            "missed"
        );
        store.create_schedule(&json!({"schedule_id":"next","project_id":"p","session_id":"s","prompt":"check","trigger":{"type":"at_time","run_at_unix":86501}}),86501).unwrap();
        store.due_schedules(86501).unwrap();
        assert!(store.claim_schedule("next", 86501).unwrap());
    }

    #[test]
    fn legacy_parallel_running_identities_migrate_without_replay() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.db");
        let store = setup(&path, dir.path());
        {
            let c = store.lock().unwrap();
            c.execute_batch("DROP TABLE execution_queue; PRAGMA user_version=1;")
                .unwrap();
            for (id, state) in [
                ("running-a", "running"),
                ("running-b", "running"),
                ("done", "completed"),
            ] {
                c.execute("INSERT INTO remote_codex_commands(command_id,action,expires_at_unix,payload_json,state,updated_at_unix) VALUES(?1,'codex.turn.start',200,?2,?3,100)",params![id,json!({"session_id":"s","project_id":"p","prompt":"legacy"}).to_string(),state]).unwrap();
            }
        }
        drop(store);
        let store = ExperimentStore::open(&path).unwrap();
        assert_eq!(store.orphan_running_remote_commands(101).unwrap(), 2);
        assert!(store.pending_remote_commands().unwrap().is_empty());
        for id in ["running-a", "running-b", "done"] {
            assert!(!store.claim_remote_command(id, 101).unwrap());
        }
        let c = store.lock().unwrap();
        assert_eq!(
            c.query_row("SELECT count(*) FROM remote_codex_commands", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            3
        );
        let version: i64 = c
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, 9);
        // V0.6 guards schema <= 1; opening this live database is a refused rollback.
        assert!(version > 1);
    }

    #[test]
    fn busy_session_backlog_does_not_starve_receipts_or_other_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let store = setup(&dir.path().join("a.db"), dir.path());
        for n in 0..20 {
            let id = format!("cmd-{n:02}");
            store
                .receive_remote_command(
                    &AgentCommand {
                        protocol: FARHELM_PROTOCOL.into(),
                        command_id: id.clone(),
                        agent_id: "agent".into(),
                        action: CommandAction::CodexTurnStart,
                        created_at_unix: 100,
                        expires_at_unix: 200,
                        payload: Some(
                            json!({"session_id":if n==19{"other"}else{"s"},"prompt":"go"}),
                        ),
                    },
                    100,
                )
                .unwrap();
            if n < 19 {
                store.mark_remote_accepted_reported(&id, 100).unwrap();
            }
        }
        assert!(store.claim_remote_command("cmd-00", 100).unwrap());
        assert_eq!(
            store.pending_remote_commands().unwrap()[0].command_id,
            "cmd-19"
        );
        store.mark_remote_accepted_reported("cmd-19", 100).unwrap();
        let ready = store.runnable_remote_commands(100).unwrap();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].command_id, "cmd-19");
    }
}
