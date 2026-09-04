use std::{
    collections::BTreeMap,
    convert::Infallible,
    path::{Component, Path as FsPath, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Result, ensure};
use argon2::{Argon2, PasswordHash, PasswordVerifier};
use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Extension, Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, Uri, header},
    middleware::{self, Next},
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{any, get, post},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use constant_time_eq::constant_time_eq;
use farhelm_core::PRODUCT_VERSION;
use farhelm_protocol::{
    AgentCredentialState, AgentEnrollRequest, AgentEnrollResponse, AgentEventAck, AgentEventBatch,
    AgentHeartbeat, AgentHeartbeatAck, AgentListResponse, AgentSummary, CommandAccepted,
    CommandAction, CommandClaimRequest, CommandClaimResponse, CommandReportRequest,
    CreateCodexSessionRequest, CreatePairingCodeRequest, CreateProbeCommand,
    DeletePairingCodeRequest, FARHELM_PROTOCOL, HealthResponse, ImportProjectsRequest,
    PairingCodeResponse, PromptDelivery, SendCodexMessageRequest,
};
use rand::RngCore;
use reqwest::{Client, redirect::Policy};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::sync::{RwLock, broadcast};
use web_push_native::{
    Auth, WebPushBuilder,
    p256::{
        PublicKey, SecretKey,
        ecdsa::{Signature, SigningKey, signature::Signer},
        elliptic_curve::sec1::ToEncodedPoint,
    },
};

include!(concat!(env!("OUT_DIR"), "/embedded_console.rs"));

pub const ONLINE_WINDOW_SECS: u64 = 45;

mod command_store;
mod event_store;
mod typed_command_store;

use command_store::{CommandStore, CreateCommandError, ReportCommandError};
use event_store::{ArchiveFilter, EventStore, PushDelivery, StoredBrowserSession, StoredEvent};
use typed_command_store::TypedCommandStore;

#[derive(Clone)]
pub struct HubConfig {
    pub admin_user: String,
    pub admin_password: String,
    pub admin_totp_secret: Option<String>,
    pub recovery_code_hashes: Vec<String>,
    pub agent_token: String,
    pub agent_tokens: BTreeMap<String, String>,
    pub push: Option<PushConfig>,
    pub console_dir: Option<PathBuf>,
    pub database_path: PathBuf,
}

#[derive(Clone)]
pub struct PushConfig {
    pub private_key: String,
    pub public_key: String,
    pub contact: String,
}

#[derive(Clone)]
pub struct AppState {
    config: Arc<HubConfig>,
    agents: Arc<RwLock<BTreeMap<String, StoredAgent>>>,
    commands: Arc<CommandStore>,
    events: Arc<EventStore>,
    event_bus: broadcast::Sender<StoredEvent>,
    typed_commands: Arc<TypedCommandStore>,
    push_client: Client,
}

#[derive(Clone)]
enum AgentIdentity {
    Dedicated(String),
    Legacy,
}

#[derive(Clone)]
struct StoredAgent {
    hostname: String,
    agent_version: String,
    last_seen_unix: u64,
    credential_state: AgentCredentialState,
}

#[derive(Serialize)]
struct ApiError {
    error: &'static str,
}

impl AppState {
    pub fn new(config: HubConfig) -> Result<Self> {
        let commands = CommandStore::open(&config.database_path)?;
        let events = EventStore::open(&config.database_path)?;
        let typed_commands = TypedCommandStore::open(&config.database_path)?;
        let (event_bus, _) = broadcast::channel(256);
        let now = unix_time();
        for (agent_id, token) in &config.agent_tokens {
            events.import_agent_credential(agent_id, &secret_hash(token), now)?;
        }
        let push_client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(20))
            .redirect(Policy::none())
            .user_agent(format!("farhelm-hub/{PRODUCT_VERSION}"))
            .build()?;
        Ok(Self {
            config: Arc::new(config),
            agents: Arc::new(RwLock::new(BTreeMap::new())),
            commands: Arc::new(commands),
            events: Arc::new(events),
            event_bus,
            typed_commands: Arc::new(typed_commands),
            push_client,
        })
    }

    pub fn spawn_background_tasks(&self) {
        let state = self.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(10));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                if let Err(error) = deliver_pending_pushes(&state).await {
                    tracing::warn!(%error, "Web Push delivery cycle failed; retrying");
                }
            }
        });
    }

    pub fn revoke_browser_sessions(&self) -> Result<()> {
        self.events.revoke_browser_sessions()
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
            self.admin_password.starts_with("$argon2") || self.admin_password.len() >= 12,
            "admin password hash is invalid"
        );
        ensure!(
            self.agent_token.is_empty() || self.agent_token.len() >= 32,
            "legacy Agent token is invalid"
        );
        for (agent_id, token) in &self.agent_tokens {
            ensure!(
                valid_agent_id(agent_id) && token.len() >= 32,
                "dedicated Agent token is invalid"
            );
        }
        if let Some(push) = &self.push {
            let private = URL_SAFE_NO_PAD
                .decode(&push.private_key)
                .map_err(|error| anyhow::anyhow!("push.private_key is invalid: {error}"))?;
            ensure!(private.len() == 32, "push.private_key has invalid length");
            let secret = SecretKey::from_slice(&private)
                .map_err(|error| anyhow::anyhow!("push.private_key is invalid: {error}"))?;
            let public = URL_SAFE_NO_PAD
                .decode(&push.public_key)
                .map_err(|error| anyhow::anyhow!("push.public_key is invalid: {error}"))?;
            PublicKey::from_sec1_bytes(&public)
                .map_err(|_| anyhow::anyhow!("push.public_key is invalid"))?;
            ensure!(
                secret.public_key().to_encoded_point(false).as_bytes() == public,
                "push public and private keys do not match"
            );
            ensure!(
                push.contact.starts_with("mailto:") || push.contact.starts_with("https://"),
                "push.contact must use mailto: or HTTPS"
            );
        }
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
        .route("/api/v1/auth/login", post(auth_login))
        .route("/api/v1/auth/logout", post(auth_logout))
        .route("/api/v1/auth/session", get(auth_session))
        .route("/api/v1/agents", get(list_agents))
        .route(
            "/api/v1/agents/pairing-codes",
            post(create_pairing_code).delete(delete_pairing_code),
        )
        .route("/api/v1/agent/enroll", post(enroll_agent))
        .route("/api/v1/agents/heartbeat", post(agent_heartbeat))
        .route("/api/v1/agents/{agent_id}/probe", post(create_probe))
        .route("/api/v1/commands/{command_id}", get(command_status))
        .route("/api/v1/agent/commands/claim", post(claim_command))
        .route("/api/v1/agent/commands/report", post(report_command))
        .route("/api/v1/agent/events", post(agent_events))
        .route("/api/v1/experiments", get(list_experiments))
        .route("/api/v1/projects", get(list_projects))
        .route("/api/v1/projects/import", post(import_projects))
        .route(
            "/api/v1/codex/sessions",
            get(list_codex_sessions).post(create_codex_session),
        )
        .route(
            "/api/v1/codex/sessions/{session_id}",
            get(get_codex_session),
        )
        .route(
            "/api/v1/codex/sessions/{session_id}/messages",
            post(send_codex_message),
        )
        .route(
            "/api/v1/codex/sessions/{session_id}/interrupt",
            post(interrupt_codex_session),
        )
        .route("/api/v1/events/stream", get(event_stream))
        .route(
            "/api/v1/push/subscriptions",
            post(save_push_subscription).delete(delete_push_subscription),
        )
        .route("/api/v1/push/public-key", get(push_public_key))
        .route("/api/{*path}", any(api_not_found))
        .fallback(static_content)
        .layer(DefaultBodyLimit::max(512 * 1024))
        .layer(middleware::from_fn_with_state(state.clone(), authorize))
        .with_state(state)
}

#[derive(Serialize)]
struct PushPublicKeyResponse {
    public_key: String,
}

async fn push_public_key(State(state): State<AppState>) -> Response {
    match &state.config.push {
        Some(push) => Json(PushPublicKeyResponse {
            public_key: push.public_key.clone(),
        })
        .into_response(),
        None => api_error(StatusCode::SERVICE_UNAVAILABLE, "push_not_configured"),
    }
}

async fn deliver_pending_pushes(state: &AppState) -> Result<()> {
    let Some(push) = &state.config.push else {
        return Ok(());
    };
    let private = URL_SAFE_NO_PAD.decode(&push.private_key)?;
    let key_pair = SigningKey::from_slice(&private)
        .map_err(|error| anyhow::anyhow!("invalid configured VAPID key: {error}"))?;
    for delivery in state.events.pending_push_deliveries(unix_time(), 32)? {
        let Some(payload) = push_payload(&delivery) else {
            state.events.mark_push_sent(&delivery)?;
            continue;
        };
        let result = send_push(state, push, &key_pair, &delivery, payload).await;
        match result {
            Ok(()) => state.events.mark_push_sent(&delivery)?,
            Err((permanent, detail)) => {
                state
                    .events
                    .mark_push_failed(&delivery, permanent, &detail, unix_time())?;
            }
        }
    }
    Ok(())
}

async fn send_push(
    state: &AppState,
    push: &PushConfig,
    key_pair: &SigningKey,
    delivery: &PushDelivery,
    payload: serde_json::Value,
) -> std::result::Result<(), (bool, String)> {
    let public = URL_SAFE_NO_PAD
        .decode(&delivery.p256dh)
        .map_err(|_| (true, "invalid subscription public key".to_owned()))?;
    let auth = URL_SAFE_NO_PAD
        .decode(&delivery.auth)
        .map_err(|_| (true, "invalid subscription auth key".to_owned()))?;
    if auth.len() != 16 {
        return Err((true, "invalid subscription auth key length".to_owned()));
    }
    let request = WebPushBuilder::new(
        delivery
            .endpoint
            .parse()
            .map_err(|_| (true, "invalid subscription endpoint".to_owned()))?,
        PublicKey::from_sec1_bytes(&public)
            .map_err(|_| (true, "invalid subscription public key".to_owned()))?,
        Auth::clone_from_slice(&auth),
    )
    .with_valid_duration(Duration::from_secs(300))
    .build(
        serde_json::to_vec(&payload)
            .map_err(|error| (false, format!("failed to encode Push payload: {error}")))?,
    )
    .map_err(|error| (true, format!("failed to encrypt Push payload: {error}")))?;
    let (parts, body) = request.into_parts();
    let endpoint = reqwest::Url::parse(&parts.uri.to_string())
        .map_err(|_| (true, "invalid subscription endpoint".to_owned()))?;
    let authorization =
        vapid_authorization(&endpoint, push, key_pair).map_err(|detail| (true, detail))?;
    let mut outgoing = state
        .push_client
        .post(endpoint)
        .header(header::AUTHORIZATION, authorization)
        .body(body);
    for (name, value) in &parts.headers {
        outgoing = outgoing.header(name, value);
    }
    let response = outgoing
        .send()
        .await
        .map_err(|error| (false, format!("Push transport failed: {error}")))?;
    if response.status().is_success() {
        return Ok(());
    }
    let code = response.status().as_u16();
    Err((
        matches!(code, 404 | 410),
        format!("Push service returned HTTP {code}"),
    ))
}

fn vapid_authorization(
    endpoint: &reqwest::Url,
    push: &PushConfig,
    key_pair: &SigningKey,
) -> std::result::Result<String, String> {
    if endpoint.scheme() != "https" || endpoint.host_str().is_none() {
        return Err("Push endpoint must use HTTPS".to_owned());
    }
    let header_json = serde_json::json!({"typ": "JWT", "alg": "ES256"});
    let claims_json = serde_json::json!({
        "aud": endpoint.origin().ascii_serialization(),
        "exp": unix_time().saturating_add(300),
        "sub": push.contact,
    });
    let header = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&header_json)
            .map_err(|error| format!("failed to encode VAPID header: {error}"))?,
    );
    let claims = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&claims_json)
            .map_err(|error| format!("failed to encode VAPID claims: {error}"))?,
    );
    let signing_input = format!("{header}.{claims}");
    let signature: Signature = key_pair.sign(signing_input.as_bytes());
    let token = format!(
        "{signing_input}.{}",
        URL_SAFE_NO_PAD.encode(signature.to_bytes())
    );
    Ok(format!("vapid t={token}, k={}", push.public_key))
}

fn push_payload(delivery: &PushDelivery) -> Option<serde_json::Value> {
    let (summary, url) = match delivery.event_type.as_str() {
        "experiment.updated" => {
            let state = delivery.payload.get("state")?.as_str()?;
            let name = delivery
                .payload
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("实验");
            let watch_id = delivery.payload.get("watch_id")?.as_str()?;
            let label = match state {
                "succeeded" => "已完成",
                "failed" => "失败",
                "unknown" => "结果未知",
                _ => return None,
            };
            (
                format!("{name}{label}"),
                format!("/experiments?watch={}", urlencoding::encode(watch_id)),
            )
        }
        "codex.turn.completed" | "codex.turn.failed" | "codex.turn.orphaned" => {
            let session_id = delivery
                .payload
                .get("session_id")
                .and_then(serde_json::Value::as_str)
                .or_else(|| {
                    delivery
                        .payload
                        .get("data")
                        .and_then(|data| data.get("session_id"))
                        .and_then(serde_json::Value::as_str)
                })?;
            let summary = match delivery.event_type.as_str() {
                "codex.turn.completed" => "Codex 回复已完成",
                "codex.turn.failed" => "Codex 执行失败",
                _ => "Codex turn 已中断",
            };
            (
                summary.to_owned(),
                format!("/codex?session={}", urlencoding::encode(session_id)),
            )
        }
        _ => return None,
    };
    Some(serde_json::json!({
        "summary":summary,
        "event_id":delivery.event_id,
        "url":url
    }))
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

async fn authorize(State(state): State<AppState>, mut request: Request, next: Next) -> Response {
    let path = request.uri().path();
    if path == "/api/v1/health"
        || path == "/api/v1/auth/login"
        || path == "/api/v1/agent/enroll"
        || !path.starts_with("/api/")
    {
        return next.run(request).await;
    }

    if path == "/api/v1/agents/heartbeat"
        || path == "/api/v1/agent/events"
        || path.starts_with("/api/v1/agent/commands/")
    {
        if let Some(agent_id) = bearer_token(request.headers()).and_then(|token| {
            state
                .events
                .agent_for_token_hash(&secret_hash(token))
                .ok()
                .flatten()
        }) {
            request
                .extensions_mut()
                .insert(AgentIdentity::Dedicated(agent_id));
            return next.run(request).await;
        }
        if !state.config.agent_token.is_empty()
            && bearer_matches(request.headers(), &state.config.agent_token)
        {
            request.extensions_mut().insert(AgentIdentity::Legacy);
            return next.run(request).await;
        }
        return unauthorized(false);
    }

    if let Some(session) = browser_session(request.headers(), &state).await {
        if matches!(
            *request.method(),
            axum::http::Method::POST
                | axum::http::Method::PUT
                | axum::http::Method::PATCH
                | axum::http::Method::DELETE
        ) && request
            .headers()
            .get("x-csrf-token")
            .and_then(|value| value.to_str().ok())
            .is_none_or(|value| !secure_eq(value, &session.csrf_token))
        {
            return api_error(StatusCode::FORBIDDEN, "csrf_failed");
        }
        return next.run(request).await;
    }
    unauthorized(false)
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

fn bearer_matches(headers: &HeaderMap, expected: &str) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|value| secure_eq(value, expected))
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

#[derive(Debug, Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
    #[serde(default, rename = "totp")]
    _totp: Option<String>,
}

#[derive(Debug, Serialize)]
struct SessionResponse {
    authenticated: bool,
    user: String,
    csrf_token: String,
    expires_at_unix: u64,
}

async fn auth_login(State(state): State<AppState>, Json(request): Json<LoginRequest>) -> Response {
    let now = unix_time();
    match state.events.login_failure_count(now, 300) {
        Ok(count) if count >= 5 => {
            return api_error(StatusCode::TOO_MANY_REQUESTS, "login_rate_limited");
        }
        Err(error) => {
            tracing::error!(%error, "failed to read login rate limit");
            return api_error(StatusCode::INTERNAL_SERVER_ERROR, "auth_store_failed");
        }
        _ => {}
    }
    let password_ok = verify_secret(&request.password, &state.config.admin_password);
    let user_ok = secure_eq(&request.username, &state.config.admin_user);
    if !(password_ok && user_ok) {
        if let Err(error) = state.events.record_login_failure(now) {
            tracing::error!(%error, "failed to persist login failure");
        }
        return unauthorized(false);
    }
    if let Err(error) = state.events.clear_login_failures() {
        tracing::error!(%error, "failed to clear login failures");
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, "auth_store_failed");
    }
    let token = random_token();
    let csrf_token = random_token();
    let expires_at_unix = now + 30 * 24 * 60 * 60;
    if let Err(error) = state.events.save_browser_session(
        &secret_hash(&token),
        &request.username,
        &csrf_token,
        now,
        expires_at_unix,
    ) {
        tracing::error!(%error, "failed to persist browser session");
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, "auth_store_failed");
    }
    let mut response = Json(SessionResponse {
        authenticated: true,
        user: request.username,
        csrf_token,
        expires_at_unix,
    })
    .into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "farhelm_session={token}; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=2592000"
        ))
        .expect("session cookie is valid"),
    );
    response
}

async fn auth_logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(token) = cookie_value(&headers, "farhelm_session") {
        let _ = state.events.delete_browser_session(&secret_hash(token));
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_static(
            "farhelm_session=; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=0",
        ),
    );
    response
}

async fn auth_session(State(state): State<AppState>, headers: HeaderMap) -> Response {
    match browser_session(&headers, &state).await {
        Some(session) => Json(SessionResponse {
            authenticated: true,
            user: session.user,
            csrf_token: session.csrf_token,
            expires_at_unix: session.expires_at_unix,
        })
        .into_response(),
        None => unauthorized(false),
    }
}

async fn browser_session(headers: &HeaderMap, state: &AppState) -> Option<StoredBrowserSession> {
    let token = cookie_value(headers, "farhelm_session")?;
    state
        .events
        .browser_session(&secret_hash(token), unix_time())
        .ok()
        .flatten()
}

fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .map(str::trim)
        .find_map(|item| {
            let (key, value) = item.split_once('=')?;
            (key == name).then_some(value)
        })
}

fn verify_secret(secret: &str, encoded_or_plain: &str) -> bool {
    if encoded_or_plain.starts_with("$argon2") {
        return PasswordHash::new(encoded_or_plain)
            .ok()
            .is_some_and(|hash| {
                Argon2::default()
                    .verify_password(secret.as_bytes(), &hash)
                    .is_ok()
            });
    }
    secure_eq(secret, encoded_or_plain)
}

fn random_token() -> String {
    let mut bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn secret_hash(value: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(value.as_bytes()))
}

async fn api_not_found() -> (StatusCode, Json<ApiError>) {
    (StatusCode::NOT_FOUND, Json(ApiError { error: "not_found" }))
}

async fn create_pairing_code(
    State(state): State<AppState>,
    Json(request): Json<CreatePairingCodeRequest>,
) -> Response {
    if !valid_agent_id(&request.agent_id) {
        return api_error(StatusCode::BAD_REQUEST, "invalid_agent_id");
    }
    let code = format!("{:08}", rand::random::<u32>() % 100_000_000);
    let pairing_id = format!("pair_{}", &random_token()[..16]);
    let now = unix_time();
    let expires_at_unix = now + 10 * 60;
    let _ = state.events.clear_pairing_failures();
    if let Err(error) = state.events.create_pairing_code(
        &pairing_id,
        &request.agent_id,
        &secret_hash(&code),
        now,
        expires_at_unix,
    ) {
        tracing::error!(%error, agent_id = %request.agent_id, "failed to create pairing code");
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, "pairing_store_failed");
    }
    Json(PairingCodeResponse {
        protocol: FARHELM_PROTOCOL.to_owned(),
        pairing_id,
        agent_id: request.agent_id,
        code,
        expires_at_unix,
    })
    .into_response()
}

async fn delete_pairing_code(
    State(state): State<AppState>,
    Json(request): Json<DeletePairingCodeRequest>,
) -> Response {
    match state.events.delete_pairing_code(&request.pairing_id) {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => api_error(StatusCode::NOT_FOUND, "pairing_code_not_found"),
        Err(error) => {
            tracing::error!(%error, "failed to delete pairing code");
            api_error(StatusCode::INTERNAL_SERVER_ERROR, "pairing_store_failed")
        }
    }
}

async fn enroll_agent(
    State(state): State<AppState>,
    Json(request): Json<AgentEnrollRequest>,
) -> Response {
    let now = unix_time();
    if request.protocol != FARHELM_PROTOCOL
        || request.pairing_code.len() != 8
        || !request
            .pairing_code
            .bytes()
            .all(|byte| byte.is_ascii_digit())
        || request.hostname.is_empty()
        || request.hostname.len() > 255
        || request.agent_version.is_empty()
        || request.agent_version.len() > 32
    {
        return api_error(StatusCode::BAD_REQUEST, "invalid_enrollment");
    }
    if state
        .events
        .pairing_failure_count(now, 10 * 60)
        .unwrap_or(5)
        >= 5
    {
        return api_error(StatusCode::TOO_MANY_REQUESTS, "pairing_rate_limited");
    }
    let record = match state
        .events
        .pairing_by_hash(&secret_hash(&request.pairing_code))
    {
        Ok(Some(record))
            if !record.consumed && record.attempts < 5 && record.expires_at_unix > now =>
        {
            record
        }
        _ => {
            let _ = state.events.record_pairing_failure(now);
            return unauthorized(false);
        }
    };
    let token = random_token();
    let transaction_result = state
        .events
        .consume_pairing_and_set_credential(
            &record.pairing_id,
            &record.agent_id,
            &secret_hash(&token),
            now,
        )
        .and_then(|consumed| {
            ensure!(consumed, "pairing code was already consumed");
            Ok(())
        });
    if let Err(error) = transaction_result {
        tracing::warn!(%error, agent_id = %record.agent_id, "Agent enrollment rejected");
        return api_error(StatusCode::CONFLICT, "pairing_code_unavailable");
    }
    let _ = state.events.clear_pairing_failures();
    Json(AgentEnrollResponse {
        protocol: FARHELM_PROTOCOL.to_owned(),
        agent_id: record.agent_id,
        token,
    })
    .into_response()
}

async fn agent_heartbeat(
    State(state): State<AppState>,
    Extension(identity): Extension<AgentIdentity>,
    Json(heartbeat): Json<AgentHeartbeat>,
) -> Result<Json<AgentHeartbeatAck>, (StatusCode, Json<ApiError>)> {
    validate_heartbeat(&heartbeat)?;
    if !identity_allows(&identity, &heartbeat.agent_id) {
        return Err((
            StatusCode::FORBIDDEN,
            Json(ApiError {
                error: "agent_token_scope",
            }),
        ));
    }
    let now = unix_time();
    let credential_state = match identity {
        AgentIdentity::Dedicated(_) => AgentCredentialState::Paired,
        AgentIdentity::Legacy => AgentCredentialState::NeedsPairing,
    };
    state.agents.write().await.insert(
        heartbeat.agent_id,
        StoredAgent {
            hostname: heartbeat.hostname,
            agent_version: heartbeat.agent_version,
            last_seen_unix: now,
            credential_state,
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
            credential_state: stored.credential_state,
        })
        .collect();
    Json(AgentListResponse {
        protocol: FARHELM_PROTOCOL.to_owned(),
        agents,
    })
}

async fn agent_events(
    State(state): State<AppState>,
    Extension(identity): Extension<AgentIdentity>,
    Json(batch): Json<AgentEventBatch>,
) -> Response {
    if !matches!(&identity, AgentIdentity::Dedicated(agent_id) if agent_id == &batch.agent_id) {
        return api_error(StatusCode::FORBIDDEN, "dedicated_agent_token_required");
    }
    if batch.protocol != FARHELM_PROTOCOL
        || batch.events.is_empty()
        || batch.events.len() > 100
        || batch
            .events
            .iter()
            .any(|event| event.agent_id != batch.agent_id)
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: "invalid_event_batch",
            }),
        )
            .into_response();
    }
    let inserted = match state.events.ingest(&batch.agent_id, &batch.events) {
        Ok(inserted) => inserted,
        Err(error) => {
            tracing::warn!(%error, agent_id = %batch.agent_id, "Agent event batch rejected");
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiError {
                    error: "invalid_event_batch",
                }),
            )
                .into_response();
        }
    };
    for event in inserted {
        let _ = state.event_bus.send(event);
    }
    Json(AgentEventAck {
        protocol: FARHELM_PROTOCOL.to_owned(),
        accepted_event_ids: batch
            .events
            .into_iter()
            .map(|event| event.event_id)
            .collect(),
    })
    .into_response()
}

async fn list_experiments(State(state): State<AppState>) -> Response {
    match state.events.experiments() {
        Ok(experiments) => Json(experiments).into_response(),
        Err(error) => {
            tracing::error!(%error, "failed to list experiments");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError {
                    error: "internal_error",
                }),
            )
                .into_response()
        }
    }
}

async fn list_projects(State(state): State<AppState>) -> Response {
    match state.events.projects() {
        Ok(projects) => Json(projects).into_response(),
        Err(error) => {
            tracing::error!(%error, "failed to list project candidates");
            api_error(StatusCode::INTERNAL_SERVER_ERROR, "event_store_failed")
        }
    }
}

async fn import_projects(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ImportProjectsRequest>,
) -> Response {
    if !valid_agent_id(&request.agent_id)
        || request.candidate_ids.is_empty()
        || request.candidate_ids.len() > 100
        || request
            .candidate_ids
            .iter()
            .any(|id| id.is_empty() || id.len() > 128)
    {
        return api_error(StatusCode::BAD_REQUEST, "invalid_project_import");
    }
    for candidate_id in &request.candidate_ids {
        match state.events.project_candidate_agent(candidate_id) {
            Ok(Some(agent_id)) if agent_id == request.agent_id => {}
            Ok(_) => return api_error(StatusCode::BAD_REQUEST, "invalid_project_candidate"),
            Err(error) => {
                tracing::error!(%error, "failed to validate project candidate");
                return api_error(StatusCode::INTERNAL_SERVER_ERROR, "event_store_failed");
            }
        }
    }
    let Some(key) = idempotency_header(&headers) else {
        return api_error(StatusCode::BAD_REQUEST, "missing_idempotency_key");
    };
    create_typed_response(
        &state,
        &request.agent_id,
        CommandAction::ProjectApprove,
        serde_json::json!({"candidate_ids": request.candidate_ids}),
        key,
        24 * 60 * 60,
    )
}

#[derive(Debug, Deserialize)]
struct SessionQuery {
    project: Option<String>,
    archived: Option<String>,
}

async fn list_codex_sessions(
    State(state): State<AppState>,
    Query(query): Query<SessionQuery>,
) -> Response {
    let archived = match query.archived.as_deref().unwrap_or("false") {
        "false" => ArchiveFilter::Current,
        "true" => ArchiveFilter::Archived,
        "all" => ArchiveFilter::All,
        _ => return api_error(StatusCode::BAD_REQUEST, "invalid_archive_filter"),
    };
    match state.events.sessions(query.project.as_deref(), archived) {
        Ok(sessions) => Json(sessions).into_response(),
        Err(error) => {
            tracing::error!(%error, "failed to list Codex sessions");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError {
                    error: "internal_error",
                }),
            )
                .into_response()
        }
    }
}

async fn create_codex_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateCodexSessionRequest>,
) -> Response {
    if !valid_agent_id(&request.agent_id) || !valid_project_id(&request.project_id) {
        return api_error(StatusCode::BAD_REQUEST, "invalid_session");
    }
    let Some(key) = idempotency_header(&headers) else {
        return api_error(StatusCode::BAD_REQUEST, "missing_idempotency_key");
    };
    let payload = serde_json::json!({"project_id":request.project_id,"mode":request.mode});
    create_typed_response(
        &state,
        &request.agent_id,
        CommandAction::CodexSessionCreate,
        payload,
        key,
        300,
    )
}

async fn get_codex_session(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Response {
    match state.events.session(&session_id) {
        Ok(Some(session)) => Json(session).into_response(),
        Ok(None) => api_error(StatusCode::NOT_FOUND, "session_not_found"),
        Err(error) => {
            tracing::error!(%error, "failed to read Codex session");
            api_error(StatusCode::INTERNAL_SERVER_ERROR, "event_store_failed")
        }
    }
}

async fn send_codex_message(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<SendCodexMessageRequest>,
) -> Response {
    if request.prompt.is_empty() || request.prompt.len() > 32 * 1024 {
        return api_error(StatusCode::BAD_REQUEST, "invalid_prompt");
    }
    let Some(key) = idempotency_header(&headers) else {
        return api_error(StatusCode::BAD_REQUEST, "missing_idempotency_key");
    };
    let session = match state.events.session(&session_id) {
        Ok(Some(session)) => session,
        Ok(None) => return api_error(StatusCode::NOT_FOUND, "session_not_found"),
        Err(error) => {
            tracing::error!(%error, "failed to read Codex session");
            return api_error(StatusCode::INTERNAL_SERVER_ERROR, "event_store_failed");
        }
    };
    let (action, turn_id) = match request.delivery {
        PromptDelivery::Queue => (CommandAction::CodexTurnStart, None),
        PromptDelivery::Steer => {
            let Some(turn_id) = session.active_turn_id.clone() else {
                return api_error(StatusCode::CONFLICT, "session_is_not_running");
            };
            (CommandAction::CodexTurnSteer, Some(turn_id))
        }
    };
    let payload = serde_json::json!({
        "session_id":session_id,"project_id":session.project_id,"mode":session.mode,
        "turn_id":turn_id,"prompt":request.prompt,"delivery":request.delivery
    });
    create_typed_response(&state, &session.agent_id, action, payload, key, 300)
}

async fn interrupt_codex_session(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let Some(key) = idempotency_header(&headers) else {
        return api_error(StatusCode::BAD_REQUEST, "missing_idempotency_key");
    };
    let session = match state.events.session(&session_id) {
        Ok(Some(session)) => session,
        Ok(None) => return api_error(StatusCode::NOT_FOUND, "session_not_found"),
        Err(error) => {
            tracing::error!(%error, "failed to read Codex session");
            return api_error(StatusCode::INTERNAL_SERVER_ERROR, "event_store_failed");
        }
    };
    let Some(turn_id) = session.active_turn_id else {
        return api_error(StatusCode::CONFLICT, "session_is_not_running");
    };
    create_typed_response(
        &state,
        &session.agent_id,
        CommandAction::CodexTurnInterrupt,
        serde_json::json!({"session_id":session_id,"project_id":session.project_id,"turn_id":turn_id}),
        key,
        300,
    )
}

fn create_typed_response(
    state: &AppState,
    agent_id: &str,
    action: CommandAction,
    payload: serde_json::Value,
    idempotency_key: &str,
    ttl: u64,
) -> Response {
    match state.typed_commands.create(
        agent_id,
        action,
        &payload,
        idempotency_key,
        ttl,
        unix_time(),
    ) {
        Ok(command) => (
            StatusCode::ACCEPTED,
            Json(CommandAccepted {
                protocol: FARHELM_PROTOCOL.to_owned(),
                command_id: command.command_id.clone(),
                state: command.state,
                expires_at_unix: command.expires_at_unix,
                status_url: format!("/api/v1/commands/{}", command.command_id),
            }),
        )
            .into_response(),
        Err(error) => {
            tracing::warn!(%error, "failed to create typed command");
            api_error(StatusCode::CONFLICT, "idempotency_conflict")
        }
    }
}

fn idempotency_header(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .filter(|value| valid_idempotency_key(value))
}

fn valid_project_id(value: &str) -> bool {
    valid_agent_id(value)
}

async fn event_stream(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let after = headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    // Subscribe before reading history so an event committed between replay and
    // live delivery remains buffered in the receiver. Sequence filtering below
    // removes the resulting overlap.
    let mut receiver = state.event_bus.subscribe();
    let replay = match state.events.replay(after, 1000) {
        Ok(replay) => replay,
        Err(error) => {
            tracing::error!(%error, "failed to replay events");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError {
                    error: "internal_error",
                }),
            )
                .into_response();
        }
    };
    let stream = async_stream::stream! {
        let mut cursor = after;
        for event in replay {
            cursor = cursor.max(event.sequence);
            yield Ok::<Event, Infallible>(sse_event(&event));
        }
        loop {
            let page = match state.events.replay(cursor, 1000) {
                Ok(page) => page,
                Err(error) => {
                    tracing::warn!(%error, "failed to continue SSE replay");
                    break;
                }
            };
            if page.is_empty() {
                break;
            }
            for event in page {
                if event.sequence > cursor {
                    cursor = event.sequence;
                    yield Ok::<Event, Infallible>(sse_event(&event));
                }
            }
        }
        loop {
            match receiver.recv().await {
                Ok(event) if event.sequence > cursor => {
                    cursor = event.sequence;
                    yield Ok::<Event, Infallible>(sse_event(&event));
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => loop {
                    let page = match state.events.replay(cursor, 1000) {
                        Ok(page) => page,
                        Err(error) => {
                            tracing::warn!(%error, "failed to recover lagged SSE stream");
                            break;
                        }
                    };
                    let page_len = page.len();
                    for event in page {
                        if event.sequence > cursor {
                            cursor = event.sequence;
                            yield Ok::<Event, Infallible>(sse_event(&event));
                        }
                    }
                    if page_len < 1000 {
                        break;
                    }
                },
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };
    Sse::new(stream)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("keepalive"),
        )
        .into_response()
}

fn sse_event(event: &StoredEvent) -> Event {
    Event::default()
        .id(event.sequence.to_string())
        .event(event.event_type.clone())
        .data(
            serde_json::to_string(
                &serde_json::json!({"event_id":event.event_id,"payload":event.payload}),
            )
            .unwrap_or_else(|_| "{}".to_owned()),
        )
}

#[derive(Debug, Deserialize)]
struct PushKeys {
    p256dh: String,
    auth: String,
}
#[derive(Debug, Deserialize)]
struct PushSubscriptionRequest {
    endpoint: String,
    keys: PushKeys,
}
#[derive(Debug, Deserialize)]
struct DeletePushSubscriptionRequest {
    endpoint: String,
}

async fn save_push_subscription(
    State(state): State<AppState>,
    Json(request): Json<PushSubscriptionRequest>,
) -> Response {
    let valid_endpoint = reqwest::Url::parse(&request.endpoint).is_ok_and(|endpoint| {
        endpoint.scheme() == "https"
            && endpoint.host_str().is_some()
            && endpoint.username().is_empty()
            && endpoint.password().is_none()
    });
    let valid_keys = valid_endpoint
        && URL_SAFE_NO_PAD
            .decode(&request.keys.p256dh)
            .ok()
            .and_then(|key| PublicKey::from_sec1_bytes(&key).ok())
            .is_some()
        && URL_SAFE_NO_PAD
            .decode(&request.keys.auth)
            .is_ok_and(|auth| auth.len() == 16);
    if !valid_keys {
        return api_error(StatusCode::BAD_REQUEST, "invalid_push_subscription");
    }
    match state.events.save_push_subscription(
        &request.endpoint,
        &request.keys.p256dh,
        &request.keys.auth,
        unix_time(),
    ) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => {
            tracing::warn!(%error,"push subscription rejected");
            api_error(StatusCode::BAD_REQUEST, "invalid_push_subscription")
        }
    }
}

async fn delete_push_subscription(
    State(state): State<AppState>,
    Json(request): Json<DeletePushSubscriptionRequest>,
) -> Response {
    match state.events.delete_push_subscription(&request.endpoint) {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => {
            tracing::error!(%error,"push subscription deletion failed");
            api_error(StatusCode::INTERNAL_SERVER_ERROR, "event_store_failed")
        }
    }
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
    if command_id.starts_with("cmd_cdx_") {
        return match state.typed_commands.get(&command_id) {
            Ok(Some(command)) => Json(command).into_response(),
            Ok(None) => api_error(StatusCode::NOT_FOUND, "command_not_found"),
            Err(error) => {
                tracing::error!(%error, "failed to read typed command");
                api_error(StatusCode::INTERNAL_SERVER_ERROR, "command_store_failed")
            }
        };
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
    Extension(identity): Extension<AgentIdentity>,
    Json(request): Json<CommandClaimRequest>,
) -> Response {
    if request.protocol != FARHELM_PROTOCOL || !valid_agent_id(&request.agent_id) {
        return api_error(StatusCode::BAD_REQUEST, "invalid_claim");
    }
    if !identity_allows(&identity, &request.agent_id) {
        return api_error(StatusCode::FORBIDDEN, "agent_token_scope");
    }
    if matches!(identity, AgentIdentity::Dedicated(_)) {
        match state.typed_commands.claim(&request.agent_id, unix_time()) {
            Ok(Some(command)) => {
                return Json(CommandClaimResponse {
                    protocol: FARHELM_PROTOCOL.to_owned(),
                    command: Some(command),
                })
                .into_response();
            }
            Ok(None) => {}
            Err(error) => {
                tracing::error!(%error, agent_id=%request.agent_id, "failed to claim typed command");
                return api_error(StatusCode::INTERNAL_SERVER_ERROR, "command_store_failed");
            }
        }
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
    Extension(identity): Extension<AgentIdentity>,
    Json(report): Json<CommandReportRequest>,
) -> Response {
    if report.protocol != FARHELM_PROTOCOL
        || !valid_agent_id(&report.agent_id)
        || !valid_command_id(&report.command_id)
    {
        return api_error(StatusCode::BAD_REQUEST, "invalid_report");
    }
    if !identity_allows(&identity, &report.agent_id) {
        return api_error(StatusCode::FORBIDDEN, "agent_token_scope");
    }
    if report.command_id.starts_with("cmd_cdx_") {
        if matches!(identity, AgentIdentity::Legacy) {
            return api_error(StatusCode::FORBIDDEN, "dedicated_agent_token_required");
        }
        return match state.typed_commands.report(&report, unix_time()) {
            Ok(command) => Json(command).into_response(),
            Err(error) => {
                tracing::warn!(%error, command_id=%report.command_id, "typed command report rejected");
                api_error(StatusCode::CONFLICT, "invalid_command_transition")
            }
        };
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

fn identity_allows(identity: &AgentIdentity, agent_id: &str) -> bool {
    match identity {
        AgentIdentity::Dedicated(expected) => secure_eq(expected, agent_id),
        AgentIdentity::Legacy => true,
    }
}

fn valid_idempotency_key(value: &str) -> bool {
    (16..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

fn valid_command_id(value: &str) -> bool {
    (value.len() == 20
        && value.starts_with("cmd_")
        && value[4..].bytes().all(|byte| byte.is_ascii_hexdigit()))
        || (value.len() == 24
            && value.starts_with("cmd_cdx_")
            && value[8..].bytes().all(|byte| byte.is_ascii_hexdigit()))
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
        AgentEvent, AgentEventBatch, AgentHeartbeat, CommandAccepted, CommandClaimRequest,
        CommandClaimResponse, CommandReportRequest, CommandState, CommandStatusResponse,
        CreateProbeCommand, FARHELM_PROTOCOL, HealthResponse, HealthStatus, ProbeResult,
    };
    use tower::ServiceExt;

    use super::*;

    fn test_state() -> AppState {
        let state = AppState::new(HubConfig {
            admin_user: "admin".to_owned(),
            admin_password: "correct-horse".to_owned(),
            admin_totp_secret: None,
            recovery_code_hashes: Vec::new(),
            agent_token: "agent-token-with-at-least-32-characters".to_owned(),
            agent_tokens: BTreeMap::new(),
            push: None,
            console_dir: Some(PathBuf::from("missing-test-console")),
            database_path: PathBuf::from(":memory:"),
        })
        .unwrap();
        state
            .events
            .save_browser_session(
                &secret_hash("test-session"),
                "admin",
                "test-csrf",
                1,
                u64::MAX / 2,
            )
            .unwrap();
        state
    }

    #[tokio::test]
    async fn password_cookie_and_csrf_form_one_persistent_session_boundary() {
        let router = app(test_state());
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({"username":"admin","password":"correct-horse"})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        let session: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let csrf = session["csrf_token"].as_str().unwrap();
        let restored = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/auth/session")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(restored.status(), StatusCode::OK);
        let rejected = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/logout")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
        let logout = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/logout")
                    .header(header::COOKIE, cookie)
                    .header("x-csrf-token", csrf)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(logout.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn browser_session_survives_hub_state_recreation() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("hub.db");
        let config = HubConfig {
            database_path: database,
            ..test_state().config.as_ref().clone()
        };
        let first = app(AppState::new(config.clone()).unwrap());
        let login = first
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"username":"admin","password":"correct-horse"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let cookie = login
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let restored = app(AppState::new(config).unwrap())
            .oneshot(
                Request::builder()
                    .uri("/api/v1/auth/session")
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(restored.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn pairing_code_is_bound_one_time_and_returns_a_scoped_token() {
        let state = test_state();
        let router = app(state);
        let created = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/agents/pairing-codes")
                    .header(header::COOKIE, browser_cookie())
                    .header("x-csrf-token", "test-csrf")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"agent_id":"titan"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::OK);
        let value: serde_json::Value =
            serde_json::from_slice(&to_bytes(created.into_body(), 4096).await.unwrap()).unwrap();
        let code = value["code"].as_str().unwrap();
        assert_eq!(code.len(), 8);
        let body = serde_json::json!({"protocol":FARHELM_PROTOCOL,"pairing_code":code,"hostname":"titan-rtx","agent_version":"0.5.0"}).to_string();
        let enrolled = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/agent/enroll")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(enrolled.status(), StatusCode::OK);
        let enrollment: AgentEnrollResponse =
            serde_json::from_slice(&to_bytes(enrolled.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(enrollment.agent_id, "titan");
        assert!(enrollment.token.len() >= 32);
        let replay = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/agent/enroll")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(replay.status(), StatusCode::UNAUTHORIZED);
        let heartbeat = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/agents/heartbeat")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", enrollment.token),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&AgentHeartbeat::new("titan", "titan-rtx", "0.5.0"))
                            .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(heartbeat.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn dedicated_agent_events_are_deduplicated_and_materialized() {
        let mut config = test_state().config.as_ref().clone();
        config.agent_tokens.insert(
            "gpu-a".to_owned(),
            "dedicated-agent-token-with-32-characters".to_owned(),
        );
        let state = AppState::new(config).unwrap();
        state
            .events
            .save_browser_session(
                &secret_hash("test-session"),
                "admin",
                "test-csrf",
                1,
                u64::MAX / 2,
            )
            .unwrap();
        let router = app(state);
        let event = AgentEvent {
            protocol: FARHELM_PROTOCOL.to_owned(),
            event_id: "watch-1:succeeded".to_owned(),
            agent_id: "gpu-a".to_owned(),
            sequence: 1,
            event_type: "experiment.updated".to_owned(),
            created_at_unix: 100,
            payload: serde_json::json!({"watch_id":"watch-1","agent_id":"gpu-a","project_id":"cc08","name":"trial","pid":42,"state":"succeeded","session_id":"ses_1","detail":"matched","updated_at_unix":100}),
        };
        for _ in 0..2 {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/v1/agent/events")
                        .header(
                            header::AUTHORIZATION,
                            "Bearer dedicated-agent-token-with-32-characters",
                        )
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(
                            serde_json::to_vec(&AgentEventBatch {
                                protocol: FARHELM_PROTOCOL.to_owned(),
                                agent_id: "gpu-a".to_owned(),
                                events: vec![event.clone()],
                            })
                            .unwrap(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/api/v1/experiments")
                    .header(header::COOKIE, browser_cookie())
                    .header("x-csrf-token", "test-csrf")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["experiments"].as_array().unwrap().len(), 1);
        assert_eq!(value["experiments"][0]["state"], "succeeded");
    }

    fn browser_cookie() -> String {
        "farhelm_session=test-session".to_owned()
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
    async fn admin_api_requires_authentication() {
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
        assert!(!response.headers().contains_key(header::WWW_AUTHENTICATE));
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
                    .header(header::COOKIE, browser_cookie())
                    .header("x-csrf-token", "test-csrf")
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
                    .header(header::COOKIE, browser_cookie())
                    .header("x-csrf-token", "test-csrf")
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
                    .header(header::COOKIE, browser_cookie())
                    .header("x-csrf-token", "test-csrf")
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
            admin_totp_secret: None,
            recovery_code_hashes: Vec::new(),
            agent_token: "short".to_owned(),
            agent_tokens: BTreeMap::new(),
            push: None,
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

    #[test]
    fn push_payload_contains_only_summary_identity_and_deep_link() {
        let delivery = PushDelivery {
            event_sequence: 1,
            event_id: "event-a".into(),
            event_type: "experiment.updated".into(),
            payload: serde_json::json!({
                "watch_id":"watch-a","name":"训练一","state":"succeeded",
                "prompt":"secret prompt","log":"secret log"
            }),
            endpoint: "https://push.example.test/id".into(),
            p256dh: String::new(),
            auth: String::new(),
            attempts: 0,
        };
        let payload = push_payload(&delivery).unwrap();
        assert_eq!(payload["event_id"], "event-a");
        assert_eq!(payload["url"], "/experiments?watch=watch-a");
        let encoded = serde_json::to_string(&payload).unwrap();
        assert!(!encoded.contains("secret"));
        assert_eq!(payload.as_object().unwrap().len(), 3);
    }

    #[test]
    fn vapid_authorization_is_a_verifiable_es256_jwt() {
        use web_push_native::p256::ecdsa::{VerifyingKey, signature::Verifier};

        let private = [7_u8; 32];
        let signing_key = SigningKey::from_slice(&private).unwrap();
        let public = signing_key.verifying_key().to_encoded_point(false);
        let push = PushConfig {
            private_key: URL_SAFE_NO_PAD.encode(private),
            public_key: URL_SAFE_NO_PAD.encode(public.as_bytes()),
            contact: "mailto:admin@example.test".to_owned(),
        };
        let endpoint = reqwest::Url::parse("https://push.example.test:8443/sub/1").unwrap();
        let authorization = vapid_authorization(&endpoint, &push, &signing_key).unwrap();
        let value = authorization.strip_prefix("vapid t=").unwrap();
        let (token, encoded_key) = value.split_once(", k=").unwrap();
        assert_eq!(encoded_key, push.public_key);
        let parts: Vec<_> = token.split('.').collect();
        assert_eq!(parts.len(), 3);
        let claims: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(claims["aud"], "https://push.example.test:8443");
        assert_eq!(claims["sub"], push.contact);
        let signature = Signature::from_slice(&URL_SAFE_NO_PAD.decode(parts[2]).unwrap()).unwrap();
        let verifier =
            VerifyingKey::from_sec1_bytes(&URL_SAFE_NO_PAD.decode(encoded_key).unwrap()).unwrap();
        verifier
            .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
            .unwrap();
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
                    .header(header::COOKIE, browser_cookie())
                    .header("x-csrf-token", "test-csrf")
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
                                data: None,
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
                    .header(header::COOKIE, browser_cookie())
                    .header("x-csrf-token", "test-csrf")
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
                    .header(header::COOKIE, browser_cookie())
                    .header("x-csrf-token", "test-csrf")
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
                            data: None,
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
                    .header(header::COOKIE, browser_cookie())
                    .header("x-csrf-token", "test-csrf")
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
                        .header(header::COOKIE, browser_cookie())
                        .header("x-csrf-token", "test-csrf")
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
