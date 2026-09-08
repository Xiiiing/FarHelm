use super::tests::{browser_cookie, test_state};
use super::*;
use axum::body::to_bytes;
use serde_json::{Value, json};
use tower::ServiceExt;

async fn ready() -> AppState {
    let state = test_state();
    state.agents.write().await.insert(
        "gpu-a".into(),
        StoredAgent {
            hostname: "gpu-a".into(),
            agent_version: PRODUCT_VERSION.into(),
            last_seen_unix: unix_time(),
            codex: None,
            credential_state: AgentCredentialState::Paired,
            capabilities: AgentHeartbeat::new("gpu-a", "gpu-a", PRODUCT_VERSION).capabilities,
        },
    );
    state.events.ingest("gpu-a", &[farhelm_protocol::AgentEvent {protocol:FARHELM_PROTOCOL.into(),agent_id:"gpu-a".into(),sequence:1,event_id:"settings-session".into(),event_type:"codex.session.updated".into(),created_at_unix:unix_time(),payload:json!({"session_id":"s","project_id":"cc08","mode":"inspect","state":"running","title":"Original","active_turn_id":"active","updated_at_unix":unix_time()})}]).unwrap();
    state
}

fn request(suffix: &str, body: Option<Value>, csrf: bool) -> Request {
    let mut builder = Request::builder()
        .uri(format!("/api/v1/codex/sessions/s/{suffix}"))
        .header(header::COOKIE, browser_cookie());
    if csrf {
        builder = builder.header("x-csrf-token", "test-csrf");
    }
    if body.is_some() {
        builder = builder
            .method("POST")
            .header(header::CONTENT_TYPE, "application/json")
            .header("idempotency-key", "settings-operation-0001");
    }
    builder
        .body(body.map_or_else(Body::empty, |v| Body::from(v.to_string())))
        .unwrap()
}

#[tokio::test]
async fn native_reads_are_authenticated_scoped_and_project_only_whitelisted_fields() {
    for (suffix, method, data) in [
        (
            "models",
            "codex.models.list",
            json!({"models":[{"model":"model-a","display_name":"Model A","reasoning_efforts":["high"],"default_reasoning_effort":"high","is_default":true,"credential":"PRIVATE_CATALOG"}],"cwd":"PRIVATE_PATH"}),
        ),
        (
            "native",
            "codex.session.native",
            json!({"session_id":"s","persisted":true,"native_name":"Original","source":"vscode","history_mode":"paginated","cwd":"PRIVATE_PATH","preview":"PRIVATE_PREVIEW"}),
        ),
    ] {
        let state = ready().await;
        let router = app(state.clone());
        assert_eq!(
            router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/api/v1/codex/sessions/s/{suffix}"))
                        .body(Body::empty())
                        .unwrap()
                )
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        let read = tokio::spawn(router.oneshot(request(suffix, None, false)));
        let queued = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(r) = state
                    .read_broker
                    .lock()
                    .await
                    .queues
                    .get_mut("gpu-a")
                    .and_then(|q| q.pop_front())
                {
                    break r;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(queued.method, method);
        assert_eq!(queued.params, json!({"session_id":"s"}));
        let report = AgentReadReportRequest {
            protocol: FARHELM_PROTOCOL.into(),
            agent_id: "gpu-a".into(),
            request_id: queued.request_id.clone(),
            ok: true,
            data: Some(data),
            detail: None,
        };
        assert_eq!(
            report_agent_read(
                State(state.clone()),
                Extension(AgentIdentity::Dedicated("gpu-b".into())),
                Path(queued.request_id.clone()),
                Json(report.clone())
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
        report_agent_read(
            State(state.clone()),
            Extension(AgentIdentity::Dedicated("gpu-a".into())),
            Path(queued.request_id),
            Json(report),
        )
        .await;
        let response = read.await.unwrap().unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let text = String::from_utf8(
            to_bytes(response.into_body(), 16384)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(!text.contains("PRIVATE"));
        assert!(state.read_broker.lock().await.waiters.is_empty());
        for suffix in ["", "-wal"] {
            if let Ok(bytes) =
                std::fs::read(format!("{}{suffix}", state.config.database_path.display()))
            {
                assert!(!String::from_utf8_lossy(&bytes).contains("PRIVATE"));
            }
        }
    }
}

#[tokio::test]
async fn rename_waits_for_native_execution_and_obeys_csrf_capability_and_identity() {
    let state = ready().await;
    let router = app(state.clone());
    assert_eq!(
        router
            .clone()
            .oneshot(request("name", Some(json!({"name":"New name"})), false))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        router
            .clone()
            .oneshot(request("name", Some(json!({"name":"/private/path"})), true))
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    let first = router
        .clone()
        .oneshot(request("name", Some(json!({"name":"New name"})), true))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    let first: Value =
        serde_json::from_slice(&to_bytes(first.into_body(), 8192).await.unwrap()).unwrap();
    let retry = router
        .clone()
        .oneshot(request("name", Some(json!({"name":"New name"})), true))
        .await
        .unwrap();
    let retry: Value =
        serde_json::from_slice(&to_bytes(retry.into_body(), 8192).await.unwrap()).unwrap();
    assert_eq!(first["command_id"], retry["command_id"]);
    assert_eq!(
        router
            .clone()
            .oneshot(request("name", Some(json!({"name":"Different"})), true))
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        state.events.session("s").unwrap().unwrap().title.as_deref(),
        Some("Original")
    );
    let cmd = state
        .typed_commands
        .claim("gpu-a", unix_time())
        .unwrap()
        .unwrap();
    assert_eq!(cmd.payload.unwrap()["rename_to"], "New name");
    state
        .agents
        .write()
        .await
        .get_mut("gpu-a")
        .unwrap()
        .capabilities
        .clear();
    assert_eq!(
        router
            .oneshot(request("name", Some(json!({"name":"New name"})), true))
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn model_overrides_cannot_steer_or_reach_an_older_agent() {
    let state = ready().await;
    let router = app(state.clone());
    let choice = json!({"model":"model-a","reasoning_effort":"high"});
    for (body, expected) in [
        (
            json!({"prompt":"hi","model_choice":{"model":"/path","reasoning_effort":"high"}}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"prompt":"hi","model_choice":choice,"delivery":"steer","turn_id":"active"}),
            StatusCode::CONFLICT,
        ),
    ] {
        assert_eq!(
            router
                .clone()
                .oneshot(request("messages", Some(body), true))
                .await
                .unwrap()
                .status(),
            expected
        );
    }
    state
        .agents
        .write()
        .await
        .get_mut("gpu-a")
        .unwrap()
        .capabilities
        .clear();
    assert_eq!(
        router
            .oneshot(request(
                "messages",
                Some(json!({"prompt":"hi","model_choice":choice})),
                true
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn handoff_requires_csrf_idle_metadata_and_a_durable_identity() {
    let state = ready().await;
    let router = app(state.clone());
    assert_eq!(
        router
            .clone()
            .oneshot(request("handoff", Some(json!({})), false))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        router
            .clone()
            .oneshot(request("handoff", Some(json!({})), true))
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    state.events.ingest("gpu-a", &[farhelm_protocol::AgentEvent {protocol:FARHELM_PROTOCOL.into(),agent_id:"gpu-a".into(),sequence:2,event_id:"settings-idle".into(),event_type:"codex.session.updated".into(),created_at_unix:unix_time(),payload:json!({"session_id":"s","project_id":"cc08","mode":"inspect","state":"idle","active_turn_id":null,"updated_at_unix":unix_time()})}]).unwrap();
    let first = router
        .clone()
        .oneshot(request("handoff", Some(json!({})), true))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    let first: Value =
        serde_json::from_slice(&to_bytes(first.into_body(), 8192).await.unwrap()).unwrap();
    let retry = router
        .oneshot(request("handoff", Some(json!({})), true))
        .await
        .unwrap();
    let retry: Value =
        serde_json::from_slice(&to_bytes(retry.into_body(), 8192).await.unwrap()).unwrap();
    assert_eq!(first["command_id"], retry["command_id"]);
    let command = state
        .typed_commands
        .claim("gpu-a", unix_time())
        .unwrap()
        .unwrap();
    assert_eq!(command.action, CommandAction::CodexSessionResume);
    assert_eq!(command.payload.unwrap()["handoff"], true);
}
