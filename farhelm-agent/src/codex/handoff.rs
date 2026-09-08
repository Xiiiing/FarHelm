//! Explicit idle handoff releases native exclusive writers, without touching other clients.
use super::*;

impl Codex {
    pub async fn activity(&self) -> tokio::sync::RwLockReadGuard<'_, ()> {
        self.inner.activity.read().await
    }

    pub async fn handoff(&self, session: &str) -> Result<Value> {
        tokio::time::timeout(Duration::from_secs(15), self.handoff_idle(session))
            .await
            .context("codex_handoff_unconfirmed")?
    }

    async fn handoff_idle(&self, session: &str) -> Result<Value> {
        // Never queue an exclusive lock that could block nested read guards.
        let _exclusive = self
            .inner
            .activity
            .try_write()
            .context("codex_handoff_busy")?;
        if !self.inner.loaded.read().await.contains_key(session) {
            return Ok(json!({"session_id":session}));
        }
        let connection = self.connection().await?;
        let loaded = connection
            .request("thread/loaded/list", json!({"limit":128}))
            .await
            .context("codex_handoff_unverified")?;
        ensure!(loaded["nextCursor"].is_null(), "codex_handoff_unverified");
        let ids = loaded["data"]
            .as_array()
            .context("codex_handoff_unverified")?;
        ensure!(ids.len() <= 128, "codex_handoff_unverified");
        for id in ids {
            let id = id.as_str().context("codex_handoff_unverified")?;
            let value = connection
                .request("thread/read", json!({"threadId":id,"includeTurns":false}))
                .await
                .context("codex_handoff_unverified")?;
            let thread = &value["thread"];
            ensure!(thread["status"]["type"] == "idle", "codex_handoff_busy");
            ensure!(
                thread["path"]
                    .as_str()
                    .is_some_and(|p| std::path::Path::new(p).is_file()),
                "codex_handoff_unsaved"
            );
            let terminals = connection
                .request(
                    "thread/backgroundTerminals/list",
                    json!({"threadId":id,"limit":1}),
                )
                .await
                .context("codex_handoff_unverified")?;
            ensure!(
                terminals["data"].as_array().is_some_and(Vec::is_empty)
                    && terminals["nextCursor"].is_null(),
                "codex_handoff_background"
            );
        }
        connection.close_idle().await?;
        self.inner.connection.lock().await.take();
        self.inner.loaded.write().await.clear();
        Ok(json!({"session_id":session}))
    }
}
