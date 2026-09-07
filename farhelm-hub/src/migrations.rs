//! The only schema-version owner for this role. Older binaries refuse version 7.
use anyhow::{Result, ensure};
use rusqlite::Connection;
pub(crate) fn write_transaction(
    connection: &Connection,
) -> rusqlite::Result<rusqlite::Transaction<'_>> {
    rusqlite::Transaction::new_unchecked(connection, rusqlite::TransactionBehavior::Immediate)
}
pub fn apply(connection: &Connection) -> Result<()> {
    connection.pragma_update(None, "secure_delete", true)?;
    let tx = write_transaction(connection)?;
    let version: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
    ensure!(version <= 7, "database schema is newer than this binary");
    tx.execute_batch("CREATE TABLE IF NOT EXISTS hub_maintenance(id INTEGER PRIMARY KEY CHECK(id=1),vacuum_pending INTEGER NOT NULL);")?;
    tx.execute(
        "INSERT OR IGNORE INTO hub_maintenance VALUES(1,?1)",
        [version > 0 && version < 7],
    )?;
    if version == 7 {
        tx.commit()?;
        return finish_maintenance(connection);
    }
    tx.execute_batch(
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
    tx.execute_batch(
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
            CREATE TABLE IF NOT EXISTS codex_schedules (
                schedule_id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                project_id TEXT NOT NULL,
                trigger_json TEXT NOT NULL,
                state TEXT NOT NULL,
                created_at_unix INTEGER NOT NULL,
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
            );
            CREATE TABLE IF NOT EXISTS browser_sessions (
                token_hash TEXT PRIMARY KEY,
                user TEXT NOT NULL,
                csrf_token TEXT NOT NULL,
                created_at_unix INTEGER NOT NULL,
                expires_at_unix INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS login_failures (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                occurred_at_unix INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS agent_credentials (
                agent_id TEXT PRIMARY KEY,
                token_hash TEXT NOT NULL UNIQUE,
                created_at_unix INTEGER NOT NULL,
                updated_at_unix INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS pairing_codes (
                pairing_id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                code_hash TEXT NOT NULL UNIQUE,
                attempts INTEGER NOT NULL DEFAULT 0,
                expires_at_unix INTEGER NOT NULL,
                consumed INTEGER NOT NULL DEFAULT 0 CHECK (consumed IN (0,1)),
                created_at_unix INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS pairing_failures (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                occurred_at_unix INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS project_candidates (
                candidate_id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                display_name TEXT NOT NULL,
                suggested_project_id TEXT NOT NULL,
                session_count INTEGER NOT NULL,
                state TEXT NOT NULL CHECK (state IN ('discovered','approved')),
                updated_at_unix INTEGER NOT NULL,
                UNIQUE(agent_id,candidate_id)
            );",
        )?;
    tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS typed_commands (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                command_id TEXT NOT NULL UNIQUE,
                agent_id TEXT NOT NULL,
                action TEXT NOT NULL CHECK (action IN ('codex.session.create','codex.session.resume','codex.turn.start','codex.turn.steer','codex.turn.interrupt','codex.schedule.create','codex.schedule.cancel','project.approve')),
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
    crate::typed_command_store::ensure_action_schema(&tx)?;
    crate::event_store::notifications::migrate(&tx)?;
    crate::event_store::notifications::migrate_history(&tx)?;
    tx.pragma_update(None, "user_version", 7)?;
    tx.commit()?;
    finish_maintenance(connection)?;
    Ok(())
}

fn finish_maintenance(connection: &Connection) -> Result<()> {
    let vacuum: bool = connection.query_row(
        "SELECT vacuum_pending FROM hub_maintenance WHERE id=1",
        [],
        |r| r.get(0),
    )?;
    if vacuum {
        connection.execute_batch("VACUUM;")?;
        connection.execute("UPDATE hub_maintenance SET vacuum_pending=0 WHERE id=1", [])?;
    }
    ensure!(
        checkpoint(connection)?,
        "Hub privacy checkpoint is busy; stop other database readers and retry startup"
    );
    Ok(())
}

pub(crate) fn checkpoint(connection: &Connection) -> Result<bool> {
    Ok(
        connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
            r.get::<_, i64>(0)
        })? == 0,
    )
}
