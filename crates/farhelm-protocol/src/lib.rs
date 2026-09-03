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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentListResponse {
    pub protocol: String,
    pub agents: Vec<AgentSummary>,
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
                "agent_version": "0.1.0"
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
