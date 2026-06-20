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
        /// AEON-IQ tenant agent-id. Used to correlate this execution with an
        /// AEON-IQ memory session. The raw id is never logged above `debug!`.
        #[cfg(feature = "aeon-memory")]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        aeon_agent_id: Option<String>,
        /// AEON-IQ session-id. Paired with `aeon_agent_id` to form the
        /// `AgentSessionMapping` that anchors the proof-capsule namespace.
        #[cfg(feature = "aeon-memory")]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        aeon_session_id: Option<String>,
        /// Pre-computed HMAC digest of the memory-evidence bundle, produced by
        /// the AEON-IQ recall path before dispatch. Reserved for Phase 6+.
        #[cfg(feature = "aeon-memory")]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        aeon_memory_evidence_digest: Option<String>,
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

fn default_entry() -> String {
    "_start".into()
}

/// An execution event returned inside `DaemonResponse`.
/// AEON-IQ consumes these to update its audit ledger; the ledger is
/// best-effort on the AEON-IQ side — a ledger outage degrades
/// auditability but never blocks execution (G3 fail-open invariant).
#[cfg(feature = "aeon-memory")]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NexusExecutionEvent {
    /// A required capability was denied during execution.
    CapabilityDenied { denied_category: String },
    /// A runtime snapshot was captured during this execution.
    SnapshotCreated { snapshot_id: uuid::Uuid },
    /// A proof capsule was emitted for this execution (wired in Phase 9).
    ProofCapsuleEmitted { capsule_id: uuid::Uuid },
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
    },
    Error {
        message: String,
        /// AEON-IQ audit events associated with this error (e.g. the
        /// capability category that was denied).
        #[cfg(feature = "aeon-memory")]
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        events: Vec<NexusExecutionEvent>,
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
                aeon_agent_id,
                #[cfg(feature = "aeon-memory")]
                aeon_session_id,
                #[cfg(feature = "aeon-memory")]
                aeon_memory_evidence_digest,
                ..
            } => {
                assert_eq!(name, "tool");
                #[cfg(feature = "aeon-memory")]
                {
                    assert!(aeon_agent_id.is_none());
                    assert!(aeon_session_id.is_none());
                    assert!(aeon_memory_evidence_digest.is_none());
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
            aeon_agent_id: Some("agent-42".to_string()),
            aeon_session_id: Some("session-99".to_string()),
            aeon_memory_evidence_digest: Some("abc123digest".to_string()),
        };
        let json = serde_json::to_string(&req).unwrap();
        let req2: DaemonRequest = serde_json::from_str(&json).unwrap();
        match req2 {
            DaemonRequest::Execute {
                aeon_agent_id,
                aeon_session_id,
                aeon_memory_evidence_digest,
                ..
            } => {
                assert_eq!(aeon_agent_id.as_deref(), Some("agent-42"));
                assert_eq!(aeon_session_id.as_deref(), Some("session-99"));
                assert_eq!(aeon_memory_evidence_digest.as_deref(), Some("abc123digest"));
            }
            _ => panic!("expected Execute variant"),
        }
    }
}
