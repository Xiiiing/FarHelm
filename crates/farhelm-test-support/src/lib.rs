//! Cross-component fixtures for FarHelm integration tests.
use farhelm_protocol::HealthResponse;
#[must_use]
pub fn health_fixture() -> HealthResponse {
    HealthResponse::hub(env!("CARGO_PKG_VERSION"))
}
