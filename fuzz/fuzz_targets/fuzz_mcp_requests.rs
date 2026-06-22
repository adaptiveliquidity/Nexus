//! Fuzz target: feed arbitrary bytes into MCP JSON-RPC parameter
//! deserialization for every major MCP tool param struct.
//!
//! The structs below mirror the public `*Params` types defined in
//! `src/bin/nexus_mcp.rs` (a binary, so not importable as a library).
//! Keeping them in sync is intentional — divergence is caught by
//! integration tests; the fuzz target's job is purely to ensure that
//! *no byte sequence causes a panic or `unwrap` failure* during
//! deserialization and downstream validation.
//!
//! Properties asserted:
//!   1. `serde_json::from_slice` never panics — it always returns Ok or Err.
//!   2. `Uuid::parse_str` on the resulting string fields never panics.
//!   3. Capability name lookup never panics regardless of input string.
//!   4. `sanitize_token_request`-equivalent logic never panics.
//!
//! Run with:
//!     cargo +nightly fuzz run fuzz_mcp_requests

#![no_main]

use libfuzzer_sys::fuzz_target;
use serde::Deserialize;
use uuid::Uuid;

// -- Mirrored param structs -------------------------------------------------
//
// These mirror the `pub struct *Params` in src/bin/nexus_mcp.rs.
// They are local copies because the structs live in a binary crate and
// cannot be imported as a library. Divergence is caught by integration tests.
// `#[allow(dead_code)]` suppresses warnings for fields used only via serde.

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct SnapshotRollbackParams {
    snapshot_id: String,
    include_restored_state: Option<bool>,
    caller_capabilities: Option<Vec<CapabilitySpec>>,
    parent_token_id: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct SnapshotCreateParams {
    label: Option<String>,
    source: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct IssueTokenParams {
    capability: String,
    path: Option<String>,
    validity_secs: Option<u64>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct AttenuateTokenParams {
    parent_token_id: String,
    capability: String,
    path: Option<String>,
    validity_secs: Option<u64>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct ForkAndRaceParams {
    wasm_path: String,
    base_snapshot_id: Option<String>,
    source: Option<String>,
    branches: Vec<BranchSpec>,
    strategy: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct BranchSpec {
    entry: Option<String>,
    input: Option<serde_json::Value>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct CapabilitySpec {
    #[serde(rename = "type")]
    cap_type: String,
    path: Option<String>,
}

// -- Validation logic mirrors -----------------------------------------------

/// Mirror of `parse_capability_from_str` in nexus_mcp.rs.
/// Must never panic.
fn parse_capability_type(cap_type: &str, _path: Option<&str>) -> Option<&'static str> {
    match cap_type {
        "read_file" => Some("ReadFile"),
        "write_file" => Some("WriteFile"),
        "list_dir" => Some("ListDirectory"),
        "http_get" => Some("HttpGet"),
        "http_post" => Some("HttpPost"),
        "execute" => Some("ExecuteBinary"),
        "mount_tmpfs" => Some("MountTmpfs"),
        "memory_preview" | "nexus:memory_preview" => Some("MemoryPreview"),
        "all" => Some("All"),
        _ => None,
    }
}

/// Mirror of `sanitize_token_request` in nexus_mcp.rs.
/// Returns Err for the unrestricted "all" capability; never panics.
fn sanitize_token_request(
    cap_type: &str,
    requested_secs: Option<u64>,
) -> Result<u64, &'static str> {
    if cap_type == "all" {
        return Err("capability 'all' cannot be issued to MCP clients");
    }
    const MAX_TOKEN_VALIDITY_SECS: u64 = 86_400;
    let secs = requested_secs
        .unwrap_or(MAX_TOKEN_VALIDITY_SECS)
        .min(MAX_TOKEN_VALIDITY_SECS);
    Ok(secs)
}

// -- Fuzz target -------------------------------------------------------------

fuzz_target!(|data: &[u8]| {
    // --- SnapshotRollbackParams -------------------------------------------
    if let Ok(params) = serde_json::from_slice::<SnapshotRollbackParams>(data) {
        // snapshot_id must be parseable as UUID by the real handler.
        // The real handler returns Err, not panic, on invalid UUID.
        let _ = Uuid::parse_str(&params.snapshot_id);

        // parent_token_id is also a UUID field.
        if let Some(ref pid) = params.parent_token_id {
            let _ = Uuid::parse_str(pid);
        }

        // Validate each caller_capability spec; lookup must never panic.
        if let Some(caps) = &params.caller_capabilities {
            for spec in caps {
                let _ = parse_capability_type(&spec.cap_type, spec.path.as_deref());
            }
        }
    }

    // --- IssueTokenParams ------------------------------------------------
    if let Ok(params) = serde_json::from_slice::<IssueTokenParams>(data) {
        // Capability lookup must not panic on any string input.
        let known = parse_capability_type(&params.capability, params.path.as_deref());
        if known.is_some() {
            // sanitize_token_request must not panic.
            let _ = sanitize_token_request(&params.capability, params.validity_secs);
        }
    }

    // --- AttenuateTokenParams --------------------------------------------
    if let Ok(params) = serde_json::from_slice::<AttenuateTokenParams>(data) {
        // parent_token_id UUID parse must return Err on invalid input, not panic.
        let _ = Uuid::parse_str(&params.parent_token_id);

        let known = parse_capability_type(&params.capability, params.path.as_deref());
        if known.is_some() {
            let _ = sanitize_token_request(&params.capability, params.validity_secs);
        }
    }

    // --- ForkAndRaceParams -----------------------------------------------
    if let Ok(params) = serde_json::from_slice::<ForkAndRaceParams>(data) {
        if let Some(ref sid) = params.base_snapshot_id {
            let _ = Uuid::parse_str(sid);
        }
        // strategy field: only "first_success" | "wait_all" are valid;
        // unknown values must produce an error in the real handler, not panic.
        if let Some(ref s) = params.strategy {
            let _ = matches!(s.as_str(), "first_success" | "wait_all");
        }
    }

    // --- SnapshotCreateParams -------------------------------------------
    // Minimal struct; just ensure deserialization does not panic.
    let _ = serde_json::from_slice::<SnapshotCreateParams>(data);

    // --- Raw JSON-RPC envelope ------------------------------------------
    // Try to parse as a generic JSON value; the MCP transport layer must
    // never panic on arbitrary bytes.
    let _ = serde_json::from_slice::<serde_json::Value>(data);
});
