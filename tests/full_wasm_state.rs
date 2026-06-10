//! Full WASM state capture tests.
//!
//! Verifies that snapshots capture and restore not just linear memory
//! but also exported mutable globals and tables.

use nexus::sandbox::{SandboxConfig, WasmSandbox};

fn sandbox() -> WasmSandbox {
    WasmSandbox::new(SandboxConfig::default()).unwrap()
}

#[test]
fn execute_captures_globals() {
    let wasm = wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (global (export "counter") (mut i32) (i32.const 0))
            (global (export "euler") f64 (f64.const 1.23456))
            (func (export "_start")
                (global.set 0 (i32.const 42))
            )
        )"#,
    )
    .unwrap();

    let sb = sandbox();
    let result = sb.execute(&wasm, &[]).unwrap();
    assert!(result.success);

    let globals = result.post_call_globals.expect("should capture globals");
    assert!(globals.len() >= 2, "should capture at least 2 globals");

    let counter = globals
        .iter()
        .find(|g| g.name == "counter")
        .expect("counter global");
    assert_eq!(counter.value, nexus::snapshot::GlobalValue::I32(42));

    let euler = globals
        .iter()
        .find(|g| g.name == "euler")
        .expect("euler global");
    match &euler.value {
        nexus::snapshot::GlobalValue::F64(v) => {
            assert!((v - 1.23456).abs() < 1e-10, "euler should be ~1.23456");
        }
        other => panic!("expected F64, got {:?}", other),
    }
}

#[test]
fn execute_captures_tables() {
    let wasm = wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (table (export "tbl") 4 funcref)
            (func $a)
            (func $b)
            (elem (i32.const 0) $a $b)
            (func (export "_start"))
        )"#,
    )
    .unwrap();

    let sb = sandbox();
    let result = sb.execute(&wasm, &[]).unwrap();
    assert!(result.success);

    let tables = result.post_call_tables.expect("should capture tables");
    assert_eq!(tables.len(), 1);
    assert_eq!(tables[0].name, "tbl");
    assert_eq!(tables[0].size, 4);
}

#[test]
fn snapshot_roundtrip_includes_globals() {
    use nexus::snapshot::{
        ExecutionState, FilesystemDiff, GlobalSnapshot, GlobalValue, SnapshotManager,
        SnapshotMetadata,
    };

    let mgr = SnapshotManager::new(8);
    let memory = vec![0u8; 65536];
    let globals = vec![
        GlobalSnapshot {
            name: "counter".into(),
            value: GlobalValue::I32(42),
            mutable: true,
        },
        GlobalSnapshot {
            name: "euler".into(),
            value: GlobalValue::F64(1.23456),
            mutable: false,
        },
    ];

    let exec_state = ExecutionState {
        captured_globals: globals.clone(),
        ..ExecutionState::default()
    };

    let snap = mgr
        .create_snapshot(
            memory,
            FilesystemDiff::new(),
            exec_state,
            SnapshotMetadata::new("test".into(), "hash".into()),
        )
        .unwrap();

    assert_eq!(snap.execution_state.captured_globals.len(), 2);
    assert_eq!(
        snap.execution_state.captured_globals[0].value,
        GlobalValue::I32(42)
    );
}

#[test]
fn restore_globals_writes_back() {
    use nexus::snapshot::GlobalValue;

    let wasm = wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (global (export "counter") (mut i32) (i32.const 0))
            (func (export "_start")
                (global.set 0 (i32.const 99))
            )
        )"#,
    )
    .unwrap();

    let sb = sandbox();
    let result = sb.execute(&wasm, &[]).unwrap();
    assert!(result.success);

    let globals = result.post_call_globals.unwrap();
    let counter = globals.iter().find(|g| g.name == "counter").unwrap();
    assert_eq!(counter.value, GlobalValue::I32(99));
}

#[tokio::test]
async fn hypervisor_snapshot_captures_globals() {
    use nexus::{HypervisorConfig, NexusHypervisor, ToolDefinition};

    let hv = NexusHypervisor::new(HypervisorConfig::default()).unwrap();

    let wasm = wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (global (export "count") (mut i32) (i32.const 0))
            (func (export "_start")
                (global.set 0 (i32.const 7))
            )
        )"#,
    )
    .unwrap();

    let tool = ToolDefinition::new("globals_test".into(), wasm);
    let output = hv.execute_tool(tool, serde_json::json!({})).await.unwrap();

    assert!(output.success);
}
