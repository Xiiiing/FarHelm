//! Bounded, Agent-memory-only history for native ephemeral threads.
use super::*;
const SNAPSHOT_BYTES: usize = 2 * 1024 * 1024;

impl Codex {
    pub async fn start_temporary(&self, cwd: &std::path::Path, mode: &str) -> Result<Value> {
        let _lifecycle = self.inner.lifecycle.lock().await;
        self.check_temporary_capacity().await?;
        self.call(
            "codex.session.start",
            json!({"cwd":cwd,"mode":mode,"inherit_permissions":true,"ephemeral":true}),
        )
        .await
    }

    pub async fn check_temporary_capacity(&self) -> Result<()> {
        ensure!(
            self.inner
                .loaded
                .read()
                .await
                .values()
                .filter(|v| v.thread["ephemeral"] == true)
                .count()
                < 32,
            "temporary_session_capacity"
        );
        Ok(())
    }
    pub async fn register_temporary(&self, thread: &Value, mode: &str) -> Result<()> {
        self.check_temporary_capacity().await?;
        let id = thread["id"].as_str().context("temporary_session_missing")?;
        ensure!(thread["ephemeral"] == true, "temporary_session_unverified");
        let mut thread = thread.clone();
        thread["history_missing"] = json!(true); // excludeTurns forks have no complete prefix.
        thread["turns"] = json!([]);
        self.inner.loaded.write().await.insert(
            id.into(),
            LoadedThread {
                thread,
                mode: mode.into(),
                has_turns: true,
                context: farhelm_protocol::CodexSessionContext {
                    ephemeral: Some(true),
                    ..Default::default()
                },
            },
        );
        Ok(())
    }
    pub(super) async fn temporary_history(
        &self,
        session: &str,
        resume: &Value,
    ) -> Result<Option<Value>> {
        let loaded = self.inner.loaded.read().await;
        let Some(loaded) = loaded
            .get(session)
            .filter(|loaded| loaded.thread["ephemeral"] == true)
        else {
            return Ok(None);
        };
        let mut turns: Vec<_> = loaded.thread["turns"]
            .as_array()
            .into_iter()
            .flatten()
            .rev()
            .cloned()
            .collect();
        if loaded.thread["history_missing"] == true {
            turns.push(json!({"id":"temporary-history-missing","status":"completed","items":[{"id":"temporary-history-missing","type":"agentMessage","text":"临时任务的部分历史未保留或已超出内存快照上限；此处不是完整历史。"}]}));
        }
        let mut page = history::bounded_page(session, &turns, None, None, resume)?;
        page["context"] = serde_json::to_value(&loaded.context)?;
        Ok(Some(page))
    }
    pub(super) async fn end_temporary(&self, c: &Connection, session: &str) -> Result<Value> {
        ensure!(
            self.inner
                .loaded
                .read()
                .await
                .get(session)
                .is_some_and(|v| v.thread["ephemeral"] == true),
            "not_temporary_session"
        );
        ensure!(
            c.pending_server_requests(session).is_empty(),
            "codex_archive_busy"
        );
        let live = c
            .request(
                "thread/read",
                json!({"threadId":session,"includeTurns":false}),
            )
            .await?;
        ensure!(
            live["thread"]["ephemeral"] == true && live["thread"]["status"]["type"] == "idle",
            "codex_archive_busy"
        );
        match c
            .request("thread/queue/list", json!({"threadId":session}))
            .await
        {
            Ok(queue) => ensure!(
                queue["data"].as_array().is_some_and(Vec::is_empty),
                "codex_archive_busy"
            ),
            Err(error)
                if error
                    .to_string()
                    .contains("codex_ephemeral_queue_unsupported") => {}
            Err(error) => return Err(error),
        }
        let terminals = c
            .request(
                "thread/backgroundTerminals/list",
                json!({"threadId":session,"limit":1}),
            )
            .await?;
        ensure!(
            terminals["data"].as_array().is_some_and(Vec::is_empty)
                && terminals["nextCursor"].is_null(),
            "codex_archive_busy"
        );
        let result = c
            .request("thread/unsubscribe", json!({"threadId":session}))
            .await?;
        ensure!(
            matches!(
                result["status"].as_str(),
                Some("unsubscribed" | "notSubscribed" | "notLoaded")
            ),
            "temporary_end_unverified"
        );
        self.inner.loaded.write().await.remove(session);
        self.inner.live_activity.write().await.remove(session);
        self.inner
            .image_resources
            .write()
            .await
            .retain(|_, image| image.session != session);
        self.inner.index.write().await.rows.remove(session);
        Ok(json!({"session_id":session,"status":"ended","native_unload_grace_seconds":1800}))
    }
}

pub(super) async fn observe(inner: &Inner, event: &Value) {
    let p = &event["params"];
    let Some(session) = p["threadId"].as_str() else {
        return;
    };
    let mut loaded = inner.loaded.write().await;
    let Some(loaded) = loaded
        .get_mut(session)
        .filter(|v| v.thread["ephemeral"] == true)
    else {
        return;
    };
    let method = event["method"].as_str().unwrap_or_default();
    let Some(turn_id) = p["turnId"].as_str().or_else(|| p["turn"]["id"].as_str()) else {
        return;
    };
    if !matches!(
        method,
        "turn/started"
            | "turn/completed"
            | "item/started"
            | "item/completed"
            | "item/agentMessage/delta"
    ) {
        return;
    }
    loaded.has_turns = true;
    if !loaded.thread["turns"].is_array() {
        loaded.thread["turns"] = json!([]);
    }
    let turns = loaded.thread["turns"]
        .as_array_mut()
        .expect("array initialized");
    if !turns.iter().any(|turn| turn["id"] == turn_id) {
        turns.push(json!({"id":turn_id,"status":"inProgress","items":[]}));
    }
    let turn = turns
        .iter_mut()
        .find(|turn| turn["id"] == turn_id)
        .expect("turn initialized");
    if method == "turn/completed" {
        turn["status"] = p["turn"]["status"].clone();
    }
    if matches!(method, "item/started" | "item/completed") {
        let items = turn["items"].as_array_mut().expect("items initialized");
        if let Some(item) = items.iter_mut().find(|item| item["id"] == p["item"]["id"]) {
            *item = p["item"].clone();
        } else {
            items.push(p["item"].clone());
        }
    } else if method == "item/agentMessage/delta" {
        let items = turn["items"].as_array_mut().expect("items initialized");
        if !items.iter().any(|item| item["id"] == p["itemId"]) {
            items.push(json!({"id":p["itemId"],"type":"agentMessage","text":""}));
        }
        let item = items
            .iter_mut()
            .find(|item| item["id"] == p["itemId"])
            .expect("item initialized");
        item["text"] = json!(format!(
            "{}{}",
            item["text"].as_str().unwrap_or_default(),
            p["delta"].as_str().unwrap_or_default()
        ));
    }
    if serde_json::to_vec(&loaded.thread["turns"])
        .map_or(true, |bytes| bytes.len() > SNAPSHOT_BYTES)
    {
        loaded.thread["turns"] = json!([]);
        loaded.thread["history_missing"] = json!(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn temporary_history_survives_browser_reads_and_marks_eviction() {
        let codex = Codex::new(None);
        codex
            .register_temporary(
                &json!({"id":"temporary","ephemeral":true,"cwd":"/synthetic"}),
                "inspect",
            )
            .await
            .unwrap();
        observe(&codex.inner, &json!({"method":"item/completed","params":{"threadId":"temporary","turnId":"t","item":{"id":"i","type":"agentMessage","text":"snapshot response"}}})).await;
        let page = codex
            .temporary_history("temporary", &Value::Null)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(page["turns"][0]["items"][0]["text"], "snapshot response");
        assert!(page.to_string().contains("不是完整历史"));
        observe(&codex.inner, &json!({"method":"item/completed","params":{"threadId":"temporary","turnId":"t","item":{"id":"i","type":"agentMessage","text":"x".repeat(SNAPSHOT_BYTES + 1)}}})).await;
        let page = codex
            .temporary_history("temporary", &Value::Null)
            .await
            .unwrap()
            .unwrap();
        assert!(page.to_string().len() < 4096);
        assert!(page.to_string().contains("不是完整历史"));
    }
}
