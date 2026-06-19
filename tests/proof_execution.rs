use nexus::proof::ProofScorecard;
use nexus::{HypervisorConfig, NexusHypervisor, ToolDefinition};
use uuid::Uuid;

fn trivial_wasm() -> Vec<u8> {
    wat::parse_str(r#"(module (memory (export "memory") 1) (func (export "_start")))"#).unwrap()
}

#[tokio::test]
async fn execute_tool_proof_returns_output_and_capsule() {
    let hypervisor = NexusHypervisor::new(HypervisorConfig::default()).unwrap();
    let tool = ToolDefinition::new("proof_trivial".to_string(), trivial_wasm());

    let (output, capsule) = hypervisor
        .execute_tool_proof(tool, serde_json::json!({ "message": "hello" }))
        .await
        .unwrap();

    assert!(output.success);
    assert_ne!(capsule.capsule_id, Uuid::nil());
    assert!(capsule.limitations.is_empty());

    let _scorecard = ProofScorecard::from_capsule(&capsule);
}

fn trap_wasm() -> Vec<u8> {
    wat::parse_str(
        r#"(module (memory (export "memory") 1) (func (export "_start") (unreachable)))"#,
    )
    .unwrap()
}

#[tokio::test]
async fn wasm_trap_populates_failure_evidence() {
    let hypervisor = NexusHypervisor::new(HypervisorConfig::default()).unwrap();
    let tool = ToolDefinition::new("proof_trap".to_string(), trap_wasm());

    let (output, capsule) = hypervisor
        .execute_tool_proof(tool, serde_json::json!({}))
        .await
        .unwrap();

    assert!(!output.success);
    let failure = capsule.failure.expect("trap must produce FailureEvidence");
    assert!(!failure.failure_category.is_empty());
    assert!(!failure.error_summary.is_empty());
}

#[tokio::test]
async fn rollback_after_trap_populates_rollback_evidence() {
    let hypervisor = NexusHypervisor::new(HypervisorConfig::default()).unwrap();

    // First execution: establishes a snapshot for rollback testing.
    let good = ToolDefinition::new("rollback_good".to_string(), trivial_wasm());
    let (first_output, _) = hypervisor
        .execute_tool_proof(good, serde_json::json!({}))
        .await
        .unwrap();
    assert!(first_output.success);

    // Second execution: traps and should roll back to the first snapshot.
    let bad = ToolDefinition::new("rollback_bad".to_string(), trap_wasm());
    let (trap_output, capsule) = hypervisor
        .execute_tool_proof(bad, serde_json::json!({}))
        .await
        .unwrap();

    assert!(!trap_output.success);
    assert!(trap_output.rollback_performed, "hypervisor must roll back after trap");

    let rollback = capsule
        .rollback
        .as_ref()
        .expect("capsule must have RollbackEvidence");
    assert!(rollback.occurred);
    assert!(rollback.from_snapshot_id.is_some());

    let scorecard = ProofScorecard::from_capsule(&capsule);
    assert!(scorecard.has_rollback);
}
