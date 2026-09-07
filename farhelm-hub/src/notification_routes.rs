use super::*;
use event_store::notifications::ListQuery;
use serde_json::{Value, json};

async fn db<T: Send + 'static>(
    state: &AppState,
    f: impl FnOnce(&EventStore) -> Result<T> + Send + 'static,
) -> Result<T> {
    database(state, move |state| f(&state.events)).await
}
fn response(result: Result<Value>) -> Response {
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => {
            tracing::warn!(%error,"notification request failed");
            api_error(StatusCode::BAD_REQUEST, "invalid_notification_request")
        }
    }
}
pub async fn list(State(state): State<AppState>, Query(q): Query<ListQuery>) -> Response {
    response(db(&state, move |s| s.notifications(&q)).await)
}
pub async fn runs(State(state): State<AppState>, Query(q): Query<ListQuery>) -> Response {
    response(db(&state, move |s| s.experiment_runs(&q)).await)
}
pub async fn audits(State(state): State<AppState>, Query(q): Query<ListQuery>) -> Response {
    response(db(&state, move |s| s.audits(&q)).await)
}
pub async fn devices(State(state): State<AppState>) -> Response {
    response(db(&state, |s| s.devices()).await)
}
pub async fn detail(State(state): State<AppState>, Path(id): Path<i64>) -> Response {
    match db(&state,move|s|s.notification(id)?.map(|notification|s.notification_deliveries(id).map(|deliveries|json!({"protocol":FARHELM_PROTOCOL,"notification":notification,"deliveries":deliveries}))).transpose()).await {
        Ok(Some(value))=>Json(value).into_response(),
        Ok(None)=>api_error(StatusCode::NOT_FOUND,"notification_not_found"),
        Err(error)=>response(Err(error)),
    }
}

async fn write(
    state: AppState,
    headers: HeaderMap,
    action: &'static str,
    target: String,
    body: Value,
) -> Response {
    let Some(key) = idempotency_header(&headers).map(str::to_owned) else {
        return api_error(StatusCode::BAD_REQUEST, "missing_idempotency_key");
    };
    let result = db(&state, move |s| {
        s.notification_write(&key, action, &target, &body, unix_time())
    })
    .await;
    if result.is_ok() {
        state.push_notify.notify_one();
        let _ = state.event_bus.send(StoredEvent {
            sequence: 0,
            event_id: random_token(),
            event_type: "notification.changed".to_owned(),
            payload: json!({}),
        });
    }
    response(result)
}
pub async fn read(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    headers: HeaderMap,
) -> Response {
    write(
        state,
        headers,
        "notification.read",
        id.to_string(),
        json!({}),
    )
    .await
}
pub async fn read_all(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    write(state, headers, "notification.read-all", String::new(), body).await
}
pub async fn update_device(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    write(state, headers, "notification.settings", id, body).await
}
pub async fn remove_device(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    write(state, headers, "notification.revoke", id, json!({})).await
}
pub async fn test(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    write(state, headers, "notification.test", id, json!({})).await
}
pub async fn overview(State(state): State<AppState>) -> Response {
    response(db(&state, |s| s.overview_counts()).await)
}

pub async fn preferences(State(state): State<AppState>) -> Response {
    response(db(&state, |s| s.notification_preferences()).await)
}
pub async fn set_preferences(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    write(
        state,
        headers,
        "notification.preferences",
        "administrator".into(),
        body,
    )
    .await
}
pub async fn browser_test(State(state): State<AppState>, headers: HeaderMap) -> Response {
    write(
        state,
        headers,
        "notification.browser-test",
        "administrator".into(),
        json!({}),
    )
    .await
}
