//! AEON-IQ memory proxy configuration.
//!
//! This module is compiled only with the `aeon-memory` feature. Upstream LLM
//! credentials continue to flow through the existing provider configuration;
//! the AEON-IQ management API key is read only from explicit `NEXUS_AEON_*`
//! environment configuration.

use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::Arc;
use std::sync::Once;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{debug, warn};
use uuid::Uuid;

use crate::{NexusError, Result};

const ENABLED_ENV: &str = "NEXUS_AEON_ENABLED";
const BASE_URL_ENV: &str = "NEXUS_AEON_BASE_URL";
const AGENT_ID_ENV: &str = "NEXUS_AEON_AGENT_ID";
const SESSION_ID_ENV: &str = "NEXUS_AEON_SESSION_ID";
const TIMEOUT_MS_ENV: &str = "NEXUS_AEON_TIMEOUT_MS";
const MANAGEMENT_KEY_ENV: &str = "NEXUS_AEON_MANAGEMENT_KEY";
const HMAC_KEY_ENV: &str = "NEXUS_AEON_HMAC_KEY";
const TIMELINE_SPOOL_ENV: &str = "NEXUS_AEON_TIMELINE_SPOOL";
const TIMELINE_MAX_ATTEMPTS: usize = 3;
static MISSING_MANAGEMENT_KEY_WARN: Once = Once::new();

/// Configuration for routing ai-recovery LLM calls through AEON-IQ.
#[derive(Clone, PartialEq, Eq)]
pub struct AeonConfig {
    pub enabled: bool,
    pub base_url: String,
    pub agent_id: String,
    pub session_id: Option<String>,
    pub timeout_ms: u64,
    pub management_key: Option<String>,
    /// Hex-encoded HMAC-SHA256 key shared with AEON-IQ for memory evidence binding.
    /// When absent, `build_memory_evidence_ref` returns `Absent` mode with no evidence.
    pub hmac_key: Option<Vec<u8>>,
}

impl std::fmt::Debug for AeonConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AeonConfig")
            .field("enabled", &self.enabled)
            .field("base_url", &self.base_url)
            .field("agent_id", &self.agent_id)
            .field("session_id", &self.session_id)
            .field("timeout_ms", &self.timeout_ms)
            .field(
                "management_key",
                &self.management_key.as_deref().map(|_| "[REDACTED]"),
            )
            .field(
                "hmac_key",
                &self
                    .hmac_key
                    .as_ref()
                    .map(|key| format!("[REDACTED {} bytes]", key.len())),
            )
            .finish()
    }
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
            hmac_key: None,
        }
    }
}

impl AeonConfig {
    /// Load AEON-IQ proxy configuration from `NEXUS_AEON_*` environment vars.
    pub fn from_env() -> Result<Self> {
        let defaults = Self::default();
        let base_url = env_string(BASE_URL_ENV, defaults.base_url)?;
        {
            let parsed = reqwest::Url::parse(&base_url).map_err(|e| {
                NexusError::ConfigError(format!("AEON_BASE_URL is not a valid URL: {e}"))
            })?;
            if !matches!(parsed.scheme(), "http" | "https") {
                return Err(NexusError::ConfigError(format!(
                    "AEON_BASE_URL must use http or https scheme, got '{}'",
                    parsed.scheme()
                )));
            }
        }

        let config = Self {
            enabled: env_bool(ENABLED_ENV, defaults.enabled)?,
            base_url,
            agent_id: env_string(AGENT_ID_ENV, defaults.agent_id)?,
            session_id: env_optional_string(SESSION_ID_ENV)?,
            timeout_ms: env_u64(TIMEOUT_MS_ENV, defaults.timeout_ms)?,
            management_key: env_optional_string(MANAGEMENT_KEY_ENV)?,
            hmac_key: env_optional_hex(HMAC_KEY_ENV)?,
        };
        if let Some(ref key) = config.hmac_key {
            if key.len() < 32 {
                return Err(NexusError::ConfigError(format!(
                    "AEON_HMAC_KEY must be at least 32 bytes (256 bits); got {} bytes",
                    key.len()
                )));
            }
        }

        Ok(config)
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

/// Versioned evidence bundle describing the memory recall inputs bound into a
/// proof capsule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryEvidenceV1 {
    pub version: u8,
    pub query: String,
    pub hit_count: usize,
    pub hit_digests: Vec<String>,
    pub attestation: crate::proof::schema::MemoryAttestationMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capsule_digest: Option<String>,
}

impl MemoryEvidenceV1 {
    pub const VERSION: u8 = 1;

    pub fn new(
        query: impl Into<String>,
        hits: &[MemoryHit],
        attestation: crate::proof::schema::MemoryAttestationMode,
    ) -> Self {
        Self {
            version: Self::VERSION,
            query: query.into(),
            hit_count: hits.len(),
            hit_digests: hits
                .iter()
                .map(|hit| sha256_hex(hit.content.as_bytes()))
                .collect(),
            attestation,
            capsule_digest: None,
        }
    }

    pub fn with_capsule_digest(mut self, capsule_digest: Option<String>) -> Self {
        self.capsule_digest = capsule_digest;
        self
    }

    pub fn evidence_digest(&self) -> Option<String> {
        serde_json::to_vec(self)
            .ok()
            .map(|bytes| sha256_hex(bytes.as_slice()))
    }

    pub fn validate(&self) -> std::result::Result<(), String> {
        validate_memory_evidence_v1(self)
    }
}

pub fn validate_memory_evidence_v1(evidence: &MemoryEvidenceV1) -> std::result::Result<(), String> {
    if evidence.version != MemoryEvidenceV1::VERSION {
        return Err(format!(
            "unsupported version {}; expected {}",
            evidence.version,
            MemoryEvidenceV1::VERSION
        ));
    }
    if evidence.hit_count != evidence.hit_digests.len() {
        return Err(format!(
            "hit_count {} does not match hit_digests length {}",
            evidence.hit_count,
            evidence.hit_digests.len()
        ));
    }
    if evidence
        .capsule_digest
        .as_deref()
        .is_some_and(|digest| digest.trim().is_empty())
    {
        return Err("capsule_digest must be non-empty when set".to_string());
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub struct MemoryRecallEvidence {
    pub hits: Vec<MemoryHit>,
    pub evidence: MemoryEvidenceV1,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MemorySearchOutcome {
    pub hits: Vec<MemoryHit>,
    pub attestation: crate::proof::schema::MemoryAttestationMode,
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

    pub fn timeline_sink(&self) -> AeonTimelineSink {
        AeonTimelineSink::from_memory_client(self)
    }

    pub async fn search(&self, query: &str, limit: usize) -> Vec<MemoryHit> {
        self.search_with_status(query, limit).await.hits
    }

    pub async fn search_with_status(&self, query: &str, limit: usize) -> MemorySearchOutcome {
        use crate::proof::schema::MemoryAttestationMode;

        let Some(management_key) = self.management_key() else {
            return MemorySearchOutcome {
                hits: Vec::new(),
                attestation: MemoryAttestationMode::Absent,
            };
        };
        let Some(url) = self.url(&["api", "v1", "memories", "search"]) else {
            return MemorySearchOutcome {
                hits: Vec::new(),
                attestation: MemoryAttestationMode::Degraded,
            };
        };

        let body = SearchRequest {
            agent_id: &self.agent_id,
            query,
            limit,
        };

        let response = match self.post_json(url, management_key, &body).await {
            Some(response) => response,
            None => {
                return MemorySearchOutcome {
                    hits: Vec::new(),
                    attestation: MemoryAttestationMode::Degraded,
                };
            }
        };

        if !is_success(response.status) {
            debug!(
                target: "nexus.aeon",
                status = response.status,
                "AEON-IQ memory search returned non-success status; failing open"
            );
            return MemorySearchOutcome {
                hits: Vec::new(),
                attestation: MemoryAttestationMode::Degraded,
            };
        }

        match serde_json::from_str::<SearchResponse>(&response.body) {
            Ok(body) => {
                let hits = body
                    .results
                    .into_iter()
                    .map(|hit| MemoryHit {
                        id: hit.id.unwrap_or_default(),
                        content: hit.content.unwrap_or_default(),
                        score: hit.score.or(hit.distance).or(hit.similarity),
                    })
                    .collect::<Vec<_>>();
                let attestation = if hits.is_empty() {
                    MemoryAttestationMode::AttestedNoHit
                } else {
                    MemoryAttestationMode::AttestedWithRecall
                };
                MemorySearchOutcome { hits, attestation }
            }
            Err(e) => {
                debug!(
                    target: "nexus.aeon",
                    error = %e,
                    "AEON-IQ memory search response parse failed; failing open"
                );
                MemorySearchOutcome {
                    hits: Vec::new(),
                    attestation: MemoryAttestationMode::Degraded,
                }
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

/// Best-effort AEON-IQ timeline delivery status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineDeliveryStatus {
    /// Fire-and-forget delivery; success is not guaranteed.
    FireAndForget,
    /// All requested events were delivered.
    Delivered,
    /// Delivery failed in advisory mode; execution remains unaffected.
    FailedOpen,
    /// Delivery failed in attested mode; callers may mark proof evidence degraded.
    RequiredButFailed,
}

/// Delivery mode for AEON-IQ timeline events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineDeliveryMode {
    /// Advisory delivery never blocks execution and maps failures to `FailedOpen`.
    Advisory,
    /// Required delivery maps failures to `RequiredButFailed`; callers decide
    /// whether that degrades proof evidence.
    Attested,
    /// Do not contact AEON-IQ now; append events to the local replay spool.
    Offline,
}

impl TimelineDeliveryMode {
    pub fn parse(value: Option<&str>) -> Self {
        match value
            .unwrap_or("advisory")
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "attested" | "required" => Self::Attested,
            "offline" | "spool" => Self::Offline,
            _ => Self::Advisory,
        }
    }
}

/// Summary returned by offline timeline replay.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineReplayReport {
    pub delivered: usize,
    pub failed: usize,
    pub skipped: usize,
}

/// Fail-open sink for forwarding Nexus execution events to AEON-IQ's timeline.
#[derive(Clone)]
pub struct AeonTimelineSink {
    http: reqwest::Client,
    base_url: String,
    management_key: Option<String>,
    mode: TimelineDeliveryMode,
    spool_path: PathBuf,
    #[cfg(test)]
    test_responder: Option<TestResponder>,
}

impl AeonTimelineSink {
    pub fn from_config(config: &AeonConfig) -> Self {
        AeonMemoryClient::from_config(config).timeline_sink()
    }

    pub fn from_enabled_config(config: &AeonConfig) -> Option<Self> {
        AeonMemoryClient::from_enabled_config(config).map(|client| client.timeline_sink())
    }

    pub fn from_memory_client(client: &AeonMemoryClient) -> Self {
        Self {
            http: client.http.clone(),
            base_url: client.base_url.clone(),
            management_key: client.management_key.clone(),
            mode: TimelineDeliveryMode::Advisory,
            spool_path: default_timeline_spool_path(),
            #[cfg(test)]
            test_responder: client.test_responder.clone(),
        }
    }

    pub fn with_mode(mut self, mode: TimelineDeliveryMode) -> Self {
        self.mode = mode;
        self
    }

    pub fn with_spool_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.spool_path = path.into();
        self
    }

    #[cfg(test)]
    pub(crate) fn with_test_responder(config: &AeonConfig, responder: TestResponder) -> Self {
        let mut sink = Self::from_config(config);
        sink.test_responder = Some(responder);
        sink
    }

    pub async fn deliver(
        &self,
        agent_id: &str,
        session_id: Option<&str>,
        events: &[crate::daemon::NexusExecutionEvent],
    ) -> TimelineDeliveryStatus {
        if events.is_empty() {
            return TimelineDeliveryStatus::Delivered;
        }

        if matches!(self.mode, TimelineDeliveryMode::Offline) {
            return if self.spool_events(agent_id, session_id, events).await {
                TimelineDeliveryStatus::FireAndForget
            } else {
                TimelineDeliveryStatus::FailedOpen
            };
        }

        let mut delivered_all = true;
        for event in events {
            let body = TimelineEventBody::from_event(session_id, event);
            if !self.post_event_with_retry(agent_id, &body).await {
                delivered_all = false;
            }
        }

        if delivered_all {
            TimelineDeliveryStatus::Delivered
        } else {
            match self.mode {
                TimelineDeliveryMode::Attested => TimelineDeliveryStatus::RequiredButFailed,
                TimelineDeliveryMode::Advisory | TimelineDeliveryMode::Offline => {
                    TimelineDeliveryStatus::FailedOpen
                }
            }
        }
    }

    pub async fn replay_spooled_events(
        &self,
        agent_id: &str,
        since: Option<DateTime<Utc>>,
    ) -> TimelineReplayReport {
        let content = match tokio::fs::read_to_string(&self.spool_path).await {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return TimelineReplayReport::default();
            }
            Err(error) => {
                debug!(
                    target: "nexus.aeon",
                    error = %error,
                    "AEON-IQ timeline spool read failed; failing open"
                );
                return TimelineReplayReport {
                    failed: 1,
                    ..TimelineReplayReport::default()
                };
            }
        };

        let mut report = TimelineReplayReport::default();
        let mut retained = Vec::new();

        for line in content.lines().filter(|line| !line.trim().is_empty()) {
            let record = match serde_json::from_str::<TimelineSpoolRecord>(line) {
                Ok(record) => record,
                Err(error) => {
                    debug!(
                        target: "nexus.aeon",
                        error = %error,
                        "AEON-IQ timeline spool record parse failed; retaining record"
                    );
                    report.failed += 1;
                    retained.push(line.to_string());
                    continue;
                }
            };

            if record.agent_id != agent_id || since.is_some_and(|since| record.created_at < since) {
                report.skipped += 1;
                retained.push(line.to_string());
                continue;
            }

            if self
                .post_event_with_retry(&record.agent_id, &record.event)
                .await
            {
                report.delivered += 1;
            } else {
                report.failed += 1;
                retained.push(line.to_string());
            }
        }

        if let Err(error) = rewrite_spool(&self.spool_path, &retained).await {
            debug!(
                target: "nexus.aeon",
                error = %error,
                "AEON-IQ timeline spool rewrite failed after replay; failing open"
            );
        }

        report
    }

    async fn spool_events(
        &self,
        agent_id: &str,
        session_id: Option<&str>,
        events: &[crate::daemon::NexusExecutionEvent],
    ) -> bool {
        let records = events.iter().map(|event| TimelineSpoolRecord {
            created_at: Utc::now(),
            agent_id: agent_id.to_string(),
            event: TimelineEventBody::from_event(session_id, event),
        });

        if let Some(parent) = self.spool_path.parent() {
            if let Err(error) = tokio::fs::create_dir_all(parent).await {
                debug!(
                    target: "nexus.aeon",
                    error = %error,
                    "AEON-IQ timeline spool directory create failed; failing open"
                );
                return false;
            }
        }

        let mut file = match tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.spool_path)
            .await
        {
            Ok(file) => file,
            Err(error) => {
                debug!(
                    target: "nexus.aeon",
                    error = %error,
                    "AEON-IQ timeline spool open failed; failing open"
                );
                return false;
            }
        };

        for record in records {
            let mut bytes = match serde_json::to_vec(&record) {
                Ok(bytes) => bytes,
                Err(error) => {
                    debug!(
                        target: "nexus.aeon",
                        error = %error,
                        "AEON-IQ timeline spool serialization failed; failing open"
                    );
                    return false;
                }
            };
            bytes.push(b'\n');
            if let Err(error) = tokio::io::AsyncWriteExt::write_all(&mut file, &bytes).await {
                debug!(
                    target: "nexus.aeon",
                    error = %error,
                    "AEON-IQ timeline spool write failed; failing open"
                );
                return false;
            }
        }

        true
    }

    async fn post_event_with_retry(&self, agent_id: &str, body: &TimelineEventBody) -> bool {
        let Some(management_key) = self.management_key.as_deref() else {
            debug!(
                target: "nexus.aeon",
                "AEON-IQ management key is not configured; timeline delivery disabled"
            );
            return false;
        };
        let Some(url) = self.url(&["api", "v1", "agents", agent_id, "timeline"]) else {
            return false;
        };

        for attempt in 0..TIMELINE_MAX_ATTEMPTS {
            match self.post_json(url.clone(), management_key, body).await {
                Some(response) if is_success(response.status) => return true,
                Some(response) if response.status >= 500 && attempt + 1 < TIMELINE_MAX_ATTEMPTS => {
                    tokio::time::sleep(timeline_retry_delay(attempt)).await;
                }
                Some(response) => {
                    debug!(
                        target: "nexus.aeon",
                        status = response.status,
                        "AEON-IQ timeline post returned non-success status; failing open"
                    );
                    return false;
                }
                None if attempt + 1 < TIMELINE_MAX_ATTEMPTS => {
                    tokio::time::sleep(timeline_retry_delay(attempt)).await;
                }
                None => return false,
            }
        }

        false
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
                Err(error) => {
                    debug!(
                        target: "nexus.aeon",
                        error = %error,
                        "AEON-IQ timeline request serialization failed; failing open"
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
                    Err(error) => {
                        debug!(
                            target: "nexus.aeon",
                            error = %error,
                            "AEON-IQ timeline response body read failed; failing open"
                        );
                        None
                    }
                }
            }
            Err(error) => {
                debug!(
                    target: "nexus.aeon",
                    error = %error,
                    "AEON-IQ timeline request failed open"
                );
                None
            }
        }
    }

    fn url(&self, segments: &[&str]) -> Option<reqwest::Url> {
        let mut url = match reqwest::Url::parse(&format!("{}/", self.base_url)) {
            Ok(url) => url,
            Err(error) => {
                debug!(
                    target: "nexus.aeon",
                    error = %error,
                    "invalid AEON-IQ base URL; timeline delivery failing open"
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
                        "AEON-IQ base URL cannot be a base URL; timeline delivery failing open"
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TimelineSpoolRecord {
    created_at: DateTime<Utc>,
    agent_id: String,
    event: TimelineEventBody,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct TimelineEventBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    nexus_snapshot_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    capsule_digest: Option<String>,
    event_type: String,
}

impl TimelineEventBody {
    fn from_event(session_id: Option<&str>, event: &crate::daemon::NexusExecutionEvent) -> Self {
        match event {
            crate::daemon::NexusExecutionEvent::SnapshotCreated { snapshot_id } => Self {
                session_id: session_id.map(str::to_string),
                nexus_snapshot_id: Some(snapshot_id.to_string()),
                capsule_digest: None,
                event_type: "snapshot_created".to_string(),
            },
            crate::daemon::NexusExecutionEvent::CapabilityDenied { .. } => Self {
                session_id: session_id.map(str::to_string),
                nexus_snapshot_id: None,
                capsule_digest: None,
                event_type: "capability_denied".to_string(),
            },
            crate::daemon::NexusExecutionEvent::ProofCapsuleEmitted { capsule_id } => Self {
                session_id: session_id.map(str::to_string),
                nexus_snapshot_id: None,
                capsule_digest: Some(capsule_id.to_string()),
                event_type: "proof_capsule_emitted".to_string(),
            },
        }
    }
}

fn default_timeline_spool_path() -> PathBuf {
    std::env::var_os(TIMELINE_SPOOL_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("nexus-aeon-timeline-events.jsonl"))
}

fn timeline_retry_delay(attempt: usize) -> Duration {
    #[cfg(test)]
    {
        let _ = attempt;
        Duration::from_millis(1)
    }
    #[cfg(not(test))]
    {
        Duration::from_millis(25 * (1_u64 << attempt.min(5)))
    }
}

async fn rewrite_spool(path: &Path, retained: &[String]) -> std::io::Result<()> {
    if retained.is_empty() {
        match tokio::fs::remove_file(path).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    } else {
        let mut content = retained.join("\n");
        content.push('\n');
        tokio::fs::write(path, content).await
    }
}

/// Builds a `MemoryEvidenceRef` from the resolved memory hits for inclusion in
/// a `ProofCapsule`. Requires the `aeon-memory` feature.
///
/// Returns `Absent` when `config.hmac_key` is `None` — no fallback key is
/// ever used. Returns `Degraded` when evidence construction fails.
pub fn build_memory_evidence_ref(
    config: &AeonConfig,
    hits: &[MemoryHit],
    session_id: Option<String>,
) -> (
    Option<aeon_nexus_bridge::MemoryEvidenceRef>,
    crate::proof::schema::MemoryAttestationMode,
) {
    use crate::proof::schema::MemoryAttestationMode;

    let Some(key) = config.hmac_key.as_deref() else {
        return (None, MemoryAttestationMode::Absent);
    };

    let mapping =
        aeon_nexus_bridge::AgentSessionMapping::new(&config.agent_id, session_id, &config.agent_id);

    let mut bridge_hits = Vec::with_capacity(hits.len());
    for hit in hits {
        match aeon_nexus_bridge::MemoryEvidenceHit::new(&hit.id, hit.content.as_bytes(), hit.score)
        {
            Ok(h) => bridge_hits.push(h),
            Err(e) => {
                debug!(
                    target: "nexus.aeon",
                    error = %e,
                    "memory hit has invalid score; returning degraded evidence"
                );
                return (None, MemoryAttestationMode::Degraded);
            }
        }
    }

    let evidence = mapping.memory_evidence(key, bridge_hits);
    match evidence.to_ref() {
        Ok(evidence_ref) => {
            let mode = if hits.is_empty() {
                MemoryAttestationMode::AttestedNoHit
            } else {
                MemoryAttestationMode::AttestedWithRecall
            };
            (Some(evidence_ref), mode)
        }
        Err(e) => {
            debug!(
                target: "nexus.aeon",
                error = %e,
                "failed to build memory evidence ref; returning degraded evidence"
            );
            (None, MemoryAttestationMode::Degraded)
        }
    }
}

pub async fn recall_memory_evidence_v1(
    client: Option<&AeonMemoryClient>,
    query: &str,
    limit: usize,
) -> MemoryRecallEvidence {
    use crate::proof::schema::MemoryAttestationMode;

    let Some(client) = client else {
        return MemoryRecallEvidence {
            hits: Vec::new(),
            evidence: MemoryEvidenceV1::new(query, &[], MemoryAttestationMode::Absent),
        };
    };

    let outcome = client.search_with_status(query, limit).await;
    let evidence = MemoryEvidenceV1::new(query, &outcome.hits, outcome.attestation);
    MemoryRecallEvidence {
        hits: outcome.hits,
        evidence,
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
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

/// Parse an optional hex-encoded byte sequence from an environment variable.
/// Returns `None` when the variable is absent or empty; errors on invalid hex
/// or values that decode to more than 512 bytes.
fn env_optional_hex(name: &str) -> Result<Option<Vec<u8>>> {
    match std::env::var(name) {
        Ok(value) if value.trim().is_empty() => Ok(None),
        Ok(value) => {
            let trimmed = value.trim();
            let bytes: Result<Vec<u8>> = trimmed
                .as_bytes()
                .chunks(2)
                .map(|pair| {
                    if pair.len() != 2 {
                        return Err(NexusError::ConfigError(format!(
                            "{name} must be an even-length hex string"
                        )));
                    }
                    let hi = hex_nibble(pair[0]).ok_or_else(|| {
                        NexusError::ConfigError(format!("{name} contains invalid hex character"))
                    })?;
                    let lo = hex_nibble(pair[1]).ok_or_else(|| {
                        NexusError::ConfigError(format!("{name} contains invalid hex character"))
                    })?;
                    Ok((hi << 4) | lo)
                })
                .collect();
            let bytes = bytes?;
            if bytes.len() > 512 {
                return Err(NexusError::ConfigError(format!(
                    "{name} must be at most 512 bytes (1024 hex chars)"
                )));
            }
            Ok(Some(bytes))
        }
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(NexusError::ConfigError(format!(
            "{name} must be valid Unicode"
        ))),
    }
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::NexusExecutionEvent;
    use serde_json::Value;
    use std::ffi::OsString;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use uuid::Uuid;

    static AEON_ENV_LOCK: Mutex<()> = Mutex::new(());
    const AEON_ENV_VARS: [&str; 8] = [
        ENABLED_ENV,
        BASE_URL_ENV,
        AGENT_ID_ENV,
        SESSION_ID_ENV,
        TIMEOUT_MS_ENV,
        MANAGEMENT_KEY_ENV,
        HMAC_KEY_ENV,
        TIMELINE_SPOOL_ENV,
    ];

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
    fn aeon_config_debug_redacts_secrets() {
        let cfg = AeonConfig {
            management_key: Some("mgmt-secret".to_string()),
            hmac_key: Some(vec![0x42; 32]),
            ..AeonConfig::default()
        };

        let debug = format!("{cfg:?}");

        assert!(debug.contains("management_key: Some(\"[REDACTED]\")"));
        assert!(debug.contains("hmac_key: Some(\"[REDACTED 32 bytes]\")"));
        assert!(!debug.contains("mgmt-secret"));
        assert!(!debug.contains("66"));
    }

    #[test]
    fn from_env_rejects_invalid_base_url() {
        with_clean_aeon_env(|| {
            std::env::set_var(BASE_URL_ENV, "not a url");

            let error = AeonConfig::from_env().unwrap_err();

            assert!(error
                .to_string()
                .contains("AEON_BASE_URL is not a valid URL"));
        });
    }

    #[test]
    fn from_env_rejects_non_http_base_url_scheme() {
        with_clean_aeon_env(|| {
            std::env::set_var(BASE_URL_ENV, "file:///tmp/aeon.sock");

            let error = AeonConfig::from_env().unwrap_err();

            assert_eq!(
                error.to_string(),
                "Configuration error: AEON_BASE_URL must use http or https scheme, got 'file'"
            );
        });
    }

    #[test]
    fn from_env_rejects_short_hmac_key() {
        with_clean_aeon_env(|| {
            std::env::set_var(HMAC_KEY_ENV, "00010203");

            let error = AeonConfig::from_env().unwrap_err();

            assert_eq!(
                error.to_string(),
                "Configuration error: AEON_HMAC_KEY must be at least 32 bytes (256 bits); got 4 bytes"
            );
        });
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
            hmac_key: None,
        }
    }

    fn generated_test_management_key() -> String {
        format!("test-mgmt-{}", uuid::Uuid::new_v4())
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

    fn with_clean_aeon_env(test: impl FnOnce() + std::panic::UnwindSafe) {
        let _guard = AEON_ENV_LOCK.lock().unwrap();
        let saved: [(&str, Option<OsString>); 8] =
            AEON_ENV_VARS.map(|name| (name, std::env::var_os(name)));

        for name in AEON_ENV_VARS {
            std::env::remove_var(name);
        }

        let result = std::panic::catch_unwind(test);

        for (name, value) in saved {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }

        if let Err(payload) = result {
            std::panic::resume_unwind(payload);
        }
    }

    fn timeline_events() -> Vec<NexusExecutionEvent> {
        vec![
            NexusExecutionEvent::SnapshotCreated {
                snapshot_id: Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap(),
            },
            NexusExecutionEvent::CapabilityDenied {
                denied_category: "read:/secret".to_string(),
            },
            NexusExecutionEvent::ProofCapsuleEmitted {
                capsule_id: Uuid::parse_str("550e8400-e29b-41d4-a716-446655440001").unwrap(),
            },
        ]
    }

    #[tokio::test]
    async fn aeon_timeline_sink_posts_expected_event_shape() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let captured_for_responder = Arc::clone(&captured);
        let sink = AeonTimelineSink::with_test_responder(
            &test_config("http://aeon.test", Some("mgmt-key")),
            Arc::new(move |request| {
                captured_for_responder.lock().unwrap().push(request);
                TestHttpResponse {
                    status: 200,
                    body: "{}".to_string(),
                }
            }),
        );

        let status = sink
            .deliver("agent-1", Some("session-1"), &timeline_events())
            .await;

        assert_eq!(status, TimelineDeliveryStatus::Delivered);
        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert!(requests.iter().all(|request| request.method == "POST"));
        assert!(requests
            .iter()
            .all(|request| request.path == "/api/v1/agents/agent-1/timeline"));
        assert!(requests
            .iter()
            .all(|request| request.header("x-management-key") == Some("mgmt-key")));

        let bodies = requests
            .iter()
            .map(|request| serde_json::from_str::<Value>(&request.body).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(bodies[0]["event_type"], "snapshot_created");
        assert_eq!(bodies[0]["session_id"], "session-1");
        assert_eq!(
            bodies[0]["nexus_snapshot_id"],
            "550e8400-e29b-41d4-a716-446655440000"
        );
        assert_eq!(bodies[1]["event_type"], "capability_denied");
        assert!(bodies[1].get("denied_category").is_none());
        assert_eq!(bodies[2]["event_type"], "proof_capsule_emitted");
        assert_eq!(
            bodies[2]["capsule_digest"],
            "550e8400-e29b-41d4-a716-446655440001"
        );
    }

    #[tokio::test]
    async fn aeon_timeline_sink_retries_5xx_then_delivers() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_for_responder = Arc::clone(&attempts);
        let sink = AeonTimelineSink::with_test_responder(
            &test_config("http://aeon.test", Some("mgmt-key")),
            Arc::new(move |_request| {
                let attempt = attempts_for_responder.fetch_add(1, Ordering::SeqCst);
                TestHttpResponse {
                    status: if attempt == 0 { 500 } else { 200 },
                    body: "{}".to_string(),
                }
            }),
        );
        let event = [NexusExecutionEvent::CapabilityDenied {
            denied_category: "read:/secret".to_string(),
        }];

        let status = sink.deliver("agent-1", None, &event).await;

        assert_eq!(status, TimelineDeliveryStatus::Delivered);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn aeon_timeline_sink_transport_error_fails_open() {
        let sink =
            AeonTimelineSink::from_config(&test_config("http://127.0.0.1:1", Some("mgmt-key")));
        let event = [NexusExecutionEvent::CapabilityDenied {
            denied_category: "read:/secret".to_string(),
        }];

        let status = sink.deliver("agent-1", None, &event).await;

        assert_eq!(status, TimelineDeliveryStatus::FailedOpen);
    }

    #[tokio::test]
    async fn aeon_timeline_sink_attested_mode_reports_required_failure() {
        let sink =
            AeonTimelineSink::from_config(&test_config("http://127.0.0.1:1", Some("mgmt-key")))
                .with_mode(TimelineDeliveryMode::Attested);
        let event = [NexusExecutionEvent::CapabilityDenied {
            denied_category: "read:/secret".to_string(),
        }];

        let status = sink.deliver("agent-1", None, &event).await;

        assert_eq!(status, TimelineDeliveryStatus::RequiredButFailed);
    }

    #[tokio::test]
    async fn aeon_timeline_replay_removes_delivered_events() {
        let tmp = tempfile::tempdir().unwrap();
        let spool = tmp.path().join("timeline.jsonl");
        let offline =
            AeonTimelineSink::from_config(&test_config("http://aeon.test", Some("mgmt-key")))
                .with_mode(TimelineDeliveryMode::Offline)
                .with_spool_path(&spool);
        let event = [NexusExecutionEvent::ProofCapsuleEmitted {
            capsule_id: Uuid::parse_str("550e8400-e29b-41d4-a716-446655440001").unwrap(),
        }];

        let queued = offline.deliver("agent-1", Some("session-1"), &event).await;
        assert_eq!(queued, TimelineDeliveryStatus::FireAndForget);

        let captured = Arc::new(Mutex::new(Vec::new()));
        let captured_for_responder = Arc::clone(&captured);
        let online = AeonTimelineSink::with_test_responder(
            &test_config("http://aeon.test", Some("mgmt-key")),
            Arc::new(move |request| {
                captured_for_responder.lock().unwrap().push(request);
                TestHttpResponse {
                    status: 200,
                    body: "{}".to_string(),
                }
            }),
        )
        .with_spool_path(&spool);

        let first = online.replay_spooled_events("agent-1", None).await;
        let second = online.replay_spooled_events("agent-1", None).await;

        assert_eq!(
            first,
            TimelineReplayReport {
                delivered: 1,
                failed: 0,
                skipped: 0,
            }
        );
        assert_eq!(second, TimelineReplayReport::default());
        assert_eq!(captured.lock().unwrap().len(), 1);
    }

    #[test]
    fn build_memory_evidence_ref_absent_when_no_hmac_key() {
        use crate::proof::schema::MemoryAttestationMode;
        let config = AeonConfig {
            hmac_key: None,
            ..test_config("http://aeon.test", None)
        };
        let (evidence, mode) = super::build_memory_evidence_ref(&config, &[], None);
        assert!(evidence.is_none());
        assert_eq!(mode, MemoryAttestationMode::Absent);
    }

    #[test]
    fn build_memory_evidence_ref_attested_with_valid_key_and_hits() {
        use crate::proof::schema::MemoryAttestationMode;
        let config = AeonConfig {
            hmac_key: Some(vec![0x01, 0x02, 0x03, 0x04]),
            ..test_config("http://aeon.test", None)
        };
        let hits = vec![MemoryHit {
            id: "mem-1".to_string(),
            content: "some context".to_string(),
            score: Some(0.9),
        }];
        let (evidence, mode) = super::build_memory_evidence_ref(&config, &hits, None);
        assert!(evidence.is_some());
        assert_eq!(mode, MemoryAttestationMode::AttestedWithRecall);
        assert_eq!(evidence.unwrap().injected_count, 1);
    }

    #[test]
    fn build_memory_evidence_ref_attested_no_hit_with_no_hits() {
        use crate::proof::schema::MemoryAttestationMode;
        let config = AeonConfig {
            hmac_key: Some(vec![0xde, 0xad, 0xbe, 0xef]),
            ..test_config("http://aeon.test", None)
        };
        let (evidence, mode) = super::build_memory_evidence_ref(&config, &[], None);
        assert!(evidence.is_some());
        assert_eq!(mode, MemoryAttestationMode::AttestedNoHit);
        assert_eq!(evidence.unwrap().injected_count, 0);
    }

    #[tokio::test]
    async fn memory_evidence_attested_no_hit_on_zero_hits() {
        use crate::proof::schema::MemoryAttestationMode;
        let management_key = generated_test_management_key();
        let (client, _captured) = mock_client(200, r#"{"results":[]}"#, Some(&management_key));

        let recall = super::recall_memory_evidence_v1(Some(&client), "nothing", 5).await;

        assert!(recall.hits.is_empty());
        assert_eq!(recall.evidence.hit_count, 0);
        assert_eq!(
            recall.evidence.attestation,
            MemoryAttestationMode::AttestedNoHit
        );
    }

    #[tokio::test]
    async fn memory_evidence_attested_with_recall_on_hits() {
        use crate::proof::schema::MemoryAttestationMode;
        let management_key = generated_test_management_key();
        let (client, _captured) = mock_client(
            200,
            r#"{"results":[{"id":"mem-1","content":"first","score":0.9},{"id":"mem-2","content":"second","score":0.8}]}"#,
            Some(&management_key),
        );

        let recall = super::recall_memory_evidence_v1(Some(&client), "context", 5).await;

        assert_eq!(recall.hits.len(), 2);
        assert_eq!(recall.evidence.hit_count, 2);
        assert_eq!(recall.evidence.hit_digests.len(), 2);
        assert_eq!(
            recall.evidence.attestation,
            MemoryAttestationMode::AttestedWithRecall
        );
    }

    #[tokio::test]
    async fn memory_evidence_degraded_on_error() {
        use crate::proof::schema::MemoryAttestationMode;
        let management_key = generated_test_management_key();
        let (client, _captured) = mock_client(500, r#"{"error":"boom"}"#, Some(&management_key));

        let recall = super::recall_memory_evidence_v1(Some(&client), "context", 5).await;

        assert!(recall.hits.is_empty());
        assert_eq!(recall.evidence.attestation, MemoryAttestationMode::Degraded);
    }

    #[tokio::test]
    async fn memory_evidence_absent_on_no_config() {
        use crate::proof::schema::MemoryAttestationMode;

        let recall = super::recall_memory_evidence_v1(None, "context", 5).await;

        assert!(recall.hits.is_empty());
        assert_eq!(recall.evidence.attestation, MemoryAttestationMode::Absent);
    }
}
