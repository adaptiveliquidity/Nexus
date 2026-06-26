//! Nexus: AI-Native WASM Snap-Rollback Sandbox
//!
//! Next-generation sandboxing infrastructure for AI agents with
//! microsecond snapshots, instant rollback, and opt-in self-correction.

#[cfg(feature = "aeon-memory")]
pub mod aeon;
pub mod daemon;
pub mod error;
pub mod hypervisor;
pub mod instinct;
pub mod profile;
pub mod proof;
pub mod sandbox;
pub mod security;
pub mod snapshot;
pub mod telemetry;

// Re-export commonly used types
#[cfg(feature = "aeon-memory")]
pub use aeon::{
    AeonConfig, AeonMemoryClient, AeonTimelineSink, MemoryEvidenceV1, MemoryHit,
    TimelineDeliveryMode, TimelineDeliveryStatus, TimelineReplayReport,
};
pub use error::{NexusError, Result};
pub use hypervisor::{
    fork_and_race, BranchOutcome, FailureMode, HypervisorConfig, NexusHypervisor, RecoveryConfig,
    RecoverySource, SelectionStrategy, SnapshotStrategy, SpeculativeBranch, SpeculativeConfig,
    SpeculativeResult, ToolDefinition, ToolOutput,
};
pub use instinct::{Instinct, InstinctPolicy, InstinctStats, InstinctStore};
pub use sandbox::{
    ExecutionResult, FuelBudgetPolicy, FuelMeter, FuelProfile, FuelStats, MemoryStats, ModuleCache,
    PoolConfig, PooledModulePermit, PreOpen, SandboxConfig, SandboxPool, StepCapture, WasiAccess,
    WasiMount, WasiSandboxConfig, WasiToolConfig, WasmExecutionSnapshot, WasmMemoryState,
    WasmSandbox,
};
pub use security::{Capability, CapabilityManager, CapabilityToken};
pub use snapshot::{
    apply_diff, apply_diff_chain, compute_dirty_pages, DiffSnapshot, DiffSnapshotResult,
    ExecutionState, FilesystemDiff, Snapshot, SnapshotManager, SnapshotMetadata, SnapshotStats,
    PAGE_SIZE,
};
pub use telemetry::{
    CaptureSite, CapturedCallStack, ExecutionRecord, LearnedPattern, StackFrame, TelemetrySink,
    TelemetryStats,
};

/// Version information
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const NAME: &str = env!("CARGO_PKG_NAME");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version() {
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    }
}
