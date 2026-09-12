//! Authenticated, typed native Codex controls. Vendor method names are never client input.
use super::*;
use farhelm_protocol::native::{NativeOperationRequest, NativeReadKind};
use serde_json::{Value, json};

#[derive(Deserialize)]
pub(super) struct NativeReadQuery {
    kind: NativeReadKind,
    #[serde(default)]
    force_reload: bool,
    resource_id: Option<String>,
    offset: Option<u64>,
}

pub(super) async fn read(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    Query(query): Query<NativeReadQuery>,
) -> Response {
    let session = match database(&state, {
        let id = session_id.clone();
        move |s| s.events.session(&id)
    })
    .await
    {
        Ok(Some(value)) => value,
        Ok(None) => return api_error(StatusCode::NOT_FOUND, "session_not_found"),
        Err(_) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, "event_store_failed"),
    };
    if let Err((status, code)) =
        project_management::capability(&state, &session.agent_id, "codex.native_control").await
    {
        return api_error(status, code);
    }
    if matches!(query.kind, NativeReadKind::Activity)
        && let Err((status, code)) =
            project_management::capability(&state, &session.agent_id, "codex.native_completion")
                .await
    {
        return api_error(status, code);
    }
    let kind = match serde_json::to_value(query.kind) {
        Ok(Value::String(v)) => v,
        _ => return api_error(StatusCode::BAD_REQUEST, "invalid_native_read"),
    };
    match session_display::relay_method(
        &state,
        &session.agent_id,
        "codex.native.read",
        json!({"session_id":session_id,"kind":kind,"force_reload":query.force_reload,"resource_id":query.resource_id,"offset":query.offset}),
        tokio::time::Instant::now() + Duration::from_secs(20),
    )
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(code) => api_error(StatusCode::BAD_GATEWAY, code),
    }
}

pub(super) async fn operate(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    Json(request): Json<NativeOperationRequest>,
) -> Response {
    if request.idempotency_key.is_empty()
        || request.idempotency_key.len() > 192
        || !request.operation.is_valid()
    {
        return api_error(StatusCode::BAD_REQUEST, "invalid_native_operation");
    }
    let session = match database(&state, {
        let id = session_id.clone();
        move |s| s.events.session(&id)
    })
    .await
    {
        Ok(Some(value)) => value,
        Ok(None) => return api_error(StatusCode::NOT_FOUND, "session_not_found"),
        Err(_) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, "event_store_failed"),
    };
    if let Err((status, code)) =
        project_management::capability(&state, &session.agent_id, "codex.native_control").await
    {
        return api_error(status, code);
    }
    if matches!(
        request.operation,
        farhelm_protocol::native::NativeOperation::Pin { .. }
            | farhelm_protocol::native::NativeOperation::TemporaryStart
            | farhelm_protocol::native::NativeOperation::TemporaryEnd
    ) && let Err((status, code)) =
        project_management::capability(&state, &session.agent_id, "codex.native_completion").await
    {
        return api_error(status, code);
    }
    let payload = json!({"session_id":session_id,"project_id":session.project_id,"native_operation":request.operation});
    create_typed_response(
        &state,
        &session.agent_id,
        CommandAction::CodexNativeOperation,
        payload,
        &request.idempotency_key,
        120,
    )
    .await
}
