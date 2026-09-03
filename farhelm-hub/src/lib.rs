use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Result, ensure};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{any, get, post},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use constant_time_eq::constant_time_eq;
use farhelm_core::PRODUCT_VERSION;
use farhelm_protocol::{
    AgentHeartbeat, AgentHeartbeatAck, AgentListResponse, AgentSummary, FARHELM_PROTOCOL,
    HealthResponse,
};
use serde::Serialize;
use tokio::sync::RwLock;
use tower_http::services::{ServeDir, ServeFile};

pub const ONLINE_WINDOW_SECS: u64 = 45;

#[derive(Clone)]
pub struct HubConfig {
    pub admin_user: String,
    pub admin_password: String,
    pub agent_token: String,
    pub console_dir: PathBuf,
}

#[derive(Clone)]
pub struct AppState {
    config: Arc<HubConfig>,
    agents: Arc<RwLock<BTreeMap<String, StoredAgent>>>,
}

#[derive(Clone)]
struct StoredAgent {
    hostname: String,
    agent_version: String,
    last_seen_unix: u64,
}

#[derive(Serialize)]
struct ApiError {
    error: &'static str,
}

impl AppState {
    #[must_use]
    pub fn new(config: HubConfig) -> Self {
        Self {
            config: Arc::new(config),
            agents: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }
}

impl HubConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(!self.admin_user.is_empty(), "FARHELM_ADMIN_USER is empty");
        ensure!(
            !self.admin_user.contains(':'),
            "FARHELM_ADMIN_USER cannot contain ':'"
        );
        ensure!(
            self.admin_password.len() >= 12,
            "FARHELM_ADMIN_PASSWORD must contain at least 12 characters"
        );
        ensure!(
            self.agent_token.len() >= 32,
            "FARHELM_AGENT_TOKEN must contain at least 32 characters"
        );
        ensure!(
            self.console_dir.join("index.html").is_file(),
            "FARHELM_CONSOLE_DIR must contain index.html"
        );
        Ok(())
    }
}

pub fn app(state: AppState) -> Router {
    let index = state.config.console_dir.join("index.html");
    let static_files = ServeDir::new(&state.config.console_dir).fallback(ServeFile::new(index));

    Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/agents", get(list_agents))
        .route("/api/v1/agents/heartbeat", post(agent_heartbeat))
        .route("/api/{*path}", any(api_not_found))
        .fallback_service(static_files)
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(middleware::from_fn_with_state(state.clone(), authorize))
        .with_state(state)
}

async fn authorize(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let path = request.uri().path();
    if path == "/api/v1/health" {
        return next.run(request).await;
    }

    if path == "/api/v1/agents/heartbeat" {
        if bearer_matches(request.headers(), &state.config.agent_token) {
            return next.run(request).await;
        }
        return unauthorized(false);
    }

    if basic_matches(
        request.headers(),
        &state.config.admin_user,
        &state.config.admin_password,
    ) {
        return next.run(request).await;
    }
    unauthorized(true)
}

fn bearer_matches(headers: &HeaderMap, expected: &str) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|value| secure_eq(value, expected))
}

fn basic_matches(headers: &HeaderMap, expected_user: &str, expected_password: &str) -> bool {
    let Some(encoded) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Basic "))
    else {
        return false;
    };
    let Ok(decoded) = STANDARD.decode(encoded) else {
        return false;
    };
    let Ok(credentials) = std::str::from_utf8(&decoded) else {
        return false;
    };
    let Some((user, password)) = credentials.split_once(':') else {
        return false;
    };
    secure_eq(user, expected_user) & secure_eq(password, expected_password)
}

fn secure_eq(actual: &str, expected: &str) -> bool {
    actual.len() == expected.len() && constant_time_eq(actual.as_bytes(), expected.as_bytes())
}

fn unauthorized(admin: bool) -> Response {
    let mut response = (
        StatusCode::UNAUTHORIZED,
        Json(ApiError {
            error: "unauthorized",
        }),
    )
        .into_response();
    if admin {
        response.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Basic realm=\"FarHelm\", charset=\"UTF-8\""),
        );
    }
    response
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse::hub(PRODUCT_VERSION))
}

async fn api_not_found() -> (StatusCode, Json<ApiError>) {
    (StatusCode::NOT_FOUND, Json(ApiError { error: "not_found" }))
}

async fn agent_heartbeat(
    State(state): State<AppState>,
    Json(heartbeat): Json<AgentHeartbeat>,
) -> Result<Json<AgentHeartbeatAck>, (StatusCode, Json<ApiError>)> {
    validate_heartbeat(&heartbeat)?;
    let now = unix_time();
    state.agents.write().await.insert(
        heartbeat.agent_id,
        StoredAgent {
            hostname: heartbeat.hostname,
            agent_version: heartbeat.agent_version,
            last_seen_unix: now,
        },
    );
    Ok(Json(AgentHeartbeatAck {
        accepted: true,
        protocol: FARHELM_PROTOCOL.to_owned(),
        server_time_unix: now,
    }))
}

fn validate_heartbeat(heartbeat: &AgentHeartbeat) -> Result<(), (StatusCode, Json<ApiError>)> {
    let valid_agent_id = !heartbeat.agent_id.is_empty()
        && heartbeat.agent_id.len() <= 64
        && heartbeat
            .agent_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
    let valid_hostname = !heartbeat.hostname.is_empty()
        && heartbeat.hostname.len() <= 255
        && !heartbeat.hostname.chars().any(char::is_control);
    let valid_version = !heartbeat.agent_version.is_empty() && heartbeat.agent_version.len() <= 32;
    if heartbeat.protocol != FARHELM_PROTOCOL
        || !valid_agent_id
        || !valid_hostname
        || !valid_version
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: "invalid_heartbeat",
            }),
        ));
    }
    Ok(())
}

async fn list_agents(State(state): State<AppState>) -> Json<AgentListResponse> {
    let now = unix_time();
    let agents = state
        .agents
        .read()
        .await
        .iter()
        .map(|(agent_id, stored)| AgentSummary {
            agent_id: agent_id.clone(),
            hostname: stored.hostname.clone(),
            agent_version: stored.agent_version.clone(),
            last_seen_unix: stored.last_seen_unix,
            online: is_online(now, stored.last_seen_unix),
        })
        .collect();
    Json(AgentListResponse {
        protocol: FARHELM_PROTOCOL.to_owned(),
        agents,
    })
}

const fn is_online(now: u64, last_seen: u64) -> bool {
    now.saturating_sub(last_seen) <= ONLINE_WINDOW_SECS
}

fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use farhelm_protocol::{AgentHeartbeat, FARHELM_PROTOCOL, HealthResponse, HealthStatus};
    use tower::ServiceExt;

    use super::*;

    fn test_state() -> AppState {
        AppState::new(HubConfig {
            admin_user: "admin".to_owned(),
            admin_password: "correct-horse".to_owned(),
            agent_token: "agent-token-with-at-least-32-characters".to_owned(),
            console_dir: PathBuf::from("missing-test-console"),
        })
    }

    fn basic_header() -> String {
        format!("Basic {}", STANDARD.encode("admin:correct-horse"))
    }

    #[tokio::test]
    async fn health_endpoint_is_public_and_versioned() {
        let response = app(test_state())
            .oneshot(
                Request::builder()
                    .uri("/api/v1/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        let health: HealthResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(health.status, HealthStatus::Ok);
        assert_eq!(health.service, "farhelm-hub");
        assert_eq!(health.protocol, FARHELM_PROTOCOL);
    }

    #[tokio::test]
    async fn admin_api_requires_basic_auth() {
        let response = app(test_state())
            .oneshot(
                Request::builder()
                    .uri("/api/v1/agents")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(response.headers().contains_key(header::WWW_AUTHENTICATE));
    }

    #[tokio::test]
    async fn heartbeat_requires_agent_token() {
        let response = app(test_state())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/agents/heartbeat")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&AgentHeartbeat::new("gpu-a", "trainer-a", "0.1.0"))
                            .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn authenticated_heartbeat_appears_in_agent_list() {
        let router = app(test_state());
        let heartbeat = Request::builder()
            .method("POST")
            .uri("/api/v1/agents/heartbeat")
            .header(header::CONTENT_TYPE, "application/json")
            .header(
                header::AUTHORIZATION,
                "Bearer agent-token-with-at-least-32-characters",
            )
            .body(Body::from(
                serde_json::to_vec(&AgentHeartbeat::new("gpu-a", "trainer-a", "0.1.0")).unwrap(),
            ))
            .unwrap();
        assert_eq!(
            router.clone().oneshot(heartbeat).await.unwrap().status(),
            StatusCode::OK
        );

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/api/v1/agents")
                    .header(header::AUTHORIZATION, basic_header())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        let list: AgentListResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(list.agents.len(), 1);
        assert_eq!(list.agents[0].agent_id, "gpu-a");
        assert!(list.agents[0].online);
    }

    #[tokio::test]
    async fn authenticated_spa_route_serves_console_index() {
        let console_dir =
            std::env::temp_dir().join(format!("farhelm-hub-console-test-{}", std::process::id()));
        std::fs::create_dir_all(&console_dir).unwrap();
        std::fs::write(
            console_dir.join("index.html"),
            "<title>FarHelm test</title>",
        )
        .unwrap();
        let state = AppState::new(HubConfig {
            console_dir: console_dir.clone(),
            ..test_state().config.as_ref().clone()
        });
        let response = app(state)
            .oneshot(
                Request::builder()
                    .uri("/agents")
                    .header(header::AUTHORIZATION, basic_header())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        assert!(String::from_utf8_lossy(&body).contains("FarHelm test"));
        std::fs::remove_dir_all(console_dir).unwrap();
    }

    #[tokio::test]
    async fn unknown_api_does_not_fall_back_to_console() {
        let response = app(test_state())
            .oneshot(
                Request::builder()
                    .uri("/api/v1/missing")
                    .header(header::AUTHORIZATION, basic_header())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let content_type = response.headers().get(header::CONTENT_TYPE).unwrap();
        assert_eq!(content_type, "application/json");
    }

    #[test]
    fn deployment_config_rejects_weak_secrets() {
        let config = HubConfig {
            admin_user: "admin".to_owned(),
            admin_password: "short".to_owned(),
            agent_token: "short".to_owned(),
            console_dir: PathBuf::from("missing-test-console"),
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn agent_expires_after_online_window() {
        assert!(is_online(100, 55));
        assert!(!is_online(101, 55));
    }
}
