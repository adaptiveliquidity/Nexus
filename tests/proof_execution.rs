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
