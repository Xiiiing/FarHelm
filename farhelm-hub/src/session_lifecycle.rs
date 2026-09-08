use super::*;
use farhelm_protocol::projects::ArchivePreview;
use serde_json::json;

async fn session(
    state: &AppState,
    id: String,
) -> Result<farhelm_protocol::CodexSessionSummary, (StatusCode, &'static str)> {
    match database(state, move |s| s.events.session(&id)).await {
        Ok(Some(s)) => Ok(s),
        Ok(None) => Err((StatusCode::NOT_FOUND, "session_not_found")),
        Err(_) => Err((StatusCode::INTERNAL_SERVER_ERROR, "event_store_failed")),
    }
}

pub(super) async fn preview(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let target = match session(&state, id.clone()).await {
        Ok(s) => s,
        Err((status, code)) => return api_error(status, code),
    };
    if let Err((status, code)) =
        project_management::capability(&state, &target.agent_id, "codex.session_archive").await
    {
        return api_error(status, code);
    }
    match session_display::relay_method(
        &state,
        &target.agent_id,
        "codex.session.archive_preview",
        json!({"session_id":id}),
        tokio::time::Instant::now() + Duration::from_secs(20),
    )
    .await
    {
        Ok(v) => match serde_json::from_value::<ArchivePreview>(v) {
            Ok(p)
                if !p.session_ids.is_empty()
                    && p.session_ids.len() <= 1000
                    && p.fingerprint.len() == 64 =>
            {
                ([(header::CACHE_CONTROL, "no-store")], Json(p)).into_response()
            }
            _ => api_error(StatusCode::BAD_GATEWAY, "agent_read_invalid"),
        },
        Err(code) => api_error(StatusCode::CONFLICT, code),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ArchiveRequest {
    fingerprint: String,
}
pub(super) async fn archive(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<ArchiveRequest>,
) -> Response {
    if request.fingerprint.len() != 64
        || !request.fingerprint.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return api_error(StatusCode::BAD_REQUEST, "codex_archive_changed");
    }
    submit(&state, id, &headers, true, Some(request.fingerprint)).await
}
pub(super) async fn unarchive(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    submit(&state, id, &headers, false, None).await
}

async fn submit(
    state: &AppState,
    id: String,
    headers: &HeaderMap,
    archive: bool,
    fingerprint: Option<String>,
) -> Response {
    let target = match session(state, id.clone()).await {
        Ok(s) => s,
        Err((status, code)) => return api_error(status, code),
    };
    if let Err((status, code)) =
        project_management::capability(state, &target.agent_id, "codex.session_archive").await
    {
        return api_error(status, code);
    }
    let Some(key) = idempotency_header(headers) else {
        return api_error(StatusCode::BAD_REQUEST, "missing_idempotency_key");
    };
    create_typed_response(
        state,
        &target.agent_id,
        if archive {
            CommandAction::CodexSessionArchive
        } else {
            CommandAction::CodexSessionUnarchive
        },
        json!({"session_id":id,"project_id":target.project_id,"fingerprint":fingerprint}),
        key,
        120,
    )
    .await
}
