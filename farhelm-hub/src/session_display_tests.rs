use super::tests::{browser_cookie, test_state};
use super::*;
use axum::body::to_bytes;
use farhelm_protocol::AgentEvent;
use serde_json::{Value, json};
use tower::ServiceExt;

fn session_event(id: &str, sequence: u64, state: &str, title: Value, kind: &str) -> AgentEvent {
    AgentEvent {
        protocol: FARHELM_PROTOCOL.into(),
        event_id: format!("{id}-{sequence}"),
        agent_id: "gpu-a".into(),
        sequence,
        event_type: "codex.session.updated".into(),
        created_at_unix: 100,
        payload: json!({"session_id":id,"project_id":"cc08","mode":if kind=="metadata" {"inspect"}else{"edit"},"state":state,"title":title,"update_kind":kind,"active_turn_id":if state=="running"{Some("turn-a")}else{None},"updated_at_unix":100}),
    }
}

fn display_request(body: Value, csrf: bool) -> Request {
    let builder = Request::builder()
        .method("POST")
        .uri("/api/v1/codex/session-display")
        .header(header::COOKIE, browser_cookie())
        .header(header::CONTENT_TYPE, "application/json");
    let builder = if csrf {
        builder.header("x-csrf-token", "test-csrf")
    } else {
        builder
    };
    builder.body(Body::from(body.to_string())).unwrap()
}

#[test]
fn same_second_metadata_and_late_execution_cannot_erase_title_mode_or_running_state() {
    let state = test_state();
    state
        .events
        .ingest(
            "gpu-a",
            &[session_event(
                "s",
                1,
                "idle",
                json!("正式名称"),
                "execution",
            )],
        )
        .unwrap();
    state
        .events
        .ingest(
            "gpu-a",
            &[session_event("s", 3, "running", Value::Null, "execution")],
        )
        .unwrap();
    state
        .events
        .ingest(
            "gpu-a",
            &[session_event(
                "s",
                4,
                "idle",
                json!("Codex session"),
                "metadata",
            )],
        )
        .unwrap();
    state
        .events
        .ingest(
            "gpu-a",
            &[session_event("s", 2, "idle", Value::Null, "execution")],
        )
        .unwrap();
    let session = state.events.session("s").unwrap().unwrap();
    assert_eq!(session.title.as_deref(), Some("正式名称"));
    assert_eq!(serde_json::to_value(&session).unwrap()["mode"], "edit");
    assert_eq!(serde_json::to_value(&session).unwrap()["state"], "running");
    assert_eq!(session.active_turn_id.as_deref(), Some("turn-a"));
    state
        .events
        .ingest(
            "gpu-a",
            &[session_event("s", 5, "idle", Value::Null, "execution")],
        )
        .unwrap();
    assert_eq!(
        serde_json::to_value(state.events.session("s").unwrap()).unwrap()["state"],
        "idle"
    );
}

#[tokio::test]
async fn display_requires_login_csrf_and_bounded_requests() {
    let state = test_state();
    let body = json!({"mode":"labels","session_ids":["s"]});
    let response = app(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/codex/session-display")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        app(state.clone())
            .oneshot(display_request(body, false))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let body = json!({"mode":"labels","session_ids":vec!["s";51]});
    assert_eq!(
        app(state)
            .oneshot(display_request(body, true))
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn search_uses_transient_agent_index_and_returns_partial_results_without_storing_labels() {
    let state = test_state();
    for i in 0..1000 {
        state
            .events
            .ingest(
                "gpu-a",
                &[session_event(
                    &format!("s-{i}"),
                    i + 1,
                    "idle",
                    Value::Null,
                    "metadata",
                )],
            )
            .unwrap();
    }
    state.agents.write().await.insert(
        "gpu-a".into(),
        StoredAgent {
            capabilities: vec!["codex.session_display".into()],
            hostname: "test".into(),
            agent_version: "0.7.1".into(),
            last_seen_unix: unix_time(),
            credential_state: AgentCredentialState::Paired,
        },
    );
    let mut offline = session_event("offline", 2000, "idle", Value::Null, "metadata");
    offline.agent_id = "gpu-b".into();
    state.events.ingest("gpu-b", &[offline]).unwrap();
    let agent_state = state.clone();
    let task = tokio::spawn(async move {
        loop {
            let request = agent_state
                .read_broker
                .lock()
                .await
                .queues
                .get_mut("gpu-a")
                .and_then(VecDeque::pop_front);
            if let Some(request) = request {
                assert_eq!(request.method, "codex.session.display");
                assert_eq!(request.params["query"], "PRIVATE_SEARCH_071");
                let (_, sender, _) = agent_state
                    .read_broker
                    .lock()
                    .await
                    .waiters
                    .remove(&request.request_id)
                    .unwrap();
                sender.send(AgentReadReportRequest {protocol:FARHELM_PROTOCOL.into(),agent_id:"gpu-a".into(),request_id:request.request_id,ok:true,detail:None,
                    data:Some(json!({"sessions":[{"session_id":"s-999","display_label":"PRIVATE_PREVIEW_071\n训练🙂", "cwd":"/private/never-expose","updated_at_unix":100}]}))}).unwrap();
                break;
            }
            tokio::task::yield_now().await;
        }
    });
    let response = app(state.clone())
        .oneshot(display_request(
            json!({"mode":"search","query":"PRIVATE_SEARCH_071","archived":"all"}),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let data: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 512 * 1024).await.unwrap()).unwrap();
    assert_eq!(data["sessions"][0]["session_id"], "s-999");
    assert_eq!(
        data["sessions"][0]["display_label"],
        "PRIVATE_PREVIEW_071 训练🙂"
    );
    assert!(data["sessions"][0].get("cwd").is_none());
    assert_eq!(data["incomplete_agents"][0]["agent_id"], "gpu-b");
    task.await.unwrap();
    assert!(
        state
            .events
            .session("s-999")
            .unwrap()
            .unwrap()
            .title
            .is_none()
    );
    for suffix in ["", "-wal"] {
        if let Ok(bytes) =
            std::fs::read(format!("{}{suffix}", state.config.database_path.display()))
        {
            let bytes = String::from_utf8_lossy(&bytes);
            assert!(
                !bytes.contains("PRIVATE_SEARCH_071") && !bytes.contains("PRIVATE_PREVIEW_071")
            );
        }
    }
    assert!(state.read_broker.lock().await.waiters.is_empty());
}

#[tokio::test]
async fn stale_visible_turn_is_rejected_before_steer_submission() {
    let state = test_state();
    state
        .events
        .ingest(
            "gpu-a",
            &[session_event("s", 1, "running", Value::Null, "execution")],
        )
        .unwrap();
    let response = app(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/codex/sessions/s/messages")
                .header(header::COOKIE, browser_cookie())
                .header("x-csrf-token", "test-csrf")
                .header("idempotency-key", "steer-test-key-0001")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"prompt":"continue","delivery":"steer","turn_id":"old"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let data: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
    assert_eq!(data["error"], "visible_turn_changed");
}

#[tokio::test]
async fn a_batch_broadcasts_item_deltas_before_its_terminal_and_deduplicates_terminal_retries() {
    let state = test_state();
    let mut events = state.event_bus.subscribe();
    let delta = AgentEvent {
        protocol: FARHELM_PROTOCOL.into(),
        agent_id: "gpu-a".into(),
        event_id: "delta-071".into(),
        sequence: 1,
        created_at_unix: 100,
        event_type: "codex.message.delta".into(),
        payload: json!({"session_id":"s","data":{"turn_id":"t","item_id":"i","text_offset":0,"delta":"PRIVATE_DELTA_071","cwd":"/never"}}),
    };
    let terminal = AgentEvent {
        event_id: "terminal-071".into(),
        sequence: 2,
        event_type: "codex.turn.completed".into(),
        payload: json!({"session_id":"s","operation_id":"op","data":{"turn_id":"t","status":"completed","delta":"PRIVATE_TERMINAL_071"}}),
        ..delta.clone()
    };
    let batch = AgentEventBatch {
        protocol: FARHELM_PROTOCOL.into(),
        agent_id: "gpu-a".into(),
        events: vec![delta, terminal],
    };
    let response = agent_events(
        State(state.clone()),
        Extension(AgentIdentity::Dedicated("gpu-a".into())),
        Json(batch.clone()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let first = events.try_recv().unwrap();
    let last = events.try_recv().unwrap();
    assert_eq!(first.event_type, "codex.message.delta");
    assert_eq!(first.sequence, 0);
    assert_eq!(first.payload["data"]["text_offset"], 0);
    assert!(first.payload["data"].get("cwd").is_none());
    assert_eq!(last.event_type, "codex.turn.completed");
    assert!(last.sequence > 0);
    assert!(last.payload["data"].get("delta").is_none());
    let response = agent_events(
        State(state.clone()),
        Extension(AgentIdentity::Dedicated("gpu-a".into())),
        Json(batch),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(events.try_recv().unwrap().event_type, "codex.message.delta");
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn search_skips_not_yet_imported_rows_and_keeps_stable_page_scope() {
    let state = test_state();
    state
        .events
        .ingest(
            "gpu-a",
            &[
                session_event("z-1", 1, "idle", Value::Null, "metadata"),
                session_event("z-2", 2, "idle", Value::Null, "metadata"),
            ],
        )
        .unwrap();
    state.agents.write().await.insert(
        "gpu-a".into(),
        StoredAgent {
            hostname: "test".into(),
            agent_version: "0.7.1".into(),
            capabilities: vec!["codex.session_display".into()],
            last_seen_unix: unix_time(),
            credential_state: AgentCredentialState::Paired,
        },
    );
    let agent = state.clone();
    let worker = tokio::spawn(async move {
        for page in 0..3 {
            let request = loop {
                if let Some(request) = agent
                    .read_broker
                    .lock()
                    .await
                    .queues
                    .get_mut("gpu-a")
                    .and_then(VecDeque::pop_front)
                {
                    break request;
                }
                tokio::task::yield_now().await;
            };
            let rows = match page {
                0 => (0..51).map(|i| json!({"session_id":format!("a-{i:02}"),"updated_at_unix":100,"display_label":"not imported"})).collect::<Vec<_>>(),
                1 => {
                    assert_eq!(request.params["after"]["session_id"], "a-50");
                    vec![json!({"session_id":"z-1","updated_at_unix":100,"display_label":"One"}),json!({"session_id":"z-2","updated_at_unix":100,"display_label":"Two"})]
                },
                _ => {
                    assert_eq!(request.params["after"]["session_id"], "z-1");
                    vec![json!({"session_id":"z-2","updated_at_unix":100,"display_label":"Two"})]
                },
            };
            let (_, sender, _) = agent
                .read_broker
                .lock()
                .await
                .waiters
                .remove(&request.request_id)
                .unwrap();
            sender
                .send(AgentReadReportRequest {
                    protocol: FARHELM_PROTOCOL.into(),
                    agent_id: "gpu-a".into(),
                    request_id: request.request_id,
                    ok: true,
                    detail: None,
                    data: Some(json!({"sessions":rows})),
                })
                .unwrap();
        }
    });
    let router = app(state);
    let response = router
        .clone()
        .oneshot(display_request(
            json!({"mode":"search","query":"match","limit":1}),
            true,
        ))
        .await
        .unwrap();
    let page: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    assert_eq!(page["sessions"][0]["session_id"], "z-1");
    let cursor = page["next_cursor"].as_str().unwrap();
    let invalid = router
        .clone()
        .oneshot(display_request(
            json!({"mode":"search","query":"different","cursor":cursor,"limit":1}),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    let response = router
        .oneshot(display_request(
            json!({"mode":"search","query":"match","cursor":cursor,"limit":1}),
            true,
        ))
        .await
        .unwrap();
    let page: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    assert_eq!(page["sessions"][0]["session_id"], "z-2");
    assert!(page["next_cursor"].is_null());
    worker.await.unwrap();
}
