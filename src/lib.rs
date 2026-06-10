//! Nexus: AI-Native WASM Snap-Rollback Sandbox
//!
//! Next-generation sandboxing infrastructure for AI agents with
//! microsecond snapshots, instant rollback, and opt-in self-correction.

pub mod daemon;
pub mod error;
pub mod hypervisor;
pub mod instinct;
pub mod sandbox;
pub mod security;
pub mod snapshot;
pub mod telemetry;

// Re-export commonly used types
pub use error::{NexusError, Result};
pub use hypervisor::{FailureMode, HypervisorConfig, NexusHypervisor, ToolDefinition, ToolOutput};
pub use instinct::{Instinct, InstinctPolicy, InstinctStats, InstinctStore};
pub use sandbox::{
    ExecutionResult, FuelMeter, FuelStats, MemoryStats, SandboxConfig, WasmExecutionSnapshot,
    WasmMemoryState, WasmSandbox,
};
pub use security::{Capability, CapabilityManager, CapabilityToken};
pub use snapshot::{
    ExecutionState, FilesystemDiff, Snapshot, SnapshotManager, SnapshotMetadata, SnapshotStats,
};
pub use telemetry::{ExecutionRecord, LearnedPattern, TelemetrySink, TelemetryStats};

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
