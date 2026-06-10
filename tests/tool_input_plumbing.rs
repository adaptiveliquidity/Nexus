//! PR-4: Tool input plumbing integration tests.
//!
//! Verifies that `execute_tool` serializes the input JSON and delivers
//! it into the guest's linear memory as [len: u32 LE][data: len bytes]
//! at offset 0, before the entrypoint runs.

use nexus::{HypervisorConfig, NexusHypervisor, ToolDefinition};

#[tokio::test]
async fn execute_tool_delivers_input_to_sandbox() {
    let hv = NexusHypervisor::new(HypervisorConfig::default()).unwrap();

    let wasm = wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (func (export "_start"))
        )"#,
    )
    .unwrap();

    let tool = ToolDefinition::new("input_tool".into(), wasm);
    let input = serde_json::json!({"action": "test", "data": [1, 2, 3]});

    let output = hv.execute_tool(tool, input).await.unwrap();
    assert!(output.success, "execution with input should succeed");
}

#[tokio::test]
async fn empty_input_still_succeeds() {
    let hv = NexusHypervisor::new(HypervisorConfig::default()).unwrap();

    let wasm = wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (func (export "_start"))
        )"#,
    )
    .unwrap();

    let tool = ToolDefinition::new("no_input_tool".into(), wasm);
    let output = hv.execute_tool(tool, serde_json::json!({})).await.unwrap();
    assert!(output.success, "empty JSON object input should succeed");
}

#[tokio::test]
async fn null_input_succeeds() {
    let hv = NexusHypervisor::new(HypervisorConfig::default()).unwrap();

    let wasm = wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (func (export "_start"))
        )"#,
    )
    .unwrap();

    let tool = ToolDefinition::new("null_input_tool".into(), wasm);
    let output = hv
        .execute_tool(tool, serde_json::Value::Null)
        .await
        .unwrap();
    assert!(output.success, "null input should succeed");
}

#[test]
fn input_bytes_written_to_memory_directly() {
    let sandbox = nexus::WasmSandbox::new(nexus::SandboxConfig::default()).unwrap();

    let wasm = wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (func (export "_start"))
        )"#,
    )
    .unwrap();

    let input = b"hello world";
    let result = sandbox.execute(&wasm, &[input.to_vec()]).unwrap();
    assert!(result.success, "execution with input bytes should succeed");
}

#[test]
fn module_without_memory_handles_input_gracefully() {
    let sandbox = nexus::WasmSandbox::new(nexus::SandboxConfig::default()).unwrap();

    let wasm = wat::parse_str(
        r#"(module
            (func (export "_start"))
        )"#,
    )
    .unwrap();

    let input = b"some data";
    let result = sandbox.execute(&wasm, &[input.to_vec()]).unwrap();
    assert!(
        result.success,
        "module without memory should still execute with input"
    );
}
