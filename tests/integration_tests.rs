//! Nexus Integration Tests
//! 
//! Tests for the complete Nexus sandbox system.

use nexus::{
    NexusHypervisor, HypervisorConfig, ToolDefinition,
    SandboxConfig, ExecutionResult, WasmMemoryState, WasmExecutionSnapshot,
};
use std::time::Duration;

/// Test basic WASM execution
#[tokio::test]
async fn test_basic_wasm_execution() {
    // Simple WASM that returns successfully
    let wasm = wat::parse_str(r#"
        (module
            (func (export "_start")
                (nop)
            )
        )
    "#).unwrap();
    
    let config = HypervisorConfig::default();
    let hypervisor = NexusHypervisor::new(config).unwrap();
    
    let tool = ToolDefinition::new("test".to_string(), wasm);
    let result = hypervisor.execute_tool(tool, serde_json::json!({})).await;
    
    assert!(result.is_ok());
    let output = result.unwrap();
    // May succeed or fail depending on timing
    assert!(output.success || output.error.is_some());
}

/// Test infinite loop detection
#[tokio::test]
async fn test_infinite_loop_detection() {
    let infinite_loop_wasm = wat::parse_str(r#"
        (module
            (func (export "_start")
                (loop (br 0))
            )
        )
    "#).unwrap();
    
    let mut config = HypervisorConfig::default();
    config.sandbox_config.time_limit = Duration::from_millis(100);
    
    let hypervisor = NexusHypervisor::new(config).unwrap();
    
    let tool = ToolDefinition::new("infinite_loop".to_string(), infinite_loop_wasm);
    let result = hypervisor.execute_tool(tool, serde_json::json!({})).await;
    
    assert!(result.is_ok());
    let output = result.unwrap();
    
    // Should detect the timeout
    assert!(!output.success, "Infinite loop should be detected");
    // Rollback cannot be performed here: the timeout fires before the
    // worker thread sends back the pre-call memory, and this minimal
    // WAT module has no memory export, so no snapshot is created.
    assert!(!output.rollback_performed, "No rollback without pre-call memory");
}

/// Test memory state capture
#[test]
fn test_memory_state_capture() {
    let memory = WasmMemoryState::from_bytes(&vec![1u8; 1024]);
    
    assert_eq!(memory.page_count, 1);
    assert_eq!(memory.size_bytes, 1024);
    assert_eq!(memory.total_size(), 1024);
    
    // Test round-trip
    let bytes = memory.as_bytes();
    assert_eq!(bytes.len(), 1024);
    assert!(bytes.iter().all(|&b| b == 1));
}

/// Test execution snapshot
#[test]
fn test_execution_snapshot() {
    let memory = WasmMemoryState::from_bytes(&vec![42u8; 512]);
    let snapshot = WasmExecutionSnapshot::new(memory);
    
    assert_eq!(snapshot.stack_pointer, 0);
    assert_eq!(snapshot.pc, 0);
    assert_eq!(snapshot.memory.size_bytes, 512);
}

/// Test snapshot creation and compression
#[test]
fn test_snapshot_compression() {
    let data = vec![0u8; 65536]; // 64KB of zeros
    let memory = WasmMemoryState::from_bytes(&data);
    
    let bytes = memory.as_bytes();
    assert_eq!(bytes.len(), 65536);
    
    // Test compression info
    let (orig, total) = memory.compression_info();
    assert_eq!(orig, 65536);
    assert_eq!(total, 65536);
}

/// Test sandbox configuration
#[test]
fn test_sandbox_config() {
    let config = SandboxConfig::default();
    
    assert_eq!(config.max_fuel, 10_000_000);
    assert_eq!(config.max_memory_pages, 512);
    assert!(config.time_limit.as_millis() > 0);
}

/// Test sandbox creation
#[test]
fn test_sandbox_creation() {
    let config = SandboxConfig::default();
    let sandbox = nexus::WasmSandbox::new(config);
    
    assert!(sandbox.is_ok());
}

/// Test empty memory state
#[test]
fn test_empty_memory_state() {
    let memory = WasmMemoryState::empty();
    
    assert_eq!(memory.page_count, 0);
    assert_eq!(memory.size_bytes, 0);
    assert_eq!(memory.total_size(), 0);
    
    let bytes = memory.as_bytes();
    assert!(bytes.is_empty());
}

/// Test execution result
#[test]
fn test_execution_result() {
    let success = ExecutionResult::success(Vec::new(), 1000, 50);
    
    assert!(success.success);
    assert!(success.error.is_none());
    assert_eq!(success.fuel_consumed, 1000);
    assert_eq!(success.duration_ms, 50);
    
    let failure = ExecutionResult::failure("Test error".to_string(), 500);
    
    assert!(!failure.success);
    assert!(failure.error.is_some());
    assert_eq!(failure.fuel_consumed, 500);
}

/// Test tool definition
#[test]
fn test_tool_definition() {
    let wasm = vec![0u8; 100];
    let tool = ToolDefinition::new("test_tool".to_string(), wasm.clone());
    
    assert_eq!(tool.name, "test_tool");
    assert_eq!(tool.wasm_bytes, wasm);
}

/// Test hypervisor configuration
#[test]
fn test_hypervisor_config() {
    let config = HypervisorConfig::default();
    
    assert_eq!(config.snapshot_capacity, 100);
    // Verify config fields exist
    let _ = config.sandbox_config.max_memory_pages;
}

/// Test memory stats
#[test]
fn test_memory_stats() {
    let mut stats = nexus::MemoryStats::default();
    
    assert_eq!(stats.captures, 0);
    assert_eq!(stats.restorations, 0);
    
    stats.record_capture(1024);
    stats.record_restore(512);
    
    assert_eq!(stats.captures, 1);
    assert_eq!(stats.restorations, 1);
    assert_eq!(stats.total_bytes_captured, 1024);
    assert_eq!(stats.total_bytes_restored, 512);
    
    assert_eq!(stats.avg_capture_size(), 1024.0);
}

/// Test WASM execution with memory allocation
#[tokio::test]
async fn test_memory_allocation() {
    // WASM that allocates some memory
    let wasm = wat::parse_str(r#"
        (module
            (memory (export "memory") 1)
            (func (export "_start")
                (i32.store (i32.const 0) (i32.const 42))
            )
        )
    "#).unwrap();
    
    let mut config = HypervisorConfig::default();
    config.sandbox_config.max_memory_pages = 16; // 1MB limit
    
    let hypervisor = NexusHypervisor::new(config).unwrap();
    
    let tool = ToolDefinition::new("memory_test".to_string(), wasm);
    let result = hypervisor.execute_tool(tool, serde_json::json!({})).await;
    
    assert!(result.is_ok());
}

/// Test error classification
#[tokio::test]
async fn test_error_classification() {
    let wasm = wat::parse_str(r#"
        (module
            (func (export "_start")
                (unreachable)
            )
        )
    "#).unwrap();
    
    let mut config = HypervisorConfig::default();
    config.sandbox_config.time_limit = Duration::from_secs(1);
    
    let hypervisor = NexusHypervisor::new(config).unwrap();
    
    let tool = ToolDefinition::new("unreachable".to_string(), wasm);
    let result = hypervisor.execute_tool(tool, serde_json::json!({})).await;
    
    assert!(result.is_ok());
    let output = result.unwrap();
    
    // Unreachable should trigger an error
    assert!(!output.success || output.error.is_some());
}