use base64::{engine::general_purpose::STANDARD, Engine as _};
use chrono::{DateTime, Utc};
use ed25519_dalek::{SigningKey, VerifyingKey};
use nexus::proof::{
    sign_capsule, verify_capsule, CapabilityEvidence, InputIdentity, PolicyEnforcementMode,
    PolicyProfileRef, ProofCapsule, ProofScorecard, ProofSigningConfig, ProofSubject,
    RedactionReport, ToolIdentity, TypedDigest,
};
use nexus::{HypervisorConfig, NexusHypervisor, ToolDefinition};
use rand::rngs::OsRng;
use uuid::Uuid;

fn uuid(value: &str) -> Uuid {
    Uuid::parse_str(value).unwrap()
}

fn timestamp(value: &str) -> DateTime<Utc> {
    value.parse::<DateTime<Utc>>().unwrap()
}

fn typed_digest(algorithm: &str, value: &str, public_recomputable: bool) -> TypedDigest {
    TypedDigest {
        algorithm: algorithm.to_owned(),
        value: value.to_owned(),
        public_recomputable,
    }
}

fn sample_capsule() -> ProofCapsule {
    ProofCapsule {
        version: "1".to_string(),
        capsule_id: uuid("55555555-5555-4555-8555-555555555555"),
        subject: ProofSubject {
            run_id: uuid("66666666-6666-4666-8666-666666666666"),
            tool_name: "csv_reporter".to_owned(),
            started_at: timestamp("2026-06-17T12:00:00Z"),
            finished_at: timestamp("2026-06-17T12:00:01Z"),
            duration_ms: 1000,
        },
        tool: ToolIdentity {
            module_digest: typed_digest("sha256", "module-digest", true),
            module_name: "csv_reporter.wasm".to_owned(),
            entrypoint: "_start".to_owned(),
        },
        input: InputIdentity {
            digest: typed_digest("sha256", "input-digest", true),
            media_type: "application/json".to_owned(),
            raw_included: false,
        },
        policy: PolicyProfileRef {
            profile_digest: Some(typed_digest("sha256", "profile-digest", true)),
            profile_name: Some("strict-readonly".to_owned()),
            mode: PolicyEnforcementMode::ProfileEnforcedMcpCapabilitiesOnly,
        },
        capabilities: CapabilityEvidence {
            required: vec!["fs:read:/input/orders.csv".to_owned()],
            granted: vec!["fs:read:/input/orders.csv".to_owned()],
            mismatch: None,
        },
        snapshot: None,
        failure: None,
        rollback: None,
        branches: None,
        redaction: RedactionReport {
            hashed_fields: vec!["input.body".to_owned()],
            truncated_fields: Vec::new(),
            removed_fields: Vec::new(),
            hmac_fields: Vec::new(),
        },
        limitations: vec!["runtime attestation only".to_owned()],
        signature: None,
    }
}

fn trivial_wasm() -> Vec<u8> {
    wat::parse_str(r#"(module (memory (export "memory") 1) (func (export "_start")))"#).unwrap()
}

fn proof_config(proof_signing: ProofSigningConfig) -> HypervisorConfig {
    HypervisorConfig {
        proof_signing,
        ..HypervisorConfig::default()
    }
}

fn unique_env_var(label: &str) -> String {
    format!("NEXUS_TEST_{label}_{}", Uuid::new_v4().simple())
}

#[test]
fn sign_roundtrip_verifies_with_public_key() {
    let signing_key = SigningKey::generate(&mut OsRng);
    let verifying_key = VerifyingKey::from(&signing_key);

    let signed = sign_capsule(sample_capsule(), &signing_key);

    verify_capsule(&signed, &verifying_key).unwrap();
}

#[test]
fn verify_rejects_tampered_subject() {
    let signing_key = SigningKey::generate(&mut OsRng);
    let verifying_key = VerifyingKey::from(&signing_key);
    let mut signed = sign_capsule(sample_capsule(), &signing_key);

    signed.subject.tool_name = "tampered_reporter".to_owned();

    assert!(verify_capsule(&signed, &verifying_key).is_err());
}

#[test]
fn scorecard_has_signature_after_signing() {
    let signing_key = SigningKey::generate(&mut OsRng);
    let signed = sign_capsule(sample_capsule(), &signing_key);

    let scorecard = ProofScorecard::from_capsule(&signed);

    assert!(scorecard.has_signature);
}

#[tokio::test]
async fn ephemeral_proof_key_signs_and_verifies() {
    let hypervisor =
        NexusHypervisor::new(proof_config(ProofSigningConfig::EphemeralDedicated)).unwrap();
    let tool = ToolDefinition::new("proof_ephemeral".to_string(), trivial_wasm());

    let (_, capsule) = hypervisor
        .execute_tool_proof(tool, serde_json::json!({ "message": "hello" }))
        .await
        .unwrap();

    verify_capsule(&capsule, &hypervisor.proof_verifying_key()).unwrap();
    assert_eq!(
        capsule.signature.as_ref().unwrap().key_id,
        hypervisor.proof_key_id()
    );
}

#[tokio::test]
async fn proof_key_is_separate_from_capability_key() {
    let hypervisor = NexusHypervisor::new(HypervisorConfig::default()).unwrap();
    let capability_key_bytes: [u8; 32] = hypervisor.capability_public_key().try_into().unwrap();
    let capability_verifying_key = VerifyingKey::from_bytes(&capability_key_bytes).unwrap();

    assert_ne!(
        hypervisor.proof_verifying_key().as_bytes(),
        &capability_key_bytes
    );

    let tool = ToolDefinition::new("proof_separate".to_string(), trivial_wasm());
    let (_, capsule) = hypervisor
        .execute_tool_proof(tool, serde_json::json!({}))
        .await
        .unwrap();

    verify_capsule(&capsule, &hypervisor.proof_verifying_key()).unwrap();
    assert!(verify_capsule(&capsule, &capability_verifying_key).is_err());
}

#[tokio::test]
async fn from_env_seed_is_deterministic() {
    let env_var = unique_env_var("PROOF_SIGNING_SEED");
    let seed = [7_u8; 32];
    std::env::set_var(&env_var, STANDARD.encode(seed.as_slice()));

    let first =
        NexusHypervisor::new(proof_config(ProofSigningConfig::FromEnv(env_var.clone()))).unwrap();
    let second =
        NexusHypervisor::new(proof_config(ProofSigningConfig::FromEnv(env_var.clone()))).unwrap();

    std::env::remove_var(&env_var);

    assert_eq!(first.proof_verifying_key(), second.proof_verifying_key());

    let tool = ToolDefinition::new("proof_deterministic".to_string(), trivial_wasm());
    let (_, capsule) = first
        .execute_tool_proof(tool, serde_json::json!({}))
        .await
        .unwrap();

    verify_capsule(&capsule, &second.proof_verifying_key()).unwrap();
}

#[test]
fn from_env_missing_or_malformed_fails_closed() {
    let missing_env = unique_env_var("PROOF_SIGNING_MISSING");
    std::env::remove_var(&missing_env);
    let missing = NexusHypervisor::new(proof_config(ProofSigningConfig::FromEnv(
        missing_env.clone(),
    )));
    assert!(missing.is_err());

    let malformed_env = unique_env_var("PROOF_SIGNING_MALFORMED");
    let malformed_value = "not-a-valid-proof-signing-seed";
    std::env::set_var(&malformed_env, malformed_value);
    let malformed = NexusHypervisor::new(proof_config(ProofSigningConfig::FromEnv(
        malformed_env.clone(),
    )));
    std::env::remove_var(&malformed_env);

    let Err(malformed) = malformed else {
        panic!("expected construction error");
    };
    let error = malformed.to_string();
    assert!(error.contains(&malformed_env));
    assert!(!error.contains(malformed_value));

    let short_seed_env = unique_env_var("PROOF_SIGNING_SHORT");
    let short_seed_value = STANDARD.encode([1_u8; 31].as_slice());
    std::env::set_var(&short_seed_env, &short_seed_value);
    let short_seed = NexusHypervisor::new(proof_config(ProofSigningConfig::FromEnv(
        short_seed_env.clone(),
    )));
    std::env::remove_var(&short_seed_env);

    let Err(short_seed) = short_seed else {
        panic!("expected construction error");
    };
    let error = short_seed.to_string();
    assert!(error.contains(&short_seed_env));
    assert!(!error.contains(&short_seed_value));
}
