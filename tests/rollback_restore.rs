//! PR-3: Rollback restore integration tests.
//!
//! Verifies that `rollback_to` actually preserves decompressed memory
//! bytes and that `restore_memory` can write them back into a live
//! wasmtime instance — proving rollback is behavioral, not just a bool.

use nexus::snapshot::{
    restore_memory, ExecutionState, FilesystemDiff, SnapshotManager, SnapshotMetadata,
};
use nexus::{HypervisorConfig, NexusHypervisor, SnapshotStrategy, ToolDefinition};
use wasmtime::{Config, Engine, Instance, Linker, Module, Store};

fn make_engine() -> Engine {
    let mut cfg = Config::new();
    cfg.consume_fuel(true);
    Engine::new(&cfg).unwrap()
}

fn instantiate_with_memory(engine: &Engine) -> (Store<()>, Instance) {
    let wat = r#"(module (memory (export "memory") 1))"#;
    let module = Module::new(engine, wat).unwrap();
    let linker = Linker::new(engine);
    let mut store = Store::new(engine, ());
    store.set_fuel(1_000_000).unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    (store, instance)
}

#[test]
fn restore_memory_writes_bytes_back() {
    let engine = make_engine();
    let (mut store, instance) = instantiate_with_memory(&engine);

    let memory = instance.get_memory(&mut store, "memory").unwrap();

    // Write a known pattern into memory
    let pattern: Vec<u8> = (0..256).map(|i| (i % 256) as u8).collect();
    memory.data_mut(&mut store)[..256].copy_from_slice(&pattern);

    // Snapshot the memory
    let original = memory.data(&store)[..65536].to_vec();

    // Mutate memory (simulating guest execution)
    memory.data_mut(&mut store)[..256].copy_from_slice(&[0xFF; 256]);
    assert_ne!(
        &memory.data(&store)[..256],
        &pattern[..],
        "memory should be mutated"
    );

    // Restore from the snapshot bytes
    restore_memory(&memory, &mut store, &original).unwrap();

    // Byte-exact assertion
    assert_eq!(
        &memory.data(&store)[..256],
        &pattern[..],
        "restore_memory should write the original bytes back"
    );
    assert_eq!(
        memory.data(&store)[..65536],
        original[..],
        "full memory page should match the snapshot"
    );
}

#[test]
fn snapshot_roundtrip_byte_exact() {
    let engine = make_engine();
    let (mut store, instance) = instantiate_with_memory(&engine);
    let memory = instance.get_memory(&mut store, "memory").unwrap();

    // Write recognizable data
    let data: Vec<u8> = (0u8..=255).cycle().take(4096).collect();
    memory.data_mut(&mut store)[..4096].copy_from_slice(&data);

    // Create a snapshot via SnapshotManager (compress + store)
    let mgr = SnapshotManager::new(10);
    let mem_bytes = memory.data(&store).to_vec();
    let snap = mgr
        .create_snapshot(
            mem_bytes.clone(),
            FilesystemDiff::new(),
            ExecutionState::default(),
            SnapshotMetadata::new("test".into(), "hash".into()),
        )
        .unwrap();

    // Mutate memory completely
    memory.data_mut(&mut store)[..4096].copy_from_slice(&[0x00; 4096]);

    // Rollback via manager (decompress)
    let rollback = mgr.rollback_to(&snap.id).unwrap();

    // Restore into the live instance
    restore_memory(&memory, &mut store, &rollback.memory).unwrap();

    // Byte-exact: restored memory matches what we snapshotted
    assert_eq!(
        &memory.data(&store)[..4096],
        &data[..],
        "rollback + restore should recover exact bytes"
    );
}

#[tokio::test]
async fn hypervisor_stores_rollback_memory_on_failure() {
    let hv = NexusHypervisor::new(HypervisorConfig::default()).unwrap();

    // A WASM module that traps — triggers rollback path
    let wasm = wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (func (export "_start") unreachable)
        )"#,
    )
    .unwrap();

    let tool = ToolDefinition::new("trap_tool".into(), wasm);
    let output = hv.execute_tool(tool, serde_json::json!({})).await.unwrap();

    assert!(!output.success, "trap should fail");
    assert!(
        output.rollback_performed,
        "runtime failure with memory should trigger rollback"
    );

    // The hypervisor should have stored the decompressed rollback memory
    let mem = hv.last_rollback_memory();
    assert!(
        mem.is_some(),
        "last_rollback_memory should be Some after a rollback"
    );
    let mem = mem.unwrap();
    assert!(
        !mem.is_empty(),
        "rollback memory should not be empty (it's the pre-call linear memory)"
    );

    // take_rollback_memory consumes it
    let taken = hv.take_rollback_memory();
    assert!(taken.is_some());
    assert!(
        hv.last_rollback_memory().is_none(),
        "take should consume the memory"
    );
}

#[tokio::test]
async fn successful_execution_does_not_set_rollback_memory() {
    let hv = NexusHypervisor::new(HypervisorConfig::default()).unwrap();

    let wasm = wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (func (export "_start"))
        )"#,
    )
    .unwrap();

    let tool = ToolDefinition::new("ok_tool".into(), wasm);
    let output = hv.execute_tool(tool, serde_json::json!({})).await.unwrap();

    assert!(output.success);
    assert!(
        hv.last_rollback_memory().is_none(),
        "successful execution should not store rollback memory"
    );
}

#[tokio::test]
async fn default_hypervisor_uses_full_snapshots_for_rollback() {
    let hv = NexusHypervisor::new(HypervisorConfig::default()).unwrap();

    let wasm = wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (func (export "_start") unreachable)
        )"#,
    )
    .unwrap();

    let tool = ToolDefinition::new("default_full_snapshot".into(), wasm);
    let output = hv.execute_tool(tool, serde_json::json!({})).await.unwrap();

    assert!(!output.success);
    assert!(output.rollback_performed);
    assert_eq!(hv.snapshot_strategy(), SnapshotStrategy::Full);
    assert_eq!(
        hv.snapshot_manager().diff_snapshot_count(),
        0,
        "default strategy should not create differential snapshots"
    );
    assert_eq!(hv.get_snapshot_stats().total_snapshots, 1);
    assert_eq!(hv.get_snapshot_stats().total_rollbacks, 1);
}

#[tokio::test]
async fn differential_strategy_rolls_back_via_diff_snapshot() {
    let config = HypervisorConfig {
        snapshot_strategy: SnapshotStrategy::Differential,
        ..HypervisorConfig::default()
    };
    let hv = NexusHypervisor::new(config).unwrap();

    let seed_wasm = wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (data (i32.const 0) "base")
            (func (export "_start"))
        )"#,
    )
    .unwrap();
    let seed = ToolDefinition::new("diff_seed".into(), seed_wasm);
    let seed_output = hv.execute_tool(seed, serde_json::json!({})).await.unwrap();
    assert!(seed_output.success);
    assert_eq!(hv.get_snapshot_stats().total_snapshots, 1);

    let trap_wasm = wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (data (i32.const 0) "diff")
            (func (export "_start") unreachable)
        )"#,
    )
    .unwrap();
    let tool = ToolDefinition::new("diff_trap".into(), trap_wasm);
    let output = hv.execute_tool(tool, serde_json::json!({})).await.unwrap();

    assert!(!output.success);
    assert!(output.rollback_performed);
    assert_eq!(
        hv.snapshot_manager().diff_snapshot_count(),
        1,
        "second execution should create a differential runtime snapshot"
    );
    assert_eq!(
        hv.get_snapshot_stats().total_snapshots,
        1,
        "diff snapshots should not increment full snapshot count"
    );
    assert_eq!(hv.get_snapshot_stats().total_rollbacks, 1);

    let memory = hv
        .last_rollback_memory()
        .expect("diff rollback should store reconstructed memory");
    assert_eq!(&memory[..4], b"diff");
}
