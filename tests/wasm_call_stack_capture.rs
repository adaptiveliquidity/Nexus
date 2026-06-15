use nexus::hypervisor::validator::error_log::ErrorLog;
use nexus::hypervisor::validator::health::ResourceSnapshot;
use nexus::snapshot::sync::digest_of;
use nexus::{
    CaptureSite, ExecutionState, FailureMode, FilesystemDiff, HypervisorConfig, NexusHypervisor,
    SandboxConfig, Snapshot, SnapshotMetadata, ToolDefinition, WasmSandbox,
};

fn trapping_wasm() -> Vec<u8> {
    wat::parse_str(
        r#"
        (module
            (memory (export "memory") 1)
            (func $inner
                unreachable
            )
            (func (export "_start")
                call $inner
            )
        )
        "#,
    )
    .unwrap()
}

fn fake_resources() -> ResourceSnapshot {
    ResourceSnapshot {
        cpu_usage: 0.0,
        memory_used_mb: 0,
        memory_limit_mb: 1024,
        timestamp: chrono::Utc::now(),
    }
}

#[tokio::test]
async fn trap_call_stack_reaches_error_log_context() {
    let hypervisor = NexusHypervisor::new(HypervisorConfig::default()).unwrap();
    let tool = ToolDefinition::new("trap_stack".to_string(), trapping_wasm());

    let output = hypervisor
        .execute_tool(tool, serde_json::json!({}))
        .await
        .unwrap();

    assert!(!output.success);
    let error_log = output.error_log.expect("trap should produce an ErrorLog");
    let call_stack = error_log
        .call_stack
        .as_ref()
        .expect("trap should attach a diagnostic call stack");

    assert_eq!(call_stack.captured_at, CaptureSite::Trap);
    assert!(
        !call_stack.frames.is_empty(),
        "trap call stack should include at least one WASM frame"
    );

    let context = error_log.to_llm_context();
    assert!(context.contains("## WASM Call Stack"), "{context}");
    assert!(context.contains("Captured at: Trap"), "{context}");
}

#[test]
fn captured_call_stack_does_not_change_snapshot_checksum_or_digest() {
    let sandbox = WasmSandbox::new(SandboxConfig::default()).unwrap();
    let result = sandbox.execute(&trapping_wasm(), &[]).unwrap();

    assert!(!result.success);
    let call_stack = result
        .call_stack
        .clone()
        .expect("trap should produce diagnostic call stack metadata");
    assert_eq!(call_stack.captured_at, CaptureSite::Trap);
    assert!(!call_stack.frames.is_empty());

    let memory = result
        .pre_call_memory
        .clone()
        .expect("module exports memory for snapshot invariant test");
    let execution_state = ExecutionState {
        captured_globals: result.post_call_globals.clone().unwrap_or_default(),
        captured_tables: result.post_call_tables.clone().unwrap_or_default(),
    };
    let metadata = SnapshotMetadata::new("trap_stack".into(), "same-input".into());

    let before_diagnostics = Snapshot::new(
        memory.clone(),
        FilesystemDiff::new(),
        execution_state.clone(),
        metadata.clone(),
    )
    .unwrap();

    let error_log = ErrorLog::new(
        "trap_stack".into(),
        result
            .failure_mode
            .clone()
            .unwrap_or(FailureMode::TrapUnreachable),
        fake_resources(),
    )
    .with_call_stack(Some(call_stack));
    assert!(error_log.call_stack.is_some());

    let after_diagnostics =
        Snapshot::new(memory, FilesystemDiff::new(), execution_state, metadata).unwrap();

    assert_eq!(
        before_diagnostics.memory_checksum, after_diagnostics.memory_checksum,
        "diagnostic call-stack metadata must not affect memory checksums"
    );
    assert_eq!(
        digest_of(&before_diagnostics).unwrap(),
        digest_of(&after_diagnostics).unwrap(),
        "diagnostic call-stack metadata must not affect snapshot digests"
    );
}
