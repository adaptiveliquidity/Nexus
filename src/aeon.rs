//! AEON-IQ memory proxy configuration.
//!
//! This module is compiled only with the `aeon-memory` feature. Upstream LLM
//! credentials continue to flow through the existing provider configuration;
//! the AEON-IQ management API key is read only from explicit `NEXUS_AEON_*`
//! environment configuration.

#[cfg(test)]
use std::sync::Arc;
use std::sync::Once;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};
use uuid::Uuid;

use crate::{NexusError, Result};

const ENABLED_ENV: &str = "NEXUS_AEON_ENABLED";
const BASE_URL_ENV: &str = "NEXUS_AEON_BASE_URL";
const AGENT_ID_ENV: &str = "NEXUS_AEON_AGENT_ID";
const SESSION_ID_ENV: &str = "NEXUS_AEON_SESSION_ID";
const TIMEOUT_MS_ENV: &str = "NEXUS_AEON_TIMEOUT_MS";
const MANAGEMENT_KEY_ENV: &str = "NEXUS_AEON_MANAGEMENT_KEY";
static MISSING_MANAGEMENT_KEY_WARN: Once = Once::new();

/// Configuration for routing ai-recovery LLM calls through AEON-IQ.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AeonConfig {
    pub enabled: bool,
    pub base_url: String,
    pub agent_id: String,
    pub session_id: Option<String>,
    pub timeout_ms: u64,
    pub management_key: Option<String>,
}

impl Default for AeonConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: "http://localhost:8080".to_string(),
            agent_id: "nexus".to_string(),
            session_id: None,
            timeout_ms: 30_000,
            management_key: None,
        }
    }
}

impl AeonConfig {
    /// Load AEON-IQ proxy configuration from `NEXUS_AEON_*` environment vars.
    pub fn from_env() -> Result<Self> {
        let defaults = Self::default();
        Ok(Self {
            enabled: env_bool(ENABLED_ENV, defaults.enabled)?,
            base_url: env_string(BASE_URL_ENV, defaults.base_url)?,
            agent_id: env_string(AGENT_ID_ENV, defaults.agent_id)?,
            session_id: env_optional_string(SESSION_ID_ENV)?,
            timeout_ms: env_u64(TIMEOUT_MS_ENV, defaults.timeout_ms)?,
            management_key: env_optional_string(MANAGEMENT_KEY_ENV)?,
        })
    }

    /// AEON-IQ's OpenAI-compatible chat-completions endpoint.
    pub fn chat_completions_url(&self) -> String {
        format!(
            "{}/v1/chat/completions",
            self.base_url.trim_end_matches('/')
        )
    }
}

/// A single advisory AEON-IQ memory search result.
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryHit {
    pub id: String,
    pub content: String,
    pub score: Option<f64>,
}

/// Fail-open HTTP client for the AEON-IQ management memory API.
///
/// Memory is advisory in Nexus. Every method absorbs transport, timeout,
/// status, and response-shape failures and returns the no-op value.
#[derive(Clone)]
pub struct AeonMemoryClient {
    http: reqwest::Client,
    base_url: String,
    agent_id: String,
    management_key: Option<String>,
    #[cfg(test)]
    test_responder: Option<TestResponder>,
}

impl AeonMemoryClient {
    pub fn from_config(config: &AeonConfig) -> Self {
        let http = reqwest::ClientBuilder::new()
            .timeout(Duration::from_millis(config.timeout_ms))
            .build()
            .unwrap_or_else(|e| {
                warn!(
                    target: "nexus.aeon",
                    error = %e,
                    "failed to build AEON-IQ memory client; falling back to default client"
                );
                reqwest::Client::new()
            });

        Self {
            http,
            base_url: config.base_url.trim_end_matches('/').to_string(),
            agent_id: config.agent_id.clone(),
            management_key: config.management_key.clone(),
            #[cfg(test)]
            test_responder: None,
        }
    }

    pub fn from_enabled_config(config: &AeonConfig) -> Option<Self> {
        let has_required_config = config.enabled
            && !config.base_url.trim().is_empty()
            && !config.agent_id.trim().is_empty()
            && config
                .management_key
                .as_deref()
                .is_some_and(|key| !key.trim().is_empty());

        has_required_config.then(|| Self::from_config(config))
    }

    #[cfg(test)]
    pub(crate) fn with_test_responder(config: &AeonConfig, responder: TestResponder) -> Self {
        let mut client = Self::from_config(config);
        client.test_responder = Some(responder);
        client
    }

    pub async fn search(&self, query: &str, limit: usize) -> Vec<MemoryHit> {
        let Some(management_key) = self.management_key() else {
            return Vec::new();
        };
        let Some(url) = self.url(&["api", "v1", "memories", "search"]) else {
            return Vec::new();
        };

        let body = SearchRequest {
            agent_id: &self.agent_id,
            query,
            limit,
        };

        let response = match self.post_json(url, management_key, &body).await {
            Some(response) => response,
            None => return Vec::new(),
        };

        if !is_success(response.status) {
            debug!(
                target: "nexus.aeon",
                status = response.status,
                "AEON-IQ memory search returned non-success status; failing open"
            );
            return Vec::new();
        }

        match serde_json::from_str::<SearchResponse>(&response.body) {
            Ok(body) => body
                .results
                .into_iter()
                .map(|hit| MemoryHit {
                    id: hit.id.unwrap_or_default(),
                    content: hit.content.unwrap_or_default(),
                    score: hit.score.or(hit.distance).or(hit.similarity),
                })
                .collect(),
            Err(e) => {
                debug!(
                    target: "nexus.aeon",
                    error = %e,
                    "AEON-IQ memory search response parse failed; failing open"
                );
                Vec::new()
            }
        }
    }

    pub async fn store(
        &self,
        content: &str,
        importance: Option<f32>,
        memory_type: Option<&str>,
    ) -> Option<Uuid> {
        let management_key = self.management_key()?;
        let url = self.url(&["api", "v1", "agents", &self.agent_id, "memories"])?;

        let body = StoreRequest {
            content,
            memory_type,
            importance,
        };

        let response = self.post_json(url, management_key, &body).await?;

        if !is_success(response.status) {
            debug!(
                target: "nexus.aeon",
                status = response.status,
                "AEON-IQ memory store returned non-success status; failing open"
            );
            return None;
        }

        match serde_json::from_str::<serde_json::Value>(&response.body) {
            Ok(body) => extract_memory_id(&body),
            Err(e) => {
                debug!(
                    target: "nexus.aeon",
                    error = %e,
                    "AEON-IQ memory store response parse failed; failing open"
                );
                None
            }
        }
    }

    pub async fn health(&self) -> bool {
        let Some(management_key) = self.management_key() else {
            return false;
        };
        let Some(url) = self.url(&["health"]) else {
            return false;
        };

        match self.get(url, management_key).await {
            Some(response) => is_success(response.status),
            None => false,
        }
    }

    async fn post_json<T>(
        &self,
        url: reqwest::Url,
        management_key: &str,
        body: &T,
    ) -> Option<HttpResponse>
    where
        T: Serialize + ?Sized,
    {
        #[cfg(test)]
        if let Some(responder) = &self.test_responder {
            let body = match serde_json::to_string(body) {
                Ok(body) => body,
                Err(e) => {
                    debug!(
                        target: "nexus.aeon",
                        error = %e,
                        "AEON-IQ request serialization failed; failing open"
                    );
                    return None;
                }
            };
            let response = responder(TestHttpRequest {
                method: "POST".to_string(),
                path: url.path().to_string(),
                headers: vec![("X-Management-Key".to_string(), management_key.to_string())],
                body,
            });
            return Some(HttpResponse {
                status: response.status,
                body: response.body,
            });
        }

        match self
            .http
            .post(url)
            .header("X-Management-Key", management_key)
            .json(body)
            .send()
            .await
        {
            Ok(response) => {
                let status = response.status().as_u16();
                match response.text().await {
                    Ok(body) => Some(HttpResponse { status, body }),
                    Err(e) => {
                        debug!(
                            target: "nexus.aeon",
                            error = %e,
                            "AEON-IQ response body read failed; failing open"
                        );
                        None
                    }
                }
            }
            Err(e) => {
                debug!(
                    target: "nexus.aeon",
                    error = %e,
                    "AEON-IQ memory API request failed open"
                );
                None
            }
        }
    }

    async fn get(&self, url: reqwest::Url, management_key: &str) -> Option<HttpResponse> {
        #[cfg(test)]
        if let Some(responder) = &self.test_responder {
            let response = responder(TestHttpRequest {
                method: "GET".to_string(),
                path: url.path().to_string(),
                headers: vec![("X-Management-Key".to_string(), management_key.to_string())],
                body: String::new(),
            });
            return Some(HttpResponse {
                status: response.status,
                body: response.body,
            });
        }

        match self
            .http
            .get(url)
            .header("X-Management-Key", management_key)
            .send()
            .await
        {
            Ok(response) => Some(HttpResponse {
                status: response.status().as_u16(),
                body: String::new(),
            }),
            Err(e) => {
                debug!(
                    target: "nexus.aeon",
                    error = %e,
                    "AEON-IQ health check failed open"
                );
                None
            }
        }
    }

    fn management_key(&self) -> Option<&str> {
        match self.management_key.as_deref() {
            Some(key) => Some(key),
            None => {
                MISSING_MANAGEMENT_KEY_WARN.call_once(|| {
                    warn!(
                        target: "nexus.aeon",
                        env = MANAGEMENT_KEY_ENV,
                        "AEON-IQ management key is not configured; memory API calls are disabled"
                    );
                });
                None
            }
        }
    }

    fn url(&self, segments: &[&str]) -> Option<reqwest::Url> {
        let mut url = match reqwest::Url::parse(&format!("{}/", self.base_url)) {
            Ok(url) => url,
            Err(e) => {
                debug!(
                    target: "nexus.aeon",
                    error = %e,
                    "invalid AEON-IQ base URL; failing open"
                );
                return None;
            }
        };

        {
            let mut path = match url.path_segments_mut() {
                Ok(path) => path,
                Err(()) => {
                    debug!(
                        target: "nexus.aeon",
                        "AEON-IQ base URL cannot be a base URL; failing open"
                    );
                    return None;
                }
            };

            path.clear();
            for segment in segments {
                path.push(segment);
            }
        }

        Some(url)
    }
}

struct HttpResponse {
    status: u16,
    body: String,
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) struct TestHttpRequest {
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: String,
}

#[cfg(test)]
pub(crate) struct TestHttpResponse {
    pub(crate) status: u16,
    pub(crate) body: String,
}

#[cfg(test)]
pub(crate) type TestResponder = Arc<dyn Fn(TestHttpRequest) -> TestHttpResponse + Send + Sync>;

#[derive(Serialize)]
struct SearchRequest<'a> {
    agent_id: &'a str,
    query: &'a str,
    limit: usize,
}

#[derive(Deserialize)]
struct SearchResponse {
    #[serde(default)]
    results: Vec<SearchHit>,
}

#[derive(Deserialize)]
struct SearchHit {
    id: Option<String>,
    content: Option<String>,
    score: Option<f64>,
    distance: Option<f64>,
    similarity: Option<f64>,
}

#[derive(Serialize)]
struct StoreRequest<'a> {
    content: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    memory_type: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    importance: Option<f32>,
}

fn extract_memory_id(body: &serde_json::Value) -> Option<Uuid> {
    body.as_str()
        .or_else(|| body.get("id").and_then(serde_json::Value::as_str))
        .or_else(|| body.get("memory_id").and_then(serde_json::Value::as_str))
        .and_then(|id| match Uuid::parse_str(id) {
            Ok(id) => Some(id),
            Err(e) => {
                debug!(
                    target: "nexus.aeon",
                    error = %e,
                    "AEON-IQ memory store returned an invalid UUID; failing open"
                );
                None
            }
        })
}

fn is_success(status: u16) -> bool {
    (200..300).contains(&status)
}

fn env_string(name: &str, default: String) -> Result<String> {
    match std::env::var(name) {
        Ok(value) if value.trim().is_empty() => Err(NexusError::ConfigError(format!(
            "{name} must not be empty when configured"
        ))),
        Ok(value) => Ok(value),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(std::env::VarError::NotUnicode(_)) => Err(NexusError::ConfigError(format!(
            "{name} must be valid Unicode"
        ))),
    }
}

fn env_optional_string(name: &str) -> Result<Option<String>> {
    match std::env::var(name) {
        Ok(value) if value.trim().is_empty() => Ok(None),
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(NexusError::ConfigError(format!(
            "{name} must be valid Unicode"
        ))),
    }
}

fn env_bool(name: &str, default: bool) -> Result<bool> {
    match std::env::var(name) {
        Ok(value) => match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Ok(true),
            "0" | "false" | "no" | "off" => Ok(false),
            _ => Err(NexusError::ConfigError(format!(
                "{name} must be one of true/false, yes/no, on/off, or 1/0"
            ))),
        },
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(std::env::VarError::NotUnicode(_)) => Err(NexusError::ConfigError(format!(
            "{name} must be valid Unicode"
        ))),
    }
}

fn env_u64(name: &str, default: u64) -> Result<u64> {
    match std::env::var(name) {
        Ok(value) => value
            .trim()
            .parse::<u64>()
            .map_err(|e| NexusError::ConfigError(format!("invalid {name}: {e}"))),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(std::env::VarError::NotUnicode(_)) => Err(NexusError::ConfigError(format!(
            "{name} must be valid Unicode"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::sync::{Arc, Mutex};
    use uuid::Uuid;

    #[test]
    fn default_config_is_disabled_local_proxy() {
        let cfg = AeonConfig::default();

        assert!(!cfg.enabled);
        assert_eq!(cfg.base_url, "http://localhost:8080");
        assert_eq!(cfg.agent_id, "nexus");
        assert_eq!(cfg.session_id, None);
        assert_eq!(cfg.timeout_ms, 30_000);
        assert_eq!(cfg.management_key, None);
        assert_eq!(
            cfg.chat_completions_url(),
            "http://localhost:8080/v1/chat/completions"
        );
    }

    #[test]
    fn chat_completions_url_handles_trailing_slash() {
        let cfg = AeonConfig {
            base_url: "http://localhost:8080/".to_string(),
            ..AeonConfig::default()
        };

        assert_eq!(
            cfg.chat_completions_url(),
            "http://localhost:8080/v1/chat/completions"
        );
    }

    #[tokio::test]
    async fn aeon_memory_search_sends_management_key_and_parses_results() {
        let (client, captured) = mock_client(
            200,
            r#"{"results":[{"id":"mem-1","content":"first","score":0.92,"memory_type":"semantic"},{"id":"mem-2","content":"second","distance":0.31}]}"#,
            Some("mgmt-key"),
        );

        let hits = client.search("trap recovery", 2).await;
        let request = take_request(&captured);

        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/api/v1/memories/search");
        assert_eq!(request.header("x-management-key"), Some("mgmt-key"));

        let body: Value = serde_json::from_str(&request.body).unwrap();
        assert_eq!(body["agent_id"], "agent-1");
        assert_eq!(body["query"], "trap recovery");
        assert_eq!(body["limit"], 2);

        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].id, "mem-1");
        assert_eq!(hits[0].content, "first");
        assert_eq!(hits[0].score, Some(0.92));
        assert_eq!(hits[1].id, "mem-2");
        assert_eq!(hits[1].content, "second");
        assert_eq!(hits[1].score, Some(0.31));
    }

    #[tokio::test]
    async fn aeon_memory_search_parses_similarity_as_score() {
        let (client, captured) = mock_client(
            200,
            r#"{"results":[{"id":"m1","content":"c","similarity":0.91,"memory_type":"semantic"}]}"#,
            Some("mgmt-key"),
        );

        let hits = client.search("trap recovery", 1).await;
        let _ = take_request(&captured);

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "m1");
        assert_eq!(hits[0].content, "c");
        assert_eq!(hits[0].score, Some(0.91));
    }

    #[tokio::test]
    async fn aeon_memory_search_returns_empty_on_http_500() {
        let (client, captured) = mock_client(500, r#"{"error":"boom"}"#, Some("mgmt-key"));

        let hits = client.search("trap recovery", 2).await;
        let _ = take_request(&captured);

        assert!(hits.is_empty());
    }

    #[tokio::test]
    async fn aeon_memory_search_returns_empty_on_malformed_body() {
        let (client, captured) = mock_client(200, "not-json", Some("mgmt-key"));

        let hits = client.search("trap recovery", 2).await;
        let _ = take_request(&captured);

        assert!(hits.is_empty());
    }

    #[tokio::test]
    async fn aeon_memory_search_returns_empty_on_connection_refusal() {
        let client =
            AeonMemoryClient::from_config(&test_config("http://127.0.0.1:1", Some("mgmt-key")));

        let hits = client.search("trap recovery", 2).await;

        assert!(hits.is_empty());
    }

    #[tokio::test]
    async fn aeon_memory_store_posts_memory_and_returns_id() {
        let id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let (client, captured) = mock_client(
            200,
            r#"{"id":"550e8400-e29b-41d4-a716-446655440000"}"#,
            Some("mgmt-key"),
        );

        let stored = client
            .store("execution failed", Some(0.75), Some("failure"))
            .await;
        let request = take_request(&captured);

        assert_eq!(stored, Some(id));
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/api/v1/agents/agent-1/memories");
        assert_eq!(request.header("x-management-key"), Some("mgmt-key"));

        let body: Value = serde_json::from_str(&request.body).unwrap();
        assert_eq!(body["content"], "execution failed");
        assert_eq!(body["importance"], 0.75);
        assert_eq!(body["memory_type"], "failure");
    }

    #[tokio::test]
    async fn aeon_memory_store_returns_none_on_error() {
        let (client, captured) = mock_client(500, r#"{"error":"boom"}"#, Some("mgmt-key"));

        let stored = client.store("execution failed", None, None).await;
        let _ = take_request(&captured);

        assert_eq!(stored, None);
    }

    #[tokio::test]
    async fn aeon_memory_health_returns_true_only_for_200() {
        let (healthy_client, healthy_captured) =
            mock_client(200, r#"{"ok":true}"#, Some("mgmt-key"));
        let (unhealthy_client, unhealthy_captured) =
            mock_client(500, r#"{"ok":false}"#, Some("mgmt-key"));

        assert!(healthy_client.health().await);
        assert!(!unhealthy_client.health().await);

        assert_eq!(take_request(&healthy_captured).path, "/health");
        assert_eq!(take_request(&unhealthy_captured).path, "/health");
    }

    #[tokio::test]
    async fn aeon_memory_missing_management_key_noops_search_and_store() {
        let (client, captured) = mock_client(200, r#"{"unexpected":true}"#, None);

        assert!(client.search("trap recovery", 2).await.is_empty());
        assert_eq!(
            client
                .store("execution failed", Some(0.75), Some("failure"))
                .await,
            None
        );

        assert!(captured.lock().unwrap().is_empty());
    }

    #[test]
    fn aeon_memory_client_requires_enabled_config() {
        let disabled = AeonConfig {
            enabled: false,
            management_key: Some("mgmt-key".to_string()),
            ..test_config("http://aeon.test", Some("mgmt-key"))
        };
        let missing_key = AeonConfig {
            management_key: None,
            ..test_config("http://aeon.test", None)
        };
        let configured = test_config("http://aeon.test", Some("mgmt-key"));

        assert!(AeonMemoryClient::from_enabled_config(&disabled).is_none());
        assert!(AeonMemoryClient::from_enabled_config(&missing_key).is_none());
        assert!(AeonMemoryClient::from_enabled_config(&configured).is_some());
    }

    fn test_config(base_url: &str, management_key: Option<&str>) -> AeonConfig {
        AeonConfig {
            enabled: true,
            base_url: base_url.to_string(),
            agent_id: "agent-1".to_string(),
            session_id: None,
            timeout_ms: 100,
            management_key: management_key.map(str::to_string),
        }
    }

    impl TestHttpRequest {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(header, _)| header.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.as_str())
        }
    }

    type CapturedRequests = Arc<Mutex<Vec<TestHttpRequest>>>;

    fn mock_client(
        status: u16,
        response_body: &str,
        management_key: Option<&str>,
    ) -> (AeonMemoryClient, CapturedRequests) {
        let response_body = response_body.to_string();
        let captured = Arc::new(Mutex::new(Vec::new()));
        let captured_for_responder = Arc::clone(&captured);
        let client = AeonMemoryClient::with_test_responder(
            &test_config("http://aeon.test", management_key),
            Arc::new(move |request| {
                captured_for_responder.lock().unwrap().push(request);
                TestHttpResponse {
                    status,
                    body: response_body.clone(),
                }
            }),
        );
        (client, captured)
    }

    fn take_request(captured: &CapturedRequests) -> TestHttpRequest {
        let mut requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 1);
        requests.remove(0)
    }
}
