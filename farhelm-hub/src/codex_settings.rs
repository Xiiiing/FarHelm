//! Native settings and identities use the existing authenticated memory relay.
use super::*;
use farhelm_protocol::{CodexModelList, CodexNativeIdentity, CodexSessionSummary};
type ReadError = (StatusCode, &'static str);

async fn target(
    state: &AppState,
    id: &str,
    capability: &str,
) -> Result<CodexSessionSummary, ReadError> {
    let id = id.to_owned();
    let session = match database(state, move |s| s.events.session(&id)).await {
        Ok(Some(session)) => session,
        Ok(None) => return Err((StatusCode::NOT_FOUND, "session_not_found")),
        Err(_) => {
            return Err((StatusCode::INTERNAL_SERVER_ERROR, "event_store_failed"));
        }
    };
    let agents = state.agents.read().await;
    let Some(agent) = agents
        .get(&session.agent_id)
        .filter(|a| is_online(unix_time(), a.last_seen_unix))
    else {
        return Err((StatusCode::SERVICE_UNAVAILABLE, "agent_offline"));
    };
    if !agent.capabilities.iter().any(|c| c == capability) {
        return Err((StatusCode::CONFLICT, "agent_upgrade_required"));
    }
    Ok(session)
}

async fn read(
    state: &AppState,
    id: &str,
    method: &str,
    capability: &str,
) -> Result<serde_json::Value, ReadError> {
    let session = target(state, id, capability).await?;
    session_display::relay_method(
        state,
        &session.agent_id,
        method,
        serde_json::json!({"session_id":id}),
        tokio::time::Instant::now() + Duration::from_secs(20),
    )
    .await
    .map_err(|code| {
        (
            if code == "agent_read_timeout" {
                StatusCode::GATEWAY_TIMEOUT
            } else {
                StatusCode::BAD_GATEWAY
            },
            code,
        )
    })
}

pub(super) async fn models(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match read(&state, &id, "codex.models.list", "codex.model_choice").await {
        Ok(value) => match serde_json::from_value::<CodexModelList>(value) {
            Ok(list) => Json(list).into_response(),
            Err(_) => api_error(StatusCode::BAD_GATEWAY, "agent_read_invalid"),
        },
        Err((status, code)) => api_error(status, code),
    }
}

pub(super) async fn native(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match read(&state, &id, "codex.session.native", "codex.native_identity").await {
        Ok(value) => match serde_json::from_value::<CodexNativeIdentity>(value) {
            Ok(info) if info.session_id == id => Json(info).into_response(),
            _ => api_error(StatusCode::BAD_GATEWAY, "agent_read_invalid"),
        },
        Err((status, code)) => api_error(status, code),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RenameRequest {
    name: String,
}

pub(super) async fn rename(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<RenameRequest>,
) -> Response {
    let name = request.name.trim();
    if !farhelm_protocol::valid_session_name(name) {
        return api_error(StatusCode::BAD_REQUEST, "invalid_session_name");
    }
    let Some(key) = idempotency_header(&headers) else {
        return api_error(StatusCode::BAD_REQUEST, "missing_idempotency_key");
    };
    let session = match target(&state, &id, "codex.native_identity").await {
        Ok(session) => session,
        Err((status, code)) => return api_error(status, code),
    };
    // An optional metadata operation on the existing session command preserves schema 7
    // and old receipt readers. Capability gating prevents an old Agent ignoring the name.
    create_typed_response(&state, &session.agent_id, CommandAction::CodexSessionResume,
        serde_json::json!({"session_id":id,"project_id":session.project_id,"mode":session.mode,"rename_to":name}), key, 300).await
}

pub(super) async fn handoff(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let Some(key) = idempotency_header(&headers) else {
        return api_error(StatusCode::BAD_REQUEST, "missing_idempotency_key");
    };
    let session = match target(&state, &id, "codex.native_identity").await {
        Ok(session) => session,
        Err((status, code)) => return api_error(status, code),
    };
    if session.active_turn_id.is_some()
        || session.state == farhelm_protocol::CodexSessionState::Running
    {
        return api_error(StatusCode::CONFLICT, "codex_handoff_busy");
    }
    create_typed_response(&state, &session.agent_id, CommandAction::CodexSessionResume,
        serde_json::json!({"session_id":id,"project_id":session.project_id,"mode":session.mode,"handoff":true}), key, 30).await
}
