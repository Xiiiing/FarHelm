//! The only schema-version owner for this role. Older binaries refuse version 9.
use anyhow::{Result, ensure};
use rusqlite::Connection;
pub(crate) fn write_transaction(
    connection: &Connection,
) -> rusqlite::Result<rusqlite::Transaction<'_>> {
    rusqlite::Transaction::new_unchecked(connection, rusqlite::TransactionBehavior::Immediate)
}
pub fn apply(connection: &Connection) -> Result<()> {
    let tx = write_transaction(connection)?;
    let version: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
    ensure!(version <= 9, "database schema is newer than this binary");
    if version == 9 {
        return Ok(());
    }
    if version == 8 {
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS project_members(project_id TEXT NOT NULL,directory_id TEXT NOT NULL,path TEXT NOT NULL,is_primary INTEGER NOT NULL CHECK(is_primary IN (0,1)),position INTEGER NOT NULL,PRIMARY KEY(project_id,directory_id));
             CREATE TABLE IF NOT EXISTS managed_attachments(attachment_id TEXT PRIMARY KEY,session_id TEXT NOT NULL,path TEXT NOT NULL,mime_type TEXT NOT NULL,size_bytes INTEGER NOT NULL,ephemeral INTEGER NOT NULL CHECK(ephemeral IN (0,1)),state TEXT NOT NULL CHECK(state IN ('uploading','ready','failed')),created_at_unix INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS session_tombstones(session_id TEXT PRIMARY KEY,operation_id TEXT NOT NULL,deleted_at_unix INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS temporary_sessions(session_id TEXT PRIMARY KEY,created_at_unix INTEGER NOT NULL);",
        )?;
        tx.execute_batch("INSERT OR IGNORE INTO project_members(project_id,directory_id,path,is_primary,position) SELECT project_id,'mem_'||lower(hex(randomblob(16))),path,1,0 FROM approved_projects;")?;
        tx.pragma_update(None, "user_version", 9)?;
        tx.commit()?;
        return Ok(());
    }
    if version == 7 {
        crate::experiment_store::projects::migrate(&tx)?;
        tx.execute_batch("CREATE TABLE IF NOT EXISTS project_members(project_id TEXT NOT NULL,directory_id TEXT NOT NULL,path TEXT NOT NULL,is_primary INTEGER NOT NULL CHECK(is_primary IN (0,1)),position INTEGER NOT NULL,PRIMARY KEY(project_id,directory_id)); INSERT OR IGNORE INTO project_members(project_id,directory_id,path,is_primary,position) SELECT project_id,'mem_'||lower(hex(randomblob(16))),path,1,0 FROM approved_projects; CREATE TABLE IF NOT EXISTS managed_attachments(attachment_id TEXT PRIMARY KEY,session_id TEXT NOT NULL,path TEXT NOT NULL,mime_type TEXT NOT NULL,size_bytes INTEGER NOT NULL,ephemeral INTEGER NOT NULL CHECK(ephemeral IN (0,1)),state TEXT NOT NULL CHECK(state IN ('uploading','ready','failed')),created_at_unix INTEGER NOT NULL); CREATE TABLE IF NOT EXISTS session_tombstones(session_id TEXT PRIMARY KEY,operation_id TEXT NOT NULL,deleted_at_unix INTEGER NOT NULL); CREATE TABLE IF NOT EXISTS temporary_sessions(session_id TEXT PRIMARY KEY,created_at_unix INTEGER NOT NULL);")?;
        tx.pragma_update(None, "user_version", 9)?;
        tx.commit()?;
        return Ok(());
    }
    tx.execute_batch(
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
    tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS experiment_watches (
                watch_id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                project_root TEXT NOT NULL,
                name TEXT NOT NULL,
                pid INTEGER NOT NULL,
                proc_start_time INTEGER NOT NULL,
                uid INTEGER NOT NULL,
                log_path TEXT NOT NULL,
                session_id TEXT,
                new_session_mode TEXT CHECK (new_session_mode IS NULL OR new_session_mode IN ('inspect','edit')),
                success_prompt TEXT,
                state TEXT NOT NULL CHECK (state IN ('watching','succeeded','failed','unknown','cancelled')),
                detail TEXT,
                auto_prompt_claimed INTEGER NOT NULL DEFAULT 0 CHECK (auto_prompt_claimed IN (0,1,2)),
                created_at_unix INTEGER NOT NULL,
                updated_at_unix INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS event_outbox (
                sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                event_id TEXT NOT NULL UNIQUE,
                event_type TEXT NOT NULL,
                payload_json TEXT NOT NULL,
                created_at_unix INTEGER NOT NULL,
                acknowledged INTEGER NOT NULL DEFAULT 0 CHECK (acknowledged IN (0,1))
            );
            CREATE TABLE IF NOT EXISTS remote_codex_commands (
                command_id TEXT PRIMARY KEY,
                action TEXT NOT NULL,
                expires_at_unix INTEGER NOT NULL,
                payload_json TEXT NOT NULL,
                state TEXT NOT NULL CHECK (state IN ('accepted','running','completed','failed','expired','orphaned')),
                accepted_reported INTEGER NOT NULL DEFAULT 0 CHECK (accepted_reported IN (0,1)),
                terminal_reported INTEGER NOT NULL DEFAULT 0 CHECK (terminal_reported IN (0,1)),
                data_json TEXT,
                detail TEXT,
                updated_at_unix INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS codex_session_bindings (
                session_id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                cwd TEXT NOT NULL,
                mode TEXT NOT NULL CHECK (mode IN ('inspect','edit')),
                updated_at_unix INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS discovered_projects (
                candidate_id TEXT PRIMARY KEY,
                path TEXT NOT NULL UNIQUE,
                display_name TEXT NOT NULL,
                suggested_project_id TEXT NOT NULL,
                session_count INTEGER NOT NULL,
                state TEXT NOT NULL CHECK (state IN ('discovered','approved')),
                updated_at_unix INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS approved_projects (
                project_id TEXT PRIMARY KEY,
                path TEXT NOT NULL UNIQUE,
                success_patterns_json TEXT NOT NULL DEFAULT '[]',
                failure_patterns_json TEXT NOT NULL DEFAULT '[]',
                updated_at_unix INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS codex_prompt_schedules (
                schedule_id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                trigger_json TEXT NOT NULL,
                prompt TEXT NOT NULL,
                state TEXT NOT NULL CHECK (state IN ('pending','queued','running','completed','cancelled','skipped','missed','failed','orphaned')),
                grace_expires_at_unix INTEGER,
                created_at_unix INTEGER NOT NULL,
                updated_at_unix INTEGER NOT NULL
            );",
        )?;
    crate::experiment_store::ensure_remote_command_columns(&tx)?;
    crate::experiment_store::execution::migrate(&tx)?;
    crate::experiment_store::projects::migrate(&tx)?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS project_members(project_id TEXT NOT NULL,directory_id TEXT NOT NULL,path TEXT NOT NULL,is_primary INTEGER NOT NULL CHECK(is_primary IN (0,1)),position INTEGER NOT NULL,PRIMARY KEY(project_id,directory_id));
         CREATE TABLE IF NOT EXISTS managed_attachments(attachment_id TEXT PRIMARY KEY,session_id TEXT NOT NULL,path TEXT NOT NULL,mime_type TEXT NOT NULL,size_bytes INTEGER NOT NULL,ephemeral INTEGER NOT NULL CHECK(ephemeral IN (0,1)),state TEXT NOT NULL CHECK(state IN ('uploading','ready','failed')),created_at_unix INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS session_tombstones(session_id TEXT PRIMARY KEY,operation_id TEXT NOT NULL,deleted_at_unix INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS temporary_sessions(session_id TEXT PRIMARY KEY,created_at_unix INTEGER NOT NULL);",
    )?;
    tx.execute_batch("INSERT OR IGNORE INTO project_members(project_id,directory_id,path,is_primary,position) SELECT project_id,'mem_'||lower(hex(randomblob(16))),path,1,0 FROM approved_projects;")?;
    tx.pragma_update(None, "user_version", 9)?;
    tx.commit()?;
    Ok(())
}
