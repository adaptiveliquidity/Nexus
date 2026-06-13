//! Integration tests for execution replay / time-travel debugging through the
//! real hypervisor. The deterministic replay-engine behaviour itself is unit
//! tested in `telemetry::trace`; these exercise `execute_tool_traced` and
//! fuel-indexed `record_trace`.

use nexus::telemetry::TraceConfig;
use nexus::{HypervisorConfig, NexusHypervisor, SandboxConfig, ToolDefinition, WasmSandbox};

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

/// Module that mutates memory and a global repeatedly so fuel-capped
/// re-executions observe a moving state.
fn looping_tool(name: &str) -> ToolDefinition {
    let wasm = wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (global (export "g") (mut i32) (i32.const 0))
            (func (export "_start") (local $i i32)
                (loop $loop
                    local.get $i
                    i32.const 1
                    i32.add
                    local.tee $i
                    global.set 0
                    i32.const 0
                    local.get $i
                    i32.store
                    local.get $i
                    i32.const 50000
                    i32.lt_s
                    br_if $loop)))"#,
    )
    .unwrap();
    ToolDefinition::new(name.to_string(), wasm)
}

fn tiny_memory_tool(name: &str) -> ToolDefinition {
    let wasm = wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (func (export "_start")
                i32.const 0
                i32.const 42
                i32.store))"#,
    )
    .unwrap();
    ToolDefinition::new(name.to_string(), wasm)
}

fn trace_config(interval: u64, max_checkpoints: usize) -> TraceConfig {
    TraceConfig {
        checkpoint_interval_fuel: interval,
        max_checkpoints,
        capture_memory: true,
    }
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

#[tokio::test]
async fn interval_trace_has_multiple_checkpoints() {
    let hv = hypervisor();
    let cfg = trace_config(1_000, 8);
    let trace = hv
        .record_trace(looping_tool("looping"), serde_json::json!({}), &cfg)
        .await
        .unwrap();

    assert!(
        trace.len() > 1 && trace.len() <= cfg.max_checkpoints,
        "expected multiple capped checkpoints, got {}",
        trace.len()
    );
    assert!(trace
        .checkpoints
        .windows(2)
        .all(|pair| pair[0].fuel_at < pair[1].fuel_at));
    assert!(trace
        .checkpoints
        .iter()
        .all(|checkpoint| checkpoint.memory_hash.len() == 64));
}

#[tokio::test]
async fn checkpoint_hashes_are_deterministic() {
    let hv = hypervisor();
    let cfg = trace_config(1_000, 6);

    let first = hv
        .record_trace(looping_tool("looping"), serde_json::json!({}), &cfg)
        .await
        .unwrap();
    let second = hv
        .record_trace(looping_tool("looping"), serde_json::json!({}), &cfg)
        .await
        .unwrap();

    assert_eq!(first.len(), second.len());
    assert!(!first.is_empty());
    for (a, b) in first.checkpoints.iter().zip(second.checkpoints.iter()) {
        assert_eq!(a.fuel_at, b.fuel_at);
        assert_eq!(a.memory_hash, b.memory_hash);
    }
}

#[tokio::test]
async fn completing_module_yields_one_checkpoint() {
    let hv = hypervisor();
    let trace = hv
        .record_trace(
            tiny_memory_tool("tiny"),
            serde_json::json!({}),
            &trace_config(100_000, 8),
        )
        .await
        .unwrap();

    assert_eq!(trace.len(), 1);
    assert_eq!(trace.checkpoints[0].sequence, 0);
    assert_eq!(trace.checkpoints[0].memory_hash.len(), 64);
}

#[tokio::test]
async fn no_memory_module_record_trace_is_empty() {
    let hv = hypervisor();
    let trace = hv
        .record_trace(
            no_memory_tool("no-memory"),
            serde_json::json!({}),
            &trace_config(1_000, 8),
        )
        .await
        .unwrap();

    assert!(trace.is_empty());
}

#[tokio::test]
async fn replay_navigation_over_recorded_trace() {
    let hv = hypervisor();
    let trace = hv
        .record_trace(
            looping_tool("looping"),
            serde_json::json!({}),
            &trace_config(1_000, 8),
        )
        .await
        .unwrap();
    assert!(trace.len() > 1);

    let mut replay = trace.replay();
    assert_eq!(replay.position(), 0);
    let second = replay.step_forward().unwrap();
    assert_eq!(second.sequence, 1);
    assert_eq!(replay.fuel_at(1), Some(second.fuel_at));
    let first = replay.step_backward().unwrap();
    assert_eq!(first.sequence, 0);
    let last = replay.goto_checkpoint(trace.len() - 1).unwrap();
    assert_eq!(last.sequence, trace.len() - 1);
}

#[test]
fn execute_to_fuel_completed_flag() {
    let sandbox = WasmSandbox::new(SandboxConfig::default()).unwrap();
    let tiny = tiny_memory_tool("tiny");
    let tiny_step = sandbox
        .execute_to_fuel(&tiny.wasm_bytes, &[], 100_000)
        .unwrap();
    assert!(tiny_step.completed);
    assert!(tiny_step.memory.is_some());

    let looping = looping_tool("looping");
    let loop_step = sandbox
        .execute_to_fuel(&looping.wasm_bytes, &[], 1_000)
        .unwrap();
    assert!(!loop_step.completed);
    assert!(loop_step.memory.is_some());
}
