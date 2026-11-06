//! Typed client for the Weaviate REST API. Read-only: GET requests only.

use std::{fmt, time::Duration};

use anyhow::Context;
use reqwest::{Url, header};
use serde::Serialize;
use tokio::time::Instant;

const DEFAULT_URL: &str = "http://localhost:8080";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct WeaviateClient {
    http: reqwest::Client,
    base_url: Url,
}

impl fmt::Debug for WeaviateClient {
    // The API key lives in the reqwest client's default headers, which are
    // never printed; only the URL is shown.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WeaviateClient")
            .field("base_url", &self.base_url.as_str())
            .finish_non_exhaustive()
    }
}

impl WeaviateClient {
    /// Builds a client from `WEAVIATE_URL` (default `http://localhost:8080`)
    /// and the optional `WEAVIATE_API_KEY`.
    pub fn from_env() -> anyhow::Result<Self> {
        let url = std::env::var("WEAVIATE_URL").unwrap_or_else(|_| DEFAULT_URL.to_string());
        let api_key = std::env::var("WEAVIATE_API_KEY")
            .ok()
            .filter(|k| !k.is_empty());
        Self::new(&url, api_key.as_deref(), DEFAULT_TIMEOUT)
    }

    pub fn new(base_url: &str, api_key: Option<&str>, timeout: Duration) -> anyhow::Result<Self> {
        let mut base_url =
            Url::parse(base_url).with_context(|| format!("invalid Weaviate URL {base_url:?}"))?;
        // Make `join` append to any path prefix (e.g. behind a reverse proxy)
        // instead of replacing its last segment.
        if !base_url.path().ends_with('/') {
            base_url.set_path(&format!("{}/", base_url.path()));
        }

        let mut headers = header::HeaderMap::new();
        if let Some(key) = api_key {
            let mut value = header::HeaderValue::from_str(&format!("Bearer {key}"))
                .context("WEAVIATE_API_KEY contains invalid header characters")?;
            value.set_sensitive(true);
            headers.insert(header::AUTHORIZATION, value);
        }

        let http = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(timeout)
            .user_agent(concat!("weaviate-monitor/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("failed to build HTTP client")?;

        Ok(Self { http, base_url })
    }

    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    /// Checks `/v1/.well-known/live` and `/v1/.well-known/ready` concurrently.
    pub async fn liveness(&self) -> Liveness {
        let (live, ready) = tokio::join!(
            self.probe("v1/.well-known/live"),
            self.probe("v1/.well-known/ready"),
        );
        Liveness::from_probes(live, ready)
    }

    /// GETs `path` and reports whether it answered 2xx, and how fast.
    /// Network errors and timeouts become a failed probe, not an `Err`.
    async fn probe(&self, path: &str) -> Probe {
        let url = self
            .base_url
            .join(path)
            .expect("static path is a valid URL");
        let started = Instant::now();
        let result = self.http.get(url).send().await;
        let latency_ms = (started.elapsed().as_secs_f64() * 100_000.0).round() / 100.0;

        match result {
            Ok(res) => Probe {
                ok: res.status().is_success(),
                http_status: Some(res.status().as_u16()),
                latency_ms,
                error: None,
            },
            Err(err) => Probe {
                ok: false,
                http_status: None,
                latency_ms,
                error: Some(describe(err)),
            },
        }
    }
}

/// Outcome of one GET against a well-known endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct Probe {
    pub ok: bool,
    pub http_status: Option<u16>,
    pub latency_ms: f64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LivenessStatus {
    /// Live and ready to serve requests.
    Up,
    /// The process answers but isn't ready (starting up, loading shards…).
    Degraded,
    /// Not reachable, or reports itself as not live.
    Down,
}

#[derive(Debug, Clone, Serialize)]
pub struct Liveness {
    pub status: LivenessStatus,
    pub live: Probe,
    pub ready: Probe,
}

impl Liveness {
    fn from_probes(live: Probe, ready: Probe) -> Self {
        let status = match (live.ok, ready.ok) {
            (true, true) => LivenessStatus::Up,
            (true, false) => LivenessStatus::Degraded,
            (false, _) => LivenessStatus::Down,
        };
        Self {
            status,
            live,
            ready,
        }
    }
}

fn describe(err: reqwest::Error) -> String {
    if err.is_timeout() {
        "timed out".into()
    } else if err.is_connect() {
        "connection failed".into()
    } else {
        // Strip the URL so error text stays short; it's reported separately.
        err.without_url().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path},
    };

    async fn mock(server: &MockServer, live: u16, ready: u16) {
        Mock::given(method("GET"))
            .and(path("/v1/.well-known/live"))
            .respond_with(ResponseTemplate::new(live))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/.well-known/ready"))
            .respond_with(ResponseTemplate::new(ready))
            .mount(server)
            .await;
    }

    fn client(url: &str) -> WeaviateClient {
        WeaviateClient::new(url, None, Duration::from_secs(2)).unwrap()
    }

    #[tokio::test]
    async fn up_when_live_and_ready() {
        let server = MockServer::start().await;
        mock(&server, 200, 200).await;

        let l = client(&server.uri()).liveness().await;
        assert_eq!(l.status, LivenessStatus::Up);
        assert_eq!(l.live.http_status, Some(200));
        assert!(l.ready.ok);
    }

    #[tokio::test]
    async fn degraded_when_live_but_not_ready() {
        let server = MockServer::start().await;
        mock(&server, 200, 503).await;

        let l = client(&server.uri()).liveness().await;
        assert_eq!(l.status, LivenessStatus::Degraded);
        assert_eq!(l.ready.http_status, Some(503));
    }

    #[tokio::test]
    async fn down_when_unreachable() {
        // Bind then drop a listener to get a port nothing listens on.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();

        let l = client(&format!("http://127.0.0.1:{port}")).liveness().await;
        assert_eq!(l.status, LivenessStatus::Down);
        assert_eq!(l.live.http_status, None);
        assert!(l.live.error.is_some());
    }

    #[tokio::test]
    async fn sends_api_key_and_keeps_path_prefix() {
        let server = MockServer::start().await;
        Mock::given(path("/weaviate/v1/.well-known/live"))
            .and(header("authorization", "Bearer secret"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let c = WeaviateClient::new(
            &format!("{}/weaviate", server.uri()),
            Some("secret"),
            Duration::from_secs(2),
        )
        .unwrap();
        assert!(c.liveness().await.live.ok);
        assert!(!format!("{c:?}").contains("secret"));
    }
}
