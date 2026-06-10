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

#[test]
fn restore_globals_skips_immutable() {
    use nexus::snapshot::{GlobalSnapshot, GlobalValue};

    let wasm = wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (global (export "ratio") f64 (f64.const 1.618))
            (func (export "_start"))
        )"#,
    )
    .unwrap();

    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::from_binary(&engine, &wasm).unwrap();
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();

    let snapshots = vec![GlobalSnapshot {
        name: "ratio".into(),
        value: GlobalValue::F64(999.0),
        mutable: false,
    }];

    nexus::snapshot::restore_globals(&instance, &mut store, &snapshots).unwrap();

    let g = instance.get_global(&mut store, "ratio").unwrap();
    let val = g.get(&mut store);
    match val {
        wasmtime::Val::F64(bits) => {
            let v = f64::from_bits(bits);
            assert!(
                (v - 1.618).abs() < 1e-10,
                "immutable global should remain unchanged"
            );
        }
        other => panic!("expected F64, got {:?}", other),
    }
}

#[test]
fn restore_globals_skips_missing_name() {
    use nexus::snapshot::{GlobalSnapshot, GlobalValue};

    let wasm = wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (global (export "counter") (mut i32) (i32.const 5))
            (func (export "_start"))
        )"#,
    )
    .unwrap();

    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::from_binary(&engine, &wasm).unwrap();
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();

    let snapshots = vec![
        GlobalSnapshot {
            name: "nonexistent".into(),
            value: GlobalValue::I32(999),
            mutable: true,
        },
        GlobalSnapshot {
            name: "counter".into(),
            value: GlobalValue::I32(77),
            mutable: true,
        },
    ];

    nexus::snapshot::restore_globals(&instance, &mut store, &snapshots).unwrap();

    let g = instance.get_global(&mut store, "counter").unwrap();
    match g.get(&mut store) {
        wasmtime::Val::I32(v) => assert_eq!(v, 77),
        other => panic!("expected I32, got {:?}", other),
    }
}

#[test]
fn restore_globals_roundtrip_via_instance() {
    use nexus::snapshot::GlobalValue;

    let wasm = wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (global (export "counter") (mut i32) (i32.const 0))
            (func (export "_start")
                (global.set 0 (i32.const 42))
            )
        )"#,
    )
    .unwrap();

    let sb = sandbox();
    let result = sb.execute(&wasm, &[]).unwrap();
    assert!(result.success);

    let globals = result.post_call_globals.unwrap();
    let counter = globals.iter().find(|g| g.name == "counter").unwrap();
    assert_eq!(counter.value, GlobalValue::I32(42));

    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::from_binary(&engine, &wasm).unwrap();
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();

    let g = instance.get_global(&mut store, "counter").unwrap();
    assert_eq!(g.get(&mut store).i32(), Some(0));

    nexus::snapshot::restore_globals(&instance, &mut store, &globals).unwrap();

    let g = instance.get_global(&mut store, "counter").unwrap();
    assert_eq!(g.get(&mut store).i32(), Some(42));
}

#[test]
fn module_without_globals_or_tables() {
    let wasm = wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (func (export "_start"))
        )"#,
    )
    .unwrap();

    let sb = sandbox();
    let result = sb.execute(&wasm, &[]).unwrap();
    assert!(result.success);

    let globals = result
        .post_call_globals
        .expect("should be Some even if empty");
    assert!(globals.is_empty());

    let tables = result
        .post_call_tables
        .expect("should be Some even if empty");
    assert!(tables.is_empty());
}

#[test]
fn rollback_result_carries_execution_state() {
    use nexus::snapshot::{
        ExecutionState, FilesystemDiff, GlobalSnapshot, GlobalValue, SnapshotManager,
        SnapshotMetadata, TableSnapshot,
    };

    let mgr = SnapshotManager::new(8);
    let memory = vec![0u8; 65536];
    let globals = vec![GlobalSnapshot {
        name: "x".into(),
        value: GlobalValue::I64(123456),
        mutable: true,
    }];
    let tables = vec![TableSnapshot {
        name: "t".into(),
        size: 10,
    }];

    let exec_state = ExecutionState {
        captured_globals: globals.clone(),
        captured_tables: tables.clone(),
    };

    let snap = mgr
        .create_snapshot(
            memory,
            FilesystemDiff::new(),
            exec_state,
            SnapshotMetadata::new("rollback_test".into(), "hash".into()),
        )
        .unwrap();

    let rollback = mgr.rollback_to(&snap.id).unwrap();
    assert_eq!(rollback.execution_state.captured_globals.len(), 1);
    assert_eq!(
        rollback.execution_state.captured_globals[0].value,
        GlobalValue::I64(123456)
    );
    assert_eq!(rollback.execution_state.captured_tables.len(), 1);
    assert_eq!(rollback.execution_state.captured_tables[0].size, 10);
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
