//! Cross-component fixtures for FarHelm integration tests.

use farhelm_protocol::{HealthResponse, WORKER_PROTOCOL, WorkerResponse};

#[must_use]
pub fn health_fixture() -> HealthResponse {
    HealthResponse::hub("0.1.1")
}

#[must_use]
pub fn worker_hello_fixture(request_id: impl Into<String>) -> WorkerResponse {
    WorkerResponse {
        protocol: WORKER_PROTOCOL.to_owned(),
        kind: "response".to_owned(),
        request_id: request_id.into(),
        ok: true,
        result: Some(serde_json::json!({
            "worker": "farhelm-worker-codex",
            "version": "0.1.1",
            "capabilities": ["worker.hello"]
        })),
        error: None,
    }
}
