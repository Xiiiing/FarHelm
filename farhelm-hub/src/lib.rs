use std::{
    collections::BTreeMap,
    path::{Component, Path as FsPath, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Result, ensure};
use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Path, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, Uri, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{any, get, post},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use constant_time_eq::constant_time_eq;
use farhelm_core::PRODUCT_VERSION;
use farhelm_protocol::{
    AgentHeartbeat, AgentHeartbeatAck, AgentListResponse, AgentSummary, CommandAccepted,
    CommandClaimRequest, CommandClaimResponse, CommandReportRequest, CreateProbeCommand,
    FARHELM_PROTOCOL, HealthResponse,
};
use serde::Serialize;
use tokio::sync::RwLock;

include!(concat!(env!("OUT_DIR"), "/embedded_console.rs"));

pub const ONLINE_WINDOW_SECS: u64 = 45;

mod command_store;

use command_store::{CommandStore, CreateCommandError, ReportCommandError};

#[derive(Clone)]
pub struct HubConfig {
    pub admin_user: String,
    pub admin_password: String,
    pub agent_token: String,
    pub console_dir: Option<PathBuf>,
    pub database_path: PathBuf,
}

#[derive(Clone)]
pub struct AppState {
    config: Arc<HubConfig>,
    agents: Arc<RwLock<BTreeMap<String, StoredAgent>>>,
    commands: Arc<CommandStore>,
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
    pub fn new(config: HubConfig) -> Result<Self> {
        let commands = CommandStore::open(&config.database_path)?;
        Ok(Self {
            config: Arc::new(config),
            agents: Arc::new(RwLock::new(BTreeMap::new())),
            commands: Arc::new(commands),
        })
    }
}

impl HubConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(!self.admin_user.is_empty(), "admin.user is empty");
        ensure!(
            !self.admin_user.contains(':'),
            "admin.user cannot contain ':'"
        );
        ensure!(
            self.admin_password.len() >= 12,
            "admin.password must contain at least 12 characters"
        );
        ensure!(
            self.agent_token.len() >= 32,
            "agents.token must contain at least 32 characters"
        );
        if let Some(console_dir) = &self.console_dir {
            ensure!(
                console_dir.join("index.html").is_file(),
                "hub.console_dir must contain index.html"
            );
        } else {
            ensure!(
                has_embedded_console(),
                "this Hub build has no embedded Console"
            );
        }
        ensure!(
            !self.database_path.as_os_str().is_empty(),
            "hub.database is empty"
        );
        Ok(())
    }
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/agents", get(list_agents))
        .route("/api/v1/agents/heartbeat", post(agent_heartbeat))
        .route("/api/v1/agents/{agent_id}/probe", post(create_probe))
        .route("/api/v1/commands/{command_id}", get(command_status))
        .route("/api/v1/agent/commands/claim", post(claim_command))
        .route("/api/v1/agent/commands/report", post(report_command))
        .route("/api/{*path}", any(api_not_found))
        .fallback(static_content)
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(middleware::from_fn_with_state(state.clone(), authorize))
        .with_state(state)
}

#[must_use]
pub fn has_embedded_console() -> bool {
    EMBEDDED_CONSOLE
        .iter()
        .any(|(path, _)| *path == "index.html")
}

async fn static_content(State(state): State<AppState>, uri: Uri) -> Response {
    let requested = uri.path().trim_start_matches('/');
    let requested = if requested.is_empty() {
        "index.html"
    } else {
        requested
    };
    if !safe_asset_path(requested) {
        return StatusCode::NOT_FOUND.into_response();
    }

    let asset = if let Some(console_dir) = &state.config.console_dir {
        read_external_asset(console_dir, requested).await
    } else {
        read_embedded_asset(requested)
    };
    let (path, contents) = match asset {
        Some(asset) => asset,
        None if !requested.starts_with("api/") => {
            if let Some(console_dir) = &state.config.console_dir {
                match read_external_asset(console_dir, "index.html").await {
                    Some(asset) => asset,
                    None => return StatusCode::NOT_FOUND.into_response(),
                }
            } else {
                match read_embedded_asset("index.html") {
                    Some(asset) => asset,
                    None => return StatusCode::NOT_FOUND.into_response(),
                }
            }
        }
        None => return StatusCode::NOT_FOUND.into_response(),
    };

    let mut response = Response::new(Body::from(contents));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(content_type(&path)),
    );
    response
}

fn safe_asset_path(path: &str) -> bool {
    !path.is_empty()
        && FsPath::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

async fn read_external_asset(root: &FsPath, path: &str) -> Option<(String, Vec<u8>)> {
    tokio::fs::read(root.join(path))
        .await
        .ok()
        .map(|contents| (path.to_owned(), contents))
}

fn read_embedded_asset(path: &str) -> Option<(String, Vec<u8>)> {
    EMBEDDED_CONSOLE
        .iter()
        .find(|(candidate, _)| *candidate == path)
        .map(|(_, contents)| (path.to_owned(), contents.to_vec()))
}

fn content_type(path: &str) -> &'static str {
    match FsPath::new(path)
        .extension()
        .and_then(|value| value.to_str())
    {
        Some("css") => "text/css; charset=utf-8",
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("json") | Some("webmanifest") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

async fn authorize(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let path = request.uri().path();
    if path == "/api/v1/health" {
        return next.run(request).await;
    }

    if path == "/api/v1/agents/heartbeat" || path.starts_with("/api/v1/agent/commands/") {
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

async fn create_probe(
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
    Json(request): Json<CreateProbeCommand>,
) -> Response {
    if !valid_agent_id(&agent_id)
        || !valid_idempotency_key(&request.idempotency_key)
        || !(10..=300).contains(&request.ttl_secs)
    {
        return api_error(StatusCode::BAD_REQUEST, "invalid_command");
    }
    match state.commands.create_probe(
        &agent_id,
        &request.idempotency_key,
        request.ttl_secs,
        unix_time(),
    ) {
        Ok(command) => (
            StatusCode::ACCEPTED,
            Json(CommandAccepted {
                protocol: FARHELM_PROTOCOL.to_owned(),
                status_url: format!("/api/v1/commands/{}", command.command_id),
                command_id: command.command_id,
                state: command.state,
                expires_at_unix: command.expires_at_unix,
            }),
        )
            .into_response(),
        Err(CreateCommandError::Conflict) => {
            api_error(StatusCode::CONFLICT, "idempotency_conflict")
        }
        Err(CreateCommandError::Internal(error)) => {
            tracing::error!(%error, "failed to create command");
            api_error(StatusCode::INTERNAL_SERVER_ERROR, "command_store_failed")
        }
    }
}

async fn command_status(State(state): State<AppState>, Path(command_id): Path<String>) -> Response {
    if !valid_command_id(&command_id) {
        return api_error(StatusCode::BAD_REQUEST, "invalid_command_id");
    }
    match state.commands.get(&command_id) {
        Ok(Some(command)) => Json(command).into_response(),
        Ok(None) => api_error(StatusCode::NOT_FOUND, "command_not_found"),
        Err(error) => {
            tracing::error!(%error, "failed to read command");
            api_error(StatusCode::INTERNAL_SERVER_ERROR, "command_store_failed")
        }
    }
}

async fn claim_command(
    State(state): State<AppState>,
    Json(request): Json<CommandClaimRequest>,
) -> Response {
    if request.protocol != FARHELM_PROTOCOL || !valid_agent_id(&request.agent_id) {
        return api_error(StatusCode::BAD_REQUEST, "invalid_claim");
    }
    match state.commands.claim(&request.agent_id, unix_time()) {
        Ok(command) => Json(CommandClaimResponse {
            protocol: FARHELM_PROTOCOL.to_owned(),
            command,
        })
        .into_response(),
        Err(error) => {
            tracing::error!(%error, agent_id = %request.agent_id, "failed to claim command");
            api_error(StatusCode::INTERNAL_SERVER_ERROR, "command_store_failed")
        }
    }
}

async fn report_command(
    State(state): State<AppState>,
    Json(report): Json<CommandReportRequest>,
) -> Response {
    if report.protocol != FARHELM_PROTOCOL
        || !valid_agent_id(&report.agent_id)
        || !valid_command_id(&report.command_id)
    {
        return api_error(StatusCode::BAD_REQUEST, "invalid_report");
    }
    match state.commands.report(&report, unix_time()) {
        Ok(command) => Json(command).into_response(),
        Err(ReportCommandError::NotFound) => api_error(StatusCode::NOT_FOUND, "command_not_found"),
        Err(ReportCommandError::Conflict) => {
            api_error(StatusCode::CONFLICT, "invalid_command_transition")
        }
        Err(ReportCommandError::Invalid) => api_error(StatusCode::BAD_REQUEST, "invalid_report"),
        Err(ReportCommandError::Internal(error)) => {
            tracing::error!(%error, "failed to report command");
            api_error(StatusCode::INTERNAL_SERVER_ERROR, "command_store_failed")
        }
    }
}

fn api_error(status: StatusCode, error: &'static str) -> Response {
    (status, Json(ApiError { error })).into_response()
}

fn valid_agent_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn valid_idempotency_key(value: &str) -> bool {
    (16..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

fn valid_command_id(value: &str) -> bool {
    value.len() == 20
        && value.starts_with("cmd_")
        && value[4..].bytes().all(|byte| byte.is_ascii_hexdigit())
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
    use farhelm_protocol::{
        AgentHeartbeat, CommandAccepted, CommandClaimRequest, CommandClaimResponse,
        CommandReportRequest, CommandState, CommandStatusResponse, CreateProbeCommand,
        FARHELM_PROTOCOL, HealthResponse, HealthStatus, ProbeResult,
    };
    use tower::ServiceExt;

    use super::*;

    fn test_state() -> AppState {
        AppState::new(HubConfig {
            admin_user: "admin".to_owned(),
            admin_password: "correct-horse".to_owned(),
            agent_token: "agent-token-with-at-least-32-characters".to_owned(),
            console_dir: Some(PathBuf::from("missing-test-console")),
            database_path: PathBuf::from(":memory:"),
        })
        .unwrap()
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
            console_dir: Some(console_dir.clone()),
            ..test_state().config.as_ref().clone()
        })
        .unwrap();
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
            console_dir: Some(PathBuf::from("missing-test-console")),
            database_path: PathBuf::from(":memory:"),
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn agent_expires_after_online_window() {
        assert!(is_online(100, 55));
        assert!(!is_online(101, 55));
    }

    #[tokio::test]
    async fn probe_command_completes_through_agent_endpoints() {
        let router = app(test_state());
        let create = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/agents/gpu-a/probe")
                    .header(header::AUTHORIZATION, basic_header())
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&CreateProbeCommand {
                            idempotency_key: "probe-request-0001".to_owned(),
                            ttl_secs: 60,
                        })
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create.status(), StatusCode::ACCEPTED);
        let body = to_bytes(create.into_body(), 4096).await.unwrap();
        let accepted: CommandAccepted = serde_json::from_slice(&body).unwrap();

        let claim = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/agent/commands/claim")
                    .header(
                        header::AUTHORIZATION,
                        "Bearer agent-token-with-at-least-32-characters",
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&CommandClaimRequest {
                            protocol: FARHELM_PROTOCOL.to_owned(),
                            agent_id: "gpu-a".to_owned(),
                        })
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(claim.into_body(), 4096).await.unwrap();
        let claim: CommandClaimResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(claim.command.unwrap().command_id, accepted.command_id);

        for (state, result) in [
            (CommandState::Accepted, None),
            (
                CommandState::Completed,
                Some(ProbeResult {
                    agent_version: "0.1.0".to_owned(),
                    hostname: "trainer-a".to_owned(),
                }),
            ),
        ] {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/v1/agent/commands/report")
                        .header(
                            header::AUTHORIZATION,
                            "Bearer agent-token-with-at-least-32-characters",
                        )
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(
                            serde_json::to_vec(&CommandReportRequest {
                                protocol: FARHELM_PROTOCOL.to_owned(),
                                agent_id: "gpu-a".to_owned(),
                                command_id: accepted.command_id.clone(),
                                state,
                                result,
                                detail: None,
                            })
                            .unwrap(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }

        let status = router
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/commands/{}", accepted.command_id))
                    .header(header::AUTHORIZATION, basic_header())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(status.into_body(), 4096).await.unwrap();
        let command: CommandStatusResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(command.state, CommandState::Completed);
        assert_eq!(command.result.unwrap().hostname, "trainer-a");
    }

    #[tokio::test]
    async fn command_endpoints_enforce_auth_and_command_ownership() {
        let router = app(test_state());
        let create_body = serde_json::to_vec(&CreateProbeCommand {
            idempotency_key: "probe-auth-request-0001".to_owned(),
            ttl_secs: 60,
        })
        .unwrap();
        let unauthorized = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/agents/gpu-a/probe")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(create_body.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let create = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/agents/gpu-a/probe")
                    .header(header::AUTHORIZATION, basic_header())
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(create_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(create.into_body(), 4096).await.unwrap();
        let accepted: CommandAccepted = serde_json::from_slice(&body).unwrap();

        let wrong_token = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/agent/commands/claim")
                    .header(header::AUTHORIZATION, "Bearer wrong-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&CommandClaimRequest {
                            protocol: FARHELM_PROTOCOL.to_owned(),
                            agent_id: "gpu-a".to_owned(),
                        })
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(wrong_token.status(), StatusCode::UNAUTHORIZED);

        let wrong_owner = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/agent/commands/report")
                    .header(
                        header::AUTHORIZATION,
                        "Bearer agent-token-with-at-least-32-characters",
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&CommandReportRequest {
                            protocol: FARHELM_PROTOCOL.to_owned(),
                            agent_id: "gpu-b".to_owned(),
                            command_id: accepted.command_id,
                            state: CommandState::Accepted,
                            result: None,
                            detail: None,
                        })
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(wrong_owner.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn probe_creation_rejects_invalid_and_conflicting_requests() {
        let router = app(test_state());
        let invalid = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/agents/gpu-a/probe")
                    .header(header::AUTHORIZATION, basic_header())
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&CreateProbeCommand {
                            idempotency_key: "probe-invalid-0001".to_owned(),
                            ttl_secs: 9,
                        })
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);

        for (agent_id, expected) in [
            ("gpu-a", StatusCode::ACCEPTED),
            ("gpu-b", StatusCode::CONFLICT),
        ] {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/api/v1/agents/{agent_id}/probe"))
                        .header(header::AUTHORIZATION, basic_header())
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(
                            serde_json::to_vec(&CreateProbeCommand {
                                idempotency_key: "probe-conflict-0001".to_owned(),
                                ttl_secs: 60,
                            })
                            .unwrap(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
        }
    }
}
