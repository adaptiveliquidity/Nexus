//! Phase C — `nexus-agentd` daemon support.
//!
//! Shared protocol + connection-handler code used by both `src/bin/nexus_agentd.rs`
//! (server side) and `src/main.rs::run_via_daemon` (client side).
//!
//! Wire format
//! -----------
//! Length-prefixed JSON over a Unix socket (POSIX) or named pipe
//! (Windows; see notes). Each frame is:
//!   [u32 big-endian payload length] [payload bytes (JSON)]
//!
//! Endpoints
//! ---------
//! Default socket path:
//!   * POSIX: `${XDG_RUNTIME_DIR:-/tmp}/nexus-agentd.sock`
//!   * Windows: `\\.\pipe\nexus-agentd`
//!     Override with `NEXUS_AGENTD_SOCKET=<path>` for either platform.
//!
//! Optional per-request authentication uses `NEXUS_AGENTD_AUTH_TOKEN`;
//! replay protection and transport encryption remain future work.
//!
//! Concurrency model
//! -----------------
//! Each accepted connection gets its own tokio task. Tasks share an
//! `Arc<HypervisorPool>` so concurrent requests do not serialize on the
//! single `RwLock<WasmSandbox>` inside one hypervisor.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ToolOutput;

pub mod module_cache;
pub mod pool;
pub mod protocol;

pub use module_cache::ModuleCache;
pub use pool::HypervisorPool;

/// Single request frame. Keep this small and stable — both `nexus run`
/// (client) and `nexus-agentd` (server) live in the same repo, but
/// future versions should be able to roll without breaking older
/// clients, so add fields with `#[serde(default)]` rather than
/// renumbering.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DaemonRequest {
    /// Health probe. Server replies with `DaemonResponse::Pong` and the
    /// daemon's version string.
    Ping,
    /// Execute a WASM module on the pool.
    Execute {
        /// Tool name (becomes the `operation` field in the resulting
        /// `ExecutionRecord` / `ErrorLog`).
        name: String,
        /// One of `wasm_bytes` (raw) or `wasm_path` (filesystem path)
        /// must be set; `wasm_bytes` wins if both are.
        #[serde(default, with = "serde_bytes_opt")]
        wasm_bytes: Option<Vec<u8>>,
        #[serde(default)]
        wasm_path: Option<PathBuf>,
        #[serde(default = "default_entry")]
        entry: String,
        #[serde(default)]
        input: serde_json::Value,
        /// Optional bearer token checked by `nexus-agentd` when
        /// `NEXUS_AGENTD_AUTH_TOKEN` is configured. This authenticates the
        /// request value only; replay protection remains future work.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auth_token: Option<String>,
        #[cfg(feature = "aeon-memory")]
        #[serde(default, flatten)]
        aeon: Box<DaemonAeonExecuteOptions>,
    },
    /// Graceful shutdown. Server replies `Pong` then exits.
    Shutdown {
        /// Optional bearer token checked by `nexus-agentd` when
        /// `NEXUS_AGENTD_AUTH_TOKEN` is configured. This authenticates the
        /// request value only; replay protection remains future work.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auth_token: Option<String>,
    },
}

/// AEON-IQ proof/timeline request options flattened into
/// `DaemonRequest::Execute`. Boxing this feature-only group preserves the old
/// daemon request enum size in default builds and keeps feature clippy clean
/// without changing the JSON wire shape.
#[cfg(feature = "aeon-memory")]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DaemonAeonExecuteOptions {
    /// AEON-IQ tenant agent-id. Used to correlate this execution with an
    /// AEON-IQ memory session. The raw id is never logged above `debug!`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aeon_agent_id: Option<String>,
    /// AEON-IQ session-id. Paired with `aeon_agent_id` to form the
    /// `AgentSessionMapping` that anchors the proof-capsule namespace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aeon_session_id: Option<String>,
    /// Pre-computed HMAC digest of the memory-evidence bundle, produced by
    /// the AEON-IQ recall path before dispatch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aeon_memory_evidence_digest: Option<String>,
    /// Capabilities required by this tool when proof mode is enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_capabilities: Option<Vec<crate::security::Capability>>,
    /// Opt into proof-producing daemon execution. Defaults to false so legacy
    /// precompiled execution stays unchanged.
    #[serde(default)]
    pub emit_proof: bool,
    /// Caller capability tokens used by proof mode. When absent, proof mode
    /// falls back to `execute_tool_proof`; when present, the token path can
    /// negotiate denied capabilities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_capabilities: Option<Vec<crate::security::CapabilityToken>>,
    /// Advisory, attested, or offline timeline delivery mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attestation_mode: Option<String>,
}

fn default_entry() -> String {
    "_start".into()
}

/// An execution event returned inside `DaemonResponse`.
/// AEON-IQ consumes these to update its audit ledger; the ledger is
/// best-effort on the AEON-IQ side — a ledger outage degrades
/// auditability but never blocks execution (G3 fail-open invariant).
#[cfg(feature = "aeon-memory")]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NexusExecutionEvent {
    /// A required capability was denied during execution.
    CapabilityDenied { denied_category: String },
    /// A runtime snapshot was captured during this execution.
    SnapshotCreated { snapshot_id: uuid::Uuid },
    /// A proof capsule was emitted for this execution (wired in Phase 9).
    ProofCapsuleEmitted { capsule_id: uuid::Uuid },
}

/// Structured NexusIQ proof/timeline metadata returned by proof-mode daemon
/// execution. Compiled only with `aeon-memory`; default daemon responses do
/// not carry this section.
#[cfg(feature = "aeon-memory")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonNexusIqEvidence {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof_capsule: Option<Box<crate::proof::ProofCapsule>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof_capsule_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_evidence_ref: Option<aeon_nexus_bridge::MemoryEvidenceRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeline_delivery_status: Option<crate::aeon::TimelineDeliveryStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub denial_negotiation: Option<DaemonDenialNegotiation>,
}

#[cfg(feature = "aeon-memory")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonDenialNegotiation {
    pub negotiated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rounds: Option<u32>,
}

/// Single response frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DaemonResponse {
    Pong {
        version: String,
    },
    Executed {
        output: Box<ToolOutput>,
        /// AEON-IQ audit events produced by this execution. Empty on
        /// non-feature builds and when no notable events occurred.
        #[cfg(feature = "aeon-memory")]
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        events: Vec<NexusExecutionEvent>,
        #[cfg(feature = "aeon-memory")]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        nexusiq: Option<DaemonNexusIqEvidence>,
    },
    Error {
        message: String,
        /// AEON-IQ audit events associated with this error (e.g. the
        /// capability category that was denied).
        #[cfg(feature = "aeon-memory")]
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        events: Vec<NexusExecutionEvent>,
        #[cfg(feature = "aeon-memory")]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        nexusiq: Option<DaemonNexusIqEvidence>,
    },
}

/// Compute the default socket path for this platform, honoring
/// `NEXUS_AGENTD_SOCKET` when set. The path is *not* created here; the
/// server is responsible for opening it.
pub fn default_socket_path() -> PathBuf {
    if let Ok(p) = std::env::var("NEXUS_AGENTD_SOCKET") {
        return PathBuf::from(p);
    }
    #[cfg(windows)]
    {
        PathBuf::from(r"\\.\pipe\nexus-agentd")
    }
    #[cfg(not(windows))]
    {
        let dir = std::env::var("XDG_RUNTIME_DIR")
            .ok()
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        dir.join("nexus-agentd.sock")
    }
}

mod serde_bytes_opt {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(v: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(bytes) => {
                use base64::Engine;
                let s_b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
                Some(s_b64).serialize(s)
            }
            None => Option::<String>::None.serialize(s),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<u8>>, D::Error> {
        let s = Option::<String>::deserialize(d)?;
        match s {
            Some(s) => {
                use base64::Engine;
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(s.as_bytes())
                    .map_err(serde::de::Error::custom)?;
                Ok(Some(bytes))
            }
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_client_execute_missing_correlation_is_backward_compat() {
        let json = r#"{"type":"Execute","name":"tool","wasm_bytes":"","input":{}}"#;
        let req: DaemonRequest = serde_json::from_str(json).unwrap();
        match req {
            DaemonRequest::Execute {
                name,
                #[cfg(feature = "aeon-memory")]
                aeon,
                ..
            } => {
                assert_eq!(name, "tool");
                #[cfg(feature = "aeon-memory")]
                {
                    assert!(aeon.aeon_agent_id.is_none());
                    assert!(aeon.aeon_session_id.is_none());
                    assert!(aeon.aeon_memory_evidence_digest.is_none());
                    assert!(aeon.required_capabilities.is_none());
                    assert!(!aeon.emit_proof);
                    assert!(aeon.caller_capabilities.is_none());
                    assert!(aeon.attestation_mode.is_none());
                }
            }
            _ => panic!("expected Execute variant"),
        }
    }

    #[cfg(feature = "aeon-memory")]
    #[test]
    fn daemon_request_execute_round_trips_correlation_fields() {
        let req = DaemonRequest::Execute {
            name: "test_tool".to_string(),
            wasm_bytes: None,
            wasm_path: None,
            entry: "_start".to_string(),
            input: serde_json::json!({}),
            auth_token: None,
            aeon: Box::new(DaemonAeonExecuteOptions {
                aeon_agent_id: Some("agent-42".to_string()),
                aeon_session_id: Some("session-99".to_string()),
                aeon_memory_evidence_digest: Some("abc123digest".to_string()),
                required_capabilities: Some(vec![crate::security::Capability::ReadFile(
                    std::path::PathBuf::from("/data"),
                )]),
                emit_proof: true,
                caller_capabilities: Some(Vec::new()),
                attestation_mode: Some("attested".to_string()),
            }),
        };
        let json = serde_json::to_string(&req).unwrap();
        let req2: DaemonRequest = serde_json::from_str(&json).unwrap();
        match req2 {
            DaemonRequest::Execute { aeon, .. } => {
                assert_eq!(aeon.aeon_agent_id.as_deref(), Some("agent-42"));
                assert_eq!(aeon.aeon_session_id.as_deref(), Some("session-99"));
                assert_eq!(
                    aeon.aeon_memory_evidence_digest.as_deref(),
                    Some("abc123digest")
                );
                assert!(aeon.required_capabilities.is_some());
                assert!(aeon.emit_proof);
                assert_eq!(aeon.caller_capabilities.as_ref().map(Vec::len), Some(0));
                assert_eq!(aeon.attestation_mode.as_deref(), Some("attested"));
            }
            _ => panic!("expected Execute variant"),
        }
    }
}
