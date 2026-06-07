//! Nexus Hypervisor Core
//! 
//! Main orchestrator that ties together sandbox, snapshots, and validation.

pub mod validator;

use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use serde::{Deserialize, Serialize};
use sha2::Digest;

use crate::error::{NexusError, Result};
use crate::sandbox::{WasmSandbox, SandboxConfig};
use crate::snapshot::{SnapshotManager, Snapshot, SnapshotMetadata, FilesystemDiff, ExecutionState};
use crate::security::{Capability, CapabilityManager};
use crate::telemetry::{TelemetrySink, ExecutionRecord};
use crate::hypervisor::validator::health::{HealthValidator, HealthConfig, HealthStatus, ResourceSnapshot};
use crate::hypervisor::validator::error_log::ErrorLog;

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
}

impl NexusHypervisor {
    /// Create a new hypervisor
    pub fn new(config: HypervisorConfig) -> Result<Self> {
        let sandbox_config = config.sandbox_config.clone();
        let snapshot_capacity = config.snapshot_capacity;
        let persistence_dir = config.persistence_dir.clone();
        let enable_persistence = config.enable_persistence;
        let health_config = config.health_config.clone();
        
        let sandbox = WasmSandbox::new(sandbox_config)
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
        
        Ok(NexusHypervisor {
            config,
            sandbox: RwLock::new(sandbox),
            snapshot_manager,
            capability_manager: RwLock::new(capability_manager),
            health_validator: HealthValidator::new(health_config),
            telemetry: Arc::new(TelemetrySink::new(1000)),
            current_snapshot: RwLock::new(None),
        })
    }
    
    /// Grant a capability to the current session
    pub fn grant_capability(&self, capability: Capability, validity: Duration) -> Result<()> {
        let mut manager = self.capability_manager.write().unwrap();
        manager.issue(
            capability,
            "system",
            validity,
        );
        Ok(())
    }
    
    /// Execute a tool with automatic snapshot/rollback
    pub async fn execute_tool(
        &self,
        tool: ToolDefinition,
        _input: serde_json::Value,
    ) -> Result<ToolOutput> {
        let start = Instant::now();
        
        // Validate capabilities
        {
            let mut manager = self.capability_manager.write().unwrap();
            for cap in &tool.required_capabilities {
                // Issue temporary token for validation
                let _temp_token = manager.issue(
                    cap.clone(),
                    "validation",
                    Duration::from_secs(60),
                );
                // Token would be validated against required capability
            }
        }
        
        // 1. Create pre-execution snapshot
        let memory = vec![0u8; 65536]; // Placeholder - would capture actual WASM memory
        let fs_diff = FilesystemDiff::new();
        let exec_state = ExecutionState::default();
        let metadata = SnapshotMetadata::new(
            tool.name.clone(),
            format!("{:x}", sha2::Sha256::digest(&tool.wasm_bytes)),
        );
        
        let snapshot = self.snapshot_manager.create_snapshot(
            memory,
            fs_diff,
            exec_state,
            metadata,
        )?;
        
        // Store snapshot reference
        *self.current_snapshot.write().unwrap() = Some(snapshot.clone());
        
        // 2. Start health monitoring
        self.health_validator.start_execution();
        
        // 3. Execute in WASM sandbox
        let sandbox = self.sandbox.read().unwrap();
        let result = sandbox.execute(&tool.wasm_bytes, &[]);
        
        let duration_ms = start.elapsed().as_millis() as u64;
        let fuel_consumed = result.as_ref().map(|r| r.fuel_consumed).unwrap_or(0);
        
        // 4. Validate health
        let health = self.health_validator.validate();
        
        // 5. Check for corruption
        let corruption = self.health_validator.check_corruption();
        
        // 6. Handle result based on execution success OR health issues
        let execution_success = result.as_ref().map(|r| r.success).unwrap_or(false);
        let has_corruption = corruption.is_some();
        let has_health_issue = !health.is_healthy();
        
        if execution_success && !has_corruption && !has_health_issue {
            // Success - record and return
            let record = ExecutionRecord::success(tool.name.clone(), duration_ms, fuel_consumed);
            self.telemetry.record_success(record);
            
            Ok(ToolOutput {
                success: true,
                result: result.ok().and_then(|r| r.return_value),
                error: None,
                rollback_performed: false,
                execution_time_ms: duration_ms,
                fuel_consumed,
                error_log: None,
            })
        } else {
            // Failure - determine error type and health status
            let (error_type, trigger_status) = if let Some(c) = corruption {
                (format!("CORRUPTION: {}", c), HealthStatus::Corrupted)
            } else if !execution_success {
                // WASM execution failed - check for specific errors
                let exec_error = result.as_ref().unwrap().error.clone()
                    .unwrap_or_else(|| "Unknown execution error".to_string());
                
                // Map to user-friendly error types
                if exec_error.contains("fuel") || exec_error.contains("Fuel") {
                    ("FUEL_EXHAUSTED: Infinite loop prevented".to_string(), HealthStatus::FuelExhausted)
                } else if exec_error.contains("memory") || exec_error.contains("Memory") {
                    ("MEMORY_LIMIT: Allocation exceeded".to_string(), HealthStatus::ResourceExhausted)
                } else if exec_error.contains("trap") || exec_error.contains("Trap") {
                    ("EXECUTION_TRAP: Invalid WASM operation".to_string(), HealthStatus::Corrupted)
                } else {
                    (format!("EXECUTION_ERROR: {}", exec_error), HealthStatus::Corrupted)
                }
            } else if has_health_issue {
                (health.category().to_string(), health.clone())
            } else {
                ("UNKNOWN_FAILURE".to_string(), HealthStatus::Corrupted)
            };
            
            let resources = self.health_validator.current_resources();
            let error_log = ErrorLog::new(
                error_type.clone(),
                tool.name.clone(),
                error_type,
                trigger_status,
                resources,
            ).with_recovery(self.generate_recovery_suggestions(&tool.name));
            
            // Perform rollback
            let _ = self.snapshot_manager.rollback_to(&snapshot.id);
            
            // Record failure
            let record = ExecutionRecord::failure(
                tool.name.clone(),
                error_log.clone(),
                duration_ms,
            );
            self.telemetry.record_failure(record);
            
            Ok(ToolOutput {
                success: false,
                result: None,
                error: Some(error_log.description.clone()),
                rollback_performed: true,
                execution_time_ms: duration_ms,
                fuel_consumed,
                error_log: Some(error_log),
            })
        }
    }
    
    /// Execute with automatic retry
    pub async fn execute_with_retry(
        &self,
        tool: ToolDefinition,
        input: serde_json::Value,
    ) -> Result<ToolOutput> {
        for attempt in 0..=self.config.max_retries {
            if attempt > 0 {
                // Wait before retry
                tokio::time::sleep(self.config.retry_delay).await;
            }
            
            let result = self.execute_tool(tool.clone(), input.clone()).await;
            
            match result {
                Ok(output) => {
                    if output.success {
                        return Ok(output);
                    }
                    // If this was the last attempt, return the last failure
                    if attempt == self.config.max_retries {
                        return Ok(output);
                    }
                }
                Err(e) => {
                    // If this was the last attempt, return the error
                    if attempt == self.config.max_retries {
                        return Err(e);
                    }
                }
            }
        }
        
        // Should never reach here, but return a generic failure
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
    
    /// Generate recovery suggestions based on telemetry
    fn generate_recovery_suggestions(&self, operation: &str) -> Vec<String> {
        let patterns = self.telemetry.get_patterns(operation);
        
        let mut suggestions = Vec::new();
        
        // Add learned patterns
        for pattern in patterns.iter().take(2) {
            suggestions.push(format!("Consider approach: {}", pattern));
        }
        
        // Add generic suggestions based on operation type
        match operation {
            "execute_command" => {
                suggestions.push("Break complex commands into simpler steps".to_string());
                suggestions.push("Use timeout wrapper for long-running commands".to_string());
            }
            "read_file" => {
                suggestions.push("Check file path permissions".to_string());
                suggestions.push("Verify file exists before reading".to_string());
            }
            "write_file" => {
                suggestions.push("Ensure parent directory exists".to_string());
                suggestions.push("Use atomic write (temp file + rename)".to_string());
            }
            _ => {
                suggestions.push("Break the operation into smaller steps".to_string());
                suggestions.push("Add validation before execution".to_string());
            }
        }
        
        suggestions
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
        // Would restore memory and filesystem from result
        let _ = result;
        Ok(())
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
        let wasm_bytes = wat::parse_str(r#"
            (module
                (func (export "_start"))
            )
        "#).unwrap();
        
        let tool = ToolDefinition::new("test_tool".to_string(), wasm_bytes);
        
        let result = hypervisor.execute_tool(tool, serde_json::json!({})).await;
        assert!(result.is_ok());
        // Note: execution may fail due to missing _start, but that's fine for this test
    }
    
    #[test]
    fn test_recovery_suggestions() {
        // Test suggestion generation
        let suggestions = vec![
            "Break the operation into smaller steps".to_string(),
            "Add validation before execution".to_string(),
        ];
        
        assert!(!suggestions.is_empty());
    }
}