//! Native subtree checks; paths and bodies are never part of a public preview or receipt.
use super::*;
use crate::experiment_store::{ExperimentStore, lifecycle::ArchiveTarget};
use farhelm_protocol::projects::ArchivePreview;
use sha2::{Digest, Sha256};

impl Codex {
    async fn archive_connection(&self) -> Result<Arc<Connection>> {
        let c = self.connection().await?;
        let version = self.status().version.unwrap_or_default();
        let version = version
            .split_whitespace()
            .last()
            .and_then(|s| semver::Version::parse(s).ok())
            .context("codex_archive_unverified")?;
        ensure!(
            version >= semver::Version::new(0, 153, 4),
            "codex_archive_unverified"
        );
        Ok(c)
    }

    async fn archive_list(
        &self,
        c: &Connection,
        filter: Value,
        archived: bool,
    ) -> Result<Vec<Value>> {
        let mut params = json!({"limit":100,"sourceKinds":THREAD_SOURCES,"modelProviders":[],"archived":archived});
        for (k, v) in filter.as_object().context("codex_archive_unverified")? {
            params[k] = v.clone();
        }
        let mut rows = Vec::new();
        let mut cursors = std::collections::HashSet::new();
        loop {
            let result = c
                .request("thread/list", params.clone())
                .await
                .context("codex_archive_unverified")?;
            let page = result["data"]
                .as_array()
                .context("codex_archive_unverified")?;
            ensure!(
                page.len() <= 100 && rows.len() + page.len() <= 10000,
                "codex_archive_unverified"
            );
            rows.extend(page.iter().cloned().map(|mut row| {
                row["archived"] = json!(archived);
                row
            }));
            let Some(cursor) = result["nextCursor"].as_str() else {
                return Ok(rows);
            };
            ensure!(
                cursors.insert(cursor.to_owned()),
                "codex_archive_unverified"
            );
            params["cursor"] = json!(cursor);
        }
    }

    async fn archive_rows(&self, c: &Connection, ids: &[String]) -> Result<Vec<Value>> {
        let mut archived_by_cwd = HashMap::<String, std::collections::HashSet<String>>::new();
        let mut rows = Vec::new();
        for id in ids {
            let result = c
                .request("thread/read", json!({"threadId":id,"includeTurns":false}))
                .await
                .context("codex_archive_unverified")?;
            let mut row = result["thread"].clone();
            ensure!(row["id"] == *id, "codex_archive_unverified");
            let cwd = row["cwd"]
                .as_str()
                .context("codex_archive_unapproved")?
                .to_owned();
            if !archived_by_cwd.contains_key(&cwd) {
                let values = self.archive_list(c, json!({"cwd":cwd}), true).await?;
                archived_by_cwd.insert(
                    cwd.clone(),
                    values
                        .iter()
                        .filter_map(|v| v["id"].as_str().map(str::to_owned))
                        .collect(),
                );
            }
            row["archived"] = json!(archived_by_cwd[&cwd].contains(id));
            rows.push(row);
        }
        Ok(rows)
    }

    async fn archive_plan(
        &self,
        store: &ExperimentStore,
        c: &Connection,
        id: &str,
        archive: bool,
        command: &str,
    ) -> Result<(Vec<ArchiveTarget>, String)> {
        let mut rows = self.archive_rows(c, &[id.to_owned()]).await?;
        if archive {
            for archived in [false, true] {
                rows.extend(
                    self.archive_list(c, json!({"ancestorThreadId":id}), archived)
                        .await?,
                );
            }
            ensure!(rows.len() <= 1000, "codex_archive_unverified");
            let mut ids = std::collections::HashSet::new();
            for row in &rows {
                ensure!(
                    ids.insert(row["id"].as_str().context("codex_archive_unverified")?),
                    "codex_archive_unverified"
                );
            }
            for row in rows.iter().skip(1) {
                let mut parent = row["parentThreadId"]
                    .as_str()
                    .context("codex_archive_unverified")?;
                let mut visited = std::collections::HashSet::new();
                while parent != id {
                    ensure!(visited.insert(parent), "codex_archive_unverified");
                    parent = rows
                        .iter()
                        .find(|v| v["id"] == parent)
                        .and_then(|v| v["parentThreadId"].as_str())
                        .context("codex_archive_unverified")?;
                }
            }
        }
        // Re-read runtime status: stored list rows are not evidence that a loaded child is idle.
        for row in &rows {
            let id = row["id"].as_str().context("codex_archive_unverified")?;
            ensure!(
                c.pending_server_requests(id).is_empty(),
                "codex_archive_busy"
            );
            let live = c
                .request("thread/read", json!({"threadId":id,"includeTurns":false}))
                .await
                .context("codex_archive_unverified")?;
            let live = &live["thread"];
            ensure!(
                matches!(live["status"]["type"].as_str(), Some("idle" | "notLoaded")),
                "codex_archive_busy"
            );
            ensure!(
                live["ephemeral"] == false
                    && live["path"]
                        .as_str()
                        .is_some_and(|p| std::path::Path::new(p).is_file()),
                "codex_archive_unsaved"
            );
            ensure!(live["cwd"] == row["cwd"], "codex_archive_changed");
            if live["status"]["type"] == "idle" {
                let goal = c.request("thread/goal/get", json!({"threadId":id})).await?;
                ensure!(goal["goal"]["status"] != "active", "codex_archive_busy");
                let terminals = c
                    .request(
                        "thread/backgroundTerminals/list",
                        json!({"threadId":id,"limit":1}),
                    )
                    .await
                    .context("codex_archive_unverified")?;
                ensure!(
                    terminals["data"].as_array().is_some_and(Vec::is_empty)
                        && terminals["nextCursor"].is_null(),
                    "codex_archive_busy"
                );
            }
            // Native queue/list deliberately rejects an archived, unloaded thread. Such a
            // thread cannot execute queued input; unarchive only restores persistence and
            // does not start its queue. Loaded descendants still require an empty queue.
            if row["archived"] == true && live["status"]["type"] == "notLoaded" {
                continue;
            }
            let queue = c
                .request("thread/queue/list", json!({"threadId":id}))
                .await
                .context("codex_archive_unverified")?;
            ensure!(
                queue["data"].as_array().is_some_and(Vec::is_empty)
                    && queue["nextCursor"].is_null(),
                "codex_archive_busy"
            );
        }
        let snapshot = rows.clone();
        let command = command.to_owned();
        let targets = store
            .background(move |s| s.archive_targets(&snapshot, &command))
            .await?;
        let mut fingerprint_rows: Vec<_> = rows
            .iter()
            .map(|r| {
                json!([
                    r["id"],
                    r["cwd"],
                    r["parentThreadId"],
                    r["archived"],
                    r["updatedAt"]
                ])
            })
            .collect();
        fingerprint_rows.sort_by_key(Value::to_string);
        let fingerprint = Sha256::digest(serde_json::to_vec(&fingerprint_rows)?)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        Ok((targets, fingerprint))
    }

    pub async fn archive_preview(
        &self,
        store: &ExperimentStore,
        id: &str,
    ) -> Result<ArchivePreview> {
        self.archive_preview_for_operation(store, id, "").await
    }

    pub async fn archive_preview_for_operation(
        &self,
        store: &ExperimentStore,
        id: &str,
        command: &str,
    ) -> Result<ArchivePreview> {
        let _activity = self.activity().await;
        let c = self.archive_connection().await?;
        let (targets, fingerprint) = tokio::time::timeout(
            Duration::from_secs(18),
            self.archive_plan(store, &c, id, true, command),
        )
        .await
        .context("codex_archive_unverified")??;
        Ok(ArchivePreview {
            session_ids: targets.into_iter().map(|t| t.id).collect(),
            fingerprint,
            can_archive: true,
            reason: None,
        })
    }

    pub async fn archive_session(
        &self,
        store: &ExperimentStore,
        command: &str,
        id: &str,
        archive: bool,
        fingerprint: Option<&str>,
    ) -> Result<Value> {
        // Recovery must never settle a native mutation still executing in this process.
        // This only serializes lifecycle operations; unrelated sessions keep running.
        let _lifecycle = self.inner.lifecycle.lock().await;
        let _activity = self.activity().await;
        let c = self.archive_connection().await?;
        let (targets, current) = tokio::time::timeout(
            Duration::from_secs(18),
            self.archive_plan(store, &c, id, archive, command),
        )
        .await
        .context("codex_archive_unverified")??;
        if archive {
            ensure!(
                fingerprint == Some(current.as_str()),
                "codex_archive_changed"
            );
        }
        let saved = targets.clone();
        let key = command.to_owned();
        store
            .background(move |s| s.begin_archive(&key, &saved, archive))
            .await?;
        // If the RPC or verification is interrupted, keep the gate closed. Recovery only reads
        // native state; it never repeats a destructive operation with a changed subtree.
        let result = c
            .request(
                if archive {
                    "thread/archive"
                } else {
                    "thread/unarchive"
                },
                json!({"threadId":id}),
            )
            .await;
        let ids: Vec<_> = targets.iter().map(|t| t.id.clone()).collect();
        let observed = self
            .archive_rows(&c, &ids)
            .await
            .context("codex_archive_unverified")?;
        if observed.iter().all(|r| r["archived"] == archive) {
            self.inner
                .loaded
                .write()
                .await
                .retain(|id, _| !ids.contains(id));
            self.inner.index.write().await.refreshed = None;
            let key = command.to_owned();
            return store
                .background(move |s| s.finish_archive(&key, &targets, archive, crate::unix_time()))
                .await;
        }
        if result.is_err()
            && observed
                .iter()
                .zip(&targets)
                .all(|(r, t)| r["archived"] == t.archived)
        {
            let key = command.to_owned();
            store.background(move |s| s.release_archive(&key)).await?;
        }
        bail!("codex_archive_unverified")
    }

    pub async fn recover_archives(&self, store: &ExperimentStore) -> Result<()> {
        let Ok(_lifecycle) = self.inner.lifecycle.try_lock() else {
            return Ok(());
        };
        for (session, command) in store.pending_native_lifecycles()? {
            let c = self.connection().await?;
            let live = c
                .request(
                    "thread/read",
                    json!({"threadId":session,"includeTurns":false}),
                )
                .await?;
            if matches!(
                live["thread"]["status"]["type"].as_str(),
                Some("idle" | "notLoaded")
            ) && c.pending_server_requests(&session).is_empty()
            {
                store.release_native_lifecycle(&session, &command)?;
            }
        }
        let pending = store.background(|s| s.pending_archives()).await?;
        if pending.is_empty() {
            return Ok(());
        }
        let _activity = self.activity().await;
        let c = self.archive_connection().await?;
        for (key, ids, archive) in pending {
            let rows = self.archive_rows(&c, &ids).await?;
            let snapshot = rows.clone();
            let command = key.clone();
            let targets = store
                .background(move |s| s.archive_targets(&snapshot, &command))
                .await?;
            if rows.iter().all(|r| r["archived"] == archive) {
                store
                    .background(move |s| {
                        s.finish_archive(&key, &targets, archive, crate::unix_time())
                    })
                    .await?;
            } else {
                store
                    .background(move |s| s.settle_archive(&key, &targets, None, crate::unix_time()))
                    .await?;
            }
        }
        self.inner.index.write().await.refreshed = None;
        Ok(())
    }
}
