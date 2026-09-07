//! Versioned wire contracts shared by FarHelm components.

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const FARHELM_PROTOCOL: &str = "farhelm/1";
pub const WORKER_PROTOCOL: &str = "farhelm-worker/1";
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    Ok,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: HealthStatus,
    pub service: String,
    pub version: String,
    pub protocol: String,
}

impl HealthResponse {
    #[must_use]
    pub fn hub(version: impl Into<String>) -> Self {
        Self {
            status: HealthStatus::Ok,
            service: "farhelm-hub".to_owned(),
            version: version.into(),
            protocol: FARHELM_PROTOCOL.to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentHeartbeat {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    pub protocol: String,
    pub agent_id: String,
    pub hostname: String,
    pub agent_version: String,
}

impl AgentHeartbeat {
    #[must_use]
    pub fn new(
        agent_id: impl Into<String>,
        hostname: impl Into<String>,
        agent_version: impl Into<String>,
    ) -> Self {
        Self {
            capabilities: vec!["codex.ephemeral_submit".to_owned()],
            protocol: FARHELM_PROTOCOL.to_owned(),
            agent_id: agent_id.into(),
            hostname: hostname.into(),
            agent_version: agent_version.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentHeartbeatAck {
    pub accepted: bool,
    pub protocol: String,
    pub server_time_unix: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSummary {
    pub agent_id: String,
    pub hostname: String,
    pub agent_version: String,
    pub last_seen_unix: u64,
    pub online: bool,
    #[serde(default)]
    pub credential_state: AgentCredentialState,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentCredentialState {
    #[default]
    Paired,
    Legacy,
    NeedsPairing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentListResponse {
    pub protocol: String,
    pub agents: Vec<AgentSummary>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommandAction {
    #[serde(rename = "agent.probe")]
    AgentProbe,
    #[serde(rename = "codex.session.create")]
    CodexSessionCreate,
    #[serde(rename = "codex.session.resume")]
    CodexSessionResume,
    #[serde(rename = "codex.turn.start")]
    CodexTurnStart,
    #[serde(rename = "codex.turn.steer")]
    CodexTurnSteer,
    #[serde(rename = "codex.turn.interrupt")]
    CodexTurnInterrupt,
    #[serde(rename = "codex.schedule.create")]
    CodexScheduleCreate,
    #[serde(rename = "codex.schedule.cancel")]
    CodexScheduleCancel,
    #[serde(rename = "project.approve")]
    ProjectApprove,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CommandState {
    Queued,
    Delivered,
    Accepted,
    Completed,
    Failed,
    Expired,
    Cancelled,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateProbeCommand {
    pub idempotency_key: String,
    pub ttl_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCommand {
    pub protocol: String,
    pub command_id: String,
    pub agent_id: String,
    pub action: CommandAction,
    pub created_at_unix: u64,
    pub expires_at_unix: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandAccepted {
    pub protocol: String,
    pub command_id: String,
    pub state: CommandState,
    pub expires_at_unix: u64,
    pub status_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandClaimRequest {
    pub protocol: String,
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_secs: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandClaimResponse {
    pub protocol: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<AgentCommand>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeResult {
    pub agent_version: String,
    pub hostname: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandReportRequest {
    pub protocol: String,
    pub agent_id: String,
    pub command_id: String,
    pub state: CommandState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<ProbeResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandStatusResponse {
    pub protocol: String,
    pub command_id: String,
    pub agent_id: String,
    pub action: CommandAction,
    pub state: CommandState,
    pub created_at_unix: u64,
    pub expires_at_unix: u64,
    pub updated_at_unix: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<ProbeResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExperimentState {
    Watching,
    Succeeded,
    Failed,
    Unknown,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodexSessionState {
    Creating,
    Idle,
    Queued,
    Running,
    Interrupting,
    Failed,
    Orphaned,
    Archived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodexSessionMode {
    Inspect,
    Edit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperimentSummary {
    pub watch_id: String,
    pub agent_id: String,
    pub project_id: String,
    pub name: String,
    pub pid: u32,
    pub state: ExperimentState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub updated_at_unix: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperimentListResponse {
    pub protocol: String,
    pub experiments: Vec<ExperimentSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexSessionSummary {
    pub session_id: String,
    pub agent_id: String,
    pub project_id: String,
    pub mode: CodexSessionMode,
    pub state: CodexSessionState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_turn_id: Option<String>,
    pub updated_at_unix: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexSessionListResponse {
    pub protocol: String,
    pub sessions: Vec<CodexSessionSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodexTranscriptItemKind {
    UserMessage,
    AssistantMessage,
    CommandSummary,
    FileChangeSummary,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexTranscriptItem {
    pub item_id: String,
    pub kind: CodexTranscriptItemKind,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexTranscriptTurn {
    pub turn_id: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_unix: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_unix: Option<u64>,
    pub items: Vec<CodexTranscriptItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexTranscriptPage {
    pub protocol: String,
    pub session_id: String,
    pub turns: Vec<CodexTranscriptTurn>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodexScheduleState {
    Pending,
    Queued,
    Running,
    Completed,
    Cancelled,
    Skipped,
    Missed,
    Failed,
    Orphaned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CodexScheduleTrigger {
    AtTime { run_at_unix: u64 },
    ExperimentSucceeded { watch_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateCodexScheduleRequest {
    pub prompt: String,
    pub trigger: CodexScheduleTrigger,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexScheduleSummary {
    pub schedule_id: String,
    pub agent_id: String,
    pub session_id: String,
    pub project_id: String,
    pub trigger: CodexScheduleTrigger,
    pub state: CodexScheduleState,
    pub created_at_unix: u64,
    pub updated_at_unix: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexScheduleListResponse {
    pub protocol: String,
    pub schedules: Vec<CodexScheduleSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexScheduleDetail {
    #[serde(flatten)]
    pub summary: CodexScheduleSummary,
    pub prompt: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentReadClaimRequest {
    pub protocol: String,
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_secs: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentReadRequest {
    pub request_id: String,
    pub method: String,
    pub params: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentReadClaimResponse {
    pub protocol: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request: Option<AgentReadRequest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentReadReportRequest {
    pub protocol: String,
    pub agent_id: String,
    pub request_id: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatePairingCodeRequest {
    pub agent_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletePairingCodeRequest {
    pub pairing_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingCodeResponse {
    pub protocol: String,
    pub pairing_id: String,
    pub agent_id: String,
    pub code: String,
    pub expires_at_unix: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentEnrollRequest {
    pub protocol: String,
    pub pairing_code: String,
    pub hostname: String,
    pub agent_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentEnrollResponse {
    pub protocol: String,
    pub agent_id: String,
    pub token: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectCandidateState {
    Discovered,
    Approved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectCandidateSummary {
    pub candidate_id: String,
    pub agent_id: String,
    pub display_name: String,
    pub suggested_project_id: String,
    pub session_count: u64,
    pub state: ProjectCandidateState,
    pub updated_at_unix: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectListResponse {
    pub protocol: String,
    pub projects: Vec<ProjectCandidateSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportProjectsRequest {
    pub agent_id: String,
    pub candidate_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateCodexSessionRequest {
    pub agent_id: String,
    pub project_id: String,
    pub mode: CodexSessionMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PromptDelivery {
    Queue,
    Steer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendCodexMessageRequest {
    pub prompt: String,
    #[serde(default = "default_prompt_delivery")]
    pub delivery: PromptDelivery,
}

const fn default_prompt_delivery() -> PromptDelivery {
    PromptDelivery::Queue
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentEvent {
    pub protocol: String,
    pub event_id: String,
    pub agent_id: String,
    pub sequence: u64,
    pub event_type: String,
    pub created_at_unix: u64,
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentEventBatch {
    pub protocol: String,
    pub agent_id: String,
    pub events: Vec<AgentEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentEventAck {
    pub protocol: String,
    pub accepted_event_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerEvent {
    pub protocol: String,
    pub kind: String,
    pub event: String,
    pub data: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerRequest {
    pub protocol: String,
    pub kind: String,
    pub request_id: String,
    pub method: String,
    pub params: Value,
}

impl WorkerRequest {
    #[must_use]
    pub fn hello(request_id: impl Into<String>, agent_version: impl Into<String>) -> Self {
        Self {
            protocol: WORKER_PROTOCOL.to_owned(),
            kind: "request".to_owned(),
            request_id: request_id.into(),
            method: "worker.hello".to_owned(),
            params: serde_json::json!({
                "agent_version": agent_version.into(),
                "supported_protocols": [WORKER_PROTOCOL]
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerResponse {
    pub protocol: String,
    pub kind: String,
    pub request_id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<WorkerError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerHelloResult {
    pub worker: String,
    pub version: String,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("frame I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("frame length {actual} exceeds maximum {maximum}")]
    TooLarge { actual: usize, maximum: usize },
    #[error("frame JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
}

pub async fn write_frame<W, T>(writer: &mut W, value: &T) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
    T: Serialize + ?Sized,
{
    let payload = serde_json::to_vec(value)?;
    if payload.len() > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge {
            actual: payload.len(),
            maximum: MAX_FRAME_BYTES,
        });
    }

    writer.write_u32(payload.len() as u32).await?;
    writer.write_all(&payload).await?;
    writer.flush().await?;
    Ok(())
}

pub async fn read_frame<R, T>(reader: &mut R) -> Result<T, FrameError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let length = reader.read_u32().await? as usize;
    if length > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge {
            actual: length,
            maximum: MAX_FRAME_BYTES,
        });
    }

    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload).await?;
    Ok(serde_json::from_slice(&payload)?)
}

/// Only this metadata crosses into durable Hub events. Transcript content uses the read relay.
#[must_use]
pub fn public_event_payload(event_type: &str, payload: &serde_json::Value) -> serde_json::Value {
    use serde_json::{Map, Value};
    let keys: &[&str] = match event_type {
        "experiment.reported" => &[
            "run_id",
            "report_id",
            "project_id",
            "name",
            "state",
            "message",
            "session_id",
            "followup_schedule_id",
            "source",
            "updated_at_unix",
        ],
        "experiment.updated" => &[
            "watch_id",
            "project_id",
            "name",
            "pid",
            "state",
            "session_id",
            "updated_at_unix",
        ],
        "codex.session.updated" => &[
            "session_id",
            "project_id",
            "mode",
            "state",
            "title",
            "active_turn_id",
            "updated_at_unix",
            "revision",
        ],
        "codex.schedule.updated" => &[
            "schedule_id",
            "session_id",
            "project_id",
            "trigger",
            "state",
            "created_at_unix",
            "updated_at_unix",
            "revision",
        ],
        "project.discovered" | "project.updated" => &[
            "candidate_id",
            "display_name",
            "suggested_project_id",
            "session_count",
            "state",
            "updated_at_unix",
        ],
        _ => &[
            "operation_id",
            "command_id",
            "watch_id",
            "project_id",
            "session_id",
            "turn_id",
            "status",
        ],
    };
    let mut result = Map::new();
    for key in keys {
        if let Some(value) = payload.get(*key) {
            let safe = match *key {
                "trigger" => serde_json::from_value::<CodexScheduleTrigger>(value.clone())
                    .ok()
                    .and_then(|trigger| serde_json::to_value(trigger).ok()),
                "revision" | "pid" | "session_count" | "updated_at_unix" | "created_at_unix" => {
                    value.as_u64().map(Value::from)
                }
                _ if value.is_string() || value.is_null() => Some(value.clone()),
                _ => None,
            };
            if let Some(value) = safe {
                result.insert((*key).to_owned(), value);
            }
        }
    }
    if event_type.starts_with("codex.turn.") || event_type == "codex.message.delta" {
        let data = payload.get("data").unwrap_or(payload);
        let mut safe = Map::new();
        for key in ["session_id", "turn_id", "item_id", "status"] {
            if let Some(v) = data.get(key).filter(|v| v.is_string() || v.is_null()) {
                safe.insert(key.to_owned(), v.clone());
            }
        }
        if let Some(turn) = data.get("turn") {
            for (source, target) in [("id", "turn_id"), ("status", "status")] {
                if let Some(v) = turn.get(source).filter(|v| v.is_string() || v.is_null()) {
                    safe.insert(target.to_owned(), v.clone());
                }
            }
        }
        if event_type == "codex.message.delta"
            && let Some(delta) = data.get("delta").filter(|v| v.is_string())
        {
            safe.insert("delta".to_owned(), delta.clone());
        }
        result.insert("data".to_owned(), Value::Object(safe));
    }
    Value::Object(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_shape_is_stable() {
        let value = serde_json::to_value(HealthResponse::hub("0.1.0")).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "status": "ok",
                "service": "farhelm-hub",
                "version": "0.1.0",
                "protocol": "farhelm/1"
            })
        );
    }

    #[test]
    fn agent_heartbeat_shape_is_stable() {
        let value =
            serde_json::to_value(AgentHeartbeat::new("gpu-a", "trainer-a", "0.1.0")).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "protocol": "farhelm/1",
                "agent_id": "gpu-a",
                "hostname": "trainer-a",
                "agent_version": "0.1.0",
                "capabilities": ["codex.ephemeral_submit"]
            })
        );
    }

    #[test]
    fn older_heartbeats_and_script_report_contract_remain_compatible() {
        let older:AgentHeartbeat=serde_json::from_value(serde_json::json!({"protocol":"farhelm/1","agent_id":"a","hostname":"host","agent_version":"0.6.0"})).unwrap();
        assert!(older.capabilities.is_empty());
        let event: AgentEvent =
            serde_json::from_str(include_str!("../tests/fixtures/experiment-reported.json"))
                .unwrap();
        assert_eq!(event.event_type, "experiment.reported");
        assert_eq!(
            public_event_payload(&event.event_type, &event.payload),
            event.payload
        );
        assert!(event.payload.get("pid").is_none());
        let data = public_event_payload(
            "codex.turn.completed",
            &serde_json::json!({"operation_id":"job","session_id":"session","data":{"turn":{"id":"t","status":"completed","items":[{"text":"PRIVATE"}]},"cwd":"PRIVATE"}}),
        );
        assert!(!data.to_string().contains("PRIVATE"));
        assert_eq!(data["data"]["turn_id"], "t");
    }

    #[test]
    fn command_fixture_is_stable() {
        let command: AgentCommand =
            serde_json::from_str(include_str!("../tests/fixtures/agent-command.json")).unwrap();
        assert_eq!(command.action, CommandAction::AgentProbe);
        assert_eq!(command.command_id, "cmd_0000000000000001");
        assert_eq!(
            serde_json::to_value(command).unwrap(),
            serde_json::json!({
                "protocol": "farhelm/1",
                "command_id": "cmd_0000000000000001",
                "agent_id": "gpu-a",
                "action": "agent.probe",
                "created_at_unix": 1_788_432_000_u64,
                "expires_at_unix": 1_788_432_060_u64
            })
        );
    }

    #[tokio::test]
    async fn framed_json_round_trips() {
        let request = WorkerRequest::hello("req_test", "0.1.0");
        let mut buffer = Vec::new();
        write_frame(&mut buffer, &request).await.unwrap();

        let decoded: WorkerRequest = read_frame(&mut buffer.as_slice()).await.unwrap();
        assert_eq!(decoded, request);
    }

    #[tokio::test]
    async fn oversized_frame_is_rejected_before_allocation() {
        let bytes = (MAX_FRAME_BYTES as u32 + 1).to_be_bytes();
        let mut slice = bytes.as_slice();
        let error = read_frame::<_, Value>(&mut slice).await.unwrap_err();
        assert!(matches!(error, FrameError::TooLarge { .. }));
    }
}
