//! Account presentation preferences and authenticated, path-free Agent directory operations.
use super::*;
use farhelm_protocol::projects::*;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

pub(crate) fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS project_preferences(user TEXT NOT NULL,agent_id TEXT NOT NULL,project_id TEXT NOT NULL,display_name TEXT,hidden INTEGER NOT NULL,pinned INTEGER NOT NULL,manual_order INTEGER,PRIMARY KEY(user,agent_id,project_id));
        CREATE TABLE IF NOT EXISTS project_preference_revisions(user TEXT PRIMARY KEY,revision INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS project_sync_status(agent_id TEXT NOT NULL,project_id TEXT NOT NULL,state TEXT NOT NULL,revision INTEGER NOT NULL,PRIMARY KEY(agent_id,project_id));")?;
    Ok(())
}

impl EventStore {
    pub(crate) fn project_preferences(&self, user: &str) -> Result<ProjectPreferences> {
        let c = self.lock()?;
        let revision: u64 = c
            .query_row(
                "SELECT revision FROM project_preference_revisions WHERE user=?1",
                [user],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0)
            .try_into()?;
        let mut stmt=c.prepare("SELECT agent_id,project_id,display_name,hidden,pinned,manual_order FROM project_preferences WHERE user=?1 ORDER BY agent_id,project_id")?;
        let projects = stmt
            .query_map([user], |r| {
                Ok(ProjectPreference {
                    agent_id: r.get(0)?,
                    project_id: r.get(1)?,
                    display_name: r.get(2)?,
                    hidden: r.get(3)?,
                    pinned: r.get(4)?,
                    manual_order: r
                        .get::<_, Option<i64>>(5)?
                        .map(u32::try_from)
                        .transpose()
                        .map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                5,
                                rusqlite::types::Type::Integer,
                                Box::new(e),
                            )
                        })?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(ProjectPreferences { revision, projects })
    }

    fn save_project_preferences(
        &self,
        user: &str,
        key: &str,
        request: &UpdateProjectPreferences,
        now: u64,
    ) -> Result<ProjectPreferences> {
        let fingerprint = serde_json::to_string(request)?;
        let operation = format!("projects:{user}:{key}");
        let c = self.lock()?;
        let tx = crate::migrations::write_transaction(&c)?;
        if let Some((old, response)) = tx
            .query_row(
                "SELECT fingerprint,response_json FROM browser_operations WHERE operation_key=?1",
                [&operation],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?
        {
            ensure!(old == fingerprint, "idempotency_conflict");
            return Ok(serde_json::from_str(&response)?);
        }
        let revision: u64 = tx
            .query_row(
                "SELECT revision FROM project_preference_revisions WHERE user=?1",
                [user],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0)
            .try_into()?;
        ensure!(revision == request.revision, "project_preferences_conflict");
        ensure!(
            !request.projects.is_empty() && request.projects.len() <= 1000,
            "invalid_project_preferences"
        );
        let mut seen = std::collections::HashSet::new();
        for p in &request.projects {
            ensure!(
                seen.insert((&p.agent_id, &p.project_id))
                    && valid_agent_id(&p.agent_id)
                    && valid_project_id(&p.project_id)
                    && p.display_name
                        .as_deref()
                        .is_none_or(farhelm_protocol::valid_session_name),
                "invalid_project_preferences"
            );
            let approved:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM project_candidates WHERE agent_id=?1 AND suggested_project_id=?2 AND state='approved')",params![p.agent_id,p.project_id],|r|r.get(0))?;
            ensure!(approved, "project_not_approved");
            tx.execute("INSERT INTO project_preferences(user,agent_id,project_id,display_name,hidden,pinned,manual_order) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(user,agent_id,project_id) DO UPDATE SET display_name=excluded.display_name,hidden=excluded.hidden,pinned=excluded.pinned,manual_order=excluded.manual_order",params![user,p.agent_id,p.project_id,p.display_name,p.hidden,p.pinned,p.manual_order])?;
        }
        tx.execute("INSERT INTO project_preference_revisions VALUES(?1,?2) ON CONFLICT(user) DO UPDATE SET revision=excluded.revision",params![user,i64::try_from(revision+1)?])?;
        let mut stmt=tx.prepare("SELECT agent_id,project_id,display_name,hidden,pinned,manual_order FROM project_preferences WHERE user=?1 ORDER BY agent_id,project_id")?;
        let projects = stmt
            .query_map([user], |r| {
                Ok(ProjectPreference {
                    agent_id: r.get(0)?,
                    project_id: r.get(1)?,
                    display_name: r.get(2)?,
                    hidden: r.get(3)?,
                    pinned: r.get(4)?,
                    manual_order: r
                        .get::<_, Option<i64>>(5)?
                        .map(u32::try_from)
                        .transpose()
                        .map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                5,
                                rusqlite::types::Type::Integer,
                                Box::new(e),
                            )
                        })?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        drop(stmt);
        let response = ProjectPreferences {
            revision: revision + 1,
            projects,
        };
        tx.execute(
            "INSERT INTO browser_operations VALUES(?1,?2,?3,?4)",
            params![
                operation,
                fingerprint,
                serde_json::to_string(&response)?,
                i64::try_from(now)?
            ],
        )?;
        event_store::notifications::audit(&tx, "project.preferences", user, "saved", now)?;
        tx.commit()?;
        Ok(response)
    }

    pub(crate) fn project_catalog(&self) -> Result<Value> {
        let mut result = serde_json::to_value(self.projects()?)?;
        let c = self.lock()?;
        if let Some(projects) = result["projects"].as_array_mut() {
            for p in projects {
                let agent = p["agent_id"].as_str().unwrap_or("");
                let project = p["suggested_project_id"].as_str().unwrap_or("");
                let sync: Option<String> = c
                    .query_row(
                        "SELECT state FROM project_sync_status WHERE agent_id=?1 AND project_id=?2",
                        params![agent, project],
                        |r| r.get(0),
                    )
                    .optional()?;
                let activity:i64=c.query_row("SELECT COALESCE(MAX(updated_at_unix),0) FROM codex_sessions WHERE agent_id=?1 AND project_id=?2",params![agent,project],|r|r.get(0))?;
                p["sync_state"] = json!(sync.unwrap_or_else(|| "unknown".into()));
                p["last_activity_unix"] = json!(activity);
            }
        }
        Ok(result)
    }

    fn has_project(&self, agent: &str, project: &str) -> Result<bool> {
        Ok(self.lock()?.query_row("SELECT EXISTS(SELECT 1 FROM project_candidates WHERE agent_id=?1 AND suggested_project_id=?2 AND state='approved')",params![agent,project],|r|r.get(0))?)
    }
}

pub(super) async fn list(State(state): State<AppState>) -> Response {
    match database(&state, |s| s.events.project_catalog()).await {
        Ok(v) => Json(v).into_response(),
        Err(_) => api_error(StatusCode::INTERNAL_SERVER_ERROR, "event_store_failed"),
    }
}

pub(super) async fn preferences(State(state): State<AppState>) -> Response {
    match database(&state, |s| {
        s.events.project_preferences(&s.config.admin_user)
    })
    .await
    {
        Ok(v) => Json(v).into_response(),
        Err(_) => api_error(StatusCode::INTERNAL_SERVER_ERROR, "event_store_failed"),
    }
}

pub(super) async fn save_preferences(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<UpdateProjectPreferences>,
) -> Response {
    let Some(key) = idempotency_header(&headers).map(str::to_owned) else {
        return api_error(StatusCode::BAD_REQUEST, "missing_idempotency_key");
    };
    match database(&state, move |s| {
        s.events
            .save_project_preferences(&s.config.admin_user, &key, &request, unix_time())
    })
    .await
    {
        Ok(result) => {
            let _ = state.event_bus.send(StoredEvent {
                sequence: 0,
                event_id: format!("project-preferences:{}", result.revision),
                event_type: "project.preferences.updated".into(),
                payload: json!({"revision":result.revision}),
            });
            Json(result).into_response()
        }
        Err(error) => match error.to_string().as_str() {
            "project_preferences_conflict" => {
                api_error(StatusCode::CONFLICT, "project_preferences_conflict")
            }
            "idempotency_conflict" => api_error(StatusCode::CONFLICT, "idempotency_conflict"),
            "project_not_approved" => api_error(StatusCode::BAD_REQUEST, "project_not_approved"),
            "invalid_project_preferences" => {
                api_error(StatusCode::BAD_REQUEST, "invalid_project_preferences")
            }
            _ => api_error(StatusCode::INTERNAL_SERVER_ERROR, "event_store_failed"),
        },
    }
}

pub(super) async fn capability(
    state: &AppState,
    agent: &str,
    capability: &str,
) -> Result<(), (StatusCode, &'static str)> {
    let agents = state.agents.read().await;
    let Some(agent) = agents
        .get(agent)
        .filter(|a| is_online(unix_time(), a.last_seen_unix))
    else {
        return Err((StatusCode::SERVICE_UNAVAILABLE, "agent_offline"));
    };
    if !agent.capabilities.iter().any(|c| c == capability) {
        return Err((StatusCode::CONFLICT, "agent_upgrade_required"));
    }
    Ok(())
}

async fn read<T: serde::de::DeserializeOwned + Serialize>(
    state: &AppState,
    agent: &str,
    method: &str,
    params: Value,
) -> Response {
    if let Err((status, code)) = capability(state, agent, "project.management").await {
        return api_error(status, code);
    }
    match session_display::relay_method(
        state,
        agent,
        method,
        params,
        tokio::time::Instant::now() + Duration::from_secs(20),
    )
    .await
    {
        Ok(v) => match serde_json::from_value::<T>(v) {
            Ok(v) => ([(header::CACHE_CONTROL, "no-store")], Json(v)).into_response(),
            Err(_) => api_error(StatusCode::BAD_GATEWAY, "agent_read_invalid"),
        },
        Err(code) => api_error(StatusCode::BAD_GATEWAY, code),
    }
}

pub(super) async fn roots(State(state): State<AppState>, Path(agent): Path<String>) -> Response {
    read::<ProjectRoots>(&state, &agent, "project.roots", json!({})).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DirectoryQuery {
    directory_id: String,
    cursor: Option<String>,
}

pub(super) async fn directories(
    State(state): State<AppState>,
    Path(agent): Path<String>,
    Query(query): Query<DirectoryQuery>,
) -> Response {
    if !valid_directory_id(&query.directory_id)
        || query
            .cursor
            .as_deref()
            .is_some_and(|c| !valid_directory_id(c))
    {
        return api_error(StatusCode::BAD_REQUEST, "project_directory_expired");
    }
    read::<DirectoryPage>(
        &state,
        &agent,
        "project.directories",
        json!({"directory_id":query.directory_id,"cursor":query.cursor}),
    )
    .await
}

pub(super) async fn info(
    State(state): State<AppState>,
    Path((agent, project)): Path<(String, String)>,
) -> Response {
    let a = agent.clone();
    let p = project.clone();
    if !matches!(
        database(&state, move |s| s.events.has_project(&a, &p)).await,
        Ok(true)
    ) {
        return api_error(StatusCode::NOT_FOUND, "project_not_approved");
    }
    read::<ProjectInfo>(
        &state,
        &agent,
        "project.info",
        json!({"project_id":project}),
    )
    .await
}

pub(super) async fn add(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<AddProjectRequest>,
) -> Response {
    if !valid_agent_id(request.agent_id())
        || !valid_directory_id(request.directory_id())
        || matches!(&request,AddProjectRequest::Create{name,..} if !valid_directory_name(name))
        || matches!(&request,AddProjectRequest::AttachTo{project_id,..} if !valid_project_id(project_id))
    {
        return api_error(StatusCode::BAD_REQUEST, "invalid_project_request");
    }
    let agent = request.agent_id().to_owned();
    if let Err((status, code)) = capability(&state, &agent, "project.management").await {
        return api_error(status, code);
    }
    let Some(key) = idempotency_header(&headers) else {
        return api_error(StatusCode::BAD_REQUEST, "missing_idempotency_key");
    };
    let Ok(mut payload) = serde_json::to_value(request) else {
        return api_error(StatusCode::BAD_REQUEST, "invalid_project_request");
    };
    payload
        .as_object_mut()
        .expect("project request")
        .remove("agent_id");
    create_typed_response(&state, &agent, CommandAction::ProjectAdd, payload, key, 120).await
}

pub(super) async fn sync(
    State(state): State<AppState>,
    Path((agent, project)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let a = agent.clone();
    let p = project.clone();
    if !matches!(
        database(&state, move |s| s.events.has_project(&a, &p)).await,
        Ok(true)
    ) {
        return api_error(StatusCode::NOT_FOUND, "project_not_approved");
    }
    if let Err((status, code)) = capability(&state, &agent, "project.management").await {
        return api_error(status, code);
    }
    let Some(key) = idempotency_header(&headers) else {
        return api_error(StatusCode::BAD_REQUEST, "missing_idempotency_key");
    };
    create_typed_response(
        &state,
        &agent,
        CommandAction::ProjectSync,
        json!({"project_id":project}),
        key,
        120,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{browser_cookie, test_state};
    use axum::body::to_bytes;
    use farhelm_protocol::AgentEvent;
    use tower::ServiceExt;
    fn project(state: &AppState, agent: &str, id: &str) {
        state.events.ingest(agent,&[AgentEvent{protocol:FARHELM_PROTOCOL.into(),event_id:format!("{agent}-{id}"),agent_id:agent.into(),sequence:1,event_type:"project.updated".into(),created_at_unix:1,payload:json!({"candidate_id":format!("{agent}-{id}"),"suggested_project_id":id,"display_name":"Shared","session_count":0,"state":"approved","sync_state":"pending","updated_at_unix":1})}]).unwrap();
    }
    fn pref(agent: &str, hidden: bool) -> ProjectPreference {
        ProjectPreference {
            agent_id: agent.into(),
            project_id: "p".into(),
            hidden,
            pinned: true,
            manual_order: None,
            display_name: Some("Account name".into()),
        }
    }

    #[test]
    fn preferences_are_account_scoped_atomic_cas_and_survive_agent_scan() {
        let state = test_state();
        project(&state, "a", "p");
        project(&state, "b", "p");
        assert_eq!(
            state.events.project_catalog().unwrap()["projects"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        let request = UpdateProjectPreferences {
            revision: 0,
            projects: vec![pref("a", true)],
        };
        let saved = state
            .events
            .save_project_preferences("alice", "one", &request, 3)
            .unwrap();
        assert_eq!(saved.revision, 1);
        assert_eq!(
            state
                .events
                .save_project_preferences("alice", "one", &request, 4)
                .unwrap(),
            saved
        );
        assert_eq!(
            state
                .events
                .save_project_preferences("alice", "two", &request, 4)
                .unwrap_err()
                .to_string(),
            "project_preferences_conflict"
        );
        assert!(
            state
                .events
                .project_preferences("bob")
                .unwrap()
                .projects
                .is_empty()
        );
        let mut updated = request;
        updated.revision = 1;
        updated.projects.push(pref("not-approved", false));
        assert!(
            state
                .events
                .save_project_preferences("alice", "invalid", &updated, 4)
                .is_err()
        );
        assert_eq!(state.events.project_preferences("alice").unwrap(), saved);
        project(&state, "a", "p");
        assert_eq!(state.events.project_preferences("alice").unwrap(), saved);
        // A discovery snapshot can finish after approval. It cannot revoke approval,
        // or overwrite a newer sync result just because its wall clock is later.
        state.events.ingest("a", &[
            AgentEvent { protocol:FARHELM_PROTOCOL.into(), agent_id:"a".into(), event_id:"sync-ready".into(), sequence:3, event_type:"project.sync.updated".into(), created_at_unix:5, payload:json!({"suggested_project_id":"p","sync_state":"ready"}) },
            AgentEvent { protocol:FARHELM_PROTOCOL.into(), agent_id:"a".into(), event_id:"late-discovery".into(), sequence:2, event_type:"project.discovered".into(), created_at_unix:6, payload:json!({"candidate_id":"a-p","suggested_project_id":"p","display_name":"Scanned name","session_count":3,"state":"discovered","sync_state":"pending","updated_at_unix":6}) },
        ]).unwrap();
        let catalog = state.events.project_catalog().unwrap();
        let row = catalog["projects"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["agent_id"] == "a")
            .unwrap();
        assert_eq!(row["state"], "approved");
        assert_eq!(row["sync_state"], "ready");
        assert_eq!(state.events.project_preferences("alice").unwrap(), saved);
    }

    #[tokio::test]
    async fn visibility_filters_before_pagination_and_cursor_rejects_changed_preferences() {
        let state = test_state();
        project(&state, "a", "p");
        project(&state, "b", "p");
        for i in 0..125 {
            let agent = if i < 60 { "a" } else { "b" };
            state.events.ingest(agent,&[AgentEvent{protocol:FARHELM_PROTOCOL.into(),event_id:format!("s{i}"),agent_id:agent.into(),sequence:i+2,event_type:"codex.session.updated".into(),created_at_unix:i+2,payload:json!({"session_id":format!("s{i}"),"project_id":"p","mode":"inspect","state":"idle","title":null,"updated_at_unix":i+2})}]).unwrap();
        }
        state
            .events
            .save_project_preferences(
                &state.config.admin_user,
                "one",
                &UpdateProjectPreferences {
                    revision: 0,
                    projects: vec![pref("b", true)],
                },
                200,
            )
            .unwrap();
        let get = |uri: String| {
            Request::builder()
                .uri(uri)
                .header(header::COOKIE, browser_cookie())
                .body(Body::empty())
                .unwrap()
        };
        let response = app(state.clone())
            .oneshot(get(
                "/api/v1/codex/sessions?visible_only=true&limit=50".into()
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let page: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
                .unwrap();
        assert_eq!(page["sessions"].as_array().unwrap().len(), 50);
        assert!(
            page["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .all(|s| s["agent_id"] == "a")
        );
        let cursor = page["next_cursor"].as_str().unwrap();
        let uri = format!("/api/v1/codex/sessions?visible_only=true&limit=50&cursor={cursor}");
        let second = app(state.clone()).oneshot(get(uri.clone())).await.unwrap();
        let page: Value =
            serde_json::from_slice(&to_bytes(second.into_body(), 1024 * 1024).await.unwrap())
                .unwrap();
        assert_eq!(page["sessions"].as_array().unwrap().len(), 10);
        state
            .events
            .save_project_preferences(
                &state.config.admin_user,
                "two",
                &UpdateProjectPreferences {
                    revision: 1,
                    projects: vec![pref("b", false)],
                },
                201,
            )
            .unwrap();
        let conflict = app(state.clone()).oneshot(get(uri)).await.unwrap();
        assert_eq!(conflict.status(), StatusCode::CONFLICT);
        let legacy = state
            .events
            .sessions_visible(None, ArchiveFilter::All, None, 0, 50, None)
            .unwrap();
        assert_eq!(legacy.sessions.len(), 50);
        assert_eq!(legacy.sessions[0].agent_id, "b");
    }
    #[test]
    fn migration_keeps_catalog_sessions_events_and_receipts_and_rejects_future_schema() {
        let c = Connection::open_in_memory().unwrap();
        crate::migrations::apply(&c).unwrap();
        c.execute_batch("DROP TABLE project_preferences;DROP TABLE project_preference_revisions;DROP TABLE project_sync_status;PRAGMA user_version=7;").unwrap();
        // A real schema-7 command CHECK, including a completed receipt and an
        // outstanding command: migration must rebuild this table without renumbering.
        c.execute_batch("DROP TABLE typed_commands;
            CREATE TABLE typed_commands (
                id INTEGER PRIMARY KEY AUTOINCREMENT, command_id TEXT NOT NULL UNIQUE,
                agent_id TEXT NOT NULL, action TEXT NOT NULL CHECK(action IN ('codex.session.create','codex.session.resume','codex.turn.start','codex.turn.steer','codex.turn.interrupt','codex.schedule.create','codex.schedule.cancel','project.approve')),
                payload_json TEXT NOT NULL, state TEXT NOT NULL CHECK(state IN ('queued','delivered','accepted','completed','failed','expired')),
                idempotency_key TEXT NOT NULL UNIQUE, created_at_unix INTEGER NOT NULL, expires_at_unix INTEGER NOT NULL, updated_at_unix INTEGER NOT NULL,
                data_json TEXT, detail TEXT);
            INSERT INTO typed_commands VALUES(42,'old-command','a','project.approve','{}','completed','old-operation',1,100,2,'{\"approved\":[\"candidate\"]}',NULL);
            INSERT INTO typed_commands VALUES(43,'pending-command','a','codex.session.resume','{}','accepted','pending-operation',1,100,2,NULL,NULL);
            INSERT INTO project_candidates VALUES('candidate','a','Project','p',1,'approved',1);
            INSERT INTO codex_sessions(session_id,agent_id,project_id,mode,state,updated_at_unix) VALUES('old-session','a','p','inspect','archived',1);
            INSERT INTO agent_events(event_id,agent_id,agent_sequence,event_type,payload_json,created_at_unix) VALUES('old-event','a',1,'project.updated','{}',1);").unwrap();
        crate::migrations::apply(&c).unwrap();
        assert_eq!(
            c.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                .unwrap(),
            9
        );
        let receipt: (i64, String, String) = c
            .query_row(
                "SELECT id,state,data_json FROM typed_commands WHERE command_id='old-command'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            receipt,
            (
                42,
                "completed".into(),
                "{\"approved\":[\"candidate\"]}".into()
            )
        );
        assert_eq!(
            c.query_row(
                "SELECT state FROM typed_commands WHERE command_id='pending-command'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "accepted"
        );
        for (table, column, value) in [
            ("project_candidates", "candidate_id", "candidate"),
            ("codex_sessions", "session_id", "old-session"),
            ("agent_events", "event_id", "old-event"),
        ] {
            assert_eq!(
                c.query_row(
                    &format!("SELECT count(*) FROM {table} WHERE {column}=?1"),
                    [value],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
                1
            );
        }
        c.execute("INSERT INTO typed_commands(command_id,agent_id,action,payload_json,state,idempotency_key,created_at_unix,expires_at_unix,updated_at_unix) VALUES('new-command','a','project.add','{}','queued','new-operation',3,100,3)",[]).unwrap();
        assert!(c.last_insert_rowid() > 43);
        c.pragma_update(None, "user_version", 10).unwrap();
        assert!(crate::migrations::apply(&c).is_err());
    }
}
