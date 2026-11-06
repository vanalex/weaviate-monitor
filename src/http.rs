//! HTTP API served by axum.
//!
//! Only the router and shared state live here. Handlers should stay thin and
//! read from the collector/store/analyzer layers once those exist, so the same
//! data can back both the TUI and this API.

use std::{sync::Arc, time::Instant};

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Serialize;
use tower_http::trace::TraceLayer;

use crate::client::{Liveness, LivenessStatus, WeaviateClient};

/// State shared by all handlers. Cheap to clone.
#[derive(Clone)]
pub struct AppState {
    inner: Arc<Inner>,
}

struct Inner {
    started_at: Instant,
    weaviate: WeaviateClient,
}

impl AppState {
    pub fn new(weaviate: WeaviateClient) -> Self {
        Self {
            inner: Arc::new(Inner {
                started_at: Instant::now(),
                weaviate,
            }),
        }
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/api/liveness", get(liveness))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

#[derive(Debug, Serialize)]
struct Health {
    status: &'static str,
    version: &'static str,
    uptime_secs: u64,
}

/// Liveness of the monitor itself (not of the Weaviate cluster).
async fn healthz(State(state): State<AppState>) -> Json<Health> {
    Json(Health {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        uptime_secs: state.inner.started_at.elapsed().as_secs(),
    })
}

#[derive(Debug, Serialize)]
struct LivenessResponse {
    url: String,
    #[serde(flatten)]
    liveness: Liveness,
}

/// Liveness and readiness of the Weaviate cluster.
///
/// 200 when Weaviate is up, 503 when it's degraded or down, so uptime
/// checkers can use the status code alone. The body has the details either way.
async fn liveness(State(state): State<AppState>) -> Response {
    let weaviate = &state.inner.weaviate;
    let liveness = weaviate.liveness().await;

    let code = match liveness.status {
        LivenessStatus::Up => StatusCode::OK,
        LivenessStatus::Degraded | LivenessStatus::Down => StatusCode::SERVICE_UNAVAILABLE,
    };
    if code != StatusCode::OK {
        tracing::warn!(status = ?liveness.status, "weaviate is not up");
    }

    let body = LivenessResponse {
        url: weaviate.base_url().to_string(),
        liveness,
    };
    (code, Json(body)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::path};

    fn state(url: &str) -> AppState {
        AppState::new(WeaviateClient::new(url, None, Duration::from_secs(2)).unwrap())
    }

    async fn get_json(app: Router, uri: &str) -> (StatusCode, serde_json::Value) {
        let res = app
            .oneshot(Request::get(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let body = res.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&body).unwrap())
    }

    #[tokio::test]
    async fn healthz_returns_ok() {
        let (status, json) = get_json(router(state("http://localhost:1")), "/healthz").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["status"], "ok");
    }

    #[tokio::test]
    async fn liveness_is_200_when_up() {
        let server = MockServer::start().await;
        Mock::given(path("/v1/.well-known/live"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        Mock::given(path("/v1/.well-known/ready"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let (status, json) = get_json(router(state(&server.uri())), "/api/liveness").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["status"], "up");
        assert_eq!(json["live"]["http_status"], 200);
        assert_eq!(json["url"], format!("{}/", server.uri()));
    }

    #[tokio::test]
    async fn liveness_is_503_when_not_ready() {
        let server = MockServer::start().await;
        Mock::given(path("/v1/.well-known/live"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        Mock::given(path("/v1/.well-known/ready"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let (status, json) = get_json(router(state(&server.uri())), "/api/liveness").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(json["status"], "degraded");
    }
}
