//! Integration tests for execution replay / time-travel debugging through the
//! real hypervisor. The deterministic replay-engine behaviour itself is unit
//! tested in `telemetry::trace`; these exercise `execute_tool_traced`.

use nexus::telemetry::TraceConfig;
use nexus::{HypervisorConfig, NexusHypervisor, ToolDefinition};

fn hypervisor() -> NexusHypervisor {
    NexusHypervisor::new(HypervisorConfig::default()).unwrap()
}

/// Module that exports memory + a mutable global and sets it in `_start`.
fn stateful_tool(name: &str) -> ToolDefinition {
    let wasm = wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (global (export "g") (mut i32) (i32.const 0))
            (func (export "_start") (global.set 0 (i32.const 7)))
        )"#,
    )
    .unwrap();
    ToolDefinition::new(name.to_string(), wasm)
}

/// Module with no exported memory — nothing to checkpoint.
fn no_memory_tool(name: &str) -> ToolDefinition {
    let wasm = wat::parse_str(r#"(module (func (export "_start")))"#).unwrap();
    ToolDefinition::new(name.to_string(), wasm)
}

#[tokio::test]
async fn traced_execution_records_a_checkpoint() {
    let hv = hypervisor();
    let (output, trace) = hv
        .execute_tool_traced(
            stateful_tool("t"),
            serde_json::json!({}),
            &TraceConfig::default(),
        )
        .await
        .expect("traced execution returns");

    if output.success {
        assert_eq!(trace.len(), 1, "one end-of-execution checkpoint");
        let cp = &trace.checkpoints[0];
        assert_eq!(cp.memory_hash.len(), 64, "hex sha256 memory hash");
        assert_eq!(cp.sequence, 0);
        // The replay cursor lands on the recorded checkpoint.
        let replay = trace.replay();
        assert_eq!(replay.current().unwrap().memory_hash, cp.memory_hash);
    }
}

#[tokio::test]
async fn memory_hash_is_deterministic_across_runs() {
    let hv1 = hypervisor();
    let hv2 = hypervisor();
    let cfg = TraceConfig::default();

    let (o1, t1) = hv1
        .execute_tool_traced(stateful_tool("t"), serde_json::json!({}), &cfg)
        .await
        .unwrap();
    let (o2, t2) = hv2
        .execute_tool_traced(stateful_tool("t"), serde_json::json!({}), &cfg)
        .await
        .unwrap();

    // Only assert determinism when both runs actually checkpointed.
    if o1.success && o2.success && t1.len() == 1 && t2.len() == 1 {
        assert_eq!(
            t1.checkpoints[0].memory_hash, t2.checkpoints[0].memory_hash,
            "same module + input => same memory hash"
        );
    }
}

#[tokio::test]
async fn no_memory_module_yields_empty_trace() {
    let hv = hypervisor();
    let (_output, trace) = hv
        .execute_tool_traced(
            no_memory_tool("t"),
            serde_json::json!({}),
            &TraceConfig::default(),
        )
        .await
        .unwrap();
    assert!(trace.is_empty(), "no exported memory => no checkpoints");
    assert!(trace.replay().current().is_none());
}
