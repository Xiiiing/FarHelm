//! Authenticated, non-persistent thread-index reads. Never use the command store.

use super::*;
use farhelm_protocol::CodexSessionSummary;
use serde_json::{Value, json};

use farhelm_protocol::SessionDisplayRequest as DisplayRequest;

#[derive(Clone, Deserialize, Serialize)]
struct After {
    updated_at_unix: u64,
    agent_id: String,
    session_id: String,
}

impl After {
    fn key(&self) -> (std::cmp::Reverse<u64>, &str, &str) {
        (
            std::cmp::Reverse(self.updated_at_unix),
            &self.agent_id,
            &self.session_id,
        )
    }
}

#[derive(Deserialize, Serialize)]
struct Cursor {
    after: After,
    scope: String,
}

async fn relay(
    state: &AppState,
    agent: &str,
    params: Value,
    deadline: tokio::time::Instant,
) -> Result<Value, &'static str> {
    let request_id = format!("read_{}", &random_token()[..20]);
    let (sender, receiver) = oneshot::channel();
    let notify = {
        let mut broker = state.read_broker.lock().await;
        if broker.waiters.len() >= 128 {
            return Err("read_capacity_exceeded");
        }
        broker.waiters.insert(
            request_id.clone(),
            (agent.into(), sender, std::time::Instant::now()),
        );
        broker
            .queues
            .entry(agent.into())
            .or_default()
            .push_back(AgentReadRequest {
                request_id: request_id.clone(),
                method: "codex.session.display".into(),
                params,
            });
        broker
            .notifies
            .entry(agent.into())
            .or_insert_with(|| Arc::new(Notify::new()))
            .clone()
    };
    notify.notify_one();
    let result = tokio::time::timeout_at(deadline, receiver).await;
    // Also remove requests still queued after a timeout or an early Agent error.
    let mut broker = state.read_broker.lock().await;
    broker.waiters.remove(&request_id);
    if let Some(queue) = broker.queues.get_mut(agent) {
        queue.retain(|r| r.request_id != request_id);
    }
    match result {
        Ok(Ok(report)) if report.ok => report.data.ok_or("agent_read_empty"),
        Ok(Ok(_)) => Err("agent_read_failed"),
        _ => Err("agent_read_timeout"),
    }
}

pub(super) async fn display(
    State(state): State<AppState>,
    Json(request): Json<DisplayRequest>,
) -> Response {
    let limit = request.limit.unwrap_or(50);
    if !matches!(request.mode.as_str(), "labels" | "search")
        || !matches!(request.archived.as_str(), "false" | "true" | "all")
        || !(1..=50).contains(&limit)
        || request.session_ids.len() > 50
        || request
            .session_ids
            .iter()
            .any(|id| id.is_empty() || id.len() > 128)
        || request
            .query
            .as_ref()
            .is_some_and(|q| q.chars().count() > 256)
        || request
            .agent_id
            .as_ref()
            .is_some_and(|id| !valid_agent_id(id))
        || request
            .project_id
            .as_ref()
            .is_some_and(|id| !valid_project_id(id))
        || (request.mode == "labels"
            && (request.session_ids.is_empty() || request.cursor.is_some()))
        || (request.mode == "search" && !request.session_ids.is_empty())
    {
        return api_error(StatusCode::BAD_REQUEST, "invalid_display_request");
    }
    let scope = URL_SAFE_NO_PAD.encode(Sha256::digest(
        serde_json::to_vec(&json!([
            request.query,
            request.agent_id,
            request.project_id,
            request.archived
        ]))
        .unwrap_or_default(),
    ));
    let after = match &request.cursor {
        None => None,
        Some(cursor) => {
            let decoded = (cursor.len() <= 1024)
                .then(|| URL_SAFE_NO_PAD.decode(cursor).ok())
                .flatten()
                .and_then(|bytes| serde_json::from_slice::<Cursor>(&bytes).ok());
            match decoded {
                Some(cursor) if cursor.scope == scope => Some(cursor.after),
                _ => return api_error(StatusCode::BAD_REQUEST, "invalid_display_cursor"),
            }
        }
    };
    let requested = request.clone();
    let targets = match database(&state, move |s| {
        let mut targets: BTreeMap<String, Vec<String>> = BTreeMap::new();
        if requested.mode == "labels" {
            for id in requested.session_ids {
                if let Some(session) = s.events.session(&id)? {
                    targets.entry(session.agent_id).or_default().push(id);
                }
            }
        } else {
            for agent in s.events.session_agents(
                requested.agent_id.as_deref(),
                requested.project_id.as_deref(),
            )? {
                targets.insert(agent, vec![]);
            }
        }
        Ok(targets)
    })
    .await
    {
        Ok(value) => value,
        Err(_) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, "event_store_failed"),
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let permits = Arc::new(tokio::sync::Semaphore::new(8));
    let mut tasks = tokio::task::JoinSet::new();
    let mut incomplete = Vec::new();
    for (agent, ids) in targets {
        let agents = state.agents.read().await;
        let reason = match agents.get(&agent) {
            Some(value) if !is_online(unix_time(), value.last_seen_unix) => Some("agent_offline"),
            Some(value)
                if !value
                    .capabilities
                    .iter()
                    .any(|c| c == "codex.session_display") =>
            {
                Some("agent_upgrade_required")
            }
            Some(_) => None,
            None => Some("agent_offline"),
        };
        drop(agents);
        if let Some(reason) = reason {
            incomplete.push(json!({"agent_id":agent,"reason":reason}));
            continue;
        }
        let mut params = json!({"mode":request.mode,"query":request.query,"project_id":request.project_id,
            "archived":if request.mode == "labels" {"all"} else {&request.archived}, "after":after});
        if request.mode == "labels" {
            params["session_ids"] = json!(ids);
        }
        let state = state.clone();
        let permits = permits.clone();
        tasks.spawn(async move {
            let result = match tokio::time::timeout_at(deadline, permits.acquire_owned()).await {
                Ok(Ok(_permit)) => relay_imported(&state, &agent, params, deadline).await,
                _ => Err("agent_read_timeout"),
            };
            (agent, result)
        });
    }
    let mut candidates = Vec::new();
    while let Some(result) = tasks.join_next().await {
        if let Ok((agent, result)) = result {
            match result {
                Ok(data) => {
                    if let Some(rows) = data
                        .get("sessions")
                        .and_then(Value::as_array)
                        .filter(|rows| rows.len() <= 51)
                    {
                        candidates.extend(rows.iter().cloned().map(|row| (agent.clone(), row)));
                    } else {
                        incomplete.push(json!({"agent_id":agent,"reason":"agent_read_invalid"}));
                    }
                }
                Err(reason) => incomplete.push(json!({"agent_id":agent,"reason":reason})),
            }
        }
    }
    let requested = request.clone();
    let mut rows = match database(&state, move |s| {
        let mut rows = Vec::<(After, Value)>::new();
        for (agent, row) in candidates {
            let Some(id) = row.get("session_id").and_then(Value::as_str) else {
                continue;
            };
            let Some(session): Option<CodexSessionSummary> = s.events.session(id)? else {
                continue;
            };
            if session.agent_id != agent
                || requested
                    .project_id
                    .as_ref()
                    .is_some_and(|p| p != &session.project_id)
                || (requested.mode == "labels" && !requested.session_ids.iter().any(|s| s == id))
            {
                continue;
            }
            let updated = row
                .get("updated_at_unix")
                .and_then(Value::as_u64)
                .unwrap_or(session.updated_at_unix);
            let mut value = serde_json::to_value(session)?;
            value["display_label"] = row
                .get("display_label")
                .and_then(Value::as_str)
                .map(|label| {
                    Value::String(
                        label
                            .split_whitespace()
                            .collect::<Vec<_>>()
                            .join(" ")
                            .chars()
                            .take(80)
                            .collect(),
                    )
                })
                .unwrap_or(Value::Null);
            rows.push((
                After {
                    updated_at_unix: updated,
                    agent_id: agent,
                    session_id: id.into(),
                },
                value,
            ));
        }
        Ok(rows)
    })
    .await
    {
        Ok(value) => value,
        Err(_) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, "event_store_failed"),
    };
    rows.sort_by(|a, b| a.0.key().cmp(&b.0.key()));
    rows.dedup_by(|a, b| a.0.agent_id == b.0.agent_id && a.0.session_id == b.0.session_id);
    rows.retain(|row| after.as_ref().is_none_or(|after| row.0.key() > after.key()));
    let has_more = rows.len() > limit;
    rows.truncate(limit);
    let next = if has_more {
        rows.last().map(|row| {
            URL_SAFE_NO_PAD.encode(
                serde_json::to_vec(&Cursor {
                    after: row.0.clone(),
                    scope,
                })
                .unwrap_or_default(),
            )
        })
    } else {
        None
    };
    let sessions: Vec<_> = rows.into_iter().map(|(_, row)| row).collect();
    ([(header::CACHE_CONTROL, "no-store")], Json(json!({"protocol":FARHELM_PROTOCOL,"sessions":sessions,"next_cursor":next,"incomplete_agents":incomplete}))).into_response()
}

// Discovery can have local bindings whose metadata is still in the outbox.
// Walk past these rows within the same deadline instead of losing later matches.
async fn relay_imported(
    state: &AppState,
    agent: &str,
    mut params: Value,
    deadline: tokio::time::Instant,
) -> Result<Value, &'static str> {
    let mut imported = Vec::new();
    let mut seen = std::collections::HashSet::new();
    loop {
        let data = relay(state, agent, params.clone(), deadline).await?;
        let page = data
            .get("sessions")
            .and_then(Value::as_array)
            .filter(|rows| rows.len() <= 51)
            .ok_or("agent_read_invalid")?;
        let rows = page.clone();
        let owner = agent.to_owned();
        let approved = database(state, move |s| {
            let mut approved = Vec::new();
            for row in rows {
                if let Some(id) = row.get("session_id").and_then(Value::as_str)
                    && s.events
                        .session(id)?
                        .is_some_and(|session| session.agent_id == owner)
                {
                    approved.push(row);
                }
            }
            Ok(approved)
        })
        .await
        .map_err(|_| "event_store_failed")?;
        for row in approved {
            if seen.insert(row["session_id"].as_str().unwrap_or_default().to_owned()) {
                imported.push(row);
            }
        }
        if params["mode"] == "labels" || imported.len() >= 51 || page.len() < 51 {
            return Ok(json!({"sessions": imported.into_iter().take(51).collect::<Vec<_>>()}));
        }
        let last = page.last().ok_or("agent_read_invalid")?;
        let next = After {
            updated_at_unix: last["updated_at_unix"]
                .as_u64()
                .ok_or("agent_read_invalid")?,
            agent_id: agent.into(),
            session_id: last["session_id"]
                .as_str()
                .ok_or("agent_read_invalid")?
                .into(),
        };
        if let Ok(previous) = serde_json::from_value::<After>(params["after"].clone())
            && next.key() <= previous.key()
        {
            return Err("agent_read_invalid");
        }
        params["after"] = json!(next);
    }
}
