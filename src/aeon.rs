//! AEON-IQ memory proxy configuration.
//!
//! This module is compiled only with the `aeon-memory` feature. It does not
//! read secrets; upstream LLM credentials continue to flow through the existing
//! provider configuration.

use crate::{NexusError, Result};

const ENABLED_ENV: &str = "NEXUS_AEON_ENABLED";
const BASE_URL_ENV: &str = "NEXUS_AEON_BASE_URL";
const AGENT_ID_ENV: &str = "NEXUS_AEON_AGENT_ID";
const SESSION_ID_ENV: &str = "NEXUS_AEON_SESSION_ID";
const TIMEOUT_MS_ENV: &str = "NEXUS_AEON_TIMEOUT_MS";

/// Configuration for routing ai-recovery LLM calls through AEON-IQ.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AeonConfig {
    pub enabled: bool,
    pub base_url: String,
    pub agent_id: String,
    pub session_id: Option<String>,
    pub timeout_ms: u64,
}

impl Default for AeonConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: "http://localhost:8080".to_string(),
            agent_id: "nexus".to_string(),
            session_id: None,
            timeout_ms: 30_000,
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

    #[test]
    fn default_config_is_disabled_local_proxy() {
        let cfg = AeonConfig::default();

        assert!(!cfg.enabled);
        assert_eq!(cfg.base_url, "http://localhost:8080");
        assert_eq!(cfg.agent_id, "nexus");
        assert_eq!(cfg.session_id, None);
        assert_eq!(cfg.timeout_ms, 30_000);
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
}
