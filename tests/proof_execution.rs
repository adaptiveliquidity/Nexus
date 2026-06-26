use nexus::proof::{ProofHmacKey, ProofScorecard, TypedDigest};
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
    assert!(!capsule.limitations.is_empty());
    assert!(!capsule.input.digest.public_recomputable);

    let _scorecard = ProofScorecard::from_capsule(&capsule);
}

#[tokio::test]
async fn input_digest_uses_hmac_for_sensitive_short_inputs() {
    let hmac_env = format!("NEXUS_TEST_PROOF_INPUT_HMAC_{}", Uuid::new_v4().simple());
    std::env::set_var(&hmac_env, "sensitive-input-key");
    assert_eq!(
        std::env::var(&hmac_env).as_deref(),
        Ok("sensitive-input-key"),
        "input HMAC key env var should be set in-process",
    );

    let hypervisor = NexusHypervisor::new(HypervisorConfig {
        proof_hmac_key: ProofHmacKey::FromEnv(hmac_env.clone()),
        ..HypervisorConfig::default()
    })
    .unwrap();
    let tool = ToolDefinition::new("proof_sensitive_input".to_string(), trivial_wasm());

    let (output, capsule) = hypervisor
        .execute_tool_proof(
            tool,
            serde_json::json!({
                "api_key": "test-api-key-123",
                "memory_text": "raw-memory-fragment",
            }),
        )
        .await
        .unwrap();

    assert!(output.success);
    assert_eq!(capsule.input.digest.algorithm, "hmac-sha256");
    assert!(!capsule.input.digest.public_recomputable);
    let json = serde_json::to_string(&capsule).unwrap();
    assert!(!json.contains("test-api-key-123"));
    assert!(!json.contains("raw-memory-fragment"));
    std::env::remove_var(&hmac_env);
}

#[tokio::test]
async fn execute_tool_proof_does_not_emit_api_key_secret() {
    do_no_secret_capsule_assert(
        "test-secret-api-key-abc-123",
        serde_json::json!({"api_key":"test-secret-api-key-abc-123"}),
    )
    .await;
}

#[tokio::test]
async fn execute_tool_proof_does_not_emit_absolute_path_secret() {
    do_no_secret_capsule_assert(
        "/home/user/.secrets/token.txt",
        serde_json::json!({"path":"/home/user/.secrets/token.txt"}),
    )
    .await;
}

#[tokio::test]
async fn execute_tool_proof_does_not_emit_raw_token_secret() {
    do_no_secret_capsule_assert(
        "sk-dev-123456",
        serde_json::json!({"token":"sk-dev-123456"}),
    )
    .await;
}

#[tokio::test]
async fn execute_tool_proof_does_not_emit_raw_memory_secret() {
    do_no_secret_capsule_assert(
        "raw memory text that should never be exposed",
        serde_json::json!({"raw_memory":"raw memory text that should never be exposed"}),
    )
    .await;
}

async fn do_no_secret_capsule_assert(secret: &str, input: serde_json::Value) {
    let hmac_env = format!("NEXUS_TEST_PROOF_INPUT_HMAC_{}", Uuid::new_v4().simple());
    std::env::set_var(&hmac_env, "sensitive-input-key");
    let hypervisor = NexusHypervisor::new(HypervisorConfig {
        proof_hmac_key: ProofHmacKey::FromEnv(hmac_env.clone()),
        ..HypervisorConfig::default()
    })
    .unwrap();
    let tool = ToolDefinition::new("proof_secret_guard".to_string(), trivial_wasm());

    let (_output, capsule) = hypervisor.execute_tool_proof(tool, input).await.unwrap();
    let json = serde_json::to_string(&capsule).unwrap();
    assert!(
        !json.contains(secret),
        "secret leaked into emitted capsule: {secret}"
    );
    std::env::remove_var(&hmac_env);
}

#[tokio::test]
async fn test_capsule_limitations_non_empty() {
    let hypervisor = NexusHypervisor::new(HypervisorConfig::default()).unwrap();
    let tool = ToolDefinition::new("proof_limitations".to_string(), trivial_wasm());

    let (_, capsule) = hypervisor
        .execute_tool_proof(tool, serde_json::json!({ "message": "hello" }))
        .await
        .unwrap();

    assert!(!capsule.limitations.is_empty());
    assert!(capsule
        .limitations
        .contains(&"runtime_attestation_only".to_owned()));
}

#[tokio::test]
async fn test_capsule_redaction_report_populated() {
    let hypervisor = NexusHypervisor::new(HypervisorConfig {
        proof_hmac_key: ProofHmacKey::Disabled,
        ..HypervisorConfig::default()
    })
    .unwrap();
    let tool = ToolDefinition::new("proof_redaction_report".to_string(), trivial_wasm());

    let (_, capsule) = hypervisor
        .execute_tool_proof(tool, serde_json::json!({ "message": "hello" }))
        .await
        .unwrap();

    let redaction_count = capsule.redaction.hashed_fields.len()
        + capsule.redaction.hmac_fields.len()
        + capsule.redaction.truncated_fields.len()
        + capsule.redaction.removed_fields.len();
    assert!(redaction_count > 0);
    assert!(capsule
        .redaction
        .removed_fields
        .contains(&"input.digest".to_owned()));
}

#[tokio::test]
async fn test_input_digest_not_public_sha_for_sensitive_input() {
    let hypervisor = NexusHypervisor::new(HypervisorConfig::default()).unwrap();
    let tool = ToolDefinition::new("proof_sensitive_input".to_string(), trivial_wasm());
    let input = serde_json::json!({
        "prompt": "use token sk-test-token-123 from /home/x/.ssh/id_ed25519"
    });
    let input_bytes = serde_json::to_vec(&input).unwrap();
    let public_sha = TypedDigest::sha256_public(&input_bytes);

    let (_, capsule) = hypervisor.execute_tool_proof(tool, input).await.unwrap();

    assert!(!capsule.input.digest.public_recomputable);
    assert_ne!(capsule.input.digest.algorithm, "sha256");
    assert_ne!(capsule.input.digest.value, public_sha.value);
}

#[tokio::test]
async fn input_digest_uses_hmac_when_proof_hmac_key_configured() {
    let env_var = format!("NEXUS_TEST_PROOF_INPUT_HMAC_{}", Uuid::new_v4().simple());
    std::env::set_var(&env_var, "test-proof-hmac-key");
    let hypervisor = NexusHypervisor::new(HypervisorConfig {
        proof_hmac_key: ProofHmacKey::FromEnv(env_var.clone()),
        ..HypervisorConfig::default()
    })
    .unwrap();
    let tool = ToolDefinition::new("proof_hmac_input".to_string(), trivial_wasm());

    let (_, capsule) = hypervisor
        .execute_tool_proof(tool, serde_json::json!({ "prompt": "hello" }))
        .await
        .unwrap();
    std::env::remove_var(&env_var);

    assert_eq!(capsule.input.digest.algorithm, "hmac-sha256");
    assert!(!capsule.input.digest.public_recomputable);
    assert!(capsule
        .redaction
        .hmac_fields
        .contains(&"input.digest".to_owned()));
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
    assert!(
        trap_output.rollback_performed,
        "hypervisor must roll back after trap"
    );

    let rollback = capsule
        .rollback
        .as_ref()
        .expect("capsule must have RollbackEvidence");
    assert!(rollback.occurred);
    assert!(rollback.from_snapshot_id.is_some());

    let scorecard = ProofScorecard::from_capsule(&capsule);
    assert!(scorecard.has_rollback);
}
