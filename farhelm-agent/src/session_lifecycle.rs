//! One durable gate shared by native lifecycle changes, remote input and schedules.
use super::*;

pub(super) fn writable(c: &Connection, session: &str) -> Result<()> {
    ensure!(
        !c.query_row(
            "SELECT EXISTS(SELECT 1 FROM session_tombstones WHERE session_id=?1)",
            [session],
            |r| r.get::<_, bool>(0)
        )?,
        "session_deleted"
    );
    let state: Option<(bool, Option<String>)> = c
        .query_row(
            "SELECT archived,operation_id FROM session_lifecycle WHERE session_id=?1",
            [session],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((archived, operation)) = state {
        ensure!(operation.is_none(), "codex_archive_unverified");
        ensure!(!archived, "codex_session_archived");
    }
    Ok(())
}

fn idle(c: &Connection, id: &str, command: &str) -> Result<()> {
    let busy:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM execution_queue WHERE session_id=?1 AND state IN ('queued','running')) OR EXISTS(SELECT 1 FROM codex_prompt_schedules WHERE session_id=?1 AND state IN ('pending','queued','running')) OR EXISTS(SELECT 1 FROM experiment_watches WHERE session_id=?1 AND success_prompt IS NOT NULL AND auto_prompt_claimed=0 AND state IN ('watching','succeeded')) OR EXISTS(SELECT 1 FROM remote_codex_commands WHERE command_id!=?2 AND json_extract(payload_json,'$.session_id')=?1 AND state IN ('accepted','running'))",params![id,command],|r|r.get(0))?;
    ensure!(!busy, "codex_archive_busy");
    let locked:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM session_lifecycle WHERE session_id=?1 AND operation_id IS NOT NULL AND operation_id!=?2)",params![id,command],|r|r.get(0))?;
    ensure!(!locked, "codex_archive_unverified");
    Ok(())
}

#[derive(Clone)]
pub(crate) struct ArchiveTarget {
    pub id: String,
    pub project: String,
    pub cwd: PathBuf,
    pub mode: String,
    pub title: Value,
    pub archived: bool,
}

impl ExperimentStore {
    pub fn is_temporary_session(&self, session: &str) -> Result<bool> {
        Ok(self.lock()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM temporary_sessions WHERE session_id=?1)",
            [session],
            |r| r.get(0),
        )?)
    }

    pub fn mark_temporary_session(&self, session: &str, now: u64) -> Result<()> {
        self.lock()?.execute(
            "INSERT OR REPLACE INTO temporary_sessions VALUES(?1,?2)",
            params![session, as_i64(now)?],
        )?;
        Ok(())
    }

    pub fn cleanup_stale_temporary_sessions(&self, now: u64) -> Result<()> {
        let sessions = {
            let c = self.lock()?;
            let mut s = c.prepare("SELECT session_id FROM temporary_sessions")?;
            s.query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        for session in sessions {
            self.cleanup_ephemeral_attachments(&session)?;
            let c = self.lock()?;
            let tx = crate::migrations::write_transaction(&c)?;
            tx.execute(
                "INSERT OR IGNORE INTO session_tombstones VALUES(?1,'service-restart',?2)",
                params![session, as_i64(now)?],
            )?;
            tx.execute(
                "DELETE FROM codex_session_bindings WHERE session_id=?1",
                [&session],
            )?;
            tx.execute(
                "DELETE FROM temporary_sessions WHERE session_id=?1",
                [&session],
            )?;
            insert_event(
                &tx,
                &format!("temporary-restart:{session}:{now}"),
                "codex.session.deleted",
                &json!({"session_id":session,"operation_id":"service-restart","updated_at_unix":now}),
                now,
            )?;
            tx.commit()?;
        }
        Ok(())
    }

    pub fn tombstone_session(&self, session: &str, operation: &str, now: u64) -> Result<()> {
        let c = self.lock()?;
        let tx = crate::migrations::write_transaction(&c)?;
        tx.execute(
            "INSERT INTO session_tombstones VALUES(?1,?2,?3) ON CONFLICT(session_id) DO NOTHING",
            params![session, operation, as_i64(now)?],
        )?;
        tx.execute(
            "DELETE FROM codex_session_bindings WHERE session_id=?1",
            [session],
        )?;
        tx.execute(
            "DELETE FROM temporary_sessions WHERE session_id=?1",
            [session],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn begin_native_lifecycle(&self, id: &str, command: &str) -> Result<()> {
        let c = self.lock()?;
        let tx = crate::migrations::write_transaction(&c)?;
        writable(&tx, id)?;
        idle(&tx, id, command)?;
        tx.execute("INSERT INTO session_lifecycle(session_id,archived,operation_id) VALUES(?1,0,?2) ON CONFLICT(session_id) DO UPDATE SET operation_id=excluded.operation_id", params![id,command])?;
        tx.commit()?;
        Ok(())
    }

    pub fn pending_native_lifecycles(&self) -> Result<Vec<(String, String)>> {
        let c = self.lock()?;
        let mut s = c.prepare("SELECT l.session_id,l.operation_id FROM session_lifecycle l JOIN remote_codex_commands r ON r.command_id=l.operation_id WHERE r.action='codex.native.operation' AND r.state IN ('failed','completed') AND r.command_id NOT IN (SELECT command_id FROM archive_operations) LIMIT 64")?;
        Ok(s.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn release_native_lifecycle(&self, id: &str, command: &str) -> Result<()> {
        self.lock()?.execute("UPDATE session_lifecycle SET operation_id=NULL WHERE session_id=?1 AND operation_id=?2", params![id,command])?;
        Ok(())
    }

    pub fn check_session_writable(&self, id: &str) -> Result<()> {
        let c = self.lock()?;
        writable(&c, id)
    }

    pub fn archive_targets(&self, rows: &[Value], command: &str) -> Result<Vec<ArchiveTarget>> {
        let c = self.lock()?;
        let mut targets = Vec::new();
        for row in rows {
            let id = required_payload_string(row, "id")?;
            let cwd = PathBuf::from(required_payload_string(row, "cwd")?);
            let meta = fs::symlink_metadata(&cwd).context("codex_archive_unapproved")?;
            ensure!(
                meta.is_dir()
                    && meta.uid() == unsafe { libc::geteuid() }
                    && fs::canonicalize(&cwd)? == cwd,
                "codex_archive_unapproved"
            );
            let binding:Option<(String,String,String)>=c.query_row("SELECT b.project_id,b.cwd,b.mode FROM codex_session_bindings b JOIN approved_projects p ON p.project_id=b.project_id WHERE b.session_id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            let (project, mode) = if let Some((project, bound, mode)) = binding {
                ensure!(Path::new(&bound) == cwd, "codex_archive_unapproved");
                (project, mode)
            } else {
                let project: String = c
                    .query_row(
                        "SELECT project_id FROM approved_projects WHERE path=?1",
                        [cwd.to_string_lossy().as_ref()],
                        |r| r.get(0),
                    )
                    .optional()?
                    .context("codex_archive_unapproved")?;
                (project, "inspect".into())
            };
            idle(&c, id, command)?;
            targets.push(ArchiveTarget {
                id: id.into(),
                project,
                cwd,
                mode,
                title: row["name"].clone(),
                archived: row["archived"]
                    .as_bool()
                    .context("codex_archive_unverified")?,
            });
        }
        Ok(targets)
    }

    pub fn begin_archive(
        &self,
        command: &str,
        targets: &[ArchiveTarget],
        archived: bool,
    ) -> Result<()> {
        let c = self.lock()?;
        let tx = crate::migrations::write_transaction(&c)?;
        for t in targets {
            idle(&tx, &t.id, command)?;
            ensure!(
                tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM approved_projects WHERE project_id=?1)",
                    [&t.project],
                    |r| r.get::<_, bool>(0)
                )?,
                "codex_archive_unapproved"
            );
            tx.execute("INSERT INTO session_lifecycle VALUES(?1,?2,?3) ON CONFLICT(session_id) DO UPDATE SET operation_id=excluded.operation_id",params![t.id,t.archived,command])?;
        }
        tx.execute(
            "INSERT INTO archive_operations VALUES(?1,?2,?3)",
            params![
                command,
                serde_json::to_string(&targets.iter().map(|t| t.id.as_str()).collect::<Vec<_>>())?,
                archived
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn finish_archive(
        &self,
        command: &str,
        targets: &[ArchiveTarget],
        archived: bool,
        now: u64,
    ) -> Result<Value> {
        self.settle_archive(command, targets, Some(archived), now)
    }

    pub fn settle_archive(
        &self,
        command: &str,
        targets: &[ArchiveTarget],
        desired: Option<bool>,
        now: u64,
    ) -> Result<Value> {
        let c = self.lock()?;
        let tx = crate::migrations::write_transaction(&c)?;
        for t in targets {
            let archived = desired.unwrap_or(t.archived);
            tx.execute(
                "INSERT OR IGNORE INTO codex_session_bindings VALUES(?1,?2,?3,?4,?5)",
                params![
                    t.id,
                    t.project,
                    t.cwd.to_string_lossy(),
                    t.mode,
                    as_i64(now)?
                ],
            )?;
            tx.execute("UPDATE session_lifecycle SET archived=?2,operation_id=NULL WHERE session_id=?1 AND operation_id=?3",params![t.id,archived,command])?;
            insert_event(
                &tx,
                &format!("{command}:lifecycle:{}", t.id),
                "codex.session.updated",
                &json!({"session_id":t.id,"project_id":t.project,"mode":t.mode,"state":if archived{"archived"}else{"idle"},"active_turn_id":null,"title":t.title,"update_kind":"lifecycle","updated_at_unix":now}),
                now,
            )?;
        }
        let result = json!({"session_id":targets.first().context("codex_archive_unverified")?.id});
        tx.execute("UPDATE remote_codex_commands SET state=?4,data_json=?2,detail=?5,terminal_reported=0,updated_at_unix=?3 WHERE command_id=?1",params![command,serde_json::to_string(&result)?,as_i64(now)?,if desired.is_some(){"completed"}else{"failed"},desired.is_none().then_some("codex_archive_unverified")])?;
        tx.execute(
            "DELETE FROM archive_operations WHERE command_id=?1",
            [command],
        )?;
        tx.commit()?;
        Ok(result)
    }

    pub fn pending_archives(&self) -> Result<Vec<(String, Vec<String>, bool)>> {
        let c = self.lock()?;
        let mut s = c.prepare("SELECT command_id,session_ids,archived FROM archive_operations")?;
        Ok(s.query_map([], |r| {
            Ok((
                r.get(0)?,
                serde_json::from_str(&r.get::<_, String>(1)?).map_err(json_conversion(1))?,
                r.get(2)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?)
    }

    pub fn release_archive(&self, command: &str) -> Result<()> {
        let c = self.lock()?;
        let tx = crate::migrations::write_transaction(&c)?;
        tx.execute(
            "UPDATE session_lifecycle SET operation_id=NULL WHERE operation_id=?1",
            [command],
        )?;
        tx.execute(
            "DELETE FROM archive_operations WHERE command_id=?1",
            [command],
        )?;
        tx.commit()?;
        Ok(())
    }
}
