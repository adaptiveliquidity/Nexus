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

/// Single response frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DaemonResponse {
    Pong { version: String },
    Executed { output: Box<ToolOutput> },
    Error { message: String },
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
