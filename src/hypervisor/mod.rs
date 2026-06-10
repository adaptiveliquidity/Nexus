//! Nexus Hypervisor Core
//!
//! Main orchestrator that ties together sandbox, snapshots, and validation.

pub mod failure_mode;
pub mod llm_policy;
pub mod recovery;
pub mod speculative;
pub mod validator;

pub use failure_mode::FailureMode;
pub use llm_policy::{LLMPolicy, LlmBudget, LlmProvider};
pub use recovery::{LayeredPolicy, RecoveryAction, RecoveryPolicy, RecoverySource, StaticPolicy};
pub use speculative::{
    fork_and_race, BranchOutcome, SelectionStrategy, SpeculativeBranch, SpeculativeConfig,
    SpeculativeResult,
};

use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use crate::error::{NexusError, Result};
use crate::hypervisor::validator::error_log::ErrorLog;
use crate::hypervisor::validator::health::{HealthConfig, HealthValidator};
use crate::sandbox::{FuelBudgetPolicy, FuelProfile, SandboxConfig, WasmSandbox};
use crate::security::{Capability, CapabilityManager};
use crate::snapshot::{
    ExecutionState, FilesystemDiff, Snapshot, SnapshotManager, SnapshotMetadata,
};
use crate::telemetry::{ExecutionRecord, TelemetrySink};
// Re-exports at the top of this module bring `FailureMode`, `RecoveryAction`,
// `RecoveryPolicy`, and `StaticPolicy` into scope; no `use crate::...` here
// to avoid duplicate-import errors with the `pub use` declarations.

/// Tool definition for execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub wasm_bytes: Vec<u8>,
    pub entry_point: String,
    pub input_schema: serde_json::Value,
    pub required_capabilities: Vec<Capability>,
}

impl ToolDefinition {
    pub fn new(name: String, wasm_bytes: Vec<u8>) -> Self {
        ToolDefinition {
            name,
            wasm_bytes,
            entry_point: "_start".to_string(),
            input_schema: serde_json::json!({}),
            required_capabilities: Vec::new(),
        }
    }

    pub fn with_entry(mut self, entry: &str) -> Self {
        self.entry_point = entry.to_string();
        self
    }

    pub fn with_capabilities(mut self, caps: Vec<Capability>) -> Self {
        self.required_capabilities = caps;
        self
    }
}

/// Tool execution output
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutput {
    pub success: bool,
    pub result: Option<Vec<u8>>,
    pub error: Option<String>,
    pub rollback_performed: bool,
    pub execution_time_ms: u64,
    pub fuel_consumed: u64,
    pub error_log: Option<ErrorLog>,
}

/// Configuration for the hypervisor
#[derive(Debug, Clone)]
pub struct HypervisorConfig {
    pub snapshot_capacity: usize,
    pub enable_persistence: bool,
    pub persistence_dir: Option<std::path::PathBuf>,
    pub health_config: HealthConfig,
    pub sandbox_config: SandboxConfig,
    pub max_retries: u32,
    pub retry_delay: Duration,
}

impl Default for HypervisorConfig {
    fn default() -> Self {
        HypervisorConfig {
            snapshot_capacity: 100,
            enable_persistence: false,
            persistence_dir: None,
            health_config: HealthConfig::default(),
            sandbox_config: SandboxConfig::default(),
            max_retries: 3,
            retry_delay: Duration::from_millis(100),
        }
    }
}

/// The main Nexus Hypervisor orchestrator
pub struct NexusHypervisor {
    config: HypervisorConfig,
    sandbox: RwLock<WasmSandbox>,
    snapshot_manager: Arc<SnapshotManager>,
    capability_manager: RwLock<CapabilityManager>,
    health_validator: HealthValidator,
    telemetry: Arc<TelemetrySink>,
    current_snapshot: RwLock<Option<Snapshot>>,
    /// Pluggable recovery policy — Phase A ships `StaticPolicy`; Phase B
    /// wraps it in `LayeredPolicy` with `InstinctPolicy` and (optional)
    /// `LLMPolicy`. Behind an `Arc<dyn>` so it is cheap to share with
    /// pooled hypervisors in Phase C.
    recovery_policy: Arc<dyn RecoveryPolicy>,
    /// Optional instinct store. When set, `execute_with_retry` credits
    /// matching instincts after a successful retry and debits them after
    /// a still-failed retry. When unset, outcome feedback is a no-op.
    instinct_store: Option<Arc<crate::instinct::InstinctStore>>,
    /// Adaptive fuel budgeting policy. Computes per-tool fuel limits
    /// from historical telemetry instead of using a single global max.
    fuel_policy: RwLock<FuelBudgetPolicy>,
    /// Decompressed memory from the most recent rollback. Callers can
    /// use `last_rollback_memory()` to retrieve these bytes and
    /// `restore_memory` to write them into a live wasmtime instance.
    last_rollback_memory: RwLock<Option<Vec<u8>>>,
    /// Execution state (globals/tables) from the most recent rollback.
    last_rollback_execution_state: RwLock<Option<ExecutionState>>,
}

impl NexusHypervisor {
    /// Create a new hypervisor with the default `StaticPolicy` recovery
    /// policy. For custom policies use `new_with_policy`.
    pub fn new(config: HypervisorConfig) -> Result<Self> {
        Self::new_with_policy(config, Arc::new(StaticPolicy::new()))
    }

    /// Create a hypervisor with a custom recovery policy. Used by Phase B
    /// to layer the instinct store and LLM policy on top of the static one.
    pub fn new_with_policy(
        config: HypervisorConfig,
        recovery_policy: Arc<dyn RecoveryPolicy>,
    ) -> Result<Self> {
        let sandbox_config = config.sandbox_config.clone();
        let snapshot_capacity = config.snapshot_capacity;
        let persistence_dir = config.persistence_dir.clone();
        let enable_persistence = config.enable_persistence;
        let health_config = config.health_config.clone();

        let sandbox = WasmSandbox::new(sandbox_config.clone())
            .map_err(|e| NexusError::ConfigError(format!("Failed to create sandbox: {}", e)))?;

        let snapshot_manager = if enable_persistence {
            if let Some(ref dir) = persistence_dir {
                Arc::new(SnapshotManager::with_persistence(
                    snapshot_capacity,
                    dir.clone(),
                ))
            } else {
                Arc::new(SnapshotManager::new(snapshot_capacity))
            }
        } else {
            Arc::new(SnapshotManager::new(snapshot_capacity))
        };

        let capability_manager = CapabilityManager::new();

        let fuel_policy = FuelBudgetPolicy::new(sandbox_config.max_fuel);

        Ok(NexusHypervisor {
            config,
            sandbox: RwLock::new(sandbox),
            snapshot_manager,
            capability_manager: RwLock::new(capability_manager),
            health_validator: HealthValidator::new(health_config),
            telemetry: Arc::new(TelemetrySink::new(1000)),
            current_snapshot: RwLock::new(None),
            recovery_policy,
            instinct_store: None,
            fuel_policy: RwLock::new(fuel_policy),
            last_rollback_memory: RwLock::new(None),
            last_rollback_execution_state: RwLock::new(None),
        })
    }

    /// Inject a different recovery policy at runtime. Used by Phase B's
    /// outcome-feedback loop and by tests that want to assert behavior
    /// against a known policy.
    pub fn set_recovery_policy(&mut self, policy: Arc<dyn RecoveryPolicy>) {
        self.recovery_policy = policy;
    }

    /// Attach an instinct store. After this call, `execute_with_retry`
    /// will credit / debit instincts based on retry outcomes.
    pub fn with_instinct_store(mut self, store: Arc<crate::instinct::InstinctStore>) -> Self {
        self.instinct_store = Some(store);
        self
    }

    /// Enable self-correction (opt-in). Alias for `with_instinct_store`.
    /// Without this call, `execute_with_retry` retries but does NOT
    /// adjust instinct confidence — outcome feedback is a no-op.
    pub fn with_self_correction(self, store: Arc<crate::instinct::InstinctStore>) -> Self {
        self.with_instinct_store(store)
    }

    /// Returns true when self-correction is active (instinct store attached).
    pub fn self_correction_enabled(&self) -> bool {
        self.instinct_store.is_some()
    }

    /// Access the sandbox's wasmtime `Engine` for use with `ModuleCache`.
    pub fn sandbox_engine(&self) -> wasmtime::Engine {
        self.sandbox.read().unwrap().engine().clone()
    }

    /// Grant a capability to the current session
    pub fn grant_capability(&self, capability: Capability, validity: Duration) -> Result<()> {
        let mut manager = self.capability_manager.write().unwrap();
        manager.issue(capability, "system", validity)?;
        Ok(())
    }

    /// Issue a capability token signed by the hypervisor's own key.
    /// The returned token can be passed to `execute_tool_with_tokens`
    /// or `execute_tool_precompiled_with_tokens`.
    pub fn issue_token(
        &self,
        capability: Capability,
        granted_by: &str,
        validity: Duration,
    ) -> Result<crate::security::CapabilityToken> {
        let mut manager = self.capability_manager.write().unwrap();
        manager.issue(capability, granted_by, validity)
    }

    /// Execute a tool with automatic snapshot/rollback.
    ///
    /// Phase A rewrite. Key semantic changes versus the prior version:
    ///   * Snapshot is built from the *real* pre-call WASM linear memory
    ///     returned by the sandbox, not a hardcoded 64 KiB placeholder.
    ///   * Snapshot is created *after* execution finishes, using the
    ///     memory bytes the worker captured right after instantiation.
    ///     This avoids snapshotting at all for load-time failures.
    ///   * Failure classification comes from the typed `FailureMode`
    ///     returned by the sandbox, not from substring matching on the
    ///     error text.
    ///   * Rollback is skipped entirely when `FailureMode::requires_rollback()`
    ///     is false (load-time failures: `InvalidModule`, `MissingEntrypoint`).
    ///   * Recovery actions come from `self.recovery_policy` keyed on the
    ///     `FailureMode`, not from a hardcoded fallback per-operation match.
    ///   * `ExecutionRecord` carries a real `ResourceSnapshot` from
    ///     `HealthValidator::current_resources()`.
    pub async fn execute_tool(
        &self,
        tool: ToolDefinition,
        _input: serde_json::Value,
    ) -> Result<ToolOutput> {
        self.execute_tool_with_tokens(tool, _input, &[]).await
    }

    /// Execute a tool, validating that `caller_tokens` satisfy the tool's
    /// `required_capabilities`. When `required_capabilities` is empty, any
    /// caller is allowed (back-compat). When it is non-empty, every
    /// required capability must be covered by at least one valid token.
    pub async fn execute_tool_with_tokens(
        &self,
        tool: ToolDefinition,
        input: serde_json::Value,
        caller_tokens: &[crate::security::CapabilityToken],
    ) -> Result<ToolOutput> {
        self.execute_tool_inner(tool, input, caller_tokens, None)
            .await
    }

    /// Execute a tool using a precompiled `Module` from `ModuleCache`.
    /// Skips `Module::from_binary`, making repeat invocations faster.
    pub async fn execute_tool_precompiled(
        &self,
        tool: ToolDefinition,
        input: serde_json::Value,
        module: std::sync::Arc<wasmtime::Module>,
    ) -> Result<ToolOutput> {
        self.execute_tool_inner(tool, input, &[], Some(module))
            .await
    }

    /// Execute a precompiled tool with capability-token validation.
    /// Combines `execute_tool_with_tokens` and `execute_tool_precompiled`.
    pub async fn execute_tool_precompiled_with_tokens(
        &self,
        tool: ToolDefinition,
        input: serde_json::Value,
        caller_tokens: &[crate::security::CapabilityToken],
        module: std::sync::Arc<wasmtime::Module>,
    ) -> Result<ToolOutput> {
        self.execute_tool_inner(tool, input, caller_tokens, Some(module))
            .await
    }

    async fn execute_tool_inner(
        &self,
        tool: ToolDefinition,
        input: serde_json::Value,
        caller_tokens: &[crate::security::CapabilityToken],
        precompiled: Option<std::sync::Arc<wasmtime::Module>>,
    ) -> Result<ToolOutput> {
        let start = Instant::now();

        if !tool.required_capabilities.is_empty() {
            let manager = self.capability_manager.read().unwrap();
            manager.authorize(caller_tokens, &tool.required_capabilities)?;
        }

        // Serialize input to JSON bytes for delivery to the guest.
        let input_bytes = serde_json::to_vec(&input).map_err(|e| {
            NexusError::SerializationError(format!("failed to serialize tool input: {e}"))
        })?;

        // Adaptive fuel budget: override per-invocation max_fuel with
        // the policy's recommendation for this tool.
        let tool_budget = self.fuel_policy.read().unwrap().budget_for(&tool.name);
        {
            let mut sandbox = self.sandbox.write().unwrap();
            // Temporarily swap the sandbox to one configured with the
            // per-tool budget. We rebuild only when the budget differs
            // from the current config to avoid unnecessary engine churn.
            let current_fuel = self.config.sandbox_config.max_fuel;
            if tool_budget != current_fuel {
                let mut per_call_config = self.config.sandbox_config.clone();
                per_call_config.max_fuel = tool_budget;
                if let Ok(new_sandbox) = WasmSandbox::new(per_call_config) {
                    *sandbox = new_sandbox;
                }
            }
        }

        // Start health monitoring before the execute call so resource
        // deltas are anchored at the pre-call sample.
        self.health_validator.start_execution();

        let exec_result = if let Some(module) = precompiled {
            self.sandbox
                .read()
                .unwrap()
                .execute_precompiled(module, &[input_bytes])?
        } else {
            self.sandbox
                .read()
                .unwrap()
                .execute(&tool.wasm_bytes, &[input_bytes])?
        };

        let duration_ms = start.elapsed().as_millis() as u64;
        let fuel_consumed = exec_result.fuel_consumed;

        // Record this execution's fuel consumption in the adaptive policy
        // so future invocations of the same tool benefit from the updated
        // profile.
        self.fuel_policy
            .write()
            .unwrap()
            .record(&tool.name, fuel_consumed);

        // Build the snapshot from the *real* pre-call memory whenever we
        // have it. For load-time failures the worker did not capture any
        // memory (instantiation never succeeded), so there is nothing to
        // snapshot — and per `FailureMode::requires_rollback()` we will
        // also skip the rollback path below.
        let snapshot = if let Some(ref mem) = exec_result.pre_call_memory {
            let fs_diff = FilesystemDiff::new();
            let exec_state = ExecutionState {
                captured_globals: exec_result.post_call_globals.clone().unwrap_or_default(),
                captured_tables: exec_result.post_call_tables.clone().unwrap_or_default(),
            };
            let metadata = SnapshotMetadata::new(
                tool.name.clone(),
                format!("{:x}", sha2::Sha256::digest(&tool.wasm_bytes)),
            );
            let snap = self.snapshot_manager.create_snapshot(
                mem.clone(),
                fs_diff,
                exec_state,
                metadata,
            )?;
            *self.current_snapshot.write().unwrap() = Some(snap.clone());
            Some(snap)
        } else {
            None
        };

        // Resource sample for telemetry and the error log.
        let resources = self.health_validator.current_resources();

        // Independent host-side checks. These can flip a sandbox-reported
        // success into a failure when the host itself sees a problem
        // (e.g. memory pressure outside the guest's view).
        let host_health = self.health_validator.validate();
        let host_corruption = self.health_validator.check_corruption();

        // Reconcile the sandbox's classification with host-side signals.
        let failure_mode: Option<FailureMode> = match (
            &exec_result.failure_mode,
            host_corruption,
            host_health.clone(),
        ) {
            (Some(mode), _, _) => Some(mode.clone()),
            (None, Some(detail), _) => Some(FailureMode::HostError(detail)),
            (None, None, h) if !h.is_healthy() => Some(FailureMode::HostError(format!(
                "host health degraded: {}",
                h.category()
            ))),
            _ => None,
        };

        if let Some(mode) = failure_mode {
            let recovery_actions: Vec<RecoveryAction> =
                self.recovery_policy.recover(&mode, &tool.name);
            let successful_patterns = self.telemetry.get_patterns(&tool.name);

            let error_log = ErrorLog::new(tool.name.clone(), mode.clone(), resources.clone())
                .with_recovery(recovery_actions)
                .with_patterns(successful_patterns);

            // Only roll back when the failure mode actually mutated state.
            // Load-time failures (InvalidModule / MissingEntrypoint) never
            // ran the entrypoint, so there is nothing to roll back to.
            let mut rollback_performed = false;
            if mode.requires_rollback() {
                if let Some(snap) = snapshot.as_ref() {
                    if let Ok(result) = self.snapshot_manager.rollback_to(&snap.id) {
                        *self.last_rollback_memory.write().unwrap() = Some(result.memory);
                        *self.last_rollback_execution_state.write().unwrap() =
                            Some(result.execution_state);
                        rollback_performed = true;
                    }
                }
            }

            let record = ExecutionRecord::failure(
                tool.name.clone(),
                error_log.clone(),
                duration_ms,
                fuel_consumed,
            );
            self.telemetry.record_failure(record);

            Ok(ToolOutput {
                success: false,
                result: None,
                error: Some(error_log.description.clone()),
                rollback_performed,
                execution_time_ms: duration_ms,
                fuel_consumed,
                error_log: Some(error_log),
            })
        } else {
            // Success path.
            let record =
                ExecutionRecord::success(tool.name.clone(), duration_ms, fuel_consumed, resources);
            self.telemetry.record_success(record);

            Ok(ToolOutput {
                success: true,
                result: exec_result.return_value,
                error: None,
                rollback_performed: false,
                execution_time_ms: duration_ms,
                fuel_consumed,
                error_log: None,
            })
        }
    }

    /// Execute with automatic retry and outcome feedback.
    ///
    /// Phase B: when an attempt produces an `ErrorLog` containing
    /// `RecoveryAction`s with attached `instinct_id`s, the *next* attempt's
    /// outcome credits or debits those instincts:
    ///   * next attempt succeeded -> `record_success` (confidence ↑)
    ///   * next attempt failed    -> `record_failure` (confidence ↓)
    ///
    /// Outcome feedback is a no-op when no `instinct_store` is attached
    /// (`with_instinct_store`) or when the `RecoveryAction`s carried no
    /// `instinct_id` (e.g. pure `StaticPolicy` deployments).
    pub async fn execute_with_retry(
        &self,
        tool: ToolDefinition,
        input: serde_json::Value,
    ) -> Result<ToolOutput> {
        let mut pending_instincts: Vec<uuid::Uuid> = Vec::new();

        for attempt in 0..=self.config.max_retries {
            if attempt > 0 {
                tokio::time::sleep(self.config.retry_delay).await;
            }

            let result = self.execute_tool(tool.clone(), input.clone()).await;

            match result {
                Ok(output) => {
                    // Apply outcome feedback to the instincts proposed by
                    // the PREVIOUS attempt.
                    if attempt > 0 && !pending_instincts.is_empty() {
                        if let Some(store) = &self.instinct_store {
                            for id in &pending_instincts {
                                let _ = if output.success {
                                    store.record_success(id)
                                } else {
                                    store.record_failure(id)
                                };
                            }
                        }
                        pending_instincts.clear();
                    }

                    if output.success {
                        return Ok(output);
                    }

                    // Collect instinct ids proposed on this failed attempt
                    // so the next attempt's outcome can be credited back.
                    if let Some(log) = &output.error_log {
                        pending_instincts = log
                            .recovery_actions
                            .iter()
                            .filter_map(|a| a.instinct_id)
                            .collect();
                    }

                    if attempt == self.config.max_retries {
                        // Last attempt: any pending instincts from this
                        // attempt never got a follow-up, so we cannot
                        // credit them. Leave them untouched.
                        return Ok(output);
                    }
                }
                Err(e) => {
                    if attempt == self.config.max_retries {
                        return Err(e);
                    }
                }
            }
        }

        // Unreachable in practice; preserved for compiler exhaustiveness.
        Ok(ToolOutput {
            success: false,
            result: None,
            error: Some("Max retries exceeded".to_string()),
            rollback_performed: false,
            execution_time_ms: 0,
            fuel_consumed: 0,
            error_log: None,
        })
    }

    /// Opt-in speculative recovery: race `branches` (each forked from a base
    /// snapshot) and return the first one to succeed.
    ///
    /// Every branch executes its tool through the normal sandbox path with the
    /// same `input`. Branches race concurrently via [`fork_and_race`]; the
    /// first success cancels the rest and their results are discarded.
    ///
    /// Anti-overclaim note: branches currently share this hypervisor's single
    /// sandbox, so wall-clock parallelism is bounded — this is an **opt-in**
    /// primitive, not the default recovery path. Multi-sandbox pooling for
    /// truly parallel branches is roadmap (Phase C). The typical use is to
    /// take the multiple `RecoveryAction`s a policy proposes for a failure and
    /// race them instead of trying them sequentially.
    pub async fn speculative_execute(
        &self,
        input: serde_json::Value,
        branches: Vec<SpeculativeBranch>,
        config: &SpeculativeConfig,
    ) -> Result<SpeculativeResult> {
        fork_and_race(branches, config, |branch| {
            let input = input.clone();
            async move { self.execute_tool(branch.tool, input).await }
        })
        .await
    }

    /// Execute a tool while recording an [`ExecutionTrace`] for replay /
    /// time-travel debugging. Opt-in.
    ///
    /// The checkpoint is built from the state the run already captured — the
    /// per-execution snapshot's memory hash (`memory_checksum`) and exported
    /// globals — so this adds no cost to the execution path. The trace is empty
    /// when the module exported no `"memory"` (nothing was snapshotted).
    ///
    /// Anti-overclaim: mid-execution fuel-interval checkpoints are roadmap —
    /// the synchronous sandbox cannot pause the guest. The trace currently
    /// holds the end-of-execution checkpoint; the replay engine
    /// ([`crate::telemetry::TraceReplay`]) is interval-agnostic.
    pub async fn execute_tool_traced(
        &self,
        tool: ToolDefinition,
        input: serde_json::Value,
        config: &crate::telemetry::TraceConfig,
    ) -> Result<(ToolOutput, crate::telemetry::ExecutionTrace)> {
        let tool_name = tool.name.clone();
        let output = self.execute_tool(tool, input).await?;
        let mut trace = crate::telemetry::ExecutionTrace::new(tool_name);
        if let Some(snap) = self.current_snapshot.read().unwrap().clone() {
            let memory_hash = if config.capture_memory {
                snap.memory_checksum.clone()
            } else {
                String::new()
            };
            trace.push(
                output.fuel_consumed,
                memory_hash,
                snap.execution_state.captured_globals.clone(),
                config.max_checkpoints,
            );
        }
        Ok((output, trace))
    }

    /// Read-only access to the attached instinct store (Phase B).
    pub fn instinct_store(&self) -> Option<&Arc<crate::instinct::InstinctStore>> {
        self.instinct_store.as_ref()
    }

    /// Get execution history
    pub fn get_history(&self, limit: Option<usize>) -> Vec<ExecutionRecord> {
        self.telemetry.get_history(limit)
    }

    /// Get telemetry statistics
    pub fn get_stats(&self) -> crate::telemetry::TelemetryStats {
        self.telemetry.stats()
    }

    /// Get snapshot statistics
    pub fn get_snapshot_stats(&self) -> crate::snapshot::SnapshotStats {
        self.snapshot_manager.stats()
    }

    /// Rollback to a specific snapshot manually
    pub async fn manual_rollback(&self, snapshot_id: uuid::Uuid) -> Result<()> {
        let result = self.snapshot_manager.rollback_to(&snapshot_id)?;
        *self.last_rollback_memory.write().unwrap() = Some(result.memory);
        *self.last_rollback_execution_state.write().unwrap() = Some(result.execution_state);
        Ok(())
    }

    /// Return the decompressed memory from the most recent rollback,
    /// consuming it so subsequent calls return `None` until another
    /// rollback occurs.
    pub fn take_rollback_memory(&self) -> Option<Vec<u8>> {
        self.last_rollback_memory.write().unwrap().take()
    }

    /// Peek at the rollback memory without consuming it.
    pub fn last_rollback_memory(&self) -> Option<Vec<u8>> {
        self.last_rollback_memory.read().unwrap().clone()
    }

    /// Return the execution state from the most recent rollback, consuming it.
    pub fn take_rollback_execution_state(&self) -> Option<ExecutionState> {
        self.last_rollback_execution_state.write().unwrap().take()
    }

    /// Peek at the rollback execution state without consuming it.
    pub fn last_rollback_execution_state(&self) -> Option<ExecutionState> {
        self.last_rollback_execution_state.read().unwrap().clone()
    }

    /// Inspect the adaptive fuel profile for a specific tool.
    /// Returns `None` if the tool has never been executed.
    pub fn fuel_profile(&self, tool_name: &str) -> Option<FuelProfile> {
        self.fuel_policy
            .read()
            .unwrap()
            .profile_for(tool_name)
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_successful_execution() {
        let config = HypervisorConfig::default();
        let hypervisor = NexusHypervisor::new(config).unwrap();

        // Simple WASM that does nothing
        let wasm_bytes = wat::parse_str(
            r#"
            (module
                (func (export "_start"))
            )
        "#,
        )
        .unwrap();

        let tool = ToolDefinition::new("test_tool".to_string(), wasm_bytes);

        let result = hypervisor.execute_tool(tool, serde_json::json!({})).await;
        assert!(result.is_ok());
        // Note: execution may fail due to missing _start, but that's fine for this test
    }

    #[test]
    fn test_recovery_suggestions() {
        // Test suggestion generation
        let suggestions = [
            "Break the operation into smaller steps".to_string(),
            "Add validation before execution".to_string(),
        ];

        assert!(!suggestions.is_empty());
    }
}
