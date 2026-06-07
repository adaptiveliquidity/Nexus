//! Nexus Error Types
//! 
//! Comprehensive error handling for the snap-rollback sandbox system.

use thiserror::Error;
use serde::{Serialize, Deserialize};

/// Core error types for Nexus operations
#[derive(Error, Debug, Clone, Serialize, Deserialize)]
pub enum NexusError {
    /// WASM runtime errors
    #[error("WASM execution error: {0}")]
    WasmError(String),
    
    /// Snapshot creation failed
    #[error("Failed to create snapshot: {0}")]
    SnapshotCreateFailed(String),
    
    /// Rollback to previous state failed
    #[error("Rollback failed: {0}")]
    RollbackFailed(String),
    
    /// Execution timeout exceeded
    #[error("Execution timeout after {0}ms")]
    Timeout(u64),
    
    /// Fuel exhausted (infinite loop prevented)
    #[error("Fuel exhausted: execution exceeded {0} instructions")]
    FuelExhausted(u64),
    
    /// Memory limit exceeded
    #[error("Memory limit exceeded: {0} bytes")]
    MemoryLimitExceeded(u64),
    
    /// State corruption detected
    #[error("State corruption detected: {0}")]
    StateCorruption(String),
    
    /// Capability token invalid or expired
    #[error("Invalid capability token: {0}")]
    InvalidCapability(String),
    
    /// Filesystem operation failed
    #[error("Filesystem error: {0}")]
    FilesystemError(String),
    
    /// Health validation failed
    #[error("Health validation failed: {0}")]
    HealthValidationFailed(String),
    
    /// Resource exhaustion
    #[error("Resource exhausted: {0}")]
    ResourceExhausted(String),
    
    /// Serialization error
    #[error("Serialization error: {0}")]
    SerializationError(String),
    
    /// Configuration error
    #[error("Configuration error: {0}")]
    ConfigError(String),
}

impl NexusError {
    /// Check if this error type supports automatic recovery
    pub fn is_recoverable(&self) -> bool {
        matches!(
            self,
            NexusError::Timeout(_) 
            | NexusError::FuelExhausted(_) 
            | NexusError::StateCorruption(_)
            | NexusError::HealthValidationFailed(_)
        )
    }

    /// Get error category for telemetry
    pub fn category(&self) -> &'static str {
        match self {
            NexusError::WasmError(_) => "WASM_EXECUTION",
            NexusError::SnapshotCreateFailed(_) => "SNAPSHOT_FAILURE",
            NexusError::RollbackFailed(_) => "ROLLBACK_FAILURE",
            NexusError::Timeout(_) => "TIMEOUT",
            NexusError::FuelExhausted(_) => "INFINITE_LOOP_PREVENTED",
            NexusError::MemoryLimitExceeded(_) => "MEMORY_EXCEEDED",
            NexusError::StateCorruption(_) => "STATE_CORRUPTION",
            NexusError::InvalidCapability(_) => "SECURITY_VIOLATION",
            NexusError::FilesystemError(_) => "FILESYSTEM_ERROR",
            NexusError::HealthValidationFailed(_) => "HEALTH_FAILURE",
            NexusError::ResourceExhausted(_) => "RESOURCE_EXHAUSTED",
            NexusError::SerializationError(_) => "SERIALIZATION_ERROR",
            NexusError::ConfigError(_) => "CONFIG_ERROR",
        }
    }
}

/// Result type for Nexus operations
pub type Result<T> = std::result::Result<T, NexusError>;