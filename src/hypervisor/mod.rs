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

use chrono::Utc;
use ed25519_dalek::{SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use crate::error::{NexusError, Result};
use crate::hypervisor::validator::error_log::ErrorLog;
use crate::hypervisor::validator::health::{HealthConfig, HealthValidator};
use crate::proof::receipt::{ExecutionReceipt, FailureModeLite};
use crate::proof::schema::{
    CapabilityEvidence, FailureEvidence, InputIdentity, PolicyEnforcementMode, PolicyProfileRef,
    ProofCapsule, ProofSubject, RedactionReport, RollbackEvidence, SnapshotEvidence, SnapshotKind,
    ToolIdentity, TypedDigest,
};
use crate::proof::sign_capsule;
use crate::proof::signing::verifying_key_id;
use crate::proof::ProofSigningConfig;
use crate::sandbox::{
    FuelBudgetPolicy, FuelProfile, PoolConfig, RestoredExecutionState, SandboxConfig, SandboxPool,
    WasiToolConfig, WasmSandbox,
};
use crate::security::{Capability, CapabilityManager};
use crate::snapshot::{
    DiffSnapshotResult, ExecutionState, FilesystemDiff, RollbackResult, Snapshot, SnapshotManager,
    SnapshotMetadata,
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
    /// AEON-IQ tenant agent-id. Propagated into `ExecutionReceipt` so
    /// `capsule_from_receipt` can record the session namespace mapping (G2).
    #[cfg(feature = "aeon-memory")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aeon_agent_id: Option<String>,
    /// AEON-IQ session-id. See `aeon_agent_id`.
    #[cfg(feature = "aeon-memory")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aeon_session_id: Option<String>,
}

impl ToolDefinition {
    pub fn new(name: String, wasm_bytes: Vec<u8>) -> Self {
        ToolDefinition {
            name,
            wasm_bytes,
            entry_point: "_start".to_string(),
            input_schema: serde_json::json!({}),
            required_capabilities: Vec::new(),
            #[cfg(feature = "aeon-memory")]
            aeon_agent_id: None,
            #[cfg(feature = "aeon-memory")]
            aeon_session_id: None,
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

    /// Attach AEON-IQ session correlation. Both ids are optional; passing
    /// `None` for either leaves the field unset. The raw ids are never
    /// logged above `debug!` level.
    #[cfg(feature = "aeon-memory")]
    pub fn with_aeon_context(
        mut self,
        agent_id: Option<String>,
        session_id: Option<String>,
    ) -> Self {
        self.aeon_agent_id = agent_id;
        self.aeon_session_id = session_id;
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
    /// Runtime snapshot captured for this execution, when the WASM module
    /// exported linear memory and the hypervisor could capture it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<uuid::Uuid>,
}

/// Snapshot strategy used by the hypervisor execution path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotStrategy {
    /// Always create and roll back full snapshots. This is the default.
    Full,
    /// Create a full base snapshot first, then use differential snapshots for
    /// later executions until the snapshot manager promotes a chain.
    Differential,
}

/// Recovery policy stack selected by [`HypervisorConfig`].
#[derive(Debug, Clone, Default, PartialEq)]
pub enum RecoveryConfig {
    /// Use only the deterministic built-in policy. This is the default.
    #[default]
    Static,
    /// Route recovery through `LayeredPolicy` with the static layer.
    Layered,
    /// Route recovery through `LayeredPolicy([Static, Instinct])` and attach
    /// the same store for outcome feedback in `execute_with_retry`.
    LayeredInstinct {
        store_dir: std::path::PathBuf,
        min_confidence: f32,
    },
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
    /// Opt-in warm sandbox pool. When `Some`, `execute_tool` runs the WASM on
    /// a pooling-allocator engine with a shared compiled-module cache. When
    /// `None` (default), the original per-call `WasmSandbox` path is used —
    /// behavior is unchanged.
    pub pool_config: Option<PoolConfig>,
    /// Opt-in snapshot strategy. Defaults to full snapshots to preserve the
    /// existing execution and rollback behavior.
    pub snapshot_strategy: SnapshotStrategy,
    /// Opt-in recovery policy stack. Defaults to `StaticPolicy`.
    pub recovery_config: RecoveryConfig,
    /// Dedicated proof-capsule signing key source. Defaults to a fresh
    /// ephemeral key that is independent of the capability-token authority.
    pub proof_signing: ProofSigningConfig,
    /// Optional AEON-IQ memory configuration. Compiled only when
    /// `aeon-memory` is enabled; default builds do not carry this field.
    #[cfg(feature = "aeon-memory")]
    pub aeon_config: Option<crate::aeon::AeonConfig>,
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
            pool_config: None,
            snapshot_strategy: SnapshotStrategy::Full,
            recovery_config: RecoveryConfig::Static,
            proof_signing: ProofSigningConfig::default(),
            #[cfg(feature = "aeon-memory")]
            aeon_config: None,
        }
    }
}

#[derive(Debug, Clone)]
enum RuntimeSnapshot {
    Full(Box<Snapshot>),
    Diff(uuid::Uuid),
}

impl RuntimeSnapshot {
    fn id(&self) -> uuid::Uuid {
        match self {
            RuntimeSnapshot::Full(snapshot) => snapshot.id,
            RuntimeSnapshot::Diff(id) => *id,
        }
    }
}

fn wasm_pages_for_bytes(byte_len: usize) -> u32 {
    byte_len.div_ceil(65_536).min(u32::MAX as usize) as u32
}

type RecoverySelection = (
    Arc<dyn RecoveryPolicy>,
    Option<Arc<crate::instinct::InstinctStore>>,
);

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
    /// Opt-in warm sandbox pool. Built from `config.pool_config` when set.
    /// `None` preserves the original per-call execution path.
    pool: Option<Arc<SandboxPool>>,
    /// Latest full-or-diff snapshot id used as the next differential base.
    latest_runtime_snapshot: RwLock<Option<RuntimeSnapshot>>,
    /// Dedicated key for proof-capsule attestation, separate from capability
    /// token authorization.
    proof_signing_key: SigningKey,
    /// Fail-open AEON-IQ memory client for advisory capture. `None` means
    /// disabled or unconfigured, and all capture hooks no-op.
    #[cfg(feature = "aeon-memory")]
    aeon_memory: Option<crate::aeon::AeonMemoryClient>,
}

impl NexusHypervisor {
    /// Create a new hypervisor with the default `StaticPolicy` recovery
    /// policy. For custom policies use `new_with_policy`.
    pub fn new(config: HypervisorConfig) -> Result<Self> {
        let (policy, instinct_store) = Self::recovery_from_config(&config.recovery_config)?;
        let mut hv = Self::new_with_policy(config, policy)?;
        hv.instinct_store = instinct_store;
        Ok(hv)
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
        let proof_signing_key = config.proof_signing.signing_key()?;
        #[cfg(feature = "aeon-memory")]
        let aeon_memory = config
            .aeon_config
            .as_ref()
            .and_then(crate::aeon::AeonMemoryClient::from_enabled_config);

        let fuel_policy = FuelBudgetPolicy::new(sandbox_config.max_fuel);

        // Opt-in warm pool. Built once at construction so the pooling-allocator
        // engine and its module cache live for the hypervisor's lifetime.
        let pool = match &config.pool_config {
            Some(pc) => Some(Arc::new(SandboxPool::new(pc.clone())?)),
            None => None,
        };

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
            pool,
            latest_runtime_snapshot: RwLock::new(None),
            proof_signing_key,
            #[cfg(feature = "aeon-memory")]
            aeon_memory,
        })
    }

    fn recovery_from_config(recovery_config: &RecoveryConfig) -> Result<RecoverySelection> {
        match recovery_config {
            RecoveryConfig::Static => Ok((Arc::new(StaticPolicy::new()), None)),
            RecoveryConfig::Layered => Ok((
                Arc::new(LayeredPolicy::new(vec![Box::new(StaticPolicy::new())])),
                None,
            )),
            RecoveryConfig::LayeredInstinct {
                store_dir,
                min_confidence,
            } => {
                let store = Arc::new(crate::instinct::InstinctStore::open(store_dir.clone())?);
                let policy: Arc<dyn RecoveryPolicy> = Arc::new(LayeredPolicy::new(vec![
                    Box::new(StaticPolicy::new()),
                    Box::new(
                        crate::instinct::InstinctPolicy::new(store.clone())
                            .with_min_confidence(*min_confidence),
                    ),
                ]));
                Ok((policy, Some(store)))
            }
        }
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

    #[cfg(all(test, feature = "aeon-memory"))]
    fn with_aeon_memory_client_for_test(mut self, client: crate::aeon::AeonMemoryClient) -> Self {
        self.aeon_memory = Some(client);
        self
    }

    /// Returns true when self-correction is active (instinct store attached).
    pub fn self_correction_enabled(&self) -> bool {
        self.instinct_store.is_some()
    }

    /// Returns true when the opt-in warm sandbox pool is active.
    pub fn pool_enabled(&self) -> bool {
        self.pool.is_some()
    }

    /// Access the warm sandbox pool, if configured. Used by benchmarks and
    /// callers that want pool stats (cache hits, available permits).
    pub fn pool(&self) -> Option<&Arc<SandboxPool>> {
        self.pool.as_ref()
    }

    /// Return the configured snapshot strategy.
    pub fn snapshot_strategy(&self) -> SnapshotStrategy {
        self.config.snapshot_strategy
    }

    /// Return the configured recovery stack selector.
    pub fn recovery_config(&self) -> &RecoveryConfig {
        &self.config.recovery_config
    }

    /// Access the sandbox's wasmtime `Engine` for use with `ModuleCache`.
    pub fn sandbox_engine(&self) -> wasmtime::Engine {
        self.sandbox.read().unwrap().engine().clone()
    }

    pub fn snapshot_manager(&self) -> &Arc<SnapshotManager> {
        &self.snapshot_manager
    }

    /// Public verifying key for proof-capsule attestation.
    pub fn proof_verifying_key(&self) -> VerifyingKey {
        VerifyingKey::from(&self.proof_signing_key)
    }

    /// Hex key id for the public proof-capsule verifying key.
    pub fn proof_key_id(&self) -> String {
        verifying_key_id(&self.proof_verifying_key())
    }

    /// Public key bytes for the capability-token authority.
    pub fn capability_public_key(&self) -> Vec<u8> {
        self.capability_manager.read().unwrap().public_key()
    }

    fn create_runtime_snapshot(
        &self,
        memory: Vec<u8>,
        fs_diff: FilesystemDiff,
        exec_state: ExecutionState,
        metadata: SnapshotMetadata,
    ) -> Result<RuntimeSnapshot> {
        match self.config.snapshot_strategy {
            SnapshotStrategy::Full => {
                let snap = self
                    .snapshot_manager
                    .create_snapshot(memory, fs_diff, exec_state, metadata)?;
                *self.current_snapshot.write().unwrap() = Some(snap.clone());
                let runtime = RuntimeSnapshot::Full(Box::new(snap));
                *self.latest_runtime_snapshot.write().unwrap() = Some(runtime.clone());
                Ok(runtime)
            }
            SnapshotStrategy::Differential => {
                let base_id = self
                    .latest_runtime_snapshot
                    .read()
                    .unwrap()
                    .as_ref()
                    .map(RuntimeSnapshot::id);

                let runtime = if let Some(base_id) = base_id {
                    match self
                        .snapshot_manager
                        .create_diff_snapshot(memory, &base_id, exec_state, metadata)?
                    {
                        DiffSnapshotResult::Diff(diff) => RuntimeSnapshot::Diff(diff.id),
                        DiffSnapshotResult::Promoted(snap) => {
                            *self.current_snapshot.write().unwrap() = Some(snap.clone());
                            RuntimeSnapshot::Full(Box::new(snap))
                        }
                    }
                } else {
                    let snap = self
                        .snapshot_manager
                        .create_snapshot(memory, fs_diff, exec_state, metadata)?;
                    *self.current_snapshot.write().unwrap() = Some(snap.clone());
                    RuntimeSnapshot::Full(Box::new(snap))
                };

                *self.latest_runtime_snapshot.write().unwrap() = Some(runtime.clone());
                Ok(runtime)
            }
        }
    }

    fn rollback_runtime_snapshot(&self, snapshot: &RuntimeSnapshot) -> Result<()> {
        let result = match snapshot {
            RuntimeSnapshot::Full(snap) => self.snapshot_manager.rollback_to(&snap.id)?,
            RuntimeSnapshot::Diff(id) => self.snapshot_manager.rollback_to_diff(id)?,
        };
        self.cache_rollback_result(&result);
        Ok(())
    }

    fn cache_rollback_result(&self, result: &RollbackResult) {
        *self.last_rollback_memory.write().unwrap() = Some(result.memory.clone());
        *self.last_rollback_execution_state.write().unwrap() = Some(result.execution_state.clone());
    }

    /// Return the latest runtime snapshot id captured by an execution.
    ///
    /// This is empty until a normal execution path captures WASM linear
    /// memory. Stateless/manual snapshots created directly in the manager do
    /// not update this runtime slot.
    pub fn latest_runtime_snapshot_id(&self) -> Option<uuid::Uuid> {
        self.latest_runtime_snapshot
            .read()
            .unwrap()
            .as_ref()
            .map(RuntimeSnapshot::id)
    }

    /// Roll back to a full or differential snapshot id and cache the restored
    /// memory/execution state for callers that need to inspect it.
    pub fn rollback_snapshot(&self, snapshot_id: uuid::Uuid) -> Result<RollbackResult> {
        let result = match self.snapshot_manager.rollback_to(&snapshot_id) {
            Ok(result) => result,
            Err(full_err) => match self.snapshot_manager.rollback_to_diff(&snapshot_id) {
                Ok(result) => result,
                Err(diff_err) => {
                    return Err(NexusError::RollbackFailed(format!(
                        "snapshot {snapshot_id} was not restorable as a full or differential snapshot (full: {full_err}; diff: {diff_err})"
                    )));
                }
            },
        };
        self.cache_rollback_result(&result);
        Ok(result)
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

    /// Attenuate an existing capability token by ID using the hypervisor's
    /// capability manager.
    ///
    /// The returned child token is registered with the same manager, so it can
    /// be validated by subsequent execution calls and can itself be used as an
    /// attenuation parent subject to the manager's chain-depth limit.
    pub fn attenuate_token(
        &self,
        parent_id: uuid::Uuid,
        capability: Capability,
        granted_by: &str,
        validity: Duration,
    ) -> Result<crate::security::CapabilityToken> {
        let mut manager = self.capability_manager.write().unwrap();
        manager.attenuate(parent_id, capability, granted_by, validity)
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

    /// Execute a tool through the normal sandbox path and return an unsigned
    /// runtime proof capsule for the observed execution.
    ///
    /// This is the opt-in proof path. The existing `execute_tool` method is
    /// unchanged and does not construct proof artifacts.
    pub async fn execute_tool_proof(
        &self,
        tool: ToolDefinition,
        input: serde_json::Value,
    ) -> Result<(ToolOutput, ProofCapsule)> {
        let run_id = uuid::Uuid::new_v4();
        let started_at = Utc::now();
        let input_bytes = serde_json::to_vec(&input).map_err(|e| {
            NexusError::SerializationError(format!("failed to serialize tool input: {e}"))
        })?;
        let module_digest = TypedDigest::sha256_public(&tool.wasm_bytes);
        let input_digest = TypedDigest::sha256_public(&input_bytes);
        let required_caps: Vec<String> = tool
            .required_capabilities
            .iter()
            .map(Capability::description)
            .collect();

        let output = self.execute_tool(tool.clone(), input).await?;
        let finished_at = Utc::now();

        let snapshot = output
            .snapshot_id
            .map(|snapshot_id| self.snapshot_evidence(snapshot_id));
        let failure = Self::failure_lite(&output);
        let rollback = match (output.rollback_performed, output.snapshot_id) {
            (true, Some(snapshot_id)) => Some((
                true,
                snapshot_id,
                "restore latest runtime state after failed execution".to_string(),
            )),
            _ => None,
        };

        let receipt = ExecutionReceipt {
            run_id,
            started_at,
            finished_at,
            tool_name: tool.name.clone(),
            entrypoint: tool.entry_point.clone(),
            module_sha256: module_digest.value.clone(),
            input_sha256: input_digest.value.clone(),
            input_bytes_len: input_bytes.len(),
            required_caps,
            granted_caps: Vec::new(),
            policy_mode: PolicyEnforcementMode::UnprofiledDev,
            profile: None,
            snapshot,
            failure,
            rollback,
            branches: None,
            #[cfg(feature = "aeon-memory")]
            aeon_agent_id: tool.aeon_agent_id.clone(),
            #[cfg(feature = "aeon-memory")]
            aeon_session_id: tool.aeon_session_id.clone(),
            #[cfg(feature = "aeon-memory")]
            negotiation_rounds: None,
        };

        let capsule = Self::capsule_from_receipt(&receipt, &output);
        let capsule = sign_capsule(capsule, &self.proof_signing_key);
        Ok((output, capsule))
    }

    #[cfg(feature = "aeon-memory")]
    /// Execute a proof-producing tool with caller capability tokens and
    /// fail-open AEON-IQ denial negotiation.
    pub async fn execute_tool_proof_with_tokens(
        &self,
        mut tool: ToolDefinition,
        input: serde_json::Value,
        caller_tokens: &[crate::security::CapabilityToken],
    ) -> Result<(ToolOutput, ProofCapsule)> {
        let run_id = uuid::Uuid::new_v4();
        let started_at = Utc::now();
        let input_bytes = serde_json::to_vec(&input).map_err(|e| {
            NexusError::SerializationError(format!("failed to serialize tool input: {e}"))
        })?;
        let module_digest = TypedDigest::sha256_public(&tool.wasm_bytes);
        let input_digest = TypedDigest::sha256_public(&input_bytes);
        let mut effective_capabilities = tool.required_capabilities.clone();
        let mut negotiation_rounds = None;

        if !tool.required_capabilities.is_empty() {
            let authorization = {
                let manager = self.capability_manager.read().unwrap();
                manager.authorize(caller_tokens, &tool.required_capabilities)
            };

            if let Err(original_denial) = authorization {
                if !matches!(original_denial, NexusError::CapabilityDenied(_)) {
                    return Err(original_denial);
                }

                let aeon_context_available =
                    tool.aeon_agent_id.is_some() && tool.aeon_session_id.is_some();
                let outcome = match (self.aeon_memory.as_ref(), aeon_context_available) {
                    (Some(memory), true) => {
                        crate::security::negotiator::negotiate_capability_denial_with_authorizer(
                            &tool.required_capabilities,
                            memory,
                            |narrowed| {
                                let manager = self.capability_manager.read().unwrap();
                                manager.authorize(caller_tokens, narrowed).is_ok()
                            },
                        )
                        .await
                    }
                    _ => None,
                };

                match outcome {
                    Some(outcome) => {
                        debug_assert!(
                            outcome
                                .narrowed_capabilities
                                .iter()
                                .all(|capability| tool.required_capabilities.contains(capability)),
                            "negotiated capabilities must be drawn only from the original requirement set"
                        );
                        debug_assert!(
                            outcome.narrowed_capabilities.len() < tool.required_capabilities.len(),
                            "negotiated capabilities must strictly narrow the original requirement set"
                        );
                        effective_capabilities = outcome.narrowed_capabilities;
                        negotiation_rounds = Some(outcome.rounds);
                        tool.required_capabilities = effective_capabilities.clone();
                    }
                    None => return Err(original_denial),
                }
            }
        }

        let required_caps: Vec<String> = effective_capabilities
            .iter()
            .map(Capability::description)
            .collect();
        let output = self
            .execute_tool_inner(tool.clone(), input, caller_tokens, None, None)
            .await?;
        let finished_at = Utc::now();

        let snapshot = output
            .snapshot_id
            .map(|snapshot_id| self.snapshot_evidence(snapshot_id));
        let failure = Self::failure_lite(&output);
        let rollback = match (output.rollback_performed, output.snapshot_id) {
            (true, Some(snapshot_id)) => Some((
                true,
                snapshot_id,
                "restore latest runtime state after failed execution".to_string(),
            )),
            _ => None,
        };

        let receipt = ExecutionReceipt {
            run_id,
            started_at,
            finished_at,
            tool_name: tool.name.clone(),
            entrypoint: tool.entry_point.clone(),
            module_sha256: module_digest.value.clone(),
            input_sha256: input_digest.value.clone(),
            input_bytes_len: input_bytes.len(),
            required_caps: required_caps.clone(),
            granted_caps: required_caps,
            policy_mode: PolicyEnforcementMode::UnprofiledDev,
            profile: None,
            snapshot,
            failure,
            rollback,
            branches: None,
            aeon_agent_id: tool.aeon_agent_id.clone(),
            aeon_session_id: tool.aeon_session_id.clone(),
            negotiation_rounds,
        };

        let capsule = Self::capsule_from_receipt(&receipt, &output);
        let capsule = sign_capsule(capsule, &self.proof_signing_key);
        Ok((output, capsule))
    }

    fn snapshot_evidence(&self, snapshot_id: uuid::Uuid) -> SnapshotEvidence {
        if let Some(snapshot) = self.current_snapshot.read().unwrap().clone() {
            if snapshot.id == snapshot_id {
                return SnapshotEvidence {
                    snapshot_id,
                    snapshot_kind: SnapshotKind::LatestRuntime,
                    memory_digest: TypedDigest {
                        algorithm: "sha256".to_string(),
                        value: snapshot.memory_checksum,
                        public_recomputable: true,
                    },
                    original_size: snapshot.original_size as u64,
                    compressed_size: snapshot.compressed_size as u64,
                };
            }
        }

        SnapshotEvidence {
            snapshot_id,
            snapshot_kind: SnapshotKind::Diff,
            memory_digest: TypedDigest::redacted(),
            original_size: 0,
            compressed_size: 0,
        }
    }

    fn failure_lite(output: &ToolOutput) -> Option<FailureModeLite> {
        output
            .error_log
            .as_ref()
            .map(|log| FailureModeLite {
                category: log.failure_mode.category().to_string(),
                requires_rollback: log.failure_mode.requires_rollback(),
                is_deterministic: Some(log.failure_mode.is_deterministic()),
            })
            .or_else(|| {
                if output.success {
                    None
                } else {
                    Some(FailureModeLite {
                        category: "UNKNOWN".to_string(),
                        requires_rollback: output.rollback_performed,
                        is_deterministic: None,
                    })
                }
            })
    }

    fn capsule_from_receipt(receipt: &ExecutionReceipt, output: &ToolOutput) -> ProofCapsule {
        let required = receipt.required_caps.clone();
        let granted = receipt.granted_caps.clone();
        let mismatch = if required.is_empty() || required == granted {
            None
        } else {
            Some(required.clone())
        };

        ProofCapsule {
            version: "1".to_string(),
            capsule_id: uuid::Uuid::new_v4(),
            subject: ProofSubject {
                run_id: receipt.run_id,
                tool_name: receipt.tool_name.clone(),
                started_at: receipt.started_at,
                finished_at: receipt.finished_at,
                duration_ms: output.execution_time_ms,
            },
            tool: ToolIdentity {
                module_digest: TypedDigest {
                    algorithm: "sha256".to_string(),
                    value: receipt.module_sha256.clone(),
                    public_recomputable: true,
                },
                module_name: receipt.tool_name.clone(),
                entrypoint: receipt.entrypoint.clone(),
            },
            input: InputIdentity {
                digest: TypedDigest {
                    algorithm: "sha256".to_string(),
                    value: receipt.input_sha256.clone(),
                    public_recomputable: true,
                },
                media_type: "application/json".to_string(),
                raw_included: false,
            },
            policy: PolicyProfileRef {
                profile_digest: None,
                profile_name: receipt.profile.as_ref().map(|(name, _)| name.clone()),
                mode: receipt.policy_mode.clone(),
            },
            capabilities: CapabilityEvidence {
                required,
                granted,
                mismatch,
                #[cfg(feature = "aeon-memory")]
                negotiation_rounds: receipt.negotiation_rounds,
            },
            snapshot: receipt.snapshot.clone(),
            failure: receipt.failure.as_ref().map(|failure| FailureEvidence {
                failure_category: failure.category.clone(),
                requires_rollback: failure.requires_rollback,
                deterministic: failure.is_deterministic,
                error_summary: output
                    .error
                    .clone()
                    .unwrap_or_else(|| failure.category.clone()),
            }),
            rollback: receipt
                .rollback
                .as_ref()
                .map(|(occurred, snapshot_id, reason)| RollbackEvidence {
                    occurred: *occurred,
                    from_snapshot_id: Some(*snapshot_id),
                    reason: Some(reason.clone()),
                }),
            branches: receipt.branches.clone(),
            redaction: RedactionReport {
                hashed_fields: Vec::new(),
                truncated_fields: Vec::new(),
                removed_fields: Vec::new(),
                hmac_fields: Vec::new(),
            },
            limitations: Vec::new(),
            #[cfg(feature = "aeon-memory")]
            memory_evidence: None,
            #[cfg(feature = "aeon-memory")]
            memory_mode: match (&receipt.aeon_agent_id, &receipt.aeon_session_id) {
                (Some(_), Some(_)) => Some(crate::proof::schema::MemoryAttestationMode::Advisory),
                _ => None,
            },
            signature: None,
        }
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
        self.execute_tool_inner(tool, input, caller_tokens, None, None)
            .await
    }

    /// Execute a tool after restoring a captured runtime snapshot into a
    /// fresh branch instance before entrypoint execution.
    ///
    /// This is intentionally explicit so normal `execute_tool` calls remain
    /// from-scratch executions and callers cannot silently overclaim snapshot
    /// fork semantics.
    pub async fn execute_tool_from_snapshot(
        &self,
        snapshot_id: uuid::Uuid,
        tool: ToolDefinition,
        input: serde_json::Value,
    ) -> Result<ToolOutput> {
        self.execute_tool_from_snapshot_with_tokens(snapshot_id, tool, input, &[])
            .await
    }

    /// Execute a snapshot-seeded tool with capability-token validation.
    pub async fn execute_tool_from_snapshot_with_tokens(
        &self,
        snapshot_id: uuid::Uuid,
        tool: ToolDefinition,
        input: serde_json::Value,
        caller_tokens: &[crate::security::CapabilityToken],
    ) -> Result<ToolOutput> {
        if !tool.required_capabilities.is_empty() {
            let manager = self.capability_manager.read().unwrap();
            manager.authorize(caller_tokens, &tool.required_capabilities)?;
        }

        let rollback = self.rollback_snapshot(snapshot_id)?;
        let restored_state = RestoredExecutionState::from_rollback(&rollback);
        self.execute_tool_inner(tool, input, caller_tokens, None, Some(restored_state))
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
        self.execute_tool_inner(tool, input, &[], Some(module), None)
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
        self.execute_tool_inner(tool, input, caller_tokens, Some(module), None)
            .await
    }

    /// Execute a tool with WASI host imports, gated by capability tokens.
    ///
    /// Validated capabilities are mapped to WASI pre-opens (read-only or
    /// read-write directories) via `WasiSandboxConfig::from_capabilities`.
    pub async fn execute_tool_wasi(
        &self,
        tool: ToolDefinition,
        input: serde_json::Value,
        caller_tokens: &[crate::security::CapabilityToken],
    ) -> Result<ToolOutput> {
        let start = Instant::now();

        if !tool.required_capabilities.is_empty() {
            let manager = self.capability_manager.read().unwrap();
            manager.authorize(caller_tokens, &tool.required_capabilities)?;
        }

        let input_bytes = serde_json::to_vec(&input).map_err(|e| {
            NexusError::SerializationError(format!("failed to serialize tool input: {e}"))
        })?;

        let wasi_config =
            crate::sandbox::WasiSandboxConfig::from_capabilities(&tool.required_capabilities);

        self.health_validator.start_execution();

        let exec_result = self.sandbox.read().unwrap().execute_wasi(
            &tool.wasm_bytes,
            &[input_bytes],
            &wasi_config,
        )?;

        let duration_ms = start.elapsed().as_millis() as u64;
        let fuel_consumed = exec_result.fuel_consumed;

        self.fuel_policy
            .write()
            .unwrap()
            .record(&tool.name, fuel_consumed);

        let resources = self.health_validator.current_resources();

        if !exec_result.success {
            let mode = exec_result
                .failure_mode
                .clone()
                .unwrap_or_else(|| FailureMode::HostError("unknown WASI error".into()));

            let record = ExecutionRecord::failure(
                tool.name.clone(),
                crate::hypervisor::validator::error_log::ErrorLog::new(
                    tool.name.clone(),
                    mode.clone(),
                    resources,
                ),
                duration_ms,
                fuel_consumed,
            );
            self.telemetry.record_failure(record);

            Ok(ToolOutput {
                success: false,
                result: None,
                error: Some(mode.describe()),
                rollback_performed: false,
                execution_time_ms: duration_ms,
                fuel_consumed,
                error_log: None,
                snapshot_id: None,
            })
        } else {
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
                snapshot_id: None,
            })
        }
    }

    /// Execute a WASI tool with explicit host-to-guest mount aliases.
    ///
    /// Mount requirements are derived and authorized before post-authorization
    /// mount preparation. This is the public path used by proof-grade WASI
    /// demos and benchmark runners so filesystem access is always derived from
    /// caller-held capability tokens rather than ad hoc preopens.
    pub async fn execute_tool_wasi_with_config(
        &self,
        tool: ToolDefinition,
        input: serde_json::Value,
        caller_tokens: &[crate::security::CapabilityToken],
        wasi_tool_config: WasiToolConfig,
    ) -> Result<ToolOutput> {
        let start = Instant::now();

        let mut required_capabilities = tool.required_capabilities.clone();
        required_capabilities.extend(wasi_tool_config.required_capabilities()?);
        if !required_capabilities.is_empty() {
            let manager = self.capability_manager.read().unwrap();
            manager.authorize(caller_tokens, &required_capabilities)?;
        }

        let validated_config = wasi_tool_config.prepare_mounts()?;

        let input_bytes = serde_json::to_vec(&input).map_err(|e| {
            NexusError::SerializationError(format!("failed to serialize tool input: {e}"))
        })?;

        self.health_validator.start_execution();

        let exec_result = self.sandbox.read().unwrap().execute_wasi(
            &tool.wasm_bytes,
            &[input_bytes],
            &validated_config.sandbox_config,
        )?;

        let duration_ms = start.elapsed().as_millis() as u64;
        let fuel_consumed = exec_result.fuel_consumed;

        self.fuel_policy
            .write()
            .unwrap()
            .record(&tool.name, fuel_consumed);

        let resources = self.health_validator.current_resources();

        if !exec_result.success {
            let mode = exec_result
                .failure_mode
                .clone()
                .unwrap_or_else(|| FailureMode::HostError("unknown WASI error".into()));

            let record = ExecutionRecord::failure(
                tool.name.clone(),
                crate::hypervisor::validator::error_log::ErrorLog::new(
                    tool.name.clone(),
                    mode.clone(),
                    resources,
                ),
                duration_ms,
                fuel_consumed,
            );
            self.telemetry.record_failure(record);

            Ok(ToolOutput {
                success: false,
                result: None,
                error: Some(mode.describe()),
                rollback_performed: false,
                execution_time_ms: duration_ms,
                fuel_consumed,
                error_log: None,
                snapshot_id: None,
            })
        } else {
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
                snapshot_id: None,
            })
        }
    }

    async fn execute_tool_inner(
        &self,
        tool: ToolDefinition,
        input: serde_json::Value,
        caller_tokens: &[crate::security::CapabilityToken],
        precompiled: Option<std::sync::Arc<wasmtime::Module>>,
        restored_state: Option<RestoredExecutionState>,
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
        let input_hash = format!("{:x}", sha2::Sha256::digest(&input_bytes));
        let args = vec![input_bytes];

        // Adaptive fuel budget: override per-invocation max_fuel with
        // the policy's recommendation for this tool.
        let tool_budget = self.fuel_policy.read().unwrap().budget_for(&tool.name);
        // The adaptive per-tool fuel budget is applied to the default
        // per-call sandbox. The pool path uses its own fixed sandbox config,
        // so this swap is skipped when the pool is active.
        if self.pool.is_none() {
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

        let entry_point = tool.entry_point.clone();
        let exec_result = match (precompiled, restored_state) {
            (Some(module), Some(restored_state)) => self
                .sandbox
                .read()
                .unwrap()
                .execute_precompiled_from_restored_state_with_entry(
                    module,
                    &args,
                    restored_state,
                    &entry_point,
                )?,
            (Some(module), None) => self
                .sandbox
                .read()
                .unwrap()
                .execute_precompiled_with_entry(module, &args, &entry_point)?,
            (None, Some(restored_state)) => {
                if let Some(pool) = &self.pool {
                    pool.execute_pooled_from_restored_state_with_entry(
                        &tool.wasm_bytes,
                        &args,
                        restored_state,
                        &entry_point,
                    )
                    .await?
                } else {
                    self.sandbox
                        .read()
                        .unwrap()
                        .execute_from_restored_state_with_entry(
                            &tool.wasm_bytes,
                            &args,
                            restored_state,
                            &entry_point,
                        )?
                }
            }
            (None, None) => {
                if let Some(pool) = &self.pool {
                    // Opt-in warm pool: acquire a slot, run on the pooling-allocator
                    // engine with a cached compiled module. Isolation is preserved —
                    // each call still gets a fresh Store + Instance.
                    pool.execute_pooled_with_entry(&tool.wasm_bytes, &args, &entry_point)
                        .await?
                } else {
                    self.sandbox.read().unwrap().execute_with_entry(
                        &tool.wasm_bytes,
                        &args,
                        &entry_point,
                    )?
                }
            }
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
            let mut metadata = SnapshotMetadata::new(tool.name.clone(), input_hash);
            metadata.memory_pages = wasm_pages_for_bytes(mem.len());
            metadata.preconditions = tool
                .required_capabilities
                .iter()
                .map(|capability| format!("{capability:?}"))
                .collect();
            Some(self.create_runtime_snapshot(mem.clone(), fs_diff, exec_state, metadata)?)
        } else {
            None
        };
        let snapshot_id = snapshot.as_ref().map(RuntimeSnapshot::id);

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
                .with_patterns(successful_patterns)
                .with_call_stack(exec_result.call_stack.clone());

            // Only roll back when the failure mode actually mutated state.
            // Load-time failures (InvalidModule / MissingEntrypoint) never
            // ran the entrypoint, so there is nothing to roll back to.
            let mut rollback_performed = false;
            if mode.requires_rollback() {
                if let Some(snap) = snapshot.as_ref() {
                    match self.rollback_runtime_snapshot(snap) {
                        Ok(()) => rollback_performed = true,
                        Err(e) => tracing::warn!(
                            error = %e,
                            "rollback attempt failed; sandbox state may be dirty"
                        ),
                    }
                }
            }

            #[cfg(feature = "aeon-memory")]
            self.capture_aeon_memory(
                Self::aeon_failure_memory_content(
                    &error_log,
                    rollback_performed,
                    duration_ms,
                    fuel_consumed,
                    snapshot_id,
                ),
                Some(0.75),
                "failure",
            );

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
                snapshot_id,
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
                snapshot_id,
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
        #[cfg(feature = "aeon-memory")]
        let mut pending_instincts: Vec<(uuid::Uuid, String)> = Vec::new();
        #[cfg(not(feature = "aeon-memory"))]
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
                            #[cfg(not(feature = "aeon-memory"))]
                            {
                                for id in &pending_instincts {
                                    let _ = if output.success {
                                        store.record_success(id)
                                    } else {
                                        store.record_failure(id)
                                    };
                                }
                            }

                            #[cfg(feature = "aeon-memory")]
                            {
                                for (id, description) in &pending_instincts {
                                    let _ = if output.success {
                                        store.record_success(id)
                                    } else {
                                        store.record_failure(id)
                                    };
                                    self.capture_aeon_memory(
                                        Self::aeon_recovery_outcome_memory_content(
                                            &tool.name,
                                            attempt,
                                            id,
                                            description,
                                            output.success,
                                        ),
                                        Some(if output.success { 0.7 } else { 0.55 }),
                                        "recovery",
                                    );
                                }
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
                        #[cfg(feature = "aeon-memory")]
                        {
                            pending_instincts = log
                                .recovery_actions
                                .iter()
                                .filter_map(|a| a.instinct_id.map(|id| (id, a.description.clone())))
                                .collect();
                        }
                        #[cfg(not(feature = "aeon-memory"))]
                        {
                            pending_instincts = log
                                .recovery_actions
                                .iter()
                                .filter_map(|a| a.instinct_id)
                                .collect();
                        }
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
            snapshot_id: None,
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
            async move {
                self.execute_tool_from_snapshot(branch.base_snapshot_id, branch.tool, input)
                    .await
            }
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

    /// Record fuel-indexed checkpoints by bounded deterministic re-execution.
    ///
    /// This is an opt-in debugging primitive. It re-runs the same module with
    /// fuel caps of `interval`, `2 * interval`, ... until the guest completes
    /// or `max_checkpoints` is reached, then returns the captured timeline.
    ///
    /// The sandbox read lock is held for the entire trace to guarantee the
    /// engine configuration is stable across all re-executions. This is
    /// acceptable because `record_trace` is an offline debugging primitive,
    /// not a hot-path method.
    ///
    /// Anti-overclaim: this is O(N) re-execution over N checkpoints. A
    /// single-pass paused execution recorder remains a roadmap optimization.
    pub async fn record_trace(
        &self,
        tool: ToolDefinition,
        input: serde_json::Value,
        config: &crate::telemetry::TraceConfig,
    ) -> Result<crate::telemetry::ExecutionTrace> {
        let input_bytes = serde_json::to_vec(&input)
            .map_err(|e| NexusError::SerializationError(format!("trace input: {e}")))?;
        let mut trace = crate::telemetry::ExecutionTrace::new(tool.name.clone());
        let interval = config.checkpoint_interval_fuel.max(1);
        let sandbox = self.sandbox.read().unwrap();

        for k in 1..=(config.max_checkpoints as u64) {
            let cap = interval.saturating_mul(k);
            let step = sandbox.execute_to_fuel(
                &tool.wasm_bytes,
                std::slice::from_ref(&input_bytes),
                cap,
            )?;
            if let Some(mem) = &step.memory {
                let memory_hash = if config.capture_memory {
                    crate::telemetry::hash_memory(mem)
                } else {
                    String::new()
                };
                trace.push(
                    step.fuel_consumed,
                    memory_hash,
                    step.globals.clone(),
                    config.max_checkpoints,
                );
            }
            if step.completed {
                break;
            }
        }

        Ok(trace)
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
        self.rollback_snapshot(snapshot_id)?;
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

    #[cfg(feature = "aeon-memory")]
    fn capture_aeon_memory(
        &self,
        content: String,
        importance: Option<f32>,
        memory_type: &'static str,
    ) {
        let Some(client) = self.aeon_memory.clone() else {
            return;
        };

        tokio::spawn(async move {
            let _ = client.store(&content, importance, Some(memory_type)).await;
        });
    }

    #[cfg(feature = "aeon-memory")]
    fn aeon_failure_memory_content(
        error_log: &ErrorLog,
        rollback_performed: bool,
        duration_ms: u64,
        fuel_consumed: u64,
        snapshot_id: Option<uuid::Uuid>,
    ) -> String {
        let mut content = format!(
            "execution failure: operation={} category={} description={} rollback_performed={} duration_ms={} fuel_consumed={} snapshot_id={}",
            Self::aeon_safe_fragment(&error_log.operation, 128),
            error_log.failure_mode.category(),
            Self::aeon_safe_fragment(&error_log.description, 512),
            rollback_performed,
            duration_ms,
            fuel_consumed,
            snapshot_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "none".to_string())
        );

        let actions: Vec<String> = error_log
            .recovery_actions
            .iter()
            .take(3)
            .map(|action| Self::aeon_safe_fragment(&action.description, 256))
            .filter(|action| !action.is_empty())
            .collect();
        if !actions.is_empty() {
            content.push_str(" recovery_actions=");
            content.push_str(&actions.join(" | "));
        }

        content.chars().take(2_048).collect()
    }

    #[cfg(feature = "aeon-memory")]
    fn aeon_recovery_outcome_memory_content(
        operation: &str,
        attempt: u32,
        instinct_id: &uuid::Uuid,
        description: &str,
        success: bool,
    ) -> String {
        format!(
            "recovery outcome: operation={} attempt={} instinct_id={} outcome={} action={}",
            Self::aeon_safe_fragment(operation, 128),
            attempt,
            instinct_id,
            if success { "success" } else { "failure" },
            Self::aeon_safe_fragment(description, 512)
        )
    }

    #[cfg(feature = "aeon-memory")]
    fn aeon_safe_fragment(raw: &str, max_chars: usize) -> String {
        let cleaned: String = raw
            .chars()
            .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
            .take(max_chars)
            .collect();

        cleaned
            .split_whitespace()
            .map(Self::aeon_redact_token)
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[cfg(feature = "aeon-memory")]
    fn aeon_redact_token(token: &str) -> String {
        let lower = token.to_ascii_lowercase();
        let sensitive = lower.starts_with("sk-")
            || lower.contains("api_key")
            || lower.contains("apikey")
            || lower.contains("token=")
            || lower.contains("password")
            || lower.contains("secret")
            || lower.contains("authorization")
            || lower == "bearer";

        if sensitive {
            "[redacted]".to_string()
        } else {
            token.to_string()
        }
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

    #[cfg(feature = "aeon-memory")]
    type CapturedAeonRequests = Arc<std::sync::Mutex<Vec<crate::aeon::TestHttpRequest>>>;

    #[cfg(feature = "aeon-memory")]
    fn aeon_test_config(management_key: Option<&str>) -> crate::aeon::AeonConfig {
        crate::aeon::AeonConfig {
            enabled: true,
            base_url: "http://aeon.test".to_string(),
            agent_id: "agent-1".to_string(),
            session_id: None,
            timeout_ms: 30_000,
            management_key: management_key.map(str::to_string),
            hmac_key: None,
        }
    }

    #[cfg(feature = "aeon-memory")]
    fn aeon_capture_client(
        status: u16,
        management_key: Option<&str>,
    ) -> (crate::aeon::AeonMemoryClient, CapturedAeonRequests) {
        let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured_for_responder = Arc::clone(&captured);
        let client = crate::aeon::AeonMemoryClient::with_test_responder(
            &aeon_test_config(management_key),
            Arc::new(move |request| {
                captured_for_responder.lock().unwrap().push(request);
                crate::aeon::TestHttpResponse {
                    status,
                    body: r#"{"id":"550e8400-e29b-41d4-a716-446655440000"}"#.to_string(),
                }
            }),
        );
        (client, captured)
    }

    #[cfg(feature = "aeon-memory")]
    fn aeon_search_client(
        content: &'static str,
    ) -> (crate::aeon::AeonMemoryClient, CapturedAeonRequests) {
        let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured_for_responder = Arc::clone(&captured);
        let client = crate::aeon::AeonMemoryClient::with_test_responder(
            &aeon_test_config(Some("mgmt-key")),
            Arc::new(move |request| {
                captured_for_responder.lock().unwrap().push(request);
                crate::aeon::TestHttpResponse {
                    status: 200,
                    body: format!(
                        r#"{{"results":[{{"id":"mem-1","content":{content:?},"score":0.9}}]}}"#
                    ),
                }
            }),
        );
        (client, captured)
    }

    #[cfg(feature = "aeon-memory")]
    async fn wait_for_aeon_requests(captured: &CapturedAeonRequests, expected: usize) {
        for _ in 0..50 {
            if captured.lock().unwrap().len() >= expected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        panic!(
            "timed out waiting for {expected} AEON request(s); saw {}",
            captured.lock().unwrap().len()
        );
    }

    #[cfg(feature = "aeon-memory")]
    fn divzero_tool(name: &str) -> ToolDefinition {
        let wasm = wat::parse_str(
            r#"(module
                (memory (export "memory") 1)
                (func (export "_start")
                    i32.const 1 i32.const 0 i32.div_s drop))"#,
        )
        .unwrap();
        ToolDefinition::new(name.to_string(), wasm)
    }

    #[cfg(feature = "aeon-memory")]
    fn request_header<'a>(
        request: &'a crate::aeon::TestHttpRequest,
        name: &str,
    ) -> Option<&'a str> {
        request
            .headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    #[cfg(feature = "aeon-memory")]
    #[tokio::test]
    async fn execute_tool_proof_with_tokens_records_negotiation_rounds() {
        let allowed = Capability::ReadFile(std::path::PathBuf::from("/allowed"));
        let blocked = Capability::WriteFile(std::path::PathBuf::from("/blocked"));
        let (client, captured) = aeon_search_client("use read:/allowed only");
        let hv = NexusHypervisor::new(HypervisorConfig::default())
            .unwrap()
            .with_aeon_memory_client_for_test(client);
        let token = hv
            .issue_token(allowed.clone(), "test", Duration::from_secs(60))
            .unwrap();
        let wasm =
            wat::parse_str(r#"(module (memory (export "memory") 1) (func (export "_start")))"#)
                .unwrap();
        let tool = ToolDefinition::new("aeon_negotiated_proof".to_string(), wasm)
            .with_capabilities(vec![allowed.clone(), blocked])
            .with_aeon_context(Some("agent-1".to_string()), Some("session-1".to_string()));

        let (_output, capsule) = hv
            .execute_tool_proof_with_tokens(tool, serde_json::json!({}), &[token])
            .await
            .unwrap();

        assert_eq!(capsule.capabilities.required, vec![allowed.description()]);
        assert_eq!(capsule.capabilities.granted, vec![allowed.description()]);
        assert_eq!(capsule.capabilities.negotiation_rounds, Some(1));
        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].path, "/api/v1/memories/search");
    }

    #[cfg(feature = "aeon-memory")]
    #[tokio::test]
    async fn aeon_capture_posts_execution_failure_and_store_failure_is_fail_open() {
        let (client, captured) = aeon_capture_client(500, Some("mgmt-key"));
        let hv = NexusHypervisor::new(HypervisorConfig::default())
            .unwrap()
            .with_aeon_memory_client_for_test(client);

        let output = hv
            .execute_tool(divzero_tool("aeon_capture_failure"), serde_json::json!({}))
            .await
            .unwrap();

        assert!(!output.success);
        assert!(output.error_log.is_some());

        wait_for_aeon_requests(&captured, 1).await;
        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].path, "/api/v1/agents/agent-1/memories");
        assert_eq!(
            request_header(&requests[0], "x-management-key"),
            Some("mgmt-key")
        );

        let body: serde_json::Value = serde_json::from_str(&requests[0].body).unwrap();
        assert_eq!(body["memory_type"], "failure");
        assert!(body["content"]
            .as_str()
            .unwrap()
            .contains("operation=aeon_capture_failure"));
        assert!(body["content"]
            .as_str()
            .unwrap()
            .contains("category=TRAP_DIV_BY_ZERO"));
    }

    #[cfg(feature = "aeon-memory")]
    #[tokio::test]
    async fn aeon_capture_posts_recovery_outcome_after_retry_feedback() {
        let tmp = tempfile::tempdir().unwrap();
        let store =
            Arc::new(crate::instinct::InstinctStore::open(tmp.path().to_path_buf()).unwrap());
        store
            .register(
                &FailureMode::TrapDivByZero,
                "*",
                "instinct: validate divisor before division",
            )
            .unwrap();
        let policy: Arc<dyn RecoveryPolicy> = Arc::new(LayeredPolicy::new(vec![
            Box::new(StaticPolicy::new()),
            Box::new(crate::instinct::InstinctPolicy::new(store.clone())),
        ]));
        let (client, captured) = aeon_capture_client(200, Some("mgmt-key"));
        let cfg = HypervisorConfig {
            max_retries: 1,
            ..HypervisorConfig::default()
        };
        let hv = NexusHypervisor::new_with_policy(cfg, policy)
            .unwrap()
            .with_instinct_store(store)
            .with_aeon_memory_client_for_test(client);

        let output = hv
            .execute_with_retry(divzero_tool("aeon_retry_outcome"), serde_json::json!({}))
            .await
            .unwrap();

        assert!(!output.success);
        wait_for_aeon_requests(&captured, 3).await;
        let requests = captured.lock().unwrap();
        let recovery = requests
            .iter()
            .map(|request| serde_json::from_str::<serde_json::Value>(&request.body).unwrap())
            .find(|body| body["memory_type"] == "recovery")
            .expect("recovery outcome capture request");

        assert!(recovery["content"]
            .as_str()
            .unwrap()
            .contains("operation=aeon_retry_outcome"));
        assert!(recovery["content"]
            .as_str()
            .unwrap()
            .contains("outcome=failure"));
        assert!(recovery["content"]
            .as_str()
            .unwrap()
            .contains("validate divisor"));
    }

    #[cfg(feature = "aeon-memory")]
    #[tokio::test]
    async fn aeon_capture_missing_management_key_issues_no_request() {
        let (client, captured) = aeon_capture_client(200, None);
        let hv = NexusHypervisor::new(HypervisorConfig::default())
            .unwrap()
            .with_aeon_memory_client_for_test(client);

        let output = hv
            .execute_tool(divzero_tool("aeon_disabled_capture"), serde_json::json!({}))
            .await
            .unwrap();

        assert!(!output.success);
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert!(captured.lock().unwrap().is_empty());
    }
}
