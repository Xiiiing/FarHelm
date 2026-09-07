use super::tests::{browser_cookie, test_state};
use super::*;
use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use tower::ServiceExt;

#[tokio::test]
async fn notification_routes_auth_csrf_idempotency_and_account_reads() {
    let state = test_state();
    state
        .events
        .save_push_subscription(
            "https://push.test/device",
            &"a".repeat(88),
            &"b".repeat(22),
            unix_time(),
        )
        .unwrap();
    let id = state.events.devices().unwrap()["devices"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let router = app(state.clone());
    for path in [
        "/api/v1/notifications",
        "/api/v1/notifications/preferences",
        "/api/v1/experiment-runs",
        "/api/v1/audit",
        "/api/v1/overview",
        "/api/v1/push/devices",
    ] {
        let denied = router
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header(header::COOKIE, browser_cookie())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
    }
    let test_path = format!("/api/v1/push/devices/{id}/test");
    let denied = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&test_path)
                .header(header::COOKIE, browser_cookie())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    let writes = [
        (
            "POST",
            test_path.clone(),
            serde_json::json!({}),
            "device-test-key-0001",
        ),
        (
            "POST",
            test_path,
            serde_json::json!({}),
            "device-test-key-0001",
        ),
        (
            "POST",
            format!("/api/v1/push/devices/{id}"),
            serde_json::json!({"name":"Phone","experiments":true,"codex":false}),
            "device-edit-key-0001",
        ),
        (
            "POST",
            "/api/v1/notifications/1/read".into(),
            serde_json::json!({}),
            "read-single-key-0001",
        ),
        (
            "POST",
            "/api/v1/notifications/read-all".into(),
            serde_json::json!({"through_id":1}),
            "read-all-key-000001",
        ),
        (
            "DELETE",
            format!("/api/v1/push/devices/{id}"),
            serde_json::json!({}),
            "device-delete-00001",
        ),
    ];
    for (method, path, body, key) in writes {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(&path)
                    .header(header::COOKIE, browser_cookie())
                    .header("x-csrf-token", "test-csrf")
                    .header("idempotency-key", key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{method} {path}");
    }
    let response = router
        .oneshot(
            Request::builder()
                .uri("/api/v1/notifications/1")
                .header(header::COOKIE, browser_cookie())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let detail: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 8192).await.unwrap()).unwrap();
    assert!(detail["notification"]["read_at_unix"].is_number());
    assert_eq!(detail["deliveries"][0]["state"], "failed");
    assert_eq!(
        state.events.notifications(&Default::default()).unwrap()["notifications"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn browser_preferences_and_test_notifications_are_durable_and_idempotent() {
    let state = test_state();
    let router = app(state.clone());
    assert_eq!(
        state.events.notification_preferences().unwrap(),
        serde_json::json!({"experiments":true,"codex":true})
    );
    for (path, body, key) in [
        (
            "/api/v1/notifications/preferences",
            serde_json::json!({"experiments":false,"codex":true}),
            "preferences-change-1",
        ),
        (
            "/api/v1/notifications/test",
            serde_json::json!({}),
            "browser-test-key-001",
        ),
        (
            "/api/v1/notifications/test",
            serde_json::json!({}),
            "browser-test-key-001",
        ),
    ] {
        let denied = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .header(header::COOKIE, browser_cookie())
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .header(header::COOKIE, browser_cookie())
                    .header("x-csrf-token", "test-csrf")
                    .header("idempotency-key", key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
    }
    assert_eq!(
        state.events.notification_preferences().unwrap(),
        serde_json::json!({"experiments":false,"codex":true})
    );
    let page = state.events.notifications(&Default::default()).unwrap();
    assert_eq!(page["notifications"].as_array().unwrap().len(), 1);
    assert_eq!(page["notifications"][0]["category"], "test");
    assert_eq!(page["preferences"]["experiments"], false);
}

#[tokio::test]
async fn saved_command_retry_does_not_require_a_new_heartbeat() {
    let state = test_state();
    let payload =
        serde_json::json!({"session_id":"s","project_id":"p","prompt":"private instruction"});
    let original = state
        .typed_commands
        .create(
            "agent",
            CommandAction::CodexTurnStart,
            &payload,
            "offline-retry-key-01",
            300,
            unix_time(),
        )
        .unwrap();
    state.typed_commands.claim("agent", unix_time()).unwrap();
    state
        .typed_commands
        .report(
            &farhelm_protocol::CommandReportRequest {
                protocol: FARHELM_PROTOCOL.into(),
                agent_id: "agent".into(),
                command_id: original.command_id.clone(),
                state: farhelm_protocol::CommandState::Accepted,
                result: None,
                data: None,
                detail: None,
            },
            unix_time(),
        )
        .unwrap();
    let response = create_typed_response(
        &state,
        "agent",
        CommandAction::CodexTurnStart,
        payload.clone(),
        "offline-retry-key-01",
        300,
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let receipt: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 8192).await.unwrap()).unwrap();
    assert_eq!(receipt["command_id"], original.command_id);
    let mut conflicting = payload;
    conflicting["prompt"] = serde_json::json!("different instruction");
    assert_eq!(
        create_typed_response(
            &state,
            "agent",
            CommandAction::CodexTurnStart,
            conflicting,
            "offline-retry-key-01",
            300
        )
        .await
        .status(),
        StatusCode::CONFLICT
    );
}
