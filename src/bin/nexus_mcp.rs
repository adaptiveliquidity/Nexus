//! Nexus MCP Server — exposes hypervisor operations as Model Context Protocol tools.
//!
//! Transport: stdio (for Claude Code / mcp.json integration).
//! Start with: `nexus-mcp` (no arguments needed).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use rmcp::{handler::server::wrapper::Parameters, schemars, tool, tool_router, ServiceExt, transport::stdio};
use serde::{Deserialize, Serialize};
use tracing_subscriber::{self, EnvFilter};
use uuid::Uuid;

use nexus::hypervisor::{
    fork_and_race, HypervisorConfig, NexusHypervisor, RecoveryAction, RecoverySource,
    SelectionStrategy, SpeculativeBranch, SpeculativeConfig, ToolDefinition, ToolOutput,
};
use nexus::security::Capability;
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
    #[schemars(description = "Capabilities to grant: array of {type, path?} objects. Types: read_file, write_file, list_dir, http_get, http_post, execute, mount_tmpfs, all")]
    pub capabilities: Option<Vec<CapabilitySpec>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CapabilitySpec {
    #[schemars(description = "Capability type: read_file, write_file, list_dir, http_get, http_post, execute, mount_tmpfs, all")]
    pub r#type: String,
    #[schemars(description = "Path or URL pattern for the capability (not needed for 'all')")]
    pub path: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SnapshotCreateParams {
    #[schemars(description = "Human-readable label for the snapshot")]
    pub label: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SnapshotRollbackParams {
    #[schemars(description = "UUID of the snapshot to roll back to")]
    pub snapshot_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct IssueTokenParams {
    #[schemars(description = "Capability type: read_file, write_file, list_dir, http_get, http_post, execute, mount_tmpfs, all")]
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
}

#[tool_router(server_handler)]
impl NexusMcpServer {
    #[tool(description = "Execute a WASM tool in the Nexus sandbox. Loads the .wasm file, runs it with optional JSON input, and returns structured output including success/failure, result bytes, execution time, and fuel consumed.")]
    async fn nexus_execute(&self, Parameters(params): Parameters<ExecuteParams>) -> String {
        match self.do_execute(params).await {
            Ok(output) => serde_json::to_string_pretty(&output).unwrap_or_else(|e| format!("{{\"error\": \"{e}\"}}")),
            Err(e) => format!("{{\"error\": \"{e}\"}}"),
        }
    }

    #[tool(description = "Execute a WASM tool with WASI support (filesystem, env, stdio access). Grants specified capabilities for the duration of execution.")]
    async fn nexus_execute_wasi(&self, Parameters(params): Parameters<ExecuteWasiParams>) -> String {
        match self.do_execute_wasi(params).await {
            Ok(output) => serde_json::to_string_pretty(&output).unwrap_or_else(|e| format!("{{\"error\": \"{e}\"}}")),
            Err(e) => format!("{{\"error\": \"{e}\"}}"),
        }
    }

    #[tool(description = "Create a snapshot of the current hypervisor state. Returns the snapshot UUID which can be used for rollback.")]
    async fn nexus_snapshot_create(&self, Parameters(params): Parameters<SnapshotCreateParams>) -> String {
        match self.do_snapshot_create(params) {
            Ok(id) => format!("{{\"snapshot_id\": \"{id}\", \"success\": true}}"),
            Err(e) => format!("{{\"error\": \"{e}\"}}"),
        }
    }

    #[tool(description = "Roll back to a previous snapshot, restoring memory, execution state, and filesystem to that point in time.")]
    async fn nexus_snapshot_rollback(&self, Parameters(params): Parameters<SnapshotRollbackParams>) -> String {
        match self.do_snapshot_rollback(params) {
            Ok(info) => serde_json::to_string_pretty(&info).unwrap_or_else(|e| format!("{{\"error\": \"{e}\"}}")),
            Err(e) => format!("{{\"error\": \"{e}\"}}"),
        }
    }

    #[tool(description = "Issue a capability token that can be passed to execute_wasi calls. Tokens are time-limited and scoped to a specific capability.")]
    async fn nexus_issue_token(&self, Parameters(params): Parameters<IssueTokenParams>) -> String {
        match self.do_issue_token(params) {
            Ok(info) => serde_json::to_string_pretty(&info).unwrap_or_else(|e| format!("{{\"error\": \"{e}\"}}")),
            Err(e) => format!("{{\"error\": \"{e}\"}}"),
        }
    }

    #[tool(description = "Fork execution into multiple branches and race them concurrently. Returns the first successful branch's output. Useful for speculative recovery and parallel exploration.")]
    async fn nexus_fork_and_race(&self, Parameters(params): Parameters<ForkAndRaceParams>) -> String {
        match self.do_fork_and_race(params).await {
            Ok(info) => serde_json::to_string_pretty(&info).unwrap_or_else(|e| format!("{{\"error\": \"{e}\"}}")),
            Err(e) => format!("{{\"error\": \"{e}\"}}"),
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
}

impl From<ToolOutput> for ToolOutputResponse {
    fn from(o: ToolOutput) -> Self {
        ToolOutputResponse {
            success: o.success,
            result: o.result.map(|b| {
                String::from_utf8(b.clone())
                    .unwrap_or_else(|_| base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &b))
            }),
            error: o.error,
            execution_time_ms: o.execution_time_ms,
            fuel_consumed: o.fuel_consumed,
            rollback_performed: o.rollback_performed,
        }
    }
}

#[derive(Serialize)]
struct RollbackResponse {
    snapshot_id: String,
    timestamp: String,
    fs_operations: usize,
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
}

impl NexusMcpServer {
    async fn do_execute(&self, params: ExecuteParams) -> Result<ToolOutputResponse> {
        let wasm_bytes = tokio::fs::read(&params.wasm_path).await
            .map_err(|e| anyhow::anyhow!("Failed to read wasm file '{}': {}", params.wasm_path, e))?;

        let mut tool = ToolDefinition::new("mcp_tool".to_string(), wasm_bytes);
        if let Some(entry) = params.entry {
            tool = tool.with_entry(&entry);
        }

        let input = params.input.unwrap_or(serde_json::json!({}));
        let output = self.hypervisor.execute_tool(tool, input).await?;
        Ok(ToolOutputResponse::from(output))
    }

    async fn do_execute_wasi(&self, params: ExecuteWasiParams) -> Result<ToolOutputResponse> {
        let wasm_bytes = tokio::fs::read(&params.wasm_path).await
            .map_err(|e| anyhow::anyhow!("Failed to read wasm file '{}': {}", params.wasm_path, e))?;

        let mut tool = ToolDefinition::new("mcp_tool_wasi".to_string(), wasm_bytes);
        if let Some(entry) = params.entry {
            tool = tool.with_entry(&entry);
        }

        let caps: Vec<Capability> = params.capabilities
            .unwrap_or_default()
            .into_iter()
            .filter_map(|spec| parse_capability(&spec))
            .collect();
        tool = tool.with_capabilities(caps);

        let input = params.input.unwrap_or(serde_json::json!({}));
        let output = self.hypervisor.execute_tool_wasi(tool, input, &[]).await?;
        Ok(ToolOutputResponse::from(output))
    }

    fn do_snapshot_create(&self, params: SnapshotCreateParams) -> Result<String> {
        let label = params.label.unwrap_or_else(|| "mcp_snapshot".to_string());

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

        Ok(snapshot.id.to_string())
    }

    fn do_snapshot_rollback(&self, params: SnapshotRollbackParams) -> Result<RollbackResponse> {
        let id = Uuid::parse_str(&params.snapshot_id)
            .map_err(|e| anyhow::anyhow!("Invalid snapshot UUID: {e}"))?;

        let result = self.hypervisor.snapshot_manager().rollback_to(&id)?;

        Ok(RollbackResponse {
            snapshot_id: result.snapshot_id.to_string(),
            timestamp: result.timestamp.to_rfc3339(),
            fs_operations: result.fs_operations.len(),
        })
    }

    fn do_issue_token(&self, params: IssueTokenParams) -> Result<TokenResponse> {
        let capability = parse_capability_from_str(&params.capability, params.path.as_deref())
            .ok_or_else(|| anyhow::anyhow!("Unknown capability type: {}", params.capability))?;

        let validity = Duration::from_secs(params.validity_secs.unwrap_or(3600));
        let token = self.hypervisor.issue_token(capability.clone(), "mcp_client", validity)?;

        Ok(TokenResponse {
            token_id: token.id.to_string(),
            capability: format!("{:?}", capability),
            expires_in_secs: params.validity_secs.unwrap_or(3600),
        })
    }

    async fn do_fork_and_race(&self, params: ForkAndRaceParams) -> Result<ForkAndRaceResponse> {
        let wasm_bytes = tokio::fs::read(&params.wasm_path).await
            .map_err(|e| anyhow::anyhow!("Failed to read wasm file '{}': {}", params.wasm_path, e))?;

        let base_snapshot_id = Uuid::new_v4();

        let branches: Vec<SpeculativeBranch> = params.branches
            .into_iter()
            .map(|spec| {
                let mut tool = ToolDefinition::new("fork_branch".to_string(), wasm_bytes.clone());
                if let Some(entry) = spec.entry {
                    tool = tool.with_entry(&entry);
                }
                SpeculativeBranch::new(
                    base_snapshot_id,
                    tool,
                    RecoveryAction::new("mcp_fork_branch", RecoverySource::Static),
                )
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
        let result = fork_and_race(branches, &config, |branch| {
            let hyp = hyp.clone();
            async move {
                let input = serde_json::json!({});
                hyp.execute_tool(branch.tool, input).await
            }
        }).await?;

        Ok(ForkAndRaceResponse {
            winner_branch_id: result.winner.branch_id.to_string(),
            branches_tried: result.branches_tried,
            branches_succeeded: result.branches_succeeded,
            winner_elapsed_ms: result.winner.elapsed.as_millis() as u64,
            winner_output: result.winner.output.map(ToolOutputResponse::from),
        })
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn parse_capability(spec: &CapabilitySpec) -> Option<Capability> {
    parse_capability_from_str(&spec.r#type, spec.path.as_deref())
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

    let server = NexusMcpServer { hypervisor };

    let service = server.serve(stdio()).await.inspect_err(|e| {
        tracing::error!("MCP server error: {:?}", e);
    })?;

    service.waiting().await?;
    Ok(())
}
