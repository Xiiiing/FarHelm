use axum::{Json, Router, routing::get};
use farhelm_core::PRODUCT_VERSION;
use farhelm_protocol::HealthResponse;

pub fn app() -> Router {
    Router::new().route("/api/v1/health", get(health))
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse::hub(PRODUCT_VERSION))
}

#[cfg(test)]
mod tests {
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use farhelm_protocol::{FARHELM_PROTOCOL, HealthResponse, HealthStatus};
    use tower::ServiceExt;

    use super::*;

    #[tokio::test]
    async fn health_endpoint_reports_versioned_contract() {
        let response = app()
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
}
