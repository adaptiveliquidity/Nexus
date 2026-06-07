//! Nexus: AI-Native WASM Snap-Rollback Sandbox
//! 
//! Next-generation sandboxing infrastructure for AI agents with
//! microsecond snapshots, instant rollback, and self-correction.

pub mod error;
pub mod security;
pub mod sandbox;
pub mod snapshot;
pub mod telemetry;
pub mod hypervisor;
pub mod instinct;
pub mod daemon;

// Re-export commonly used types
pub use error::{NexusError, Result};
pub use security::{Capability, CapabilityManager, CapabilityToken};
pub use sandbox::{SandboxConfig, ExecutionResult, WasmSandbox, FuelMeter, FuelStats, WasmMemoryState, WasmExecutionSnapshot, MemoryStats};
pub use snapshot::{Snapshot, SnapshotManager, SnapshotStats, FilesystemDiff, ExecutionState, SnapshotMetadata};
pub use telemetry::{TelemetrySink, ExecutionRecord, TelemetryStats, LearnedPattern};
pub use hypervisor::{NexusHypervisor, HypervisorConfig, ToolDefinition, ToolOutput, FailureMode};
pub use instinct::{Instinct, InstinctPolicy, InstinctStats, InstinctStore};

/// Version information
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const NAME: &str = env!("CARGO_PKG_NAME");

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_version() {
        assert!(!VERSION.is_empty());
    }
}