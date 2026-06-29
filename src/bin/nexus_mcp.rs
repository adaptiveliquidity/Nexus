//! Nexus MCP Server — exposes hypervisor operations as Model Context Protocol tools.
//!
//! Transport: stdio (for Claude Code / mcp.json integration).
//! Start with: `nexus-mcp` (no arguments needed).

use std::collections::{HashMap, HashSet};
#[cfg(feature = "mcp-http")]
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
#[cfg(feature = "mcp-http")]
use std::time::Instant;

use anyhow::Result;
#[cfg(feature = "mcp-http")]
use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    Router,
};
use rmcp::{
    handler::server::wrapper::Parameters, schemars, tool, tool_handler, tool_router,
    transport::stdio, ServiceExt,
};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use tracing_subscriber::{self, EnvFilter};
use uuid::Uuid;

use nexus::hypervisor::failure_mode::FailureMode;
use nexus::hypervisor::{
    fork_and_race, HypervisorConfig, NexusHypervisor, RecoveryAction, RecoverySource,
    SelectionStrategy, SpeculativeBranch, SpeculativeConfig, ToolDefinition, ToolOutput,
};
use nexus::profile::{load_and_validate, CapabilityProfileManifest};
use nexus::security::{capability::MemoryScope, Capability, CapabilityToken, DenialReason};
use nexus::snapshot::{ExecutionState, FilesystemDiff, SnapshotMetadata};
use nexus::telemetry::{ExecutionRecord, TelemetryStats};
use nexus::NexusError;

const MCP_FUEL_ENV: &str = "NEXUS_MCP_FUEL";
const MCP_TIMEOUT_MS_ENV: &str = "NEXUS_MCP_TIMEOUT_MS";

// ─── MCP Tool Parameter Types ────────────────────────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ExecuteParams {
    #[schemars(description = "Path to the .wasm file to execute")]
    pub wasm_path: String,
    #[schemars(description = "Entry point function name (default: _start)")]
    pub entry: Option<String>,
    #[schemars(description = "JSON input to pass to the WASM module")]
    pub input: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ExecuteRetryParams {
    #[schemars(description = "Path to the .wasm file to execute")]
    pub wasm_path: String,
    #[schemars(description = "Entry point function name (default: _start)")]
    pub entry: Option<String>,
    #[schemars(description = "JSON input to pass to the WASM module")]
    pub input: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct ExecuteProofParams {
    #[schemars(description = "Path to the WASM module file to execute")]
    pub wasm_path: String,
    #[schemars(description = "JSON input to pass to the WASM module")]
    pub input: Option<serde_json::Value>,
    #[cfg(feature = "aeon-memory")]
    #[schemars(description = "AEON-IQ agent id used to correlate the proof with a memory session")]
    pub aeon_agent_id: Option<String>,
    #[cfg(feature = "aeon-memory")]
    #[schemars(
        description = "AEON-IQ session id used to correlate the proof with a memory session"
    )]
    pub aeon_session_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ExecuteWasiParams {
    #[schemars(description = "Path to the .wasm file to execute")]
    pub wasm_path: String,
    #[schemars(description = "Entry point function name (default: _start)")]
    pub entry: Option<String>,
    #[schemars(description = "JSON input to pass to the WASM module")]
    pub input: Option<serde_json::Value>,
    #[schemars(
        description = "Capabilities to grant: array of {type, path?} objects. Types: read_file, write_file, list_dir, http_get, http_post, execute, mount_tmpfs, read_memory, write_memory, all"
    )]
    pub capabilities: Option<Vec<CapabilitySpec>>,
    #[schemars(
        description = "Optional parent capability token UUID. When set, requested capabilities are attenuated from this token instead of granted from the operator allowlist."
    )]
    pub parent_token_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CapabilitySpec {
    #[schemars(
        description = "Capability type: read_file, write_file, list_dir, http_get, http_post, execute, mount_tmpfs, read_memory, write_memory, all"
    )]
    pub r#type: String,
    #[schemars(
        description = "Path or URL pattern for file/http/capability. For memory capabilities, use 'agent:<agent-id>', 'session:<agent-id>:<session-id>', or 'namespace:<namespace>'."
    )]
    pub path: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SnapshotCreateParams {
    #[schemars(description = "Human-readable label for the snapshot")]
    pub label: Option<String>,
    #[schemars(
        description = "Snapshot source. Omit for the backwards-compatible empty/stateless baseline, or use latest_runtime to return the real snapshot captured by the latest nexus_execute call."
    )]
    pub source: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SnapshotRollbackParams {
    #[schemars(description = "UUID of the snapshot to roll back to")]
    pub snapshot_id: String,
    #[schemars(
        description = "When true, include a memory checksum/preview and execution-state summary for the restored snapshot."
    )]
    pub include_restored_state: Option<bool>,
    #[schemars(
        description = "Optional caller capabilities. Include {\"type\":\"nexus:memory_preview\"} to receive restored memory preview bytes."
    )]
    pub caller_capabilities: Option<Vec<CapabilitySpec>>,
    #[schemars(
        description = "Optional parent capability token UUID. When set, requested caller_capabilities are attenuated from this token."
    )]
    pub parent_token_id: Option<String>,
    #[schemars(
        description = "Optional SHA-256 hex digest of the snapshot memory content. When provided, rollback is rejected unless the stored digest matches exactly, preventing TOCTOU substitution attacks."
    )]
    pub expected_digest: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct IssueTokenParams {
    #[schemars(
        description = "Capability type: read_file, write_file, list_dir, http_get, http_post, execute, mount_tmpfs, read_memory, write_memory, all"
    )]
    pub capability: String,
    #[schemars(
        description = "Path or URL pattern for file/http/capability. For memory capabilities, use 'agent:<agent-id>', 'session:<agent-id>:<session-id>', or 'namespace:<namespace>'."
    )]
    pub path: Option<String>,
    #[schemars(description = "Token validity in seconds (default: 3600)")]
    pub validity_secs: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AttenuateTokenParams {
    #[schemars(description = "UUID of the parent token")]
    pub parent_token_id: String,
    #[schemars(
        description = "Capability type: read_file, write_file, list_dir, http_get, http_post, execute, mount_tmpfs, read_memory, write_memory, all"
    )]
    pub capability: String,
    #[schemars(
        description = "Path or URL pattern for file/http/capability. For memory capabilities, use 'agent:<agent-id>', 'session:<agent-id>:<session-id>', or 'namespace:<namespace>'."
    )]
    pub path: Option<String>,
    #[schemars(description = "Token validity in seconds (default: 3600)")]
    pub validity_secs: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ForkAndRaceParams {
    #[schemars(description = "Path to the .wasm file to execute in each branch")]
    pub wasm_path: String,
    #[schemars(
        description = "Optional UUID of a real captured runtime snapshot to restore into every branch before execution."
    )]
    pub base_snapshot_id: Option<String>,
    #[schemars(
        description = "Optional snapshot source. Use latest_runtime to restore the latest snapshot captured by nexus_execute. Omit with base_snapshot_id unset to race branches from scratch."
    )]
    pub source: Option<String>,
    #[schemars(description = "Branch definitions: array of {entry?, input?} objects")]
    pub branches: Vec<BranchSpec>,
    #[schemars(description = "Selection strategy: first_success (default) or wait_all")]
    pub strategy: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct BranchSpec {
    #[schemars(description = "Entry point override for this branch")]
    pub entry: Option<String>,
    #[schemars(description = "JSON input override for this branch")]
    pub input: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct InstinctStatsParams {}

#[derive(Debug, Serialize)]
struct InstinctStatsResponse {
    pub total_instincts: u64,
    pub categories: HashMap<String, u64>,
    pub avg_confidence: f32,
    pub highest_confidence_description: Option<String>,
    pub highest_confidence_value: Option<f32>,
    pub total_support: u64,
    pub total_failures: u64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct InstinctQueryParams {
    #[schemars(description = "Failure category to search against (e.g. TRAP_DIV_BY_ZERO)")]
    pub failure_category: String,
    #[schemars(description = "Operation pattern to match (exact name or * for all operations)")]
    pub operation: String,
}

#[derive(Debug, Serialize)]
struct InstinctQueryResponse {
    pub suggestions: Vec<InstinctSuggestion>,
    pub total: usize,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct InstinctSuggestion {
    #[schemars(with = "String")]
    pub instinct_id: Uuid,
    pub recovery_description: String,
    pub confidence: f32,
    pub operation_pattern: String,
    pub failure_category: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct InstinctRegisterParams {
    #[schemars(description = "Failure category to learn against (e.g. TRAP_DIV_BY_ZERO)")]
    pub failure_category: String,
    #[schemars(description = "Operation pattern (exact match or * for all operations)")]
    pub operation_pattern: String,
    #[schemars(description = "Human-readable recovery advice")]
    pub recovery_description: String,
}

#[derive(Debug, Serialize)]
struct InstinctRegisterResponse {
    pub instinct_id: Uuid,
    pub failure_category: String,
    pub confidence: f32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct InstinctRecordOutcomeParams {
    #[schemars(description = "Instinct UUID to reinforce or erode")]
    pub instinct_id: String,
    #[schemars(description = "Whether the retry outcome was successful")]
    pub success: bool,
}

#[derive(Debug, Serialize)]
struct InstinctRecordOutcomeResponse {
    pub instinct_id: String,
    pub reinforced: bool,
    pub success: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct InstinctExportParams {}

#[derive(Debug, Serialize)]
struct InstinctExportResponse {
    pub json: String,
    pub instinct_count: usize,
}

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct InstinctImportParams {
    #[schemars(description = "JSON payload exported by nexus_instinct_export")]
    pub json: String,
}

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
struct InstinctImportResponse {
    pub imported: usize,
    pub skipped: usize,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetHistoryParams {
    pub limit: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
struct GetHistoryResponse {
    pub records: Vec<ExecutionRecordSummary>,
    pub total: usize,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExecutionRecordSummary {
    pub id: String,
    pub timestamp: String,
    pub operation: String,
    pub success: bool,
    pub duration_ms: u64,
    pub fuel_consumed: u64,
    pub has_error: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetStatsParams {}

#[cfg(feature = "aeon-memory")]
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AeonTimelineExecuteParams {
    #[schemars(description = "Path to the WASM module file to execute")]
    pub wasm_path: String,
    #[schemars(description = "Entry point function name (default: _start)")]
    pub entry: Option<String>,
    #[schemars(description = "JSON input to pass to the WASM module")]
    pub input: Option<serde_json::Value>,
    #[schemars(description = "Capabilities required by the tool: array of {type, path?} objects")]
    pub capabilities: Option<Vec<CapabilitySpec>>,
    #[schemars(
        description = "Capabilities held by the caller. Omit to request the same set as capabilities."
    )]
    pub caller_capabilities: Option<Vec<CapabilitySpec>>,
    #[schemars(
        description = "Optional parent capability token UUID used to attenuate caller_capabilities"
    )]
    pub parent_token_id: Option<String>,
    #[schemars(description = "AEON-IQ agent id used to correlate timeline events")]
    pub aeon_agent_id: Option<String>,
    #[schemars(description = "AEON-IQ session id used to correlate timeline events")]
    pub aeon_session_id: Option<String>,
}

#[cfg(feature = "aeon-memory")]
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct NexusIqExecuteParams {
    #[schemars(description = "Tool name recorded in the proof capsule")]
    pub tool_name: String,
    #[schemars(description = "Base64-encoded WASM module bytes")]
    pub tool_wasm: String,
    #[schemars(description = "JSON string passed to the WASM module")]
    pub input: String,
    #[schemars(description = "AEON-IQ agent id used to correlate recall and timeline events")]
    pub aeon_agent_id: String,
    #[schemars(description = "Optional AEON-IQ session id used to correlate the loop")]
    pub aeon_session_id: Option<String>,
    #[schemars(description = "Timeline delivery mode: advisory, attested, or offline")]
    pub attestation_mode: Option<String>,
    #[schemars(description = "Required capabilities in Nexus description form, e.g. read:/tmp")]
    pub required_capabilities: Option<Vec<String>>,
    #[schemars(description = "Optional AEON-IQ memory recall query")]
    pub memory_query: Option<String>,
    #[schemars(description = "Maximum memory hits to recall; defaults to 5")]
    pub memory_limit: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
struct GetStatsResponse {
    pub telemetry: TelemetryStatsDto,
    pub snapshots: SnapshotStatsDto,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
struct TelemetryStatsDto {
    pub total_executions: u64,
    pub successful_executions: u64,
    pub failed_executions: u64,
    pub total_rollbacks: u64,
    pub avg_duration_ms: f64,
    pub avg_fuel_per_execution: f64,
    pub success_rate: f64,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
struct SnapshotStatsDto {
    pub total_snapshots: u64,
    pub total_rollbacks: u64,
    pub total_memory_saved_mb: f64,
    pub avg_compression_ratio: f64,
    pub last_snapshot_time_us: u64,
}

// ─── MCP Server Handler ──────────────────────────────────────────────────────

#[derive(Clone)]
pub struct NexusMcpServer {
    hypervisor: Arc<NexusHypervisor>,
    wasm_module_dirs: Arc<Vec<PathBuf>>,
    capability_allowlist: Arc<Option<Vec<Capability>>>,
    #[cfg_attr(not(feature = "aeon-memory"), allow(dead_code))]
    nexus_iq_allowlist: Arc<Option<Vec<String>>>,
    capability_profile: Option<Arc<CapabilityProfileManifest>>,
    forced_tool_allowlist: Option<HashSet<String>>,
}

#[tool_router(router = base_tool_router, vis = "pub")]
impl NexusMcpServer {
    #[tool(
        description = "Execute a WASM tool in the Nexus sandbox. Loads the .wasm file, runs it with optional JSON input, and returns structured output including success/failure, result bytes, execution time, fuel consumed, and the runtime snapshot id when WASM memory was captured."
    )]
    async fn nexus_execute(&self, Parameters(params): Parameters<ExecuteParams>) -> String {
        match self.do_execute(params).await {
            Ok(output) => serde_json::to_string_pretty(&output).unwrap_or_else(tool_error_response),
            Err(e) => tool_anyhow_error_response(e),
        }
    }

    #[tool(
        description = "Execute a WASM tool in the Nexus sandbox with instinct-guided retry, and return structured output including success/failure, result bytes, execution time, fuel consumed, and the runtime snapshot id when WASM memory was captured."
    )]
    async fn nexus_execute_retry(
        &self,
        Parameters(params): Parameters<ExecuteRetryParams>,
    ) -> String {
        match self.do_execute_retry(params).await {
            Ok(output) => serde_json::to_string_pretty(&output).unwrap_or_else(tool_error_response),
            Err(e) => tool_anyhow_error_response(e),
        }
    }

    #[tool(
        description = "Execute a WASM module and return a proof reference (digest + scorecard) alongside output. Full proof capsules are included by default in debug/dev builds, and can be enabled in release with NEXUS_MCP_RETURN_FULL_PROOF=1."
    )]
    async fn nexus_execute_proof(
        &self,
        Parameters(params): Parameters<ExecuteProofParams>,
    ) -> String {
        match self.do_execute_proof(params).await {
            Ok(response) => {
                serde_json::to_string_pretty(&response).unwrap_or_else(tool_error_response)
            }
            Err(e) => tool_anyhow_error_response(e),
        }
    }

    #[tool(
        description = "Execute a WASM tool with WASI support (filesystem, env, stdio access). Grants specified capabilities for the duration of execution."
    )]
    async fn nexus_execute_wasi(
        &self,
        Parameters(params): Parameters<ExecuteWasiParams>,
    ) -> String {
        match self.do_execute_wasi(params).await {
            Ok(output) => serde_json::to_string_pretty(&output).unwrap_or_else(tool_error_response),
            Err(e) => tool_anyhow_error_response(e),
        }
    }

    #[tool(
        description = "Create an MCP snapshot handle. By default this creates the backwards-compatible empty/stateless baseline (no WASM memory or execution state). Pass source:\"latest_runtime\" after nexus_execute to return the real runtime snapshot captured from sandbox memory/state."
    )]
    async fn nexus_snapshot_create(
        &self,
        Parameters(params): Parameters<SnapshotCreateParams>,
    ) -> String {
        match self.do_snapshot_create(params) {
            Ok(info) => serde_json::to_string_pretty(&info).unwrap_or_else(tool_error_response),
            Err(e) => tool_anyhow_error_response(e),
        }
    }

    #[tool(
        description = "Roll back to a previous snapshot, restoring memory, execution state, and filesystem to that point in time."
    )]
    async fn nexus_snapshot_rollback(
        &self,
        Parameters(params): Parameters<SnapshotRollbackParams>,
    ) -> String {
        match self.do_snapshot_rollback(params) {
            Ok(info) => serde_json::to_string_pretty(&info).unwrap_or_else(tool_error_response),
            Err(e) => tool_anyhow_error_response(e),
        }
    }

    #[tool(
        description = "Issue an operator-allowlisted capability token that can be passed to execute_wasi calls. Tokens are time-limited and scoped to a specific capability."
    )]
    async fn nexus_issue_token(&self, Parameters(params): Parameters<IssueTokenParams>) -> String {
        match self.do_issue_token(params) {
            Ok(info) => serde_json::to_string_pretty(&info).unwrap_or_else(tool_error_response),
            Err(e) => tool_anyhow_error_response(e),
        }
    }

    #[tool(
        description = "Create a restricted child capability token attenuated from an existing parent token."
    )]
    async fn nexus_attenuate_token(
        &self,
        Parameters(params): Parameters<AttenuateTokenParams>,
    ) -> String {
        match self.do_attenuate_token(params) {
            Ok(info) => serde_json::to_string_pretty(&info).unwrap_or_else(tool_error_response),
            Err(e) => tool_anyhow_error_response(e),
        }
    }

    #[tool(
        description = "Race multiple WASM branches. Pass base_snapshot_id or source:\"latest_runtime\" to restore a real captured runtime snapshot into each branch before execution; omit both for an explicitly from-scratch race."
    )]
    async fn nexus_fork_and_race(
        &self,
        Parameters(params): Parameters<ForkAndRaceParams>,
    ) -> String {
        match self.do_fork_and_race(params).await {
            Ok(info) => serde_json::to_string_pretty(&info).unwrap_or_else(tool_error_response),
            Err(e) => tool_anyhow_error_response(e),
        }
    }

    #[tool(
        description = "Read instinct store summary statistics such as category totals, confidence, support, and failures."
    )]
    async fn nexus_instinct_stats(
        &self,
        Parameters(params): Parameters<InstinctStatsParams>,
    ) -> String {
        match self.do_instinct_stats(params) {
            Ok(info) => serde_json::to_string_pretty(&info).unwrap_or_else(tool_error_response),
            Err(e) => tool_anyhow_error_response(e),
        }
    }

    #[tool(
        description = "Query ranked instinct recovery suggestions by failure category and operation."
    )]
    async fn nexus_instinct_query(
        &self,
        Parameters(params): Parameters<InstinctQueryParams>,
    ) -> String {
        match self.do_instinct_query(params) {
            Ok(info) => serde_json::to_string_pretty(&info).unwrap_or_else(tool_error_response),
            Err(e) => tool_anyhow_error_response(e),
        }
    }

    #[tool(
        description = "Register a new recovery instinct for a failure category and operation pattern."
    )]
    async fn nexus_instinct_register(
        &self,
        Parameters(params): Parameters<InstinctRegisterParams>,
    ) -> String {
        match self.do_instinct_register(params) {
            Ok(info) => serde_json::to_string_pretty(&info).unwrap_or_else(tool_error_response),
            Err(e) => tool_anyhow_error_response(e),
        }
    }

    #[tool(description = "Record instinct outcome success or failure for a UUID.")]
    async fn nexus_instinct_record_outcome(
        &self,
        Parameters(params): Parameters<InstinctRecordOutcomeParams>,
    ) -> String {
        match self.do_instinct_record_outcome(params) {
            Ok(info) => serde_json::to_string_pretty(&info).unwrap_or_else(tool_error_response),
            Err(e) => tool_anyhow_error_response(e),
        }
    }

    #[tool(description = "Export all instincts as JSON.")]
    async fn nexus_instinct_export(
        &self,
        Parameters(params): Parameters<InstinctExportParams>,
    ) -> String {
        match self.do_instinct_export(params) {
            Ok(info) => serde_json::to_string_pretty(&info).unwrap_or_else(tool_error_response),
            Err(e) => tool_anyhow_error_response(e),
        }
    }

    #[tool(description = "Import instincts from JSON payload.")]
    async fn nexus_instinct_import(
        &self,
        Parameters(params): Parameters<InstinctImportParams>,
    ) -> String {
        match self.do_instinct_import(params) {
            Ok(info) => serde_json::to_string_pretty(&info).unwrap_or_else(tool_error_response),
            Err(e) => tool_anyhow_error_response(e),
        }
    }

    #[tool(
        description = "Get recent execution history entries for troubleshooting and observability."
    )]
    async fn nexus_get_history(&self, Parameters(params): Parameters<GetHistoryParams>) -> String {
        match self.do_get_history(params) {
            Ok(info) => serde_json::to_string_pretty(&info).unwrap_or_else(tool_error_response),
            Err(e) => tool_anyhow_error_response(e),
        }
    }

    #[tool(description = "Get aggregate telemetry and snapshot statistics for the hypervisor.")]
    async fn nexus_get_stats(&self, Parameters(params): Parameters<GetStatsParams>) -> String {
        match self.do_get_stats(params) {
            Ok(info) => serde_json::to_string_pretty(&info).unwrap_or_else(tool_error_response),
            Err(e) => tool_anyhow_error_response(e),
        }
    }
}

#[cfg(feature = "aeon-memory")]
#[tool_router(router = aeon_tool_router, vis = "pub")]
impl NexusMcpServer {
    #[tool(
        description = "Execute the canonical NexusIQ loop: recall memory, bind MemoryEvidenceV1, execute with proof and capability negotiation, forward timeline events, and return structured refs."
    )]
    async fn nexus_iq_execute(
        &self,
        Parameters(params): Parameters<NexusIqExecuteParams>,
    ) -> String {
        match self.do_nexus_iq_execute(params).await {
            Ok(info) => serde_json::to_string_pretty(&info).unwrap_or_else(tool_error_response),
            Err(e) => tool_anyhow_error_response(e),
        }
    }

    #[tool(
        description = "Execute a WASM module with AEON-IQ correlation, return the proof capsule, and surface Nexus execution events for forwarding to POST /agents/:id/timeline."
    )]
    async fn nexus_aeon_execute_timeline(
        &self,
        Parameters(params): Parameters<AeonTimelineExecuteParams>,
    ) -> String {
        match self.do_aeon_execute_timeline(params).await {
            Ok(info) => serde_json::to_string_pretty(&info).unwrap_or_else(tool_error_response),
            Err(e) => tool_anyhow_error_response(e),
        }
    }
}

impl NexusMcpServer {
    #[cfg(feature = "aeon-memory")]
    fn combined_tool_router() -> rmcp::handler::server::router::tool::ToolRouter<Self> {
        Self::base_tool_router() + Self::aeon_tool_router()
    }

    #[cfg(not(feature = "aeon-memory"))]
    fn combined_tool_router() -> rmcp::handler::server::router::tool::ToolRouter<Self> {
        Self::base_tool_router()
    }
}

#[tool_handler(router = Self::combined_tool_router())]
impl rmcp::handler::server::ServerHandler for NexusMcpServer {}

// ─── Implementation ──────────────────────────────────────────────────────────

#[derive(Serialize)]
struct ToolOutputResponse {
    success: bool,
    result: Option<String>,
    error: Option<String>,
    execution_time_ms: u64,
    fuel_consumed: u64,
    rollback_performed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    snapshot_id: Option<String>,
}

#[derive(Serialize)]
struct ExecuteProofResponse {
    output: ToolOutputResponse,
    proof_reference: nexus::proof::McpProofReference,
    #[serde(skip_serializing_if = "Option::is_none")]
    proof_capsule: Option<nexus::proof::ProofCapsule>,
    #[cfg(feature = "aeon-memory")]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    events: Vec<nexus::daemon::NexusExecutionEvent>,
}

#[cfg(feature = "aeon-memory")]
#[derive(Serialize)]
struct AeonTimelineExecuteResponse {
    output: ToolOutputResponse,
    proof_capsule: nexus::proof::ProofCapsule,
    events: Vec<nexus::daemon::NexusExecutionEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timeline_delivery_status: Option<nexus::aeon::TimelineDeliveryStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    aeon_agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    aeon_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    aeon_timeline_path: Option<String>,
}

#[cfg(feature = "aeon-memory")]
#[derive(Serialize)]
struct MemoryEvidenceForMcp {
    version: u8,
    hit_count: usize,
    hit_digests: Vec<String>,
    attestation: nexus::proof::schema::MemoryAttestationMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    capsule_digest: Option<String>,
}

#[cfg(feature = "aeon-memory")]
impl From<nexus::aeon::MemoryEvidenceV1> for MemoryEvidenceForMcp {
    fn from(e: nexus::aeon::MemoryEvidenceV1) -> Self {
        Self {
            version: e.version,
            hit_count: e.hit_count,
            hit_digests: e.hit_digests,
            attestation: e.attestation,
            capsule_digest: e.capsule_digest,
        }
    }
}

#[cfg(feature = "aeon-memory")]
#[derive(Serialize)]
struct NexusIqExecuteResponse {
    output: Option<ToolOutputResponse>,
    proof_capsule_ref: Option<String>,
    memory_evidence_ref: MemoryEvidenceForMcp,
    memory_hits_count: usize,
    timeline_status: nexus::aeon::TimelineDeliveryStatus,
    denial_negotiation: Option<DenialNegotiationResponse>,
    attestation_mode: String,
    denied: bool,
    events: Vec<nexus::daemon::NexusExecutionEvent>,
}

#[cfg(feature = "aeon-memory")]
#[derive(Serialize)]
struct DenialNegotiationResponse {
    denied: bool,
    negotiated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    rounds: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

#[cfg(feature = "aeon-memory")]
struct NexusIqDenialContext {
    aeon_agent_id: String,
    aeon_session_id: Option<String>,
    mode: nexus::aeon::TimelineDeliveryMode,
    attestation_mode: String,
    memory_evidence_ref: MemoryEvidenceForMcp,
    memory_hits_count: usize,
    reason: String,
}

impl From<ToolOutput> for ToolOutputResponse {
    fn from(o: ToolOutput) -> Self {
        ToolOutputResponse {
            success: o.success,
            result: o.result.map(|b| {
                String::from_utf8(b.clone()).unwrap_or_else(|_| {
                    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &b)
                })
            }),
            error: o.error,
            execution_time_ms: o.execution_time_ms,
            fuel_consumed: o.fuel_consumed,
            rollback_performed: o.rollback_performed,
            snapshot_id: o.snapshot_id.map(|id| id.to_string()),
        }
    }
}

fn tool_error_response(error: impl std::fmt::Display) -> String {
    serde_json::json!({ "error": error.to_string() }).to_string()
}

fn tool_anyhow_error_response(error: anyhow::Error) -> String {
    let (safe_msg, code): (std::borrow::Cow<str>, Option<i64>) = if let Some(e) =
        error.downcast_ref::<NexusError>()
    {
        match e {
            NexusError::InvalidCapability(detail) => {
                // Decide between TokenExpired / TokenRevoked based on internal detail,
                // but never surface the raw detail (token ids, timestamps) to the client.
                let denial = if detail.contains("expired") {
                    DenialReason::TokenExpired
                } else if detail.contains("revoked") {
                    DenialReason::TokenRevoked
                } else {
                    DenialReason::CapabilityNotPermitted
                };
                (denial.safe_message().into(), None)
            }
            NexusError::CapabilityDenied(detail) => {
                // Profile-level denials get -32602 (Invalid Params) for MCP callers.
                let safe = DenialReason::CapabilityNotPermitted.safe_message();
                let code = if detail.starts_with("capability not permitted by active profile:") {
                    Some(-32602_i64)
                } else {
                    None
                };
                (safe.into(), code)
            }
            NexusError::FilesystemError(_) => (
                DenialReason::WasmPathInaccessible.safe_message().into(),
                None,
            ),
            NexusError::ResourceExhausted(_)
            | NexusError::MemoryLimitExceeded(_)
            | NexusError::FuelExhausted(_)
            | NexusError::Timeout(_) => {
                // Resource limits are safe to report generically.
                (e.to_string().into(), None)
            }
            _ => {
                let id = uuid::Uuid::new_v4();
                tracing::error!(
                    correlation_id = %id,
                    error = ?error,
                    "internal nexus error"
                );
                (format!("internal error [{}]", id).into(), None)
            }
        }
    } else {
        let id = uuid::Uuid::new_v4();
        tracing::error!(
            correlation_id = %id,
            error = ?error,
            "unhandled internal error"
        );
        (format!("internal error [{}]", id).into(), None)
    };

    let json = match code {
        Some(c) => serde_json::json!({ "code": c, "error": safe_msg.as_ref() }),
        None => serde_json::json!({ "error": safe_msg.as_ref() }),
    };
    serde_json::to_string(&json)
        .unwrap_or_else(|_| r#"{"error":"serialization failed"}"#.to_string())
}

#[derive(Serialize)]
struct SnapshotCreateResponse {
    snapshot_id: String,
    success: bool,
    source: String,
    semantics: String,
}

#[derive(Serialize)]
struct RollbackResponse {
    snapshot_id: String,
    timestamp: String,
    fs_operations: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    restored_state: Option<RestoredStateSummary>,
}

#[derive(Serialize)]
struct RestoredStateSummary {
    memory: RestoredMemorySummary,
    execution_state: RestoredExecutionStateSummary,
}

#[derive(Serialize)]
struct RestoredMemorySummary {
    byte_len: usize,
    sha256: String,
    preview_len: usize,
    // Empty unless the caller holds nexus:memory_preview; raw WASM memory can contain secrets.
    preview_base64: String,
}

#[derive(Serialize)]
struct RestoredExecutionStateSummary {
    captured_globals: usize,
    captured_tables: usize,
}

#[derive(Serialize)]
struct TokenResponse {
    token_id: String,
    capability: String,
    expires_in_secs: u64,
}

#[derive(Serialize)]
struct ForkAndRaceResponse {
    winner_branch_id: String,
    branches_tried: usize,
    branches_succeeded: usize,
    winner_elapsed_ms: u64,
    winner_output: Option<ToolOutputResponse>,
    base_snapshot_id: Option<String>,
    base_snapshot_source: String,
    semantics: String,
}

impl NexusMcpServer {
    fn new(hypervisor: Arc<NexusHypervisor>) -> Result<Self> {
        Self::new_with_forced_tool_allowlist(hypervisor, None)
    }

    fn new_with_forced_tool_allowlist(
        hypervisor: Arc<NexusHypervisor>,
        forced_tool_allowlist: Option<HashSet<String>>,
    ) -> Result<Self> {
        let capability_profile = profile_manifest_from_env()?.map(Arc::new);

        // Slice 2: when a profile defines execution.module_dirs, those replace
        // NEXUS_MCP_MODULE_DIR so the allowed module set is profile-declared and
        // diffable rather than implicit in the environment.
        let wasm_module_dirs = if let Some(profile) = &capability_profile {
            let dirs = &profile.execution_policy().module_dirs;
            if !dirs.is_empty() {
                canonicalize_module_dirs(dirs)?
            } else {
                allowed_wasm_module_dirs()?
            }
        } else {
            allowed_wasm_module_dirs()?
        };

        Ok(Self {
            hypervisor,
            wasm_module_dirs: Arc::new(wasm_module_dirs),
            capability_allowlist: Arc::new(capability_allowlist_from_env()?),
            nexus_iq_allowlist: Arc::new(nexus_iq_allowlist_from_env()?),
            capability_profile,
            forced_tool_allowlist,
        })
    }

    async fn do_execute(&self, params: ExecuteParams) -> Result<ToolOutputResponse> {
        self.ensure_tool_allowed("nexus_execute")?;
        let wasm_path = self.resolve_wasm_path(&params.wasm_path)?;
        let wasm_bytes = tokio::fs::read(&wasm_path).await.map_err(|e| {
            tracing::warn!(error = %e, "wasm file read failed");
            anyhow::anyhow!("{}", DenialReason::WasmPathInaccessible.safe_message())
        })?;

        let mut tool = ToolDefinition::new("mcp_tool".to_string(), wasm_bytes);
        if let Some(entry) = params.entry {
            tool = tool.with_entry(&entry);
        }

        let input = params.input.unwrap_or(serde_json::json!({}));
        // Pure-compute path carries no capability tokens; profile gating for
        // this tool is handled by ensure_tool_allowed above (MCP tool allowlist).
        let output = self.hypervisor.execute_tool(tool, input).await?;
        Ok(ToolOutputResponse::from(output))
    }

    async fn do_execute_retry(&self, params: ExecuteRetryParams) -> Result<ToolOutputResponse> {
        self.ensure_tool_allowed("nexus_execute_retry")?;
        let wasm_path = self.resolve_wasm_path(&params.wasm_path)?;
        let wasm_bytes = tokio::fs::read(&wasm_path).await.map_err(|e| {
            tracing::warn!(error = %e, "wasm file read failed");
            anyhow::anyhow!("{}", DenialReason::WasmPathInaccessible.safe_message())
        })?;

        let mut tool = ToolDefinition::new("mcp_tool".to_string(), wasm_bytes);
        if let Some(entry) = params.entry {
            tool = tool.with_entry(&entry);
        }

        let input = params.input.unwrap_or(serde_json::json!({}));
        let output = self.hypervisor.execute_with_retry(tool, input).await?;
        Ok(ToolOutputResponse::from(output))
    }

    async fn do_execute_proof(&self, params: ExecuteProofParams) -> Result<ExecuteProofResponse> {
        self.ensure_tool_allowed("nexus_execute_proof")?;
        self.ensure_proof_enabled()?;
        let wasm_path = self.resolve_wasm_path(&params.wasm_path)?;
        let wasm_bytes = tokio::fs::read(&wasm_path).await.map_err(|e| {
            tracing::warn!(error = %e, "wasm file read failed");
            anyhow::anyhow!("{}", DenialReason::WasmPathInaccessible.safe_message())
        })?;

        let tool = ToolDefinition::new("mcp_tool_proof".to_string(), wasm_bytes);
        #[cfg(feature = "aeon-memory")]
        let tool = tool.with_aeon_context(params.aeon_agent_id, params.aeon_session_id);
        let input = params.input.unwrap_or(serde_json::json!({}));
        let (output, proof_capsule) = self.hypervisor.execute_tool_proof(tool, input).await?;
        #[cfg(feature = "aeon-memory")]
        let events = proof_events(&output, proof_capsule.capsule_id, None);
        let proof_reference = proof_reference(&proof_capsule)?;

        Ok(ExecuteProofResponse {
            output: ToolOutputResponse::from(output),
            proof_reference,
            proof_capsule: if should_include_full_proof_capsule() {
                Some(proof_capsule)
            } else {
                None
            },
            #[cfg(feature = "aeon-memory")]
            events,
        })
    }

    #[cfg(feature = "aeon-memory")]
    async fn do_nexus_iq_execute(
        &self,
        params: NexusIqExecuteParams,
    ) -> Result<NexusIqExecuteResponse> {
        self.ensure_tool_allowed("nexus_iq_execute")?;
        self.ensure_proof_enabled()?;

        let mode = nexus::aeon::TimelineDeliveryMode::parse(params.attestation_mode.as_deref());
        let attestation_mode = timeline_mode_label(mode).to_string();

        if let Some(allowlist) = (*self.nexus_iq_allowlist).as_ref() {
            if !allowlist.iter().any(|allowed| allowed == &params.tool_name) {
                return self
                    .nexus_iq_denial_response(NexusIqDenialContext {
                        aeon_agent_id: params.aeon_agent_id,
                        aeon_session_id: params.aeon_session_id,
                        mode,
                        attestation_mode,
                        memory_evidence_ref: nexus::aeon::MemoryEvidenceV1::new(
                            "",
                            &[],
                            nexus::proof::schema::MemoryAttestationMode::Absent,
                        )
                        .into(),
                        memory_hits_count: 0,
                        reason: DenialReason::ToolNotAllowed.safe_message().to_string(),
                    })
                    .await;
            }
        }

        let aeon_config = nexus::aeon::AeonConfig::from_env().ok();
        let memory_client = if params.memory_query.is_some() {
            match aeon_config.as_ref().filter(|c| c.hmac_key.is_some()) {
                Some(config) => nexus::aeon::AeonMemoryClient::from_enabled_config(config)?,
                None => None,
            }
        } else {
            None
        };

        // Gate memory recall against the requesting AEON context before issuing the call.
        if params.memory_query.is_some() {
            let mem_cap = required_read_memory_scope(
                &params.aeon_agent_id,
                params.aeon_session_id.as_deref(),
            );
            if let Err(e) = self.iq_caller_tokens_for_required(std::slice::from_ref(&mem_cap)) {
                return Err(anyhow::anyhow!(
                    "memory recall requires matching read_memory capability for scope: {e}"
                ));
            }
        }

        // M3: rate-limit memory recall per agent to prevent unbounded AEON-IQ load
        if params.memory_query.is_some() {
            use std::collections::{HashMap, VecDeque};
            use std::sync::{Mutex, OnceLock};
            use std::time::{Duration, Instant};
            const RECALL_RATE_MAX: usize = 10;
            const RECALL_RATE_WINDOW: Duration = Duration::from_secs(60);
            static RECALL_RATE_LIMITER: OnceLock<Mutex<HashMap<String, VecDeque<Instant>>>> =
                OnceLock::new();
            let limiter = RECALL_RATE_LIMITER.get_or_init(|| Mutex::new(HashMap::new()));
            let mut map = limiter.lock().unwrap();
            let now = Instant::now();
            let bucket = map.entry(params.aeon_agent_id.clone()).or_default();
            bucket.retain(|&t| now.duration_since(t) < RECALL_RATE_WINDOW);
            if bucket.len() >= RECALL_RATE_MAX {
                return Err(anyhow::anyhow!(
                    "memory recall rate limit exceeded: max {RECALL_RATE_MAX} calls per {}s per agent",
                    RECALL_RATE_WINDOW.as_secs()
                ));
            }
            bucket.push_back(now);
        }
        let memory_limit = params.memory_limit.unwrap_or(5);
        let recall = match params.memory_query.as_deref() {
            Some(query) => {
                nexus::aeon::recall_memory_evidence_v1(memory_client.as_ref(), query, memory_limit)
                    .await
            }
            None => nexus::aeon::MemoryRecallEvidence {
                hits: Vec::new(),
                evidence: nexus::aeon::MemoryEvidenceV1::new(
                    "",
                    &[],
                    nexus::proof::schema::MemoryAttestationMode::Absent,
                ),
            },
        };
        let memory_hits_count = recall.hits.len();
        let memory_digest = match recall.evidence.attestation {
            nexus::proof::schema::MemoryAttestationMode::Attested
            | nexus::proof::schema::MemoryAttestationMode::AttestedNoHit
            | nexus::proof::schema::MemoryAttestationMode::AttestedWithRecall => Some(
                aeon_config
                    .as_ref()
                    .and_then(|config| config.hmac_key.as_deref())
                    .map_or_else(
                        || recall.evidence.evidence_sha256_digest(),
                        |key| Some(recall.evidence.evidence_hmac_digest(key)),
                    )
                    .ok_or_else(|| anyhow::anyhow!("failed to digest AEON memory evidence"))?,
            ),
            _ => None,
        };

        use base64::Engine as _;
        let wasm_bytes = base64::engine::general_purpose::STANDARD
            .decode(params.tool_wasm.trim())
            .map_err(|e| anyhow::anyhow!("tool_wasm is not valid base64: {e}"))?;
        let input = serde_json::from_str::<serde_json::Value>(&params.input)
            .map_err(|e| anyhow::anyhow!("input is not valid JSON: {e}"))?;
        let required_capabilities = params
            .required_capabilities
            .unwrap_or_default()
            .into_iter()
            .map(|spec| parse_iq_capability(&spec))
            .collect::<Result<Vec<_>>>()?;

        let tool = ToolDefinition::new(params.tool_name, wasm_bytes)
            .with_capabilities(required_capabilities.clone())
            .with_aeon_context(
                Some(params.aeon_agent_id.clone()),
                params.aeon_session_id.clone(),
            )
            .with_aeon_memory_evidence_digest(memory_digest.clone());

        let execution = if required_capabilities.is_empty() {
            self.hypervisor.execute_tool_proof(tool, input).await
        } else {
            match self.iq_caller_tokens_for_required(&required_capabilities) {
                Ok(caller_tokens) => {
                    self.check_tokens_against_active_profile(&caller_tokens)?;
                    self.hypervisor
                        .execute_tool_proof_with_tokens(tool, input, &caller_tokens)
                        .await
                }
                Err(error) => {
                    tracing::warn!(error = %error, "nexus_iq capability check failed");
                    return self
                        .nexus_iq_denial_response(NexusIqDenialContext {
                            aeon_agent_id: params.aeon_agent_id,
                            aeon_session_id: params.aeon_session_id,
                            mode,
                            attestation_mode,
                            memory_evidence_ref: MemoryEvidenceForMcp::from(recall.evidence),
                            memory_hits_count,
                            reason: DenialReason::CapabilityNotPermitted
                                .safe_message()
                                .to_string(),
                        })
                        .await;
                }
            }
        };

        match execution {
            Ok((output, proof_capsule)) => {
                if let Some(expected_digest) = memory_digest.as_deref() {
                    verify_proof_capsule_memory_digest(&proof_capsule, expected_digest)?;
                }
                let negotiation_rounds = proof_capsule.capabilities.negotiation_rounds;
                let events = proof_events(&output, proof_capsule.capsule_id, negotiation_rounds);
                let timeline_status = deliver_nexus_iq_timeline(
                    aeon_config.as_ref(),
                    params.aeon_agent_id,
                    params.aeon_session_id,
                    mode,
                    events.clone(),
                )
                .await;
                let memory_evidence_ref = MemoryEvidenceForMcp::from(
                    recall
                        .evidence
                        .with_capsule_digest(Some(proof_capsule.capsule_id.to_string())),
                );
                Ok(NexusIqExecuteResponse {
                    output: Some(ToolOutputResponse::from(output)),
                    proof_capsule_ref: Some(proof_capsule.capsule_id.to_string()),
                    memory_evidence_ref,
                    memory_hits_count,
                    timeline_status,
                    denial_negotiation: negotiation_rounds.map(|rounds| {
                        DenialNegotiationResponse {
                            denied: false,
                            negotiated: true,
                            rounds: Some(rounds),
                            reason: None,
                        }
                    }),
                    attestation_mode,
                    denied: false,
                    events,
                })
            }
            Err(error) if is_capability_denial(&error) => {
                tracing::warn!(error = %error, "nexus_iq execution capability denied");
                self.nexus_iq_denial_response(NexusIqDenialContext {
                    aeon_agent_id: params.aeon_agent_id,
                    aeon_session_id: params.aeon_session_id,
                    mode,
                    attestation_mode,
                    memory_evidence_ref: MemoryEvidenceForMcp::from(recall.evidence),
                    memory_hits_count,
                    reason: DenialReason::CapabilityNotPermitted
                        .safe_message()
                        .to_string(),
                })
                .await
            }
            Err(error) => Err(error.into()),
        }
    }

    #[cfg(feature = "aeon-memory")]
    async fn do_aeon_execute_timeline(
        &self,
        params: AeonTimelineExecuteParams,
    ) -> Result<AeonTimelineExecuteResponse> {
        self.ensure_tool_allowed("nexus_aeon_execute_timeline")?;
        self.ensure_proof_enabled()?;
        let wasm_path = self.resolve_wasm_path(&params.wasm_path)?;
        let wasm_bytes = tokio::fs::read(&wasm_path).await.map_err(|e| {
            tracing::warn!(error = %e, "wasm file read failed");
            anyhow::anyhow!("{}", DenialReason::WasmPathInaccessible.safe_message())
        })?;

        let required_capabilities = params
            .capabilities
            .unwrap_or_default()
            .into_iter()
            .map(|spec| parse_capability(&spec))
            .collect::<Result<Vec<_>>>()?;
        let caller_capabilities = match params.caller_capabilities {
            Some(specs) => specs
                .into_iter()
                .map(|spec| parse_capability(&spec))
                .collect::<Result<Vec<_>>>()?,
            None => required_capabilities.clone(),
        };
        let caller_tokens =
            self.execute_wasi_tokens(&caller_capabilities, params.parent_token_id.as_deref())?;
        self.check_tokens_against_active_profile(&caller_tokens)?;

        let mut tool = ToolDefinition::new("mcp_aeon_timeline".to_string(), wasm_bytes)
            .with_capabilities(required_capabilities)
            .with_aeon_context(params.aeon_agent_id.clone(), params.aeon_session_id.clone());
        if let Some(entry) = params.entry {
            tool = tool.with_entry(&entry);
        }

        let input = params.input.unwrap_or(serde_json::json!({}));
        self.check_tokens_against_active_profile(&caller_tokens)?;
        let (output, proof_capsule) = self
            .hypervisor
            .execute_tool_proof_with_tokens(tool, input, &caller_tokens)
            .await?;
        let negotiation_rounds = proof_capsule.capabilities.negotiation_rounds;
        let events = proof_events(&output, proof_capsule.capsule_id, negotiation_rounds);
        let aeon_timeline_path = params
            .aeon_agent_id
            .as_ref()
            .map(|agent_id| format!("/agents/{agent_id}/timeline"));
        let timeline_delivery_status = if let Some(agent_id) = params.aeon_agent_id.clone() {
            let sink = match nexus::aeon::AeonConfig::from_env() {
                Ok(config) => match nexus::aeon::AeonTimelineSink::from_enabled_config(&config) {
                    Ok(Some(sink)) => Some(sink),
                    Ok(None) | Err(_) => None,
                },
                Err(_) => None,
            };
            match sink {
                Some(sink) => {
                    let events = events.clone();
                    let session_id = params.aeon_session_id.clone();
                    tokio::spawn(async move {
                        let _ = sink
                            .deliver(&agent_id, session_id.as_deref(), &events)
                            .await;
                    });
                    Some(nexus::aeon::TimelineDeliveryStatus::FireAndForget)
                }
                None => Some(nexus::aeon::TimelineDeliveryStatus::FailedOpen),
            }
        } else {
            None
        };

        Ok(AeonTimelineExecuteResponse {
            output: ToolOutputResponse::from(output),
            proof_capsule,
            events,
            timeline_delivery_status,
            aeon_agent_id: params.aeon_agent_id,
            aeon_session_id: params.aeon_session_id,
            aeon_timeline_path,
        })
    }

    async fn do_execute_wasi(&self, params: ExecuteWasiParams) -> Result<ToolOutputResponse> {
        self.ensure_tool_allowed("nexus_execute_wasi")?;
        self.ensure_wasi_enabled()?;
        let wasm_path = self.resolve_wasm_path(&params.wasm_path)?;
        let wasm_bytes = tokio::fs::read(&wasm_path).await.map_err(|e| {
            tracing::warn!(error = %e, "wasm file read failed");
            anyhow::anyhow!("{}", DenialReason::WasmPathInaccessible.safe_message())
        })?;

        let mut tool = ToolDefinition::new("mcp_tool_wasi".to_string(), wasm_bytes);
        if let Some(entry) = params.entry {
            tool = tool.with_entry(&entry);
        }

        let caps: Vec<Capability> = params
            .capabilities
            .unwrap_or_default()
            .into_iter()
            .map(|spec| parse_capability(&spec))
            .collect::<Result<_>>()?;
        let caller_tokens = self.execute_wasi_tokens(&caps, params.parent_token_id.as_deref())?;
        self.check_tokens_against_active_profile(&caller_tokens)?;
        tool = tool.with_capabilities(caps);

        let input = params.input.unwrap_or(serde_json::json!({}));
        self.check_tokens_against_active_profile(&caller_tokens)?;
        let output = self
            .hypervisor
            .execute_tool_wasi(tool, input, &caller_tokens)
            .await?;
        Ok(ToolOutputResponse::from(output))
    }

    fn do_snapshot_create(&self, params: SnapshotCreateParams) -> Result<SnapshotCreateResponse> {
        self.ensure_tool_allowed("nexus_snapshot_create")?;
        self.ensure_snapshot_enabled()?;
        let label = params.label.unwrap_or_else(|| "mcp_snapshot".to_string());
        let source = params.source.as_deref().unwrap_or("empty_baseline");

        if source == "latest_runtime" {
            let snapshot_id = self.hypervisor.latest_runtime_snapshot_id().ok_or_else(|| {
                anyhow::anyhow!(
                    "no latest runtime snapshot is available; call nexus_execute first or omit source for an empty/stateless baseline"
                )
            })?;
            return Ok(SnapshotCreateResponse {
                snapshot_id: snapshot_id.to_string(),
                success: true,
                source: "latest_runtime".to_string(),
                semantics: "runtime_capture_from_execute".to_string(),
            });
        }

        if source != "empty_baseline" {
            anyhow::bail!(
                "unsupported snapshot source '{source}'; expected 'latest_runtime' or omit source for the empty/stateless baseline"
            );
        }

        let metadata = SnapshotMetadata {
            operation_name: label,
            input_hash: String::new(),
            creation_time_us: 0,
            memory_pages: 0,
            preconditions: vec![],
        };

        let snapshot = self.hypervisor.snapshot_manager().create_snapshot(
            vec![],
            FilesystemDiff::default(),
            ExecutionState::default(),
            metadata,
        )?;

        Ok(SnapshotCreateResponse {
            snapshot_id: snapshot.id.to_string(),
            success: true,
            source: "empty_baseline".to_string(),
            semantics: "empty_stateless_baseline_no_wasm_memory_or_execution_state".to_string(),
        })
    }

    fn do_snapshot_rollback(&self, params: SnapshotRollbackParams) -> Result<RollbackResponse> {
        self.ensure_tool_allowed("nexus_snapshot_rollback")?;
        self.ensure_snapshot_enabled()?;
        let id = Uuid::parse_str(&params.snapshot_id)
            .map_err(|e| anyhow::anyhow!("Invalid snapshot UUID: {e}"))?;
        let caller_capabilities: Vec<Capability> = params
            .caller_capabilities
            .unwrap_or_default()
            .into_iter()
            .map(|spec| parse_capability(&spec))
            .collect::<Result<_>>()?;
        if let Some(ref expected) = params.expected_digest {
            match self.hypervisor.snapshot_content_digest(&id) {
                Some(ref actual) if actual.eq_ignore_ascii_case(expected) => {}
                _ => return Err(anyhow::anyhow!("snapshot digest mismatch")),
            }
        }
        let caller_tokens =
            self.execute_wasi_tokens(&caller_capabilities, params.parent_token_id.as_deref())?;
        self.check_tokens_against_active_profile(&caller_tokens)?;

        let result = self.hypervisor.rollback_snapshot(id)?;
        let restored_state = if params.include_restored_state.unwrap_or(false) {
            Some(restored_state_summary(
                &result,
                caller_has_memory_preview(&caller_tokens),
            ))
        } else {
            None
        };

        Ok(RollbackResponse {
            snapshot_id: result.snapshot_id.to_string(),
            timestamp: result.timestamp.to_rfc3339(),
            fs_operations: result.fs_operations.len(),
            restored_state,
        })
    }

    fn do_issue_token(&self, params: IssueTokenParams) -> Result<TokenResponse> {
        self.ensure_tool_allowed("nexus_issue_token")?;
        if matches!(params.capability.as_str(), "http_get" | "http_post") {
            if let Some(ref url) = params.path {
                nexus::security::validate_http_capability_pattern(url)
                    .map_err(|e| anyhow::anyhow!("{}", e))?;
            }
        }
        let capability = parse_capability_from_str(&params.capability, params.path.as_deref())
            .ok_or_else(|| anyhow::anyhow!("Unknown capability type: {}", params.capability))?;

        // Security: reject the unrestricted `All` capability and clamp the
        // caller-supplied validity to a bounded maximum (see SECURITY.md).
        let (capability, validity_secs) = sanitize_token_request(capability, params.validity_secs)?;
        self.ensure_operator_allowlisted(&capability)?;
        let validity = Duration::from_secs(validity_secs);
        let token = self
            .hypervisor
            .issue_token(capability.clone(), "mcp_client", validity)?;

        Ok(TokenResponse {
            token_id: token.id.to_string(),
            capability: format!("{:?}", capability),
            expires_in_secs: validity_secs,
        })
    }

    fn do_attenuate_token(&self, params: AttenuateTokenParams) -> Result<TokenResponse> {
        self.ensure_tool_allowed("nexus_attenuate_token")?;
        let parent_id = Uuid::parse_str(&params.parent_token_id)
            .map_err(|e| anyhow::anyhow!("invalid parent_token_id UUID: {e}"))?;
        let capability = parse_capability_from_str(&params.capability, params.path.as_deref())
            .ok_or_else(|| anyhow::anyhow!("Unknown capability type: {}", params.capability))?;

        // Security: reject the unrestricted `All` capability and clamp the
        // caller-supplied validity to a bounded maximum (see SECURITY.md).
        let (capability, validity_secs) = sanitize_token_request(capability, params.validity_secs)?;
        let validity = Duration::from_secs(validity_secs);
        let token = self.hypervisor.attenuate_token(
            parent_id,
            capability.clone(),
            "mcp_client",
            validity,
        )?;

        Ok(TokenResponse {
            token_id: token.id.to_string(),
            capability: format!("{:?}", capability),
            expires_in_secs: validity_secs,
        })
    }

    async fn do_fork_and_race(&self, params: ForkAndRaceParams) -> Result<ForkAndRaceResponse> {
        self.ensure_tool_allowed("nexus_fork_and_race")?;
        self.ensure_fork_enabled()?;
        let wasm_path = self.resolve_wasm_path(&params.wasm_path)?;
        let wasm_bytes = tokio::fs::read(&wasm_path).await.map_err(|e| {
            tracing::warn!(error = %e, "wasm file read failed");
            anyhow::anyhow!("{}", DenialReason::WasmPathInaccessible.safe_message())
        })?;

        let (base_snapshot_id, base_snapshot_source, semantics) =
            self.resolve_fork_base_snapshot(&params)?;
        let branch_base_snapshot_id = base_snapshot_id.unwrap_or_else(Uuid::new_v4);
        let mut branch_inputs = HashMap::new();

        let branches: Vec<SpeculativeBranch> = params
            .branches
            .into_iter()
            .map(|spec| {
                let mut tool = ToolDefinition::new("fork_branch".to_string(), wasm_bytes.clone());
                if let Some(entry) = spec.entry {
                    tool = tool.with_entry(&entry);
                }
                let branch = SpeculativeBranch::new(
                    branch_base_snapshot_id,
                    tool,
                    RecoveryAction::new("mcp_fork_branch", RecoverySource::Static),
                );
                branch_inputs.insert(
                    branch.id,
                    spec.input.unwrap_or_else(|| serde_json::json!({})),
                );
                branch
            })
            .collect();

        let strategy = match params.strategy.as_deref() {
            Some("wait_all") => SelectionStrategy::WaitAll,
            _ => SelectionStrategy::FirstSuccess,
        };

        let config = SpeculativeConfig {
            max_branches: branches.len(),
            branch_timeout: Duration::from_secs(30),
            selection_strategy: strategy,
        };

        let hyp = self.hypervisor.clone();
        let branch_inputs = Arc::new(branch_inputs);
        let result = fork_and_race(branches, &config, |branch| {
            let hyp = hyp.clone();
            let branch_inputs = branch_inputs.clone();
            async move {
                let input = branch_inputs
                    .get(&branch.id)
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}));
                if let Some(base_snapshot_id) = base_snapshot_id {
                    hyp.execute_tool_from_snapshot(base_snapshot_id, branch.tool, input)
                        .await
                } else {
                    hyp.execute_tool(branch.tool, input).await
                }
            }
        })
        .await?;

        Ok(ForkAndRaceResponse {
            winner_branch_id: result.winner.branch_id.to_string(),
            branches_tried: result.branches_tried,
            branches_succeeded: result.branches_succeeded,
            winner_elapsed_ms: result.winner.elapsed.as_millis() as u64,
            winner_output: result.winner.output.map(ToolOutputResponse::from),
            base_snapshot_id: base_snapshot_id.map(|id| id.to_string()),
            base_snapshot_source,
            semantics,
        })
    }

    fn do_instinct_stats(&self, _params: InstinctStatsParams) -> Result<InstinctStatsResponse> {
        self.ensure_tool_allowed("nexus_instinct_stats")?;
        self.ensure_instinct_enabled()?;
        let Some(store) = self.hypervisor.instinct_store() else {
            anyhow::bail!("instinct store not initialised");
        };

        let stats = store.stats();
        let (highest_confidence_description, highest_confidence_value) =
            match stats.highest_confidence {
                Some((description, value)) => (Some(description), Some(value)),
                None => (None, None),
            };

        Ok(InstinctStatsResponse {
            total_instincts: stats.total_instincts,
            categories: stats.categories,
            avg_confidence: stats.avg_confidence,
            highest_confidence_description,
            highest_confidence_value,
            total_support: stats.total_support,
            total_failures: stats.total_failures,
        })
    }

    fn do_instinct_query(&self, params: InstinctQueryParams) -> Result<InstinctQueryResponse> {
        self.ensure_tool_allowed("nexus_instinct_query")?;
        self.ensure_instinct_enabled()?;
        let Some(store) = self.hypervisor.instinct_store() else {
            anyhow::bail!("instinct store not initialised");
        };

        let mode = failure_mode_from_category(&params.failure_category)?;
        let instincts = store.query(&mode, &params.operation);
        let suggestions = instincts
            .into_iter()
            .map(|instinct| InstinctSuggestion {
                instinct_id: instinct.id,
                recovery_description: instinct.recovery_description,
                confidence: instinct.confidence,
                operation_pattern: instinct.operation_pattern,
                failure_category: instinct.failure_category,
            })
            .collect::<Vec<_>>();

        Ok(InstinctQueryResponse {
            total: suggestions.len(),
            suggestions,
        })
    }

    fn do_instinct_register(
        &self,
        params: InstinctRegisterParams,
    ) -> Result<InstinctRegisterResponse> {
        self.ensure_tool_allowed("nexus_instinct_register")?;
        self.ensure_instinct_enabled()?;
        let Some(store) = self.hypervisor.instinct_store() else {
            anyhow::bail!("instinct store not initialised");
        };

        // Validate inputs before touching the on-disk store.
        const MAX_DESC_LEN: usize = 1024;
        if params.recovery_description.len() > MAX_DESC_LEN {
            anyhow::bail!("recovery_description exceeds {MAX_DESC_LEN} characters");
        }
        let valid_pattern = params.operation_pattern == "*"
            || params
                .operation_pattern
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                && params.operation_pattern.len() <= 128;
        if !valid_pattern {
            anyhow::bail!(
                "operation_pattern must be '*' or a name containing only alphanumerics, underscores, and hyphens (max 128 chars)"
            );
        }

        let mode = failure_mode_from_category(&params.failure_category)
            .map_err(|e| anyhow::anyhow!("unknown failure_category: {e}"))?;
        let instinct_id = store.register(
            &mode,
            &params.operation_pattern,
            &params.recovery_description,
        )?;

        Ok(InstinctRegisterResponse {
            instinct_id,
            failure_category: params.failure_category,
            confidence: 0.5,
        })
    }

    fn do_instinct_record_outcome(
        &self,
        params: InstinctRecordOutcomeParams,
    ) -> Result<InstinctRecordOutcomeResponse> {
        self.ensure_tool_allowed("nexus_instinct_record_outcome")?;
        self.ensure_instinct_enabled()?;
        let Some(store) = self.hypervisor.instinct_store() else {
            anyhow::bail!("instinct store not initialised");
        };

        let instinct_id = Uuid::parse_str(&params.instinct_id)
            .map_err(|e| anyhow::anyhow!("invalid instinct UUID: {e}"))?;
        let reinforced = if params.success {
            store.record_success(&instinct_id)?
        } else {
            store.record_failure(&instinct_id)?
        };

        Ok(InstinctRecordOutcomeResponse {
            instinct_id: params.instinct_id,
            reinforced,
            success: params.success,
        })
    }

    fn do_instinct_export(&self, _params: InstinctExportParams) -> Result<InstinctExportResponse> {
        self.ensure_tool_allowed("nexus_instinct_export")?;
        self.ensure_instinct_enabled()?;
        let Some(store) = self.hypervisor.instinct_store() else {
            anyhow::bail!("instinct store not initialised");
        };

        let json = store.export_all()?;
        let parsed: serde_json::Value = serde_json::from_str(&json)?;
        let instinct_count = parsed.as_array().map(|array| array.len()).unwrap_or(0);

        Ok(InstinctExportResponse {
            json,
            instinct_count,
        })
    }

    fn do_instinct_import(&self, params: InstinctImportParams) -> Result<InstinctImportResponse> {
        self.ensure_tool_allowed("nexus_instinct_import")?;
        let Some(store) = self.hypervisor.instinct_store() else {
            anyhow::bail!("instinct store not initialised");
        };

        if params.json.len() > 10_485_760 {
            anyhow::bail!("json payload exceeds 10 MiB limit");
        }

        let (imported, skipped) = store.import_all(&params.json)?;
        Ok(InstinctImportResponse { imported, skipped })
    }

    fn do_get_history(&self, params: GetHistoryParams) -> Result<GetHistoryResponse> {
        self.ensure_tool_allowed("nexus_get_history")?;

        let limit = params.limit.map(|l| l as usize).or(Some(50));
        let records: Vec<ExecutionRecordSummary> = self
            .hypervisor
            .get_history(limit)
            .into_iter()
            .map(|record: ExecutionRecord| ExecutionRecordSummary {
                id: record.id,
                timestamp: record.timestamp.to_rfc3339(),
                operation: record.operation,
                success: record.success,
                duration_ms: record.duration_ms,
                fuel_consumed: record.fuel_consumed,
                has_error: record.error.is_some(),
            })
            .collect();

        let total = records.len();
        Ok(GetHistoryResponse { records, total })
    }

    fn do_get_stats(&self, _params: GetStatsParams) -> Result<GetStatsResponse> {
        self.ensure_tool_allowed("nexus_get_stats")?;

        let telemetry: TelemetryStats = self.hypervisor.get_stats();
        let snapshots = self.hypervisor.get_snapshot_stats();

        Ok(GetStatsResponse {
            telemetry: TelemetryStatsDto {
                total_executions: telemetry.total_executions,
                successful_executions: telemetry.successful_executions,
                failed_executions: telemetry.failed_executions,
                total_rollbacks: telemetry.total_rollbacks,
                avg_duration_ms: telemetry.avg_duration_ms,
                avg_fuel_per_execution: telemetry.avg_fuel_per_execution,
                success_rate: telemetry.success_rate,
            },
            snapshots: SnapshotStatsDto {
                total_snapshots: snapshots.total_snapshots,
                total_rollbacks: snapshots.total_rollbacks,
                total_memory_saved_mb: snapshots.total_memory_saved_mb,
                avg_compression_ratio: snapshots.avg_compression_ratio,
                last_snapshot_time_us: snapshots.last_snapshot_time_us,
            },
        })
    }

    fn resolve_fork_base_snapshot(
        &self,
        params: &ForkAndRaceParams,
    ) -> Result<(Option<Uuid>, String, String)> {
        if params.base_snapshot_id.is_some() && params.source.is_some() {
            anyhow::bail!(
                "base_snapshot_id and source are mutually exclusive; provide one snapshot seed or omit both for from-scratch racing"
            );
        }

        if let Some(snapshot_id) = params.base_snapshot_id.as_deref() {
            let id = Uuid::parse_str(snapshot_id)
                .map_err(|e| anyhow::anyhow!("Invalid base_snapshot_id UUID: {e}"))?;
            return Ok((
                Some(id),
                "explicit_snapshot_id".to_string(),
                "fork_from_captured_runtime_snapshot".to_string(),
            ));
        }

        if let Some(source) = params.source.as_deref() {
            if source != "latest_runtime" {
                anyhow::bail!(
                    "unsupported fork_and_race snapshot source '{source}'; expected 'latest_runtime' or omit source for from-scratch racing"
                );
            }
            let id = self.hypervisor.latest_runtime_snapshot_id().ok_or_else(|| {
                anyhow::anyhow!(
                    "no latest runtime snapshot is available; call nexus_execute first or omit source for from-scratch racing"
                )
            })?;
            return Ok((
                Some(id),
                "latest_runtime".to_string(),
                "fork_from_captured_runtime_snapshot".to_string(),
            ));
        }

        Ok((
            None,
            "from_scratch".to_string(),
            "from_scratch_no_snapshot_restore".to_string(),
        ))
    }

    fn resolve_wasm_path(&self, wasm_path: impl AsRef<Path>) -> Result<PathBuf> {
        resolve_wasm_path(wasm_path.as_ref(), self.wasm_module_dirs.as_slice())
    }

    fn execute_wasi_tokens(
        &self,
        capabilities: &[Capability],
        parent_token_id: Option<&str>,
    ) -> Result<Vec<CapabilityToken>> {
        if capabilities.is_empty() {
            return Ok(Vec::new());
        }

        let mut sanitized = Vec::with_capacity(capabilities.len());
        for capability in capabilities {
            sanitized.push(sanitize_token_request(capability.clone(), None)?);
        }

        if let Some(parent_token_id) = parent_token_id {
            let parent_id = Uuid::parse_str(parent_token_id)
                .map_err(|e| anyhow::anyhow!("invalid parent_token_id '{parent_token_id}': {e}"))?;
            return sanitized
                .into_iter()
                .map(|(capability, validity_secs)| {
                    self.hypervisor
                        .attenuate_token(
                            parent_id,
                            capability,
                            "mcp_client",
                            Duration::from_secs(validity_secs),
                        )
                        .map_err(|e| {
                            tracing::warn!(
                                parent_id = %parent_id,
                                error = %e,
                                "attenuate_token: capability outside parent scope"
                            );
                            anyhow::Error::from(NexusError::CapabilityDenied(
                                "capability not permitted by active profile: requested capability outside parent token scope".to_string(),
                            ))
                        })
                })
                .collect();
        }

        let Some(allowlist) = self.capability_allowlist.as_ref() else {
            return Err(NexusError::CapabilityDenied(
                "capability not permitted by active profile: no operator allowlist configured"
                    .to_string(),
            )
            .into());
        };

        let mut tokens = Vec::with_capacity(sanitized.len());
        for (capability, validity_secs) in sanitized {
            if !capability_allowed_by(allowlist, &capability) {
                return Err(NexusError::CapabilityDenied(
                    "capability not permitted by active profile: requested capability not in allowlist".to_string(),
                )
                .into());
            }
            tokens.push(self.hypervisor.issue_token(
                capability,
                "mcp_operator_allowlist",
                Duration::from_secs(validity_secs),
            )?);
        }
        Ok(tokens)
    }

    fn ensure_operator_allowlisted(&self, capability: &Capability) -> Result<()> {
        let Some(allowlist) = self.capability_allowlist.as_ref() else {
            return Err(NexusError::CapabilityDenied(
                "capability not permitted by active profile: no operator allowlist configured"
                    .to_string(),
            )
            .into());
        };

        if !capability_allowed_by(allowlist, capability) {
            return Err(NexusError::CapabilityDenied(
                "capability not permitted by active profile: requested capability not in allowlist"
                    .to_string(),
            )
            .into());
        }
        Ok(())
    }

    fn check_tokens_against_active_profile(&self, tokens: &[CapabilityToken]) -> Result<()> {
        for token in tokens {
            if self.hypervisor.is_token_revoked(&token.id) {
                return Err(anyhow::anyhow!("capability token has been revoked"));
            }
        }
        if let Some(profile) = self.capability_profile.as_deref() {
            check_tokens_against_profile(tokens, profile)?;
        } else {
            check_tokens_fresh(tokens)?;
        }
        Ok(())
    }

    #[cfg(feature = "aeon-memory")]
    fn iq_caller_tokens_for_required(
        &self,
        capabilities: &[Capability],
    ) -> Result<Vec<CapabilityToken>> {
        if capabilities.is_empty() {
            return Ok(Vec::new());
        }

        let Some(allowlist) = self.capability_allowlist.as_ref() else {
            return Ok(Vec::new());
        };

        let mut tokens = Vec::new();
        for capability in capabilities {
            let (capability, validity_secs) = sanitize_token_request(capability.clone(), None)?;
            if capability_allowed_by(allowlist, &capability) {
                tokens.push(self.hypervisor.issue_token(
                    capability,
                    "mcp_nexus_iq_allowlist",
                    Duration::from_secs(validity_secs),
                )?);
            }
        }
        self.check_tokens_against_active_profile(&tokens)?;
        Ok(tokens)
    }

    #[cfg(feature = "aeon-memory")]
    async fn nexus_iq_denial_response(
        &self,
        context: NexusIqDenialContext,
    ) -> Result<NexusIqExecuteResponse> {
        let aeon_config = nexus::aeon::AeonConfig::from_env().ok();
        let events = vec![nexus::daemon::NexusExecutionEvent::CapabilityDenied {
            denied_category: "capability_denied".to_string(),
        }];
        let timeline_status = deliver_nexus_iq_timeline(
            aeon_config.as_ref(),
            context.aeon_agent_id,
            context.aeon_session_id,
            context.mode,
            events.clone(),
        )
        .await;

        Ok(NexusIqExecuteResponse {
            output: None,
            proof_capsule_ref: None,
            memory_evidence_ref: context.memory_evidence_ref,
            memory_hits_count: context.memory_hits_count,
            timeline_status,
            denial_negotiation: Some(DenialNegotiationResponse {
                denied: true,
                negotiated: false,
                rounds: None,
                reason: Some(context.reason),
            }),
            attestation_mode: context.attestation_mode,
            denied: true,
            events,
        })
    }

    /// Deny a tool call when the active profile's `[mcp].tool_allowlist` excludes it.
    fn ensure_tool_allowed(&self, tool: &str) -> Result<()> {
        if let Some(allowlist) = &self.forced_tool_allowlist {
            if !allowlist.contains(tool) {
                return Err(profile_denial(format!(
                    "tool {tool} is not allowed in HTTP read-only mode"
                )));
            }
        }

        let Some(profile) = self.capability_profile.as_deref() else {
            return Ok(());
        };
        if profile.mcp_policy().allows_tool(tool) {
            return Ok(());
        }
        Err(profile_denial(format!(
            "tool {tool} is not in the MCP tool allowlist"
        )))
    }

    /// Deny the snapshot tools when the active profile sets `snapshot_enabled = false`.
    fn ensure_snapshot_enabled(&self) -> Result<()> {
        let Some(profile) = self.capability_profile.as_deref() else {
            return Ok(());
        };
        if profile.mcp_policy().snapshot_enabled {
            return Ok(());
        }
        Err(profile_denial(
            "snapshot tools are disabled by the active profile".to_string(),
        ))
    }

    /// Deny fork-and-race when the active profile sets `fork_enabled = false`.
    fn ensure_fork_enabled(&self) -> Result<()> {
        let Some(profile) = self.capability_profile.as_deref() else {
            return Ok(());
        };
        if profile.mcp_policy().fork_enabled {
            return Ok(());
        }
        Err(profile_denial(
            "fork_and_race is disabled by the active profile".to_string(),
        ))
    }

    fn ensure_proof_enabled(&self) -> Result<()> {
        let Some(profile) = self.capability_profile.as_deref() else {
            return Ok(());
        };
        if profile.mcp_policy().proof_enabled {
            return Ok(());
        }
        Err(profile_denial(
            "nexus_execute_proof is disabled by the active profile".to_string(),
        ))
    }

    fn ensure_wasi_enabled(&self) -> Result<()> {
        let Some(profile) = self.capability_profile.as_deref() else {
            return Ok(());
        };
        if profile.mcp_policy().wasi_enabled {
            return Ok(());
        }
        Err(profile_denial(
            "nexus_execute_wasi is disabled by the active profile".to_string(),
        ))
    }

    fn ensure_instinct_enabled(&self) -> Result<()> {
        let Some(profile) = self.capability_profile.as_deref() else {
            return Ok(());
        };
        if profile.mcp_policy().instinct_enabled {
            return Ok(());
        }
        Err(profile_denial(
            "instinct tools are disabled by the active profile".to_string(),
        ))
    }

    #[allow(dead_code)]
    fn ensure_retry_enabled(&self) -> Result<()> {
        let Some(profile) = self.capability_profile.as_deref() else {
            return Ok(());
        };
        if profile.mcp_policy().retry_enabled {
            return Ok(());
        }
        Err(profile_denial(
            "nexus_execute_retry is disabled by the active profile".to_string(),
        ))
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

const NEXUS_MCP_MODULE_DIR_ENV: &str = "NEXUS_MCP_MODULE_DIR";
const NEXUS_MCP_CAPABILITY_ALLOWLIST_ENV: &str = "NEXUS_MCP_CAPABILITY_ALLOWLIST";
const NEXUS_IQ_ALLOWLIST_ENV: &str = "NEXUS_IQ_ALLOWLIST";
const NEXUS_MCP_PROFILE_ENV: &str = "NEXUS_MCP_PROFILE";
const NEXUS_MCP_RETURN_FULL_PROOF_ENV: &str = "NEXUS_MCP_RETURN_FULL_PROOF";
const NEXUS_MCP_TRANSPORT_ENV: &str = "NEXUS_MCP_TRANSPORT";
const NEXUS_MCP_TRANSPORT_STDIO: &str = "stdio";
#[cfg(feature = "mcp-http")]
const NEXUS_MCP_TRANSPORT_HTTP: &str = "http";
#[cfg(feature = "mcp-http")]
const NEXUS_MCP_HTTP_ADDR_ENV: &str = "NEXUS_MCP_HTTP_ADDR";
#[cfg(feature = "mcp-http")]
const NEXUS_MCP_HTTP_TOKEN_ENV: &str = "NEXUS_MCP_HTTP_TOKEN";
#[cfg(feature = "mcp-http")]
const NEXUS_MCP_HTTP_TENANTS_ENV: &str = "NEXUS_MCP_HTTP_TENANTS";
#[cfg(feature = "mcp-http")]
const NEXUS_MCP_HTTP_DEFAULT_ADDR: &str = "127.0.0.1:8765";
#[cfg(feature = "mcp-http")]
const NEXUS_MCP_TENANT_SOURCE_ENV: &str = "NEXUS_MCP_TENANT_SOURCE";
#[cfg(feature = "mcp-http")]
const NEXUS_MCP_TENANT_SOURCE_FILE: &str = "file";
#[cfg(feature = "mcp-http")]
const NEXUS_MCP_TENANT_SOURCE_POSTGRES: &str = "postgres";
#[cfg(feature = "mcp-http")]
const NEXUS_MCP_TENANT_DB_URL_ENV: &str = "NEXUS_MCP_TENANT_DB_URL";
#[cfg(feature = "mcp-http")]
const NEXUS_MCP_TENANT_DB_RELATION_ENV: &str = "NEXUS_MCP_TENANT_DB_RELATION";
#[cfg(feature = "mcp-http")]
const NEXUS_MCP_TENANT_DB_RELATION_DEFAULT: &str = "api_keys";
#[cfg(feature = "mcp-http")]
const NEXUS_MCP_TENANT_ACTIVE_API_KEYS_VIEW: &str = "active_api_keys";
#[cfg(feature = "mcp-http")]
const NEXUS_MCP_TENANT_REFRESH_SECS_ENV: &str = "NEXUS_MCP_TENANT_REFRESH_SECS";
#[cfg(feature = "mcp-http")]
const NEXUS_MCP_TENANT_REFRESH_SECS_DEFAULT: u64 = 20;
#[cfg(feature = "mcp-http")]
const NEXUS_MCP_TENANT_MAX_STALE_SECS_ENV: &str = "NEXUS_MCP_TENANT_MAX_STALE_SECS";
#[cfg(feature = "mcp-http")]
const NEXUS_MCP_TENANT_MAX_STALE_SECS_DEFAULT: u64 = 60;
#[cfg(feature = "mcp-http")]
const NEXUS_MCP_TENANT_DB_TIMEOUT_SECS_ENV: &str = "NEXUS_MCP_TENANT_DB_TIMEOUT_SECS";
#[cfg(feature = "mcp-http")]
const NEXUS_MCP_TENANT_DB_TIMEOUT_SECS_DEFAULT: u64 = 10;
#[cfg(feature = "mcp-http")]
const NEXUS_MCP_HTTP_DEFAULT_TENANT_RATE_LIMIT_RPM: u64 = 60;
#[cfg(feature = "mcp-http")]
const NEXUS_MCP_HTTP_TENANT_ID_FALLBACK: &str = "default";
#[cfg(feature = "mcp-http")]
const NEXUS_MCP_HTTP_READ_ONLY_TOOL_ALLOWLIST: [&str; 6] = [
    "nexus_get_history",
    "nexus_get_stats",
    "nexus_instinct_stats",
    "nexus_instinct_query",
    "nexus_instinct_export",
    "nexus_aeon_execute_timeline",
];
pub const NEXUS_MEMORY_PREVIEW_CAPABILITY: &str = "nexus:memory_preview";
const RESTORED_MEMORY_PREVIEW_BYTES: usize = 64;

#[cfg(feature = "mcp-http")]
fn read_only_http_tool_allowlist() -> HashSet<String> {
    NEXUS_MCP_HTTP_READ_ONLY_TOOL_ALLOWLIST
        .into_iter()
        .map(|tool| tool.to_string())
        .collect()
}

#[cfg(feature = "mcp-http")]
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct TenantContext {
    tenant_id: String,
}

#[cfg(feature = "mcp-http")]
#[derive(Debug, Clone)]
struct TenantInfo {
    tenant_id: String,
    rate_limit_rpm: u64,
}

#[cfg(feature = "mcp-http")]
#[derive(Deserialize, Debug)]
struct TenantConfigFileEntry {
    tenant_id: String,
    api_key_sha256: String,
    rate_limit_rpm: Option<u64>,
}

#[cfg(feature = "mcp-http")]
#[derive(Clone)]
struct TenantAuthState {
    registry: Arc<dyn TenantRegistry>,
    rate_limits: Arc<std::sync::Mutex<HashMap<String, TenantRateWindow>>>,
}

#[cfg(feature = "mcp-http")]
impl TenantAuthState {
    fn new(registry: Arc<dyn TenantRegistry>) -> Self {
        Self {
            registry,
            rate_limits: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
        }
    }
}

#[cfg(feature = "mcp-http")]
type TenantSnapshot = HashMap<String, TenantInfo>;

#[cfg(feature = "mcp-http")]
#[derive(Debug)]
enum TenantSource {
    File,
    Postgres,
}

#[cfg(feature = "mcp-http")]
#[derive(Debug)]
struct TenantRateWindow {
    window_start: Instant,
    request_count: u64,
}

#[cfg(feature = "mcp-http")]
trait TenantRegistry: Send + Sync {
    fn current_snapshot(&self) -> Arc<TenantSnapshot>;
}

#[cfg(feature = "mcp-http")]
#[derive(Clone)]
struct FileTenantRegistry {
    snapshot: Arc<TenantSnapshot>,
}

#[cfg(feature = "mcp-http")]
impl FileTenantRegistry {
    fn new(snapshot: TenantSnapshot) -> Self {
        Self {
            snapshot: Arc::new(snapshot),
        }
    }
}

#[cfg(feature = "mcp-http")]
impl TenantRegistry for FileTenantRegistry {
    fn current_snapshot(&self) -> Arc<TenantSnapshot> {
        self.snapshot.clone()
    }
}

#[cfg(all(feature = "mcp-http", feature = "tenant-registry-postgres"))]
#[derive(Clone)]
struct PostgresTenantRegistry {
    relation: String,
    default_rate_limit_rpm: u64,
    refresh_interval: Duration,
    max_stale: Duration,
    refresh_timeout: Duration,
    pool: sqlx::PgPool,
    snapshot: Arc<arc_swap::ArcSwap<TenantSnapshotState>>,
}

#[cfg(all(feature = "mcp-http", feature = "tenant-registry-postgres"))]
#[derive(Clone)]
struct TenantSnapshotState {
    snapshot: Arc<TenantSnapshot>,
    refreshed_at: Option<Instant>,
}

#[cfg(all(feature = "mcp-http", feature = "tenant-registry-postgres"))]
impl TenantSnapshotState {
    fn fresh(snapshot: TenantSnapshot, refreshed_at: Instant) -> Self {
        Self {
            snapshot: Arc::new(snapshot),
            refreshed_at: Some(refreshed_at),
        }
    }

    fn empty() -> Self {
        Self {
            snapshot: Arc::new(HashMap::new()),
            refreshed_at: None,
        }
    }
}

#[cfg(all(feature = "mcp-http", feature = "tenant-registry-postgres"))]
impl TenantRegistry for PostgresTenantRegistry {
    fn current_snapshot(&self) -> Arc<TenantSnapshot> {
        self.snapshot.load_full().snapshot.clone()
    }
}

#[cfg(feature = "mcp-http")]
fn parse_nexus_mcp_transport() -> String {
    std::env::var(NEXUS_MCP_TRANSPORT_ENV).unwrap_or_else(|_| NEXUS_MCP_TRANSPORT_STDIO.to_string())
}

#[cfg(feature = "mcp-http")]
fn parse_nexus_mcp_http_addr() -> Result<SocketAddr> {
    std::env::var(NEXUS_MCP_HTTP_ADDR_ENV)
        .unwrap_or_else(|_| NEXUS_MCP_HTTP_DEFAULT_ADDR.to_string())
        .parse::<SocketAddr>()
        .map_err(|error| anyhow::anyhow!("invalid {NEXUS_MCP_HTTP_ADDR_ENV}: {error}"))
}

#[cfg(feature = "mcp-http")]
fn parse_mcp_http_token() -> Result<Option<String>> {
    match std::env::var(NEXUS_MCP_HTTP_TOKEN_ENV) {
        Ok(token) => Ok(normalize_http_token(&token)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            anyhow::bail!("{NEXUS_MCP_HTTP_TOKEN_ENV} must be valid UTF-8")
        }
    }
}

#[cfg(feature = "mcp-http")]
fn parse_mcp_http_tenants_path() -> Result<Option<String>> {
    match std::env::var(NEXUS_MCP_HTTP_TENANTS_ENV) {
        Ok(path) => {
            let trimmed = path.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            anyhow::bail!("{NEXUS_MCP_HTTP_TENANTS_ENV} must be valid UTF-8 path")
        }
    }
}

#[cfg(feature = "mcp-http")]
fn normalize_http_token(raw: &str) -> Option<String> {
    let token = raw.trim();
    if token.is_empty() {
        None
    } else {
        Some(token.to_string())
    }
}

#[cfg(feature = "mcp-http")]
fn parse_bearer_token(raw: &str) -> Option<String> {
    let (scheme, value) = raw.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    normalize_http_token(value)
}

#[cfg(feature = "mcp-http")]
fn parse_hex_sha256(raw: &str) -> Result<[u8; 32]> {
    if raw.len() != 64 {
        anyhow::bail!("invalid SHA-256 hex length");
    }
    let mut bytes = [0u8; 32];
    for i in 0..32 {
        bytes[i] = u8::from_str_radix(&raw[i * 2..i * 2 + 2], 16)
            .map_err(|_| anyhow::anyhow!("tenant api key hash must be hex"))?;
    }
    Ok(bytes)
}

#[cfg(feature = "mcp-http")]
fn parse_tenant_config_file(path: &Path) -> Result<TenantSnapshot> {
    let contents = std::fs::read_to_string(path).map_err(|error| {
        anyhow::anyhow!("failed to read tenants file '{}': {error}", path.display())
    })?;
    let tenants: Vec<TenantConfigFileEntry> = serde_json::from_str(&contents)
        .map_err(|error| anyhow::anyhow!("invalid tenants JSON: {error}"))?;
    let mut tenant_ids = HashSet::new();
    let mut tenant_hashes = HashSet::new();
    let mut tenant_configs = HashMap::with_capacity(tenants.len());

    if tenants.is_empty() {
        anyhow::bail!("tenant configuration must contain at least one tenant");
    }

    for tenant in tenants {
        let tenant_id = tenant.tenant_id.trim();
        if tenant_id.is_empty() {
            anyhow::bail!("tenant_id must not be empty");
        }
        if !tenant_ids.insert(tenant_id.to_string()) {
            anyhow::bail!("duplicate tenant_id '{tenant_id}'");
        }
        let api_key_sha256_str = tenant.api_key_sha256.trim();
        let api_key_sha256 = parse_hex_sha256(api_key_sha256_str)?;
        let api_key_sha256 = sha256_hex(&api_key_sha256);
        if !tenant_hashes.insert(api_key_sha256.clone()) {
            anyhow::bail!("duplicate tenant api_key_sha256 '{api_key_sha256_str}'");
        }
        let rate_limit_rpm = tenant
            .rate_limit_rpm
            .unwrap_or(NEXUS_MCP_HTTP_DEFAULT_TENANT_RATE_LIMIT_RPM);
        if rate_limit_rpm == 0 {
            anyhow::bail!("rate_limit_rpm for tenant '{tenant_id}' must be greater than 0");
        }
        tenant_configs.insert(
            api_key_sha256,
            TenantInfo {
                tenant_id: tenant_id.to_string(),
                rate_limit_rpm,
            },
        );
    }
    Ok(tenant_configs)
}

#[cfg(feature = "mcp-http")]
fn parse_tenant_source() -> Result<TenantSource> {
    let source = std::env::var(NEXUS_MCP_TENANT_SOURCE_ENV)
        .unwrap_or_else(|_| NEXUS_MCP_TENANT_SOURCE_FILE.to_string());
    match source.trim().to_ascii_lowercase().as_str() {
        NEXUS_MCP_TENANT_SOURCE_FILE => Ok(TenantSource::File),
        NEXUS_MCP_TENANT_SOURCE_POSTGRES => Ok(TenantSource::Postgres),
        _ => anyhow::bail!(
            "invalid {NEXUS_MCP_TENANT_SOURCE_ENV}; expected '{NEXUS_MCP_TENANT_SOURCE_FILE}' or '{NEXUS_MCP_TENANT_SOURCE_POSTGRES}'"
        ),
    }
}

#[cfg(feature = "mcp-http")]
fn parse_tenant_db_url() -> Result<Option<String>> {
    match std::env::var(NEXUS_MCP_TENANT_DB_URL_ENV) {
        Ok(url) => Ok(normalize_http_token(&url)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            anyhow::bail!("{NEXUS_MCP_TENANT_DB_URL_ENV} must be valid UTF-8")
        }
    }
}

#[cfg(feature = "mcp-http")]
fn parse_tenant_db_relation() -> Result<String> {
    let relation = std::env::var(NEXUS_MCP_TENANT_DB_RELATION_ENV)
        .unwrap_or_else(|_| NEXUS_MCP_TENANT_DB_RELATION_DEFAULT.to_string())
        .trim()
        .to_string();

    let valid_relation = relation
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
    if !valid_relation || relation.is_empty() {
        anyhow::bail!("{NEXUS_MCP_TENANT_DB_RELATION_ENV} must be a non-empty relation name");
    }

    Ok(relation)
}

#[cfg(feature = "mcp-http")]
fn parse_tenant_env_u64(name: &str, default: u64) -> Result<u64> {
    Ok(match std::env::var(name) {
        Ok(value) => value
            .parse::<u64>()
            .map_err(|error| anyhow::anyhow!("{name} must be an unsigned integer: {error}"))?,
        Err(std::env::VarError::NotPresent) => default,
        Err(std::env::VarError::NotUnicode(_)) => {
            anyhow::bail!("{name} must be valid UTF-8")
        }
    })
}

#[cfg(feature = "mcp-http")]
fn sha256_hex(raw: &[u8; 32]) -> String {
    let mut hex = String::with_capacity(64);
    for byte in raw {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

#[cfg(feature = "mcp-http")]
fn parse_tenant_auth_state_from_file() -> Result<Option<Arc<TenantAuthState>>> {
    if let Some(path) = parse_mcp_http_tenants_path()? {
        let tenants = parse_tenant_config_file(Path::new(&path)).map_err(|error| {
            anyhow::anyhow!("failed to load {NEXUS_MCP_HTTP_TENANTS_ENV}='{path}': {error}")
        })?;
        return Ok(Some(Arc::new(TenantAuthState::new(Arc::new(
            FileTenantRegistry::new(tenants),
        )))));
    }

    Ok(parse_mcp_http_token()?.map(|token| {
        let mut snapshot = TenantSnapshot::new();
        snapshot.insert(
            sha256_hex_bytes(&token),
            TenantInfo {
                tenant_id: NEXUS_MCP_HTTP_TENANT_ID_FALLBACK.to_string(),
                rate_limit_rpm: NEXUS_MCP_HTTP_DEFAULT_TENANT_RATE_LIMIT_RPM,
            },
        );
        Arc::new(TenantAuthState::new(Arc::new(FileTenantRegistry::new(
            snapshot,
        ))))
    }))
}

#[cfg(all(feature = "mcp-http", feature = "tenant-registry-postgres"))]
impl PostgresTenantRegistry {
    fn new(
        db_url: String,
        relation: String,
        default_rate_limit_rpm: u64,
        refresh_interval: Duration,
        max_stale: Duration,
        refresh_timeout: Duration,
    ) -> Result<Arc<Self>> {
        let connect_options = db_url
            .parse::<sqlx::postgres::PgConnectOptions>()
            .map_err(|error| anyhow::anyhow!("invalid {NEXUS_MCP_TENANT_DB_URL_ENV}: {error}"))?
            .ssl_mode(tenant_db_ssl_mode(&db_url));
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect_lazy_with(connect_options);

        Ok(Arc::new(Self {
            relation: relation.clone(),
            default_rate_limit_rpm,
            refresh_interval,
            max_stale,
            refresh_timeout,
            pool,
            snapshot: Arc::new(arc_swap::ArcSwap::from_pointee(TenantSnapshotState::empty())),
        }))
    }

    fn refresh_from_snapshot(&self, snapshot: TenantSnapshot) {
        self.snapshot.store(Arc::new(TenantSnapshotState::fresh(
            snapshot,
            Instant::now(),
        )));
    }

    fn clear_if_stale(&self, now: Instant) {
        let current = self.snapshot.load();
        let Some(refreshed_at) = current.refreshed_at else {
            return;
        };

        if now.duration_since(refreshed_at) < self.max_stale {
            return;
        }

        if !current.snapshot.is_empty() {
            self.snapshot.store(Arc::new(TenantSnapshotState::empty()));
        }
    }
}

#[cfg(all(feature = "mcp-http", feature = "tenant-registry-postgres"))]
fn tenant_db_ssl_mode(db_url: &str) -> sqlx::postgres::PgSslMode {
    match parse_tenant_db_ssl_mode(db_url) {
        Some(mode @ sqlx::postgres::PgSslMode::VerifyCa)
        | Some(mode @ sqlx::postgres::PgSslMode::VerifyFull) => mode,
        Some(_) => sqlx::postgres::PgSslMode::Require,
        None => sqlx::postgres::PgSslMode::Require,
    }
}

#[cfg(all(feature = "mcp-http", feature = "tenant-registry-postgres"))]
fn parse_tenant_db_ssl_mode(db_url: &str) -> Option<sqlx::postgres::PgSslMode> {
    let query = db_url.split_once('?')?.1;
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find_map(|(key, value)| {
            if !key.eq_ignore_ascii_case("sslmode") {
                return None;
            }
            value.parse().ok()
        })
}

#[cfg(all(feature = "mcp-http", feature = "tenant-registry-postgres"))]
async fn build_postgres_tenant_registry() -> Result<Arc<TenantAuthState>> {
    let relation = parse_tenant_db_relation()?;
    let refresh_interval_secs = parse_tenant_env_u64(
        NEXUS_MCP_TENANT_REFRESH_SECS_ENV,
        NEXUS_MCP_TENANT_REFRESH_SECS_DEFAULT,
    )?;
    if refresh_interval_secs == 0 {
        anyhow::bail!("{NEXUS_MCP_TENANT_REFRESH_SECS_ENV} must be greater than 0");
    }
    let max_stale_secs = parse_tenant_env_u64(
        NEXUS_MCP_TENANT_MAX_STALE_SECS_ENV,
        NEXUS_MCP_TENANT_MAX_STALE_SECS_DEFAULT,
    )?;
    if max_stale_secs == 0 || max_stale_secs < refresh_interval_secs {
        anyhow::bail!(
            "{NEXUS_MCP_TENANT_MAX_STALE_SECS_ENV} must be >= {NEXUS_MCP_TENANT_REFRESH_SECS_ENV} and greater than 0"
        );
    }
    let refresh_interval = Duration::from_secs(refresh_interval_secs);
    let max_stale = Duration::from_secs(max_stale_secs);
    let refresh_timeout_secs = parse_tenant_env_u64(
        NEXUS_MCP_TENANT_DB_TIMEOUT_SECS_ENV,
        NEXUS_MCP_TENANT_DB_TIMEOUT_SECS_DEFAULT,
    )?;
    if refresh_timeout_secs == 0 {
        anyhow::bail!("{NEXUS_MCP_TENANT_DB_TIMEOUT_SECS_ENV} must be greater than 0");
    }
    let refresh_timeout = Duration::from_secs(refresh_timeout_secs);
    let db_url = parse_tenant_db_url()?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{NEXUS_MCP_TENANT_DB_URL_ENV} is required when {NEXUS_MCP_TENANT_SOURCE_ENV} is set to '{NEXUS_MCP_TENANT_SOURCE_POSTGRES}'"
            )
        })?;
    let registry = PostgresTenantRegistry::new(
        db_url,
        relation,
        NEXUS_MCP_HTTP_DEFAULT_TENANT_RATE_LIMIT_RPM,
        refresh_interval,
        max_stale,
        refresh_timeout,
    )?;

    if let Err(error) = refresh_postgres_tenants(&registry).await {
        tracing::error!("failed initial tenant registry refresh from PostgreSQL: {error}");
        registry.clear_if_stale(Instant::now());
    }

    tokio::spawn(refresh_postgres_tenants_loop(registry.clone()));
    Ok(Arc::new(TenantAuthState::new(registry)))
}

#[cfg(all(feature = "mcp-http", feature = "tenant-registry-postgres"))]
fn quote_postgres_relation_identifier(relation: &str) -> String {
    let mut output = String::new();
    for (index, segment) in relation.split('.').enumerate() {
        if index > 0 {
            output.push('.');
        }
        output.push('"');
        output.push_str(&segment.replace('"', "\"\""));
        output.push('"');
    }
    output
}

#[cfg(all(feature = "mcp-http", feature = "tenant-registry-postgres"))]
fn format_tenant_query(relation: &str, use_status_filter: bool) -> String {
    if use_status_filter {
        format!(
            "SELECT key_sha256, workspace_id::text AS workspace_id, rate_limit_rpm::bigint AS rate_limit_rpm FROM {relation} WHERE status = 'active'"
        )
    } else {
        format!(
            "SELECT key_sha256, workspace_id::text AS workspace_id, rate_limit_rpm::bigint AS rate_limit_rpm FROM {relation}"
        )
    }
}

#[cfg(all(feature = "mcp-http", feature = "tenant-registry-postgres"))]
async fn postgres_relation_exists(pool: &sqlx::PgPool, relation: &str) -> Result<bool> {
    let exists = sqlx::query_scalar::<_, bool>("SELECT to_regclass($1::text) IS NOT NULL")
        .bind(relation)
        .fetch_one(pool)
        .await?;
    Ok(exists)
}

#[cfg(all(feature = "mcp-http", feature = "tenant-registry-postgres"))]
async fn resolve_postgres_relation(pool: &sqlx::PgPool, relation: &str) -> Result<String> {
    if relation == NEXUS_MCP_TENANT_DB_RELATION_DEFAULT
        && postgres_relation_exists(pool, NEXUS_MCP_TENANT_ACTIVE_API_KEYS_VIEW).await?
    {
        return Ok(NEXUS_MCP_TENANT_ACTIVE_API_KEYS_VIEW.to_string());
    }

    Ok(relation.to_string())
}

#[cfg(all(feature = "mcp-http", feature = "tenant-registry-postgres"))]
fn relation_uses_status_filter(relation: &str) -> bool {
    relation != NEXUS_MCP_TENANT_ACTIVE_API_KEYS_VIEW
}

#[cfg(all(feature = "mcp-http", feature = "tenant-registry-postgres"))]
async fn load_postgres_tenant_snapshot(
    pool: &sqlx::PgPool,
    relation: &str,
    default_rate_limit_rpm: u64,
) -> Result<TenantSnapshot> {
    use sqlx::Row;

    let resolved_relation = resolve_postgres_relation(pool, relation).await?;
    let resolved_relation_quoted = quote_postgres_relation_identifier(&resolved_relation);
    let query = format_tenant_query(
        &resolved_relation_quoted,
        relation_uses_status_filter(&resolved_relation),
    );
    let rows = sqlx::query(&query).fetch_all(pool).await?;
    let mut snapshot = TenantSnapshot::with_capacity(rows.len());
    for row in rows {
        let raw_hash = row.try_get::<String, _>("key_sha256")?;
        let key_sha256 = raw_hash.trim().to_ascii_lowercase();
        parse_hex_sha256(&key_sha256)?;
        let tenant_id = row.try_get::<String, _>("workspace_id")?;
        let rate_limit_rpm = row.try_get::<Option<i64>, _>("rate_limit_rpm")?;
        let rate_limit_rpm =
            rate_limit_rpm.unwrap_or_else(|| i64::try_from(default_rate_limit_rpm).unwrap_or(0));
        let rate_limit_rpm = u64::try_from(rate_limit_rpm).map_err(|_| {
            anyhow::anyhow!(
                "rate_limit_rpm for tenant '{tenant_id}' must be a non-negative integer"
            )
        })?;
        if rate_limit_rpm == 0 {
            anyhow::bail!("rate_limit_rpm for tenant '{tenant_id}' must be greater than 0");
        }

        if snapshot
            .insert(
                key_sha256,
                TenantInfo {
                    tenant_id,
                    rate_limit_rpm,
                },
            )
            .is_some()
        {
            anyhow::bail!("duplicate tenant api_key_sha256 in tenant registry snapshot");
        }
    }
    Ok(snapshot)
}

#[cfg(all(feature = "mcp-http", feature = "tenant-registry-postgres"))]
async fn refresh_postgres_tenants_with_loader<F>(
    registry: &PostgresTenantRegistry,
    timeout: Duration,
    source: F,
) -> Result<()>
where
    F: std::future::Future<Output = Result<TenantSnapshot>>,
{
    let now = Instant::now();
    registry.clear_if_stale(now);

    let timeout = refresh_timeout_budget(registry, timeout, now);
    let result = tokio::time::timeout(timeout, source).await;
    let snapshot = match result {
        Ok(result) => match result {
            Ok(snapshot) => snapshot,
            Err(error) => {
                registry.clear_if_stale(Instant::now());
                return Err(error);
            }
        },
        Err(_) => {
            registry.clear_if_stale(Instant::now());
            return Err(anyhow::anyhow!(
                "tenant registry refresh from PostgreSQL timed out"
            ));
        }
    };
    registry.refresh_from_snapshot(snapshot);
    Ok(())
}

#[cfg(all(feature = "mcp-http", feature = "tenant-registry-postgres"))]
fn refresh_timeout_budget(
    registry: &PostgresTenantRegistry,
    configured_timeout: Duration,
    now: Instant,
) -> Duration {
    let current = registry.snapshot.load_full();
    let Some(refreshed_at) = current.refreshed_at else {
        return configured_timeout;
    };

    let age = now.duration_since(refreshed_at);
    if age >= registry.max_stale {
        Duration::ZERO
    } else {
        configured_timeout.min(registry.max_stale - age)
    }
}

#[cfg(all(feature = "mcp-http", feature = "tenant-registry-postgres"))]
async fn refresh_postgres_tenants(registry: &PostgresTenantRegistry) -> Result<()> {
    let pool = registry.pool.clone();
    let relation = registry.relation.clone();
    let default_rate_limit_rpm = registry.default_rate_limit_rpm;
    let timeout = registry.refresh_timeout;
    refresh_postgres_tenants_with_loader(registry, timeout, async move {
        load_postgres_tenant_snapshot(&pool, &relation, default_rate_limit_rpm).await
    })
    .await
}

#[cfg(all(feature = "mcp-http", feature = "tenant-registry-postgres"))]
async fn refresh_postgres_tenants_loop(registry: Arc<PostgresTenantRegistry>) {
    let mut ticker = tokio::time::interval_at(
        tokio::time::Instant::now() + registry.refresh_interval,
        registry.refresh_interval,
    );
    loop {
        ticker.tick().await;
        let result = tokio::spawn({
            let registry = registry.clone();
            async move { refresh_postgres_tenants(&registry).await }
        })
        .await;

        match result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::error!("failed to refresh tenant registry from PostgreSQL: {error}");
                registry.clear_if_stale(Instant::now());
            }
            Err(error) => {
                tracing::error!("tenant registry refresh task panicked: {error}");
                registry.clear_if_stale(Instant::now());
            }
        }
    }
}

#[cfg(feature = "mcp-http")]
fn sha256_hex_bytes(raw: &str) -> String {
    sha256_hex(&sha256_bytes(raw))
}

#[cfg(feature = "mcp-http")]
async fn load_tenant_auth_state() -> Result<Option<Arc<TenantAuthState>>> {
    match parse_tenant_source()? {
        TenantSource::File => parse_tenant_auth_state_from_file(),
        TenantSource::Postgres => {
            #[cfg(feature = "tenant-registry-postgres")]
            {
                Ok(Some(build_postgres_tenant_registry().await?))
            }
            #[cfg(not(feature = "tenant-registry-postgres"))]
            {
                anyhow::bail!("NEXUS_MCP_TENANT_SOURCE=postgres requires the tenant-registry-postgres feature")
            }
        }
    }
}

#[cfg(feature = "mcp-http")]
fn sha256_bytes(key: &str) -> [u8; 32] {
    let digest = sha2::Sha256::digest(key.as_bytes());
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&digest);
    bytes
}

#[cfg(feature = "mcp-http")]
#[cfg(test)]
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        let left = a.get(i).copied().unwrap_or(0);
        let right = b.get(i).copied().unwrap_or(0);
        diff |= usize::from(left ^ right);
    }
    diff == 0
}

#[cfg(feature = "mcp-http")]
fn tenant_for_token<'a>(snapshot: &'a TenantSnapshot, token_hash: &str) -> Option<&'a TenantInfo> {
    snapshot.get(token_hash)
}

#[cfg(feature = "mcp-http")]
fn rate_limit_blocked(
    state: &TenantAuthState,
    tenant_id: &str,
    limit_rpm: u64,
    now: Instant,
) -> bool {
    let mut rate_limits = state
        .rate_limits
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let window = rate_limits
        .entry(tenant_id.to_string())
        .or_insert(TenantRateWindow {
            window_start: now,
            request_count: 0,
        });

    if now.duration_since(window.window_start) >= Duration::from_secs(60) {
        window.window_start = now;
        window.request_count = 0;
    }

    if window.request_count >= limit_rpm {
        return true;
    }
    window.request_count += 1;
    false
}

#[cfg(feature = "mcp-http")]
fn is_loopback_addr(addr: &SocketAddr) -> bool {
    addr.ip().is_loopback()
}

/// Maximum token validity an MCP client may request, in seconds (1 hour).
/// Larger caller-supplied values are clamped to this. See SECURITY.md.
const MAX_TOKEN_VALIDITY_SECS: u64 = 3600;

fn restored_state_summary(
    result: &nexus::snapshot::RollbackResult,
    include_memory_preview: bool,
) -> RestoredStateSummary {
    let preview_len = if include_memory_preview {
        result.memory.len().min(RESTORED_MEMORY_PREVIEW_BYTES)
    } else {
        0
    };
    let sha256 = format!("{:x}", sha2::Sha256::digest(&result.memory));
    let preview_base64 = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        &result.memory[..preview_len],
    );

    RestoredStateSummary {
        memory: RestoredMemorySummary {
            byte_len: result.memory.len(),
            sha256,
            preview_len,
            preview_base64,
        },
        execution_state: RestoredExecutionStateSummary {
            captured_globals: result.execution_state.captured_globals.len(),
            captured_tables: result.execution_state.captured_tables.len(),
        },
    }
}

fn memory_preview_capability() -> Capability {
    Capability::MemoryPreview
}

fn caller_has_memory_preview(tokens: &[CapabilityToken]) -> bool {
    let required = memory_preview_capability();
    tokens.iter().any(|token| token.allows(&required))
}

fn proof_reference(
    capsule: &nexus::proof::ProofCapsule,
) -> Result<nexus::proof::McpProofReference> {
    Ok(nexus::proof::McpProofReference {
        capsule_digest: nexus::proof::capsule_digest(capsule)
            .map_err(|e| anyhow::anyhow!("unable to compute proof capsule digest: {e}"))?,
        artifact_id: None,
        inline_summary: nexus::proof::ProofScorecard::from_capsule(capsule),
    })
}

fn should_include_full_proof_capsule() -> bool {
    if let Ok(value) = std::env::var(NEXUS_MCP_RETURN_FULL_PROOF_ENV) {
        return matches!(
            value.to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on" | "debug" | "enabled"
        );
    }

    !cfg!(test) && cfg!(debug_assertions)
}

#[cfg(feature = "aeon-memory")]
fn proof_events(
    output: &ToolOutput,
    capsule_id: Uuid,
    negotiation_rounds: Option<u32>,
) -> Vec<nexus::daemon::NexusExecutionEvent> {
    let mut events = Vec::new();

    if negotiation_rounds.is_some() {
        events.push(nexus::daemon::NexusExecutionEvent::CapabilityDenied {
            denied_category: "capability_denial_negotiated".to_string(),
        });
    }
    if let Some(snapshot_id) = output.snapshot_id {
        events.push(nexus::daemon::NexusExecutionEvent::SnapshotCreated { snapshot_id });
    }
    events.push(nexus::daemon::NexusExecutionEvent::ProofCapsuleEmitted { capsule_id });

    events
}

#[cfg(feature = "aeon-memory")]
fn verify_proof_capsule_memory_digest(
    proof_capsule: &nexus::proof::ProofCapsule,
    expected_digest: &str,
) -> Result<()> {
    let Some(actual_digest) = proof_capsule
        .memory_evidence
        .as_ref()
        .map(|evidence| evidence.digest.value.as_str())
    else {
        anyhow::bail!(
            "proof capsule missing AEON memory evidence digest; expected {expected_digest}"
        );
    };

    if !actual_digest.eq_ignore_ascii_case(expected_digest) {
        anyhow::bail!(
            "proof capsule AEON memory evidence digest mismatch: expected {expected_digest}, got {actual_digest}"
        );
    }

    Ok(())
}

#[cfg(feature = "aeon-memory")]
async fn deliver_nexus_iq_timeline(
    config: Option<&nexus::aeon::AeonConfig>,
    agent_id: String,
    session_id: Option<String>,
    mode: nexus::aeon::TimelineDeliveryMode,
    events: Vec<nexus::daemon::NexusExecutionEvent>,
) -> nexus::aeon::TimelineDeliveryStatus {
    let Some(sink) = config.and_then(|config| {
        match nexus::aeon::AeonTimelineSink::from_enabled_config(config) {
            Ok(Some(sink)) => Some(sink.with_mode(mode)),
            Ok(None) | Err(_) => None,
        }
    }) else {
        return match mode {
            nexus::aeon::TimelineDeliveryMode::Attested => {
                nexus::aeon::TimelineDeliveryStatus::RequiredButFailed
            }
            nexus::aeon::TimelineDeliveryMode::Advisory
            | nexus::aeon::TimelineDeliveryMode::Offline => {
                nexus::aeon::TimelineDeliveryStatus::FailedOpen
            }
        };
    };

    if matches!(mode, nexus::aeon::TimelineDeliveryMode::Attested) {
        return sink
            .deliver(&agent_id, session_id.as_deref(), &events)
            .await;
    }

    tokio::spawn(async move {
        let _ = sink
            .deliver(&agent_id, session_id.as_deref(), &events)
            .await;
    });
    nexus::aeon::TimelineDeliveryStatus::FireAndForget
}

#[cfg(feature = "aeon-memory")]
fn timeline_mode_label(mode: nexus::aeon::TimelineDeliveryMode) -> &'static str {
    match mode {
        nexus::aeon::TimelineDeliveryMode::Advisory => "advisory",
        nexus::aeon::TimelineDeliveryMode::Attested => "attested",
        nexus::aeon::TimelineDeliveryMode::Offline => "offline",
    }
}

#[cfg(feature = "aeon-memory")]
fn parse_iq_capability(spec: &str) -> Result<Capability> {
    if let Ok(value) = serde_json::from_str::<CapabilitySpec>(spec) {
        return parse_capability(&value);
    }
    if spec == "memory_preview" || spec == NEXUS_MEMORY_PREVIEW_CAPABILITY {
        return Ok(Capability::MemoryPreview);
    }

    let (kind, value) = spec
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("invalid capability '{spec}'; expected kind:value"))?;
    let value = Some(value);
    match kind {
        "read" | "read_file" => parse_capability_from_str("read_file", value),
        "write" | "write_file" => parse_capability_from_str("write_file", value),
        "list" | "list_dir" => parse_capability_from_str("list_dir", value),
        "http_get" => {
            if let Some(url) = value {
                nexus::security::validate_http_capability_pattern(url)
                    .map_err(|e| anyhow::anyhow!("{}", e))?;
                Some(Capability::HttpGet(url.to_string()))
            } else {
                None
            }
        }
        "http_post" => {
            if let Some(url) = value {
                nexus::security::validate_http_capability_pattern(url)
                    .map_err(|e| anyhow::anyhow!("{}", e))?;
                Some(Capability::HttpPost(url.to_string()))
            } else {
                None
            }
        }
        "read_memory" => parse_capability_from_str("read_memory", value),
        "write_memory" => parse_capability_from_str("write_memory", value),
        "exec" | "execute" => parse_capability_from_str("execute", value),
        "tmpfs" | "mount_tmpfs" => parse_capability_from_str("mount_tmpfs", value),
        "nexus" => match value {
            Some("memory_recall") => Some(Capability::MemoryRecall),
            Some(rest) if rest.starts_with("read_memory:") => {
                parse_capability_from_str("read_memory", rest.strip_prefix("read_memory:"))
            }
            Some(rest) if rest.starts_with("write_memory:") => {
                parse_capability_from_str("write_memory", rest.strip_prefix("write_memory:"))
            }
            _ => None,
        },
        _ => None,
    }
    .ok_or_else(|| anyhow::anyhow!("unknown capability type: {kind}"))
}

#[cfg(feature = "aeon-memory")]
fn is_capability_denial(error: &NexusError) -> bool {
    matches!(error, NexusError::CapabilityDenied(_))
}

fn failure_mode_from_category(category: &str) -> anyhow::Result<FailureMode> {
    match category {
        "TIMEOUT" => Ok(FailureMode::Timeout {
            limit_ms: 0,
            observed_ms: 0,
        }),
        "FUEL_EXHAUSTED" => Ok(FailureMode::FuelExhausted { limit: 0 }),
        "TRAP_UNREACHABLE" => Ok(FailureMode::TrapUnreachable),
        "TRAP_DIV_BY_ZERO" => Ok(FailureMode::TrapDivByZero),
        "TRAP_INTEGER_OVERFLOW" => Ok(FailureMode::TrapIntegerOverflow),
        "TRAP_BAD_FLOAT_TO_INT" => Ok(FailureMode::TrapBadConversionToInteger),
        "TRAP_STACK_OVERFLOW" => Ok(FailureMode::TrapStackOverflow),
        "TRAP_MEMORY_OOB" => Ok(FailureMode::TrapMemoryOutOfBounds),
        "TRAP_HEAP_MISALIGNED" => Ok(FailureMode::TrapHeapMisaligned),
        "TRAP_TABLE_OOB" => Ok(FailureMode::TrapTableOutOfBounds),
        "TRAP_INDIRECT_NULL" => Ok(FailureMode::TrapIndirectCallToNull),
        "TRAP_BAD_SIGNATURE" => Ok(FailureMode::TrapBadSignature),
        "TRAP_NULL_REFERENCE" => Ok(FailureMode::TrapNullReference),
        "TRAP_CAST_FAILURE" => Ok(FailureMode::TrapCastFailure),
        "TRAP_OTHER" => Ok(FailureMode::TrapOther("TRAP_OTHER".to_string())),
        "MEMORY_LIMIT_EXCEEDED" => Ok(FailureMode::MemoryLimitExceeded {
            pages: 0,
            limit_pages: 0,
        }),
        "INVALID_MODULE" => Ok(FailureMode::InvalidModule(String::new())),
        "MISSING_ENTRYPOINT" => Ok(FailureMode::MissingEntrypoint {
            expected: String::new(),
        }),
        "HOST_ERROR" => Ok(FailureMode::HostError(String::new())),
        _ => Err(anyhow::anyhow!(
            "unrecognised failure category '{category}'"
        )),
    }
}

fn capability_allowed_by(allowlist: &[Capability], capability: &Capability) -> bool {
    allowlist
        .iter()
        .any(|allowed| capability.is_subset_of(allowed))
}

fn check_tokens_against_profile(
    tokens: &[CapabilityToken],
    manifest: &CapabilityProfileManifest,
) -> nexus::Result<()> {
    check_tokens_fresh(tokens)?;

    for token in tokens {
        let permitted = manifest
            .allowed_capabilities()
            .iter()
            .any(|allowed| allowed.allows(&token.capability));
        if !permitted {
            return Err(NexusError::CapabilityDenied(format!(
                "capability not permitted by active profile: {}",
                token.capability.description()
            )));
        }
    }

    Ok(())
}

fn check_tokens_fresh(tokens: &[CapabilityToken]) -> nexus::Result<()> {
    for token in tokens {
        if !token.is_valid() {
            return Err(NexusError::InvalidCapability(
                DenialReason::TokenExpired.safe_message().to_string(),
            ));
        }
    }

    Ok(())
}

/// Build a profile-denial error using the canonical prefix that
/// [`mcp_error_code`] maps to the MCP `-32602` invalid-params code.
fn profile_denial(detail: String) -> anyhow::Error {
    NexusError::CapabilityDenied(format!(
        "capability not permitted by active profile: {detail}"
    ))
    .into()
}

fn profile_manifest_from_env() -> Result<Option<CapabilityProfileManifest>> {
    let Some(raw) = std::env::var_os(NEXUS_MCP_PROFILE_ENV) else {
        return Ok(None);
    };
    let path = PathBuf::from(raw);

    match load_and_validate(&path) {
        Ok(manifest) => {
            tracing::info!(
                profile = %manifest.name,
                path = %path.display(),
                "Loaded Nexus MCP capability profile"
            );
            Ok(Some(manifest))
        }
        Err(errors) => {
            for error in &errors {
                tracing::error!(
                    path = %path.display(),
                    error = %error,
                    "Invalid Nexus MCP capability profile"
                );
            }
            let joined = errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ");
            anyhow::bail!(
                "invalid {NEXUS_MCP_PROFILE_ENV} '{}': {joined}",
                path.display()
            );
        }
    }
}

fn capability_allowlist_from_env() -> Result<Option<Vec<Capability>>> {
    let Some(raw) = std::env::var_os(NEXUS_MCP_CAPABILITY_ALLOWLIST_ENV) else {
        return Ok(None);
    };
    let raw = raw.into_string().map_err(|_| {
        anyhow::anyhow!(
            "{NEXUS_MCP_CAPABILITY_ALLOWLIST_ENV} must be a UTF-8 JSON array of capability objects"
        )
    })?;

    // Format: JSON array using the same object shape as execute_wasi
    // capabilities, for example:
    // [{"type":"read_file","path":"/srv/nexus/modules"}]
    // Capability type "all" is rejected even when configured by the operator.
    let specs: Vec<CapabilitySpec> = serde_json::from_str(&raw).map_err(|e| {
        anyhow::anyhow!(
            "{NEXUS_MCP_CAPABILITY_ALLOWLIST_ENV} must be a JSON array of capability objects: {e}"
        )
    })?;

    specs
        .into_iter()
        .map(|spec| {
            let capability = parse_capability(&spec)?;
            let (capability, _) = sanitize_token_request(capability, None).map_err(|e| {
                anyhow::anyhow!(
                    "invalid {NEXUS_MCP_CAPABILITY_ALLOWLIST_ENV} entry {:?}: {e}",
                    spec
                )
            })?;
            Ok(capability)
        })
        .collect::<Result<Vec<_>>>()
        .map(Some)
}

fn nexus_iq_allowlist_from_env() -> Result<Option<Vec<String>>> {
    let Some(raw) = std::env::var_os(NEXUS_IQ_ALLOWLIST_ENV) else {
        return Ok(None);
    };
    let raw = raw.into_string().map_err(|_| {
        anyhow::anyhow!("{NEXUS_IQ_ALLOWLIST_ENV} must be UTF-8 JSON or comma-separated tool names")
    })?;
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(Some(Vec::new()));
    }

    let tools = if raw.starts_with('[') {
        serde_json::from_str::<Vec<String>>(raw).map_err(|e| {
            anyhow::anyhow!("{NEXUS_IQ_ALLOWLIST_ENV} must be a JSON array of strings: {e}")
        })?
    } else {
        raw.split(',')
            .map(str::trim)
            .filter(|tool| !tool.is_empty())
            .map(str::to_string)
            .collect()
    };

    Ok(Some(tools))
}

fn allowed_wasm_module_dirs() -> Result<Vec<PathBuf>> {
    let raw_dirs: Vec<PathBuf> = match std::env::var_os(NEXUS_MCP_MODULE_DIR_ENV) {
        Some(value) => std::env::split_paths(&value).collect(),
        None => vec![std::env::current_dir()?],
    };

    if raw_dirs.is_empty() {
        return Err(anyhow::anyhow!(
            "{NEXUS_MCP_MODULE_DIR_ENV} must contain at least one module directory"
        ));
    }

    raw_dirs
        .into_iter()
        .map(|dir| {
            let canonical = std::fs::canonicalize(&dir)
                .map_err(|e| anyhow::anyhow!("invalid MCP module dir '{}': {e}", dir.display()))?;
            if !canonical.is_dir() {
                anyhow::bail!(
                    "invalid MCP module dir '{}': resolved path is not a directory",
                    dir.display()
                );
            }
            Ok(canonical)
        })
        .collect()
}

/// Canonicalize profile-declared module directories (Slice 2 path).
fn canonicalize_module_dirs(dirs: &[PathBuf]) -> Result<Vec<PathBuf>> {
    if dirs.is_empty() {
        anyhow::bail!("profile execution.module_dirs must contain at least one directory");
    }
    dirs.iter()
        .map(|dir| {
            let canonical = std::fs::canonicalize(dir).map_err(|e| {
                anyhow::anyhow!("invalid profile module dir '{}': {e}", dir.display())
            })?;
            if !canonical.is_dir() {
                anyhow::bail!(
                    "invalid profile module dir '{}': not a directory",
                    dir.display()
                );
            }
            Ok(canonical)
        })
        .collect()
}

fn resolve_wasm_path(wasm_path: &Path, allowed_dirs: &[PathBuf]) -> Result<PathBuf> {
    let requested_path = absolute_request_path(wasm_path)?;
    if !path_is_lexically_allowed(&requested_path, allowed_dirs) {
        return Err(NexusError::FilesystemError(format!(
            "wasm path '{}' is outside allowed MCP module directories",
            wasm_path.display()
        ))
        .into());
    }

    let canonical = std::fs::canonicalize(wasm_path).map_err(|_| {
        NexusError::FilesystemError(format!(
            "wasm path '{}' not found or inaccessible",
            wasm_path.display()
        ))
    })?;

    if !canonical.is_file() {
        return Err(NexusError::FilesystemError(format!(
            "wasm path '{}' is not a file",
            wasm_path.display()
        ))
        .into());
    }

    if allowed_dirs.iter().any(|dir| canonical.starts_with(dir)) {
        return Ok(canonical);
    }

    Err(NexusError::FilesystemError(format!(
        "wasm path '{}' is outside allowed MCP module directories",
        wasm_path.display()
    ))
    .into())
}

fn absolute_request_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }

    Ok(std::env::current_dir()?.join(path))
}

fn path_is_lexically_allowed(path: &Path, allowed_dirs: &[PathBuf]) -> bool {
    let normalized_path = lexical_normalize_path(path);
    allowed_dirs.iter().any(|dir| {
        let normalized_dir = lexical_normalize_path(dir);
        normalized_path.starts_with(normalized_dir)
    })
}

fn lexical_normalize_path(path: &Path) -> PathBuf {
    // On Windows, `std::fs::canonicalize` returns verbatim (extended-length)
    // paths with a `\\?\` prefix, while paths built from `tempdir` or user
    // input do not carry that prefix.  Strip it so that `starts_with`
    // comparisons between canonicalized allowed-dirs and ordinary input paths
    // agree on the prefix form.
    #[cfg(windows)]
    let path: std::borrow::Cow<Path> = {
        use std::path::Component;
        if let Some(Component::Prefix(p)) = path.components().next() {
            use std::path::Prefix;
            if matches!(p.kind(), Prefix::VerbatimDisk(_)) {
                // \\?\C:\... -> C:\...
                let s = path.to_string_lossy();
                let stripped = s.strip_prefix(r"\\?\").unwrap_or(&s);
                std::borrow::Cow::Owned(PathBuf::from(stripped.to_string()))
            } else {
                std::borrow::Cow::Borrowed(path)
            }
        } else {
            std::borrow::Cow::Borrowed(path)
        }
    };
    let is_absolute = path.is_absolute();
    let mut normalized = PathBuf::new();

    for component in path.components() {
        match component {
            std::path::Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            std::path::Component::RootDir => normalized.push(component.as_os_str()),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !normalized.pop() && !is_absolute {
                    normalized.push("..");
                }
            }
            std::path::Component::Normal(part) => normalized.push(part),
        }
    }

    normalized
}

/// Apply security limits to an MCP token request: reject the unrestricted
/// `All` capability (MCP clients must request a specific capability), and clamp
/// the requested validity to `MAX_TOKEN_VALIDITY_SECS`. Returns the (possibly
/// adjusted) capability and the effective validity in seconds.
fn sanitize_token_request(
    capability: Capability,
    requested_secs: Option<u64>,
) -> Result<(Capability, u64)> {
    if matches!(capability, Capability::All) {
        return Err(anyhow::anyhow!(
            "capability 'all' cannot be issued to MCP clients; request a specific capability"
        ));
    }
    let secs = requested_secs
        .unwrap_or(MAX_TOKEN_VALIDITY_SECS)
        .min(MAX_TOKEN_VALIDITY_SECS);
    Ok((capability, secs))
}

fn parse_capability(spec: &CapabilitySpec) -> Result<Capability> {
    let path_required = matches!(
        spec.r#type.as_str(),
        "read_file"
            | "write_file"
            | "list_dir"
            | "execute"
            | "mount_tmpfs"
            | "read_memory"
            | "write_memory"
    );
    if path_required && spec.path.is_none() {
        anyhow::bail!("capability type '{}' requires a 'path' field", spec.r#type);
    }
    let url_required = matches!(spec.r#type.as_str(), "http_get" | "http_post");
    if url_required && spec.path.is_none() {
        anyhow::bail!(
            "capability type '{}' requires a 'path' (URL pattern) field",
            spec.r#type
        );
    }
    parse_capability_from_str(&spec.r#type, spec.path.as_deref())
        .ok_or_else(|| anyhow::anyhow!("Unknown capability type: {}", spec.r#type))
}

fn parse_capability_from_str(cap_type: &str, path: Option<&str>) -> Option<Capability> {
    match cap_type {
        "read_file" => Some(Capability::ReadFile(PathBuf::from(path?))),
        "write_file" => Some(Capability::WriteFile(PathBuf::from(path?))),
        "list_dir" => Some(Capability::ListDirectory(PathBuf::from(path?))),
        "http_get" => Some(Capability::HttpGet(path?.to_string())),
        "http_post" => Some(Capability::HttpPost(path?.to_string())),
        "execute" => Some(Capability::ExecuteBinary(PathBuf::from(path?))),
        "mount_tmpfs" => Some(Capability::MountTmpfs(PathBuf::from(path?))),
        "read_memory" => Some(Capability::ReadMemory(MemoryScope::parse(path?)?)),
        "write_memory" => Some(Capability::WriteMemory(MemoryScope::parse(path?)?)),
        "memory_preview" | NEXUS_MEMORY_PREVIEW_CAPABILITY => Some(Capability::MemoryPreview),
        "all" => Some(Capability::All),
        _ => None,
    }
}

#[cfg(feature = "aeon-memory")]
fn required_read_memory_scope(aeon_agent_id: &str, aeon_session_id: Option<&str>) -> Capability {
    match aeon_session_id {
        Some(session_id) => Capability::ReadMemory(MemoryScope::Session {
            agent_id: aeon_agent_id.to_string(),
            session_id: session_id.to_string(),
        }),
        None => Capability::ReadMemory(MemoryScope::Agent(aeon_agent_id.to_string())),
    }
}

fn apply_mcp_sandbox_env(config: &mut HypervisorConfig) -> Result<()> {
    if let Some(fuel) = env_u64(MCP_FUEL_ENV)? {
        config.sandbox_config.max_fuel = fuel;
    }
    if let Some(timeout_ms) = env_u64(MCP_TIMEOUT_MS_ENV)? {
        config.sandbox_config.time_limit = Duration::from_millis(timeout_ms);
    }
    Ok(())
}

async fn run_mcp_stdio_server(server: NexusMcpServer) -> Result<()> {
    let service = server.serve(stdio()).await.inspect_err(|e| {
        tracing::error!("MCP server error: {:?}", e);
    })?;

    service.waiting().await?;
    Ok(())
}

#[cfg(feature = "mcp-http")]
async fn require_bearer_token(
    State(auth): State<Option<Arc<TenantAuthState>>>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let Some(auth) = auth else {
        return next.run(request).await;
    };

    let method = request.method().to_string();
    let path = request.uri().path().to_string();
    let presented_key = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_bearer_token)
        .filter(|token| !token.is_empty());
    let Some(presented_key) = presented_key else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    let snapshot = auth.registry.current_snapshot();
    let presented_key_hash = sha256_hex_bytes(&presented_key);
    let Some(tenant) = tenant_for_token(&snapshot, &presented_key_hash) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if rate_limit_blocked(
        auth.as_ref(),
        &tenant.tenant_id,
        tenant.rate_limit_rpm,
        Instant::now(),
    ) {
        tracing::info!(
            tenant_id = %tenant.tenant_id,
            method = %method,
            path = %path,
            status_class = format!("{}xx", StatusCode::TOO_MANY_REQUESTS.as_u16() / 100),
            "authenticated MCP HTTP request"
        );
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }

    request.extensions_mut().insert(TenantContext {
        tenant_id: tenant.tenant_id.clone(),
    });

    let response = next.run(request).await;
    tracing::info!(
        tenant_id = %tenant.tenant_id,
        method = %method,
        path = %path,
        status_class = format!("{}xx", response.status().as_u16() / 100),
        "authenticated MCP HTTP request"
    );
    response
}

#[cfg(feature = "mcp-http")]
async fn run_mcp_http_server(server: NexusMcpServer) -> Result<()> {
    use rmcp::transport::streamable_http_server::{
        session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
    };

    let server = NexusMcpServer::new_with_forced_tool_allowlist(
        server.hypervisor.clone(),
        Some(read_only_http_tool_allowlist()),
    )?;
    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        std::sync::Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );

    let addr = parse_nexus_mcp_http_addr()?;
    let tenant_auth = load_tenant_auth_state().await?;
    if tenant_auth.is_none() && !is_loopback_addr(&addr) {
        anyhow::bail!("tenant auth source is required for non-loopback HTTP bind");
    }
    if tenant_auth.is_none() {
        tracing::warn!(
            addr = %addr,
            "NEXUS_MCP_TENANT_SOURCE is not configured with a usable source; unauthenticated HTTP MCP endpoint is loopback-only"
        );
    }

    let app = Router::new()
        .nest_service("/", service)
        .route_layer(middleware::from_fn_with_state(
            tenant_auth.clone(),
            require_bearer_token,
        ))
        .with_state(tenant_auth);

    tracing::info!(addr = %addr, "Starting Nexus MCP HTTP transport");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .await
        .map_err(|e| anyhow::anyhow!("failed to serve Nexus MCP HTTP transport: {e}"))?;
    Ok(())
}

fn env_u64(name: &str) -> Result<Option<u64>> {
    match std::env::var(name) {
        Ok(value) => value
            .parse::<u64>()
            .map(Some)
            .map_err(|error| anyhow::anyhow!("{name} must be an unsigned integer: {error}")),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            anyhow::bail!("{name} must be valid Unicode")
        }
    }
}

// ─── Main ────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive(tracing::Level::INFO.into()))
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    tracing::info!("Starting Nexus MCP server");

    #[cfg(feature = "aeon-memory")]
    let aeon_config = nexus::aeon::AeonConfig::from_env().ok();
    #[cfg(feature = "aeon-memory")]
    if matches!(aeon_config.as_ref(), Some(config) if config.enabled && config.hmac_key.is_none()) {
        eprintln!("[nexus] SECURITY WARNING: aeon-memory is active but NEXUS_AEON_HMAC_KEY is not set — memory_digest will use unauthenticated SHA-256 (forgeable). Set NEXUS_AEON_HMAC_KEY (>=32 bytes) in production.");
    }

    let mut config = HypervisorConfig {
        #[cfg(feature = "aeon-memory")]
        aeon_config,
        ..HypervisorConfig::default()
    };
    apply_mcp_sandbox_env(&mut config)?;
    let hypervisor = Arc::new(NexusHypervisor::new(config)?);

    #[cfg(feature = "mcp-http")]
    {
        let transport = parse_nexus_mcp_transport();
        let server = NexusMcpServer::new(hypervisor)?;
        match transport.as_str() {
            NEXUS_MCP_TRANSPORT_STDIO => {
                run_mcp_stdio_server(server).await?;
            }
            NEXUS_MCP_TRANSPORT_HTTP => {
                run_mcp_http_server(server).await?;
            }
            other => {
                anyhow::bail!(
                    "unsupported {NEXUS_MCP_TRANSPORT_ENV} value '{other}', expected '{NEXUS_MCP_TRANSPORT_STDIO}' or '{NEXUS_MCP_TRANSPORT_HTTP}'"
                );
            }
        }
    }

    #[cfg(not(feature = "mcp-http"))]
    {
        if matches!(
            std::env::var(NEXUS_MCP_TRANSPORT_ENV).ok().as_deref(),
            Some("http")
        ) {
            anyhow::bail!(
                "NEXUS_MCP_TRANSPORT=http requires compiling with feature mcp-http (use cargo build --features mcp-http)"
            );
        }
        if let Ok(raw) = std::env::var(NEXUS_MCP_TRANSPORT_ENV) {
            if raw != NEXUS_MCP_TRANSPORT_STDIO {
                anyhow::bail!(
                    "unsupported {NEXUS_MCP_TRANSPORT_ENV} value '{raw}', expected '{NEXUS_MCP_TRANSPORT_STDIO}'"
                );
            }
        }

        run_mcp_stdio_server(NexusMcpServer::new(hypervisor)?).await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic;

    #[cfg(feature = "mcp-http")]
    static MCP_HTTP_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    #[cfg(feature = "mcp-http")]
    const MCP_HTTP_ENV_VARS: [&str; 8] = [
        NEXUS_MCP_HTTP_TOKEN_ENV,
        NEXUS_MCP_HTTP_TENANTS_ENV,
        NEXUS_MCP_TENANT_SOURCE_ENV,
        NEXUS_MCP_TENANT_DB_URL_ENV,
        NEXUS_MCP_TENANT_DB_RELATION_ENV,
        NEXUS_MCP_TENANT_REFRESH_SECS_ENV,
        NEXUS_MCP_TENANT_MAX_STALE_SECS_ENV,
        NEXUS_MCP_TENANT_DB_TIMEOUT_SECS_ENV,
    ];

    #[cfg(feature = "mcp-http")]
    struct McpHttpEnvGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }

    #[cfg(feature = "mcp-http")]
    impl Drop for McpHttpEnvGuard {
        fn drop(&mut self) {
            for (name, value) in self.saved.drain(..).rev() {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }

    #[cfg(feature = "mcp-http")]
    fn acquire_mcp_http_env() -> McpHttpEnvGuard {
        let lock = MCP_HTTP_ENV_LOCK.lock().unwrap();
        let saved = MCP_HTTP_ENV_VARS
            .iter()
            .map(|name| (*name, std::env::var_os(name)))
            .collect::<Vec<_>>();

        for name in MCP_HTTP_ENV_VARS {
            std::env::remove_var(name);
        }

        McpHttpEnvGuard { _lock: lock, saved }
    }

    #[cfg(feature = "mcp-http")]
    fn with_clean_mcp_http_env(test: impl FnOnce() + std::panic::UnwindSafe) {
        let _guard = acquire_mcp_http_env();
        let result = panic::catch_unwind(test);

        if let Err(payload) = result {
            std::panic::resume_unwind(payload);
        }
    }

    #[test]
    fn get_history_returns_empty_for_fresh_hypervisor() {
        let hypervisor = Arc::new(NexusHypervisor::new(HypervisorConfig::default()).unwrap());
        let server = NexusMcpServer::new(hypervisor).unwrap();
        let response = server
            .do_get_history(GetHistoryParams { limit: None })
            .unwrap();

        assert!(response.records.is_empty());
    }

    #[test]
    fn get_stats_returns_zero_for_fresh_hypervisor() {
        let hypervisor = Arc::new(NexusHypervisor::new(HypervisorConfig::default()).unwrap());
        let server = NexusMcpServer::new(hypervisor).unwrap();
        let response = server.do_get_stats(GetStatsParams {}).unwrap();

        assert_eq!(response.telemetry.total_executions, 0);
    }

    #[test]
    fn instinct_stats_errors_when_store_not_initialised() {
        let hypervisor = Arc::new(NexusHypervisor::new(HypervisorConfig::default()).unwrap());
        let server = NexusMcpServer::new(hypervisor).unwrap();
        let error = server
            .do_instinct_stats(InstinctStatsParams {})
            .unwrap_err()
            .to_string();

        assert!(error.contains("not initialised"));
    }

    #[test]
    fn instinct_import_errors_when_store_not_initialised() {
        let hypervisor = Arc::new(NexusHypervisor::new(HypervisorConfig::default()).unwrap());
        let server = NexusMcpServer::new(hypervisor).unwrap();
        let error = server
            .do_instinct_import(InstinctImportParams {
                json: "[]".to_string(),
            })
            .unwrap_err()
            .to_string();

        assert!(error.contains("not initialised"));
    }

    #[test]
    fn instinct_query_errors_when_store_not_initialised() {
        let hypervisor = Arc::new(NexusHypervisor::new(HypervisorConfig::default()).unwrap());
        let server = NexusMcpServer::new(hypervisor).unwrap();
        let error = server
            .do_instinct_query(InstinctQueryParams {
                failure_category: "TIMEOUT".to_string(),
                operation: "*".to_string(),
            })
            .unwrap_err()
            .to_string();

        assert!(error.contains("not initialised"));
    }

    #[test]
    fn instinct_query_rejects_unknown_category() {
        assert!(failure_mode_from_category("NOT_A_REAL_CATEGORY").is_err());
    }

    #[test]
    fn rejects_all_capability_for_mcp_clients() {
        let r = sanitize_token_request(Capability::All, Some(60));
        assert!(
            r.is_err(),
            "MCP clients must not be able to mint Capability::All"
        );
    }

    #[test]
    fn clamps_excessive_validity() {
        let (_, secs) =
            sanitize_token_request(Capability::ReadFile(PathBuf::from("/data")), Some(u64::MAX))
                .unwrap();
        assert_eq!(secs, MAX_TOKEN_VALIDITY_SECS);
    }

    #[test]
    fn preserves_reasonable_validity() {
        let (_, secs) =
            sanitize_token_request(Capability::ReadFile(PathBuf::from("/data")), Some(120))
                .unwrap();
        assert_eq!(secs, 120);
    }

    #[test]
    fn defaults_to_max_when_unset() {
        let (_, secs) =
            sanitize_token_request(Capability::ReadFile(PathBuf::from("/data")), None).unwrap();
        assert_eq!(secs, MAX_TOKEN_VALIDITY_SECS);
    }

    #[test]
    fn token_recheck_fires_before_execution() {
        let tmp = tempfile::tempdir().unwrap();
        let profile_path = tmp.path().join("token-recheck-profile.toml");
        std::fs::write(
            &profile_path,
            "name = 'token-recheck'\n\n[[capabilities]]\ntype = 'read_file'\npath = '/data'\n",
        )
        .unwrap();
        let profile = load_and_validate(&profile_path).expect("valid profile");
        let token = CapabilityToken::new(
            Capability::ReadFile(PathBuf::from("/data")),
            "test",
            Duration::from_secs(0),
            &ed25519_dalek::SigningKey::generate(&mut rand::rngs::OsRng),
        )
        .unwrap();

        let error = check_tokens_against_profile(&[token], &profile).unwrap_err();

        match error {
            NexusError::InvalidCapability(message) => {
                assert!(message.contains("expired"), "unexpected error: {message}");
            }
            other => panic!("expected InvalidCapability, got: {other:?}"),
        }
    }

    #[test]
    fn expired_token_error_omits_identifier_and_expiry() {
        let token = CapabilityToken::new(
            Capability::ReadFile(PathBuf::from("/data")),
            "test",
            Duration::from_secs(0),
            &ed25519_dalek::SigningKey::generate(&mut rand::rngs::OsRng),
        )
        .unwrap();

        let error = check_tokens_fresh(&[token]).unwrap_err();
        let message = match error {
            NexusError::InvalidCapability(message) => message,
            other => panic!("expected InvalidCapability, got: {other:?}"),
        };

        assert_eq!(message, DenialReason::TokenExpired.safe_message());
        assert!(
            !message.contains('-'),
            "expired token denial must not include token UUID: {message}"
        );
        assert!(
            !message.contains("expired at"),
            "expired token denial must not include expiry timestamp: {message}"
        );
    }

    #[test]
    fn preview_base64_requires_capability() {
        let result = nexus::snapshot::RollbackResult {
            snapshot_id: Uuid::new_v4(),
            memory: b"secret-memory-preview".to_vec(),
            execution_state: ExecutionState::default(),
            fs_operations: Vec::new(),
            timestamp: chrono::Utc::now(),
        };

        let without_preview = restored_state_summary(&result, caller_has_memory_preview(&[]));
        assert_eq!(without_preview.memory.preview_len, 0);
        assert!(without_preview.memory.preview_base64.is_empty());

        let token = CapabilityToken::new(
            Capability::MemoryPreview,
            "test",
            Duration::from_secs(60),
            &ed25519_dalek::SigningKey::generate(&mut rand::rngs::OsRng),
        )
        .unwrap();
        let with_preview = restored_state_summary(&result, caller_has_memory_preview(&[token]));

        assert_eq!(
            with_preview.memory.preview_len,
            result.memory.len().min(RESTORED_MEMORY_PREVIEW_BYTES)
        );
        assert!(!with_preview.memory.preview_base64.is_empty());
    }

    #[test]
    fn attenuate_token_rejects_invalid_uuid() {
        let hypervisor = Arc::new(NexusHypervisor::new(HypervisorConfig::default()).unwrap());
        let server = NexusMcpServer::new(hypervisor).unwrap();
        let error = match server.do_attenuate_token(AttenuateTokenParams {
            parent_token_id: "not-a-uuid".to_string(),
            capability: "read_file".to_string(),
            path: Some("/tmp".to_string()),
            validity_secs: None,
        }) {
            Ok(_) => panic!("expected invalid parent_token_id UUID error"),
            Err(err) => err.to_string(),
        };

        assert!(
            error.contains("invalid parent_token_id UUID"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn rejects_wasm_path_outside_allowed_module_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let allowed = tmp.path().join("allowed");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&allowed).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let wasm_path = outside.join("tool.wasm");
        std::fs::write(&wasm_path, b"\0asm").unwrap();

        let allowed_dirs = vec![std::fs::canonicalize(&allowed).unwrap()];
        let err = resolve_wasm_path(&wasm_path, &allowed_dirs).unwrap_err();

        assert!(
            err.to_string().contains("outside allowed MCP module"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_missing_wasm_path_outside_allowed_module_dir_without_stat_leak() {
        let tmp = tempfile::tempdir().unwrap();
        let allowed = tmp.path().join("allowed");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&allowed).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let wasm_path = outside.join("missing.wasm");

        let allowed_dirs = vec![std::fs::canonicalize(&allowed).unwrap()];
        let err = resolve_wasm_path(&wasm_path, &allowed_dirs).unwrap_err();
        let message = err.to_string();

        assert!(
            message.contains("outside allowed MCP module"),
            "unexpected error: {err}"
        );
        assert!(
            !message.contains("No such file"),
            "outside-root misses must not reveal host path existence: {err}"
        );
    }

    #[test]
    fn rejects_missing_wasm_path_inside_allowed_module_dir_without_os_error() {
        let tmp = tempfile::tempdir().unwrap();
        let allowed = tmp.path().join("allowed");
        std::fs::create_dir_all(&allowed).unwrap();
        let wasm_path = allowed.join("missing.wasm");

        let allowed_dirs = vec![std::fs::canonicalize(&allowed).unwrap()];
        let err = resolve_wasm_path(&wasm_path, &allowed_dirs).unwrap_err();
        let message = err.to_string();

        assert!(
            message.contains("not found or inaccessible"),
            "unexpected error: {err}"
        );
        assert!(
            !message.contains("Failed to canonicalize"),
            "canonicalization failure detail must not be exposed: {err}"
        );
        assert!(
            !message.contains("No such file") && !message.contains("os error"),
            "OS error detail must not be exposed: {err}"
        );
    }

    #[test]
    fn rejects_wasm_directory_without_canonical_path_leak() {
        let tmp = tempfile::Builder::new()
            .prefix("nexus-mcp-path-leak")
            .tempdir_in(".")
            .unwrap();
        let allowed = tmp.path().join("allowed");
        std::fs::create_dir_all(&allowed).unwrap();
        let wasm_path = allowed.join("module-dir.wasm");
        std::fs::create_dir_all(&wasm_path).unwrap();
        let relative_wasm_path = wasm_path
            .strip_prefix(std::env::current_dir().unwrap())
            .unwrap()
            .to_path_buf();

        let allowed_dirs = vec![std::fs::canonicalize(&allowed).unwrap()];
        let canonical = std::fs::canonicalize(&wasm_path).unwrap();
        let err = resolve_wasm_path(&relative_wasm_path, &allowed_dirs).unwrap_err();
        let message = err.to_string();

        assert!(message.contains("is not a file"), "unexpected error: {err}");
        assert!(
            !message.contains("non-file"),
            "directory denial must not use the canonical-path leak wording: {err}"
        );
        assert!(
            !message.contains(&canonical.to_string_lossy().to_string()),
            "directory denial must not expose canonical host path: {err}"
        );
        assert!(
            !message_contains_quoted_absolute_path(&message),
            "directory denial must not expose absolute host paths: {err}"
        );
    }

    #[cfg(unix)]
    fn message_contains_quoted_absolute_path(message: &str) -> bool {
        message.contains("'/") || message.contains("\"/")
    }

    #[cfg(windows)]
    fn message_contains_quoted_absolute_path(message: &str) -> bool {
        let contains_drive_path = message.as_bytes().windows(3).any(|window| {
            matches!(window[0], b'\'' | b'"')
                && window[1].is_ascii_alphabetic()
                && window[2] == b':'
        });
        contains_drive_path || message.contains(r#"'\\"#) || message.contains(r#""\\"#)
    }

    #[cfg(unix)]
    #[test]
    fn rejects_wasm_path_symlink_to_directory_without_canonical_path_leak() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let allowed = tmp.path().join("allowed");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&allowed).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let linked_wasm = allowed.join("linked-dir.wasm");
        symlink(&outside, &linked_wasm).unwrap();

        let allowed_dirs = vec![std::fs::canonicalize(&allowed).unwrap()];
        let canonical = std::fs::canonicalize(&linked_wasm).unwrap();
        let err = resolve_wasm_path(&linked_wasm, &allowed_dirs).unwrap_err();
        let message = err.to_string();

        assert!(message.contains("is not a file"), "unexpected error: {err}");
        assert!(
            !message.contains("non-file"),
            "directory symlink denial must not use the canonical-path leak wording: {err}"
        );
        assert!(
            !message.contains(&canonical.to_string_lossy().to_string()),
            "directory symlink denial must not expose canonical host path: {err}"
        );
    }

    #[test]
    fn accepts_wasm_path_inside_allowed_module_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let allowed = tmp.path().join("allowed");
        std::fs::create_dir_all(&allowed).unwrap();
        let wasm_path = allowed.join("tool.wasm");
        std::fs::write(&wasm_path, b"\0asm").unwrap();

        let allowed_dirs = vec![std::fs::canonicalize(&allowed).unwrap()];
        let resolved = resolve_wasm_path(&wasm_path, &allowed_dirs).unwrap();

        assert_eq!(resolved, std::fs::canonicalize(wasm_path).unwrap());
    }

    #[cfg(feature = "mcp-http")]
    #[test]
    fn ensure_tool_allowed_enforces_read_only_forced_allowlist() {
        let hypervisor = Arc::new(NexusHypervisor::new(HypervisorConfig::default()).unwrap());
        let server = NexusMcpServer::new_with_forced_tool_allowlist(
            hypervisor,
            Some(read_only_http_tool_allowlist()),
        )
        .unwrap();

        let error = server
            .ensure_tool_allowed("nexus_execute_wasi")
            .unwrap_err()
            .to_string();
        assert!(error.contains("not allowed in HTTP read-only mode"));

        assert!(server.ensure_tool_allowed("nexus_get_stats").is_ok());
    }

    #[cfg(feature = "mcp-http")]
    fn build_tenant_auth_state_from_snapshot(snapshot: TenantSnapshot) -> Arc<TenantAuthState> {
        Arc::new(TenantAuthState::new(Arc::new(TestTenantRegistry::new(
            snapshot,
        ))))
    }

    #[cfg(feature = "mcp-http")]
    fn build_tenant_test_state(
        tenant_id: &str,
        api_key: &str,
        rate_limit_rpm: u64,
    ) -> Arc<TenantAuthState> {
        let mut snapshot = TenantSnapshot::new();
        snapshot.insert(
            sha256_hex(api_key),
            TenantInfo {
                tenant_id: tenant_id.to_string(),
                rate_limit_rpm,
            },
        );
        build_tenant_auth_state_from_snapshot(snapshot)
    }

    #[cfg(feature = "mcp-http")]
    #[derive(Clone)]
    struct TestTenantRegistry {
        snapshot: std::sync::Arc<std::sync::Mutex<TenantSnapshot>>,
    }

    #[cfg(feature = "mcp-http")]
    impl TestTenantRegistry {
        fn new(snapshot: TenantSnapshot) -> Self {
            Self {
                snapshot: std::sync::Arc::new(std::sync::Mutex::new(snapshot)),
            }
        }

        fn replace_snapshot(&self, snapshot: TenantSnapshot) {
            *self
                .snapshot
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = snapshot;
        }
    }

    #[cfg(feature = "mcp-http")]
    #[test]
    fn tenant_for_token_uses_snapshot_map_match() {
        let mut snapshot = TenantSnapshot::new();
        let key = "acme-api-key";
        snapshot.insert(
            sha256_hex(key),
            TenantInfo {
                tenant_id: "acme".to_string(),
                rate_limit_rpm: 120,
            },
        );

        let tenant = tenant_for_token(&snapshot, &sha256_hex(key)).expect("token should resolve");
        assert_eq!(tenant.tenant_id, "acme");
        assert_eq!(tenant.rate_limit_rpm, 120);
        assert!(tenant_for_token(&snapshot, "deadbeef").is_none());
    }

    #[cfg(feature = "mcp-http")]
    #[tokio::test]
    async fn tenant_registry_refresh_revocation_removes_access() {
        let tenant_key = "expected-secret";
        let mut snapshot = TenantSnapshot::new();
        snapshot.insert(
            sha256_hex(tenant_key),
            TenantInfo {
                tenant_id: "acme".to_string(),
                rate_limit_rpm: 120,
            },
        );
        let registry = std::sync::Arc::new(TestTenantRegistry::new(snapshot));
        let state = Arc::new(TenantAuthState::new(registry.clone()));

        let request = Request::builder()
            .method("GET")
            .uri("/")
            .header(
                axum::http::header::AUTHORIZATION,
                format!("Bearer {tenant_key}"),
            )
            .body(Body::empty())
            .unwrap();
        let authorized = call_tenant_route(Some(state.clone()), request).await;
        assert_eq!(authorized.status(), StatusCode::OK);

        registry.replace_snapshot(TenantSnapshot::new());
        let denied = call_tenant_route(
            Some(state),
            Request::builder()
                .method("GET")
                .uri("/")
                .header(
                    axum::http::header::AUTHORIZATION,
                    format!("Bearer {tenant_key}"),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    }

    #[cfg(feature = "mcp-http")]
    #[test]
    fn tenant_auth_state_with_empty_snapshot_denies_all_requests() {
        let state = build_tenant_auth_state_from_snapshot(TenantSnapshot::new());
        let request = Request::builder()
            .method("GET")
            .uri("/")
            .header(axum::http::header::AUTHORIZATION, "Bearer shared-secret")
            .body(Body::empty())
            .unwrap();
        let response = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(call_tenant_route(Some(state), request));
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[cfg(all(feature = "mcp-http", feature = "tenant-registry-postgres"))]
    #[tokio::test]
    async fn postgres_registry_stale_refresh_behavior() {
        let registry = PostgresTenantRegistry::new(
            "postgres://localhost/postgres".to_string(),
            NEXUS_MCP_TENANT_DB_RELATION_DEFAULT.to_string(),
            NEXUS_MCP_HTTP_DEFAULT_TENANT_RATE_LIMIT_RPM,
            Duration::from_secs(20),
            Duration::from_secs(2),
            Duration::from_secs(10),
        )
        .unwrap();
        let mut snapshot = TenantSnapshot::new();
        snapshot.insert(
            sha256_hex("known-key"),
            TenantInfo {
                tenant_id: "acme".to_string(),
                rate_limit_rpm: 120,
            },
        );
        let refreshed_at = Instant::now();
        registry.snapshot.store(Arc::new(TenantSnapshotState {
            snapshot: Arc::new(snapshot),
            refreshed_at: Some(refreshed_at),
        }));

        registry.clear_if_stale(refreshed_at + Duration::from_millis(500));
        assert_eq!(registry.current_snapshot().len(), 1);

        registry.clear_if_stale(refreshed_at + Duration::from_secs(3));
        assert!(registry.current_snapshot().is_empty());
        assert!(registry.snapshot.load_full().refreshed_at.is_none());
    }

    #[cfg(all(feature = "mcp-http", feature = "tenant-registry-postgres"))]
    #[tokio::test]
    async fn postgres_tenant_refresh_timeout_clears_stale_snapshot() {
        let _guard = acquire_mcp_http_env();
        let registry = PostgresTenantRegistry::new(
            "postgres://localhost/postgres".to_string(),
            NEXUS_MCP_TENANT_DB_RELATION_DEFAULT.to_string(),
            NEXUS_MCP_HTTP_DEFAULT_TENANT_RATE_LIMIT_RPM,
            Duration::from_secs(20),
            Duration::from_secs(2),
            Duration::from_secs(5),
        )
        .unwrap();

        let tenant_key = "known-key";
        let mut snapshot = TenantSnapshot::new();
        snapshot.insert(
            sha256_hex(tenant_key),
            TenantInfo {
                tenant_id: "acme".to_string(),
                rate_limit_rpm: 120,
            },
        );
        let refreshed_at = Instant::now() - Duration::from_millis(1500);
        registry.snapshot.store(Arc::new(TenantSnapshotState {
            snapshot: Arc::new(snapshot),
            refreshed_at: Some(refreshed_at),
        }));

        let tenant_registry: Arc<dyn TenantRegistry> = registry.clone();
        let state = Arc::new(TenantAuthState::new(tenant_registry));
        let authorized = call_tenant_route(
            Some(state.clone()),
            Request::builder()
                .method("GET")
                .uri("/")
                .header(
                    axum::http::header::AUTHORIZATION,
                    format!("Bearer {tenant_key}"),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(authorized.status(), StatusCode::OK);

        let started = tokio::time::Instant::now();
        let timed_out = tokio::time::timeout(Duration::from_secs(3), async {
            refresh_postgres_tenants_with_loader(&registry, Duration::from_secs(5), async {
                tokio::time::sleep(Duration::from_secs(10)).await;
                Ok(TenantSnapshot::new())
            })
            .await
        })
        .await;
        assert!(
            timed_out.is_ok(),
            "refresh must finish within bounded timeout"
        );
        assert!(
            timed_out.unwrap().is_err(),
            "expected timeout-based refresh error"
        );

        let elapsed = started.elapsed();
        assert!(elapsed < Duration::from_secs(2));
        assert!(registry.current_snapshot().is_empty());
        assert!(
            registry.snapshot.load_full().refreshed_at.is_none(),
            "refresh failure should allow stale eviction"
        );

        let denied = call_tenant_route(
            Some(state),
            Request::builder()
                .method("GET")
                .uri("/")
                .header(
                    axum::http::header::AUTHORIZATION,
                    format!("Bearer {tenant_key}"),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    }

    #[cfg(all(feature = "mcp-http", feature = "tenant-registry-postgres"))]
    #[test]
    fn parse_tenant_db_ssl_mode_requires_verification_or_require() {
        // PgSslMode does not implement PartialEq, so compare via matches!.
        assert!(matches!(
            tenant_db_ssl_mode("postgres://localhost/postgres"),
            sqlx::postgres::PgSslMode::Require
        ));
        assert!(matches!(
            tenant_db_ssl_mode("postgres://localhost/postgres?sslmode=prefer"),
            sqlx::postgres::PgSslMode::Require
        ));
        assert!(matches!(
            tenant_db_ssl_mode("postgres://localhost/postgres?sslmode=verify-full"),
            sqlx::postgres::PgSslMode::VerifyFull
        ));
    }

    #[cfg(all(feature = "mcp-http", feature = "tenant-registry-postgres"))]
    #[test]
    fn quote_postgres_relation_identifier_quotes_segments() {
        assert_eq!(
            quote_postgres_relation_identifier("public.api_keys"),
            "\"public\".\"api_keys\""
        );
        assert_eq!(
            quote_postgres_relation_identifier(r#"schema.with"dot"#),
            "\"schema\".\"with\"\"dot\""
        );
    }

    #[cfg(all(feature = "mcp-http", feature = "tenant-registry-postgres"))]
    #[tokio::test]
    async fn build_postgres_tenant_registry_requires_db_url() {
        let _guard = acquire_mcp_http_env();
        std::env::set_var(
            NEXUS_MCP_TENANT_SOURCE_ENV,
            NEXUS_MCP_TENANT_SOURCE_POSTGRES,
        );
        let error = match build_postgres_tenant_registry().await {
            Ok(_) => panic!("expected build_postgres_tenant_registry() to fail"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains(&format!("{NEXUS_MCP_TENANT_DB_URL_ENV} is required")),
            "unexpected error: {error}"
        );
    }

    #[cfg(all(feature = "mcp-http", feature = "tenant-registry-postgres"))]
    #[tokio::test]
    async fn build_postgres_tenant_registry_rejects_invalid_ttls() {
        let _guard = acquire_mcp_http_env();
        std::env::set_var(
            NEXUS_MCP_TENANT_SOURCE_ENV,
            NEXUS_MCP_TENANT_SOURCE_POSTGRES,
        );
        std::env::set_var(NEXUS_MCP_TENANT_DB_URL_ENV, "postgres://localhost/postgres");

        std::env::set_var(NEXUS_MCP_TENANT_REFRESH_SECS_ENV, "0");
        let zero_refresh = match build_postgres_tenant_registry().await {
            Ok(_) => panic!("expected build_postgres_tenant_registry() to fail"),
            Err(error) => error,
        };
        assert!(
            zero_refresh.to_string().contains(&format!(
                "{NEXUS_MCP_TENANT_REFRESH_SECS_ENV} must be greater than 0"
            )),
            "unexpected error: {zero_refresh}"
        );

        std::env::set_var(NEXUS_MCP_TENANT_REFRESH_SECS_ENV, "10");
        std::env::set_var(NEXUS_MCP_TENANT_MAX_STALE_SECS_ENV, "2");
        let bad_stale = match build_postgres_tenant_registry().await {
            Ok(_) => panic!("expected build_postgres_tenant_registry() to fail"),
            Err(error) => error,
        };
        assert!(
            bad_stale
                .to_string()
                .contains(&format!(
                    "{NEXUS_MCP_TENANT_MAX_STALE_SECS_ENV} must be >= {NEXUS_MCP_TENANT_REFRESH_SECS_ENV}"
                )),
            "unexpected error: {bad_stale}"
        );
    }

    #[cfg(feature = "mcp-http")]
    impl TenantRegistry for TestTenantRegistry {
        fn current_snapshot(&self) -> Arc<TenantSnapshot> {
            Arc::new(
                self.snapshot
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .clone(),
            )
        }
    }

    #[cfg(feature = "mcp-http")]
    async fn tenant_echo_handler(
        axum::extract::Extension(ctx): axum::extract::Extension<TenantContext>,
    ) -> axum::response::Response {
        let mut response = axum::response::Response::new(Body::empty());
        response.headers_mut().insert(
            "x-tenant-id",
            ctx.tenant_id.parse().expect("tenant id is header-safe"),
        );
        response
    }

    #[cfg(feature = "mcp-http")]
    async fn call_tenant_route(
        auth: Option<Arc<TenantAuthState>>,
        request: Request<Body>,
    ) -> Response {
        let app = axum::Router::new()
            .route("/", axum::routing::get(tenant_echo_handler))
            .route_layer(axum::middleware::from_fn_with_state(
                auth.clone(),
                require_bearer_token,
            ))
            .with_state(auth);

        tower::util::ServiceExt::oneshot(app, request)
            .await
            .expect("request should be handled")
    }

    #[cfg(feature = "mcp-http")]
    #[tokio::test]
    async fn parse_tenant_store_loads_from_temporary_json_file() {
        let tmp = tempfile::tempdir().unwrap();
        let tenants_path = tmp.path().join("tenants.json");
        let api_key = "acme-secret-key";
        let body = serde_json::json!([{
            "tenant_id": "acme",
            "api_key_sha256": sha256_hex(api_key),
            "rate_limit_rpm": 120
        }])
        .to_string();
        std::fs::write(&tenants_path, body).unwrap();
        let tenants = parse_tenant_config_file(&tenants_path).unwrap();
        let tenant = tenants
            .get(&sha256_hex(api_key))
            .expect("tenant should be in snapshot");

        assert_eq!(tenants.len(), 1);
        assert_eq!(tenant.tenant_id, "acme");
        assert_eq!(tenant.rate_limit_rpm, 120);
    }

    #[cfg(all(feature = "mcp-http", feature = "tenant-registry-postgres"))]
    #[tokio::test]
    async fn postgres_tenant_snapshot_loads_active_rows_from_db_table() {
        let _guard = acquire_mcp_http_env();
        let db_url = match std::env::var(NEXUS_MCP_TENANT_DB_URL_ENV) {
            Ok(url) if !url.trim().is_empty() => url,
            _ => return,
        };

        let pool = match sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&db_url)
            .await
        {
            Ok(pool) => pool,
            Err(_) => return,
        };

        let relation = format!(
            "tenant_registry_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock must be available")
                .as_nanos()
        );
        let create = format!(
            "CREATE TEMP TABLE {relation} (key_sha256 TEXT NOT NULL, workspace_id TEXT NOT NULL, rate_limit_rpm INTEGER, status TEXT NOT NULL)"
        );
        if sqlx::query(&create).execute(&pool).await.is_err() {
            return;
        }

        let active_key = sha256_hex("postgres-active-key");
        let revoked_key = sha256_hex("postgres-revoked-key");
        let active_rate_limit_rpm: i64 = 180;
        let insert = format!(
            "INSERT INTO {relation} (key_sha256, workspace_id, rate_limit_rpm, status) VALUES ($1, $2, $3, $4), ($5, $6, $7, $8)"
        );
        if sqlx::query(&insert)
            .bind(active_key.clone())
            .bind("tenant-acme")
            .bind(Option::<i64>::Some(active_rate_limit_rpm))
            .bind("active")
            .bind(revoked_key)
            .bind("tenant-old")
            .bind(Option::<i64>::Some(10))
            .bind("revoked")
            .execute(&pool)
            .await
            .is_err()
        {
            return;
        }

        let snapshot = match load_postgres_tenant_snapshot(
            &pool,
            &relation,
            NEXUS_MCP_HTTP_DEFAULT_TENANT_RATE_LIMIT_RPM,
        )
        .await
        {
            Ok(snapshot) => snapshot,
            Err(_) => return,
        };
        assert_eq!(snapshot.len(), 1);

        let tenant = snapshot
            .get(&active_key)
            .expect("active key must be present in snapshot");
        assert_eq!(tenant.tenant_id, "tenant-acme");
        assert_eq!(tenant.rate_limit_rpm, active_rate_limit_rpm as u64,);

        let state = build_tenant_auth_state_from_snapshot(snapshot);
        let authorized = call_tenant_route(
            Some(state),
            Request::builder()
                .method("GET")
                .uri("/")
                .header(
                    axum::http::header::AUTHORIZATION,
                    "Bearer postgres-active-key",
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(authorized.status(), StatusCode::OK);
    }

    #[cfg(feature = "mcp-http")]
    #[test]
    fn parse_tenant_store_rejects_duplicate_api_key_hashes() {
        let tmp = tempfile::tempdir().unwrap();
        let tenants_path = tmp.path().join("tenants.json");
        let body = serde_json::json!([
            {
                "tenant_id": "acme",
                "api_key_sha256": sha256_hex("shared-key"),
            },
            {
                "tenant_id": "second",
                "api_key_sha256": sha256_hex("shared-key"),
            }
        ])
        .to_string();
        std::fs::write(&tenants_path, body).unwrap();

        let error = parse_tenant_config_file(&tenants_path)
            .unwrap_err()
            .to_string();

        assert!(error.contains("duplicate tenant api_key_sha256"));
    }

    #[cfg(feature = "mcp-http")]
    #[test]
    fn parse_mcp_http_token_normalizes_empty() {
        with_clean_mcp_http_env(|| {
            std::env::set_var(NEXUS_MCP_HTTP_TOKEN_ENV, "   ");
            assert!(parse_mcp_http_token().unwrap().is_none());
        });
    }

    #[cfg(feature = "mcp-http")]
    #[test]
    fn parse_mcp_http_tenants_path_empty_string_is_none() {
        with_clean_mcp_http_env(|| {
            std::env::set_var(NEXUS_MCP_HTTP_TENANTS_ENV, "   ");
            assert!(parse_mcp_http_tenants_path().unwrap().is_none());
        });
    }

    #[cfg(feature = "mcp-http")]
    #[tokio::test]
    async fn load_tenant_auth_state_falls_back_to_token_when_tenants_unset_or_empty() {
        let _guard = acquire_mcp_http_env();
        std::env::set_var(NEXUS_MCP_HTTP_TENANTS_ENV, "   ");
        std::env::set_var(NEXUS_MCP_HTTP_TOKEN_ENV, "shared-secret");
        std::env::set_var(NEXUS_MCP_TENANT_SOURCE_ENV, NEXUS_MCP_TENANT_SOURCE_FILE);

        let state = load_tenant_auth_state()
            .await
            .unwrap()
            .expect("token auth should be configured");

        let request = Request::builder()
            .method("GET")
            .uri("/")
            .header(axum::http::header::AUTHORIZATION, "Bearer shared-secret")
            .body(Body::empty())
            .unwrap();
        let response = call_tenant_route(Some(state), request).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("x-tenant-id")
                .and_then(|value| value.to_str().ok()),
            Some(NEXUS_MCP_HTTP_TENANT_ID_FALLBACK)
        );
    }

    #[cfg(all(feature = "mcp-http", unix))]
    #[test]
    fn parse_mcp_http_token_rejects_non_utf8() {
        with_clean_mcp_http_env(|| {
            use std::os::unix::ffi::OsStringExt;
            std::env::set_var(
                NEXUS_MCP_HTTP_TOKEN_ENV,
                std::ffi::OsString::from_vec(vec![0xff]),
            );

            assert!(parse_mcp_http_token().is_err());
        });
    }

    #[cfg(feature = "mcp-http")]
    fn sha256_hex(value: &str) -> String {
        let digest = sha2::Sha256::digest(value.as_bytes());
        let mut hex = String::with_capacity(64);
        for byte in digest {
            hex.push_str(&format!("{byte:02x}"));
        }
        hex
    }

    #[cfg(feature = "mcp-http")]
    #[tokio::test]
    async fn require_bearer_token_rejects_missing_and_unknown() {
        let state = build_tenant_test_state("acme", "expected-secret", 120);

        let missing = Request::builder()
            .method("GET")
            .uri("/")
            .body(Body::empty())
            .unwrap();
        let missing = call_tenant_route(Some(state.clone()), missing).await;
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);

        let unknown = Request::builder()
            .method("GET")
            .uri("/")
            .header(axum::http::header::AUTHORIZATION, "Bearer wrong")
            .body(Body::empty())
            .unwrap();
        let unknown = call_tenant_route(Some(state), unknown).await;
        assert_eq!(unknown.status(), StatusCode::UNAUTHORIZED);
    }

    #[cfg(feature = "mcp-http")]
    #[tokio::test]
    async fn require_bearer_token_sets_tenant_context() {
        let state = build_tenant_test_state("acme", "expected-secret", 120);
        let request = Request::builder()
            .method("GET")
            .uri("/")
            .header(axum::http::header::AUTHORIZATION, "Bearer expected-secret")
            .body(Body::empty())
            .unwrap();

        let response = call_tenant_route(Some(state), request).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("x-tenant-id")
                .and_then(|value| value.to_str().ok()),
            Some("acme")
        );
    }

    #[cfg(feature = "mcp-http")]
    #[tokio::test]
    async fn require_bearer_token_matches_normalized_secret_with_whitespace() {
        let state = build_tenant_test_state("acme", "expected-secret", 120);
        let request = Request::builder()
            .method("GET")
            .uri("/")
            .header(
                axum::http::header::AUTHORIZATION,
                "Bearer   expected-secret   ",
            )
            .body(Body::empty())
            .unwrap();

        let response = call_tenant_route(Some(state), request).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("x-tenant-id")
                .and_then(|value| value.to_str().ok()),
            Some("acme")
        );
    }

    #[cfg(feature = "mcp-http")]
    #[tokio::test]
    async fn require_bearer_token_rate_limit_429() {
        let state = build_tenant_test_state("acme", "expected-secret", 1);
        let request = Request::builder()
            .method("GET")
            .uri("/")
            .header(axum::http::header::AUTHORIZATION, "Bearer expected-secret")
            .body(Body::empty())
            .unwrap();

        let first = call_tenant_route(
            Some(state.clone()),
            Request::builder()
                .method("GET")
                .uri("/")
                .header(axum::http::header::AUTHORIZATION, "Bearer expected-secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(first.status(), StatusCode::OK);

        let second = call_tenant_route(Some(state), request).await;
        assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[cfg(feature = "mcp-http")]
    #[test]
    fn constant_time_compare_checks_equal_and_mismatch_values() {
        let a = b"\x01\x02\x03";
        let b = b"\x01\x02\x03";
        let c = b"\x01\x02\x04";
        let d = b"\x01\x02\x03\x04";

        assert!(constant_time_eq(a, b));
        assert!(!constant_time_eq(a, c));
        assert!(!constant_time_eq(a, d));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_wasm_path_symlink_escape() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let allowed = tmp.path().join("allowed");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&allowed).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let outside_wasm = outside.join("tool.wasm");
        let linked_wasm = allowed.join("linked.wasm");
        std::fs::write(&outside_wasm, b"\0asm").unwrap();
        symlink(&outside_wasm, &linked_wasm).unwrap();

        let allowed_dirs = vec![std::fs::canonicalize(&allowed).unwrap()];
        let err = resolve_wasm_path(&linked_wasm, &allowed_dirs).unwrap_err();

        assert!(
            err.to_string().contains("outside allowed MCP module"),
            "unexpected error: {err}"
        );
    }
}
