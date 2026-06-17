//! Nexus MCP Server — exposes hypervisor operations as Model Context Protocol tools.
//!
//! Transport: stdio (for Claude Code / mcp.json integration).
//! Start with: `nexus-mcp` (no arguments needed).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use rmcp::{
    handler::server::wrapper::Parameters, schemars, tool, tool_router, transport::stdio, ServiceExt,
};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use tracing_subscriber::{self, EnvFilter};
use uuid::Uuid;

use nexus::hypervisor::{
    fork_and_race, HypervisorConfig, NexusHypervisor, RecoveryAction, RecoverySource,
    SelectionStrategy, SpeculativeBranch, SpeculativeConfig, ToolDefinition, ToolOutput,
};
use nexus::security::{Capability, CapabilityToken};
use nexus::snapshot::{ExecutionState, FilesystemDiff, SnapshotMetadata};

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
pub struct ExecuteWasiParams {
    #[schemars(description = "Path to the .wasm file to execute")]
    pub wasm_path: String,
    #[schemars(description = "Entry point function name (default: _start)")]
    pub entry: Option<String>,
    #[schemars(description = "JSON input to pass to the WASM module")]
    pub input: Option<serde_json::Value>,
    #[schemars(
        description = "Capabilities to grant: array of {type, path?} objects. Types: read_file, write_file, list_dir, http_get, http_post, execute, mount_tmpfs, all"
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
        description = "Capability type: read_file, write_file, list_dir, http_get, http_post, execute, mount_tmpfs, all"
    )]
    pub r#type: String,
    #[schemars(description = "Path or URL pattern for the capability (not needed for 'all')")]
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
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct IssueTokenParams {
    #[schemars(
        description = "Capability type: read_file, write_file, list_dir, http_get, http_post, execute, mount_tmpfs, all"
    )]
    pub capability: String,
    #[schemars(description = "Path or URL pattern for the capability")]
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

// ─── MCP Server Handler ──────────────────────────────────────────────────────

#[derive(Clone)]
pub struct NexusMcpServer {
    hypervisor: Arc<NexusHypervisor>,
    wasm_module_dirs: Arc<Vec<PathBuf>>,
    capability_allowlist: Arc<Option<Vec<Capability>>>,
}

#[tool_router(server_handler)]
impl NexusMcpServer {
    #[tool(
        description = "Execute a WASM tool in the Nexus sandbox. Loads the .wasm file, runs it with optional JSON input, and returns structured output including success/failure, result bytes, execution time, fuel consumed, and the runtime snapshot id when WASM memory was captured."
    )]
    async fn nexus_execute(&self, Parameters(params): Parameters<ExecuteParams>) -> String {
        match self.do_execute(params).await {
            Ok(output) => serde_json::to_string_pretty(&output).unwrap_or_else(tool_error_response),
            Err(e) => tool_error_response(e),
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
            Err(e) => tool_error_response(e),
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
            Err(e) => tool_error_response(e),
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
            Err(e) => tool_error_response(e),
        }
    }

    #[tool(
        description = "Issue an operator-allowlisted capability token that can be passed to execute_wasi calls. Tokens are time-limited and scoped to a specific capability."
    )]
    async fn nexus_issue_token(&self, Parameters(params): Parameters<IssueTokenParams>) -> String {
        match self.do_issue_token(params) {
            Ok(info) => serde_json::to_string_pretty(&info).unwrap_or_else(tool_error_response),
            Err(e) => tool_error_response(e),
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
            Err(e) => tool_error_response(e),
        }
    }
}

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
        Ok(Self {
            hypervisor,
            wasm_module_dirs: Arc::new(allowed_wasm_module_dirs()?),
            capability_allowlist: Arc::new(capability_allowlist_from_env()?),
        })
    }

    async fn do_execute(&self, params: ExecuteParams) -> Result<ToolOutputResponse> {
        let wasm_path = self.resolve_wasm_path(&params.wasm_path)?;
        let wasm_bytes = tokio::fs::read(&wasm_path).await.map_err(|e| {
            anyhow::anyhow!("Failed to read wasm file '{}': {}", params.wasm_path, e)
        })?;

        let mut tool = ToolDefinition::new("mcp_tool".to_string(), wasm_bytes);
        if let Some(entry) = params.entry {
            tool = tool.with_entry(&entry);
        }

        let input = params.input.unwrap_or(serde_json::json!({}));
        let output = self.hypervisor.execute_tool(tool, input).await?;
        Ok(ToolOutputResponse::from(output))
    }

    async fn do_execute_wasi(&self, params: ExecuteWasiParams) -> Result<ToolOutputResponse> {
        let wasm_path = self.resolve_wasm_path(&params.wasm_path)?;
        let wasm_bytes = tokio::fs::read(&wasm_path).await.map_err(|e| {
            anyhow::anyhow!("Failed to read wasm file '{}': {}", params.wasm_path, e)
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
        tool = tool.with_capabilities(caps);

        let input = params.input.unwrap_or(serde_json::json!({}));
        let output = self
            .hypervisor
            .execute_tool_wasi(tool, input, &caller_tokens)
            .await?;
        Ok(ToolOutputResponse::from(output))
    }

    fn do_snapshot_create(&self, params: SnapshotCreateParams) -> Result<SnapshotCreateResponse> {
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
        let id = Uuid::parse_str(&params.snapshot_id)
            .map_err(|e| anyhow::anyhow!("Invalid snapshot UUID: {e}"))?;

        let result = self.hypervisor.rollback_snapshot(id)?;
        let restored_state = if params.include_restored_state.unwrap_or(false) {
            Some(restored_state_summary(&result))
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

    async fn do_fork_and_race(&self, params: ForkAndRaceParams) -> Result<ForkAndRaceResponse> {
        let wasm_path = self.resolve_wasm_path(&params.wasm_path)?;
        let wasm_bytes = tokio::fs::read(&wasm_path).await.map_err(|e| {
            anyhow::anyhow!("Failed to read wasm file '{}': {}", params.wasm_path, e)
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
                            anyhow::anyhow!(
                                "parent_token_id {parent_id} does not authorize requested capability: {e}"
                            )
                        })
                })
                .collect();
        }

        let Some(allowlist) = self.capability_allowlist.as_ref() else {
            anyhow::bail!(
                "execute_wasi capability requests require parent_token_id or operator allowlist {NEXUS_MCP_CAPABILITY_ALLOWLIST_ENV}"
            );
        };

        let mut tokens = Vec::with_capacity(sanitized.len());
        for (capability, validity_secs) in sanitized {
            if !capability_allowed_by(allowlist, &capability) {
                anyhow::bail!(
                    "requested capability {:?} is not allowed by {NEXUS_MCP_CAPABILITY_ALLOWLIST_ENV}",
                    capability
                );
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
            anyhow::bail!(
                "capability token issuance requires operator allowlist {NEXUS_MCP_CAPABILITY_ALLOWLIST_ENV}"
            );
        };

        if !capability_allowed_by(allowlist, capability) {
            anyhow::bail!(
                "requested capability {:?} is not allowed by {NEXUS_MCP_CAPABILITY_ALLOWLIST_ENV}",
                capability
            );
        }
        Ok(())
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

const NEXUS_MCP_MODULE_DIR_ENV: &str = "NEXUS_MCP_MODULE_DIR";
const NEXUS_MCP_CAPABILITY_ALLOWLIST_ENV: &str = "NEXUS_MCP_CAPABILITY_ALLOWLIST";
const RESTORED_MEMORY_PREVIEW_BYTES: usize = 64;

/// Maximum token validity an MCP client may request, in seconds (1 hour).
/// Larger caller-supplied values are clamped to this. See SECURITY.md.
const MAX_TOKEN_VALIDITY_SECS: u64 = 3600;

fn restored_state_summary(result: &nexus::snapshot::RollbackResult) -> RestoredStateSummary {
    let preview_len = result.memory.len().min(RESTORED_MEMORY_PREVIEW_BYTES);
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

fn capability_allowed_by(allowlist: &[Capability], capability: &Capability) -> bool {
    allowlist
        .iter()
        .any(|allowed| capability.is_subset_of(allowed))
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

fn resolve_wasm_path(wasm_path: &Path, allowed_dirs: &[PathBuf]) -> Result<PathBuf> {
    let canonical = std::fs::canonicalize(wasm_path).map_err(|e| {
        anyhow::anyhow!(
            "Failed to canonicalize wasm file '{}': {e}",
            wasm_path.display()
        )
    })?;

    if !canonical.is_file() {
        anyhow::bail!(
            "wasm path '{}' resolved to non-file '{}'",
            wasm_path.display(),
            canonical.display()
        );
    }

    if allowed_dirs.iter().any(|dir| canonical.starts_with(dir)) {
        return Ok(canonical);
    }

    anyhow::bail!(
        "wasm path '{}' is outside allowed MCP module directories",
        wasm_path.display()
    )
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
    parse_capability_from_str(&spec.r#type, spec.path.as_deref())
        .ok_or_else(|| anyhow::anyhow!("Unknown capability type: {}", spec.r#type))
}

fn parse_capability_from_str(cap_type: &str, path: Option<&str>) -> Option<Capability> {
    let p = || PathBuf::from(path.unwrap_or("."));
    let s = || path.unwrap_or("*").to_string();

    match cap_type {
        "read_file" => Some(Capability::ReadFile(p())),
        "write_file" => Some(Capability::WriteFile(p())),
        "list_dir" => Some(Capability::ListDirectory(p())),
        "http_get" => Some(Capability::HttpGet(s())),
        "http_post" => Some(Capability::HttpPost(s())),
        "execute" => Some(Capability::ExecuteBinary(p())),
        "mount_tmpfs" => Some(Capability::MountTmpfs(p())),
        "all" => Some(Capability::All),
        _ => None,
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

    let config = HypervisorConfig::default();
    let hypervisor = Arc::new(NexusHypervisor::new(config)?);

    let server = NexusMcpServer::new(hypervisor)?;

    let service = server.serve(stdio()).await.inspect_err(|e| {
        tracing::error!("MCP server error: {:?}", e);
    })?;

    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
