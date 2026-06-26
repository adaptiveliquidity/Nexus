use chrono::Utc;
use nexus::proof::{
    default_proof_capsule_limitations,
    receipt::ExecutionReceipt,
    schema::{
        CapabilityEvidence, InputIdentity, PolicyEnforcementMode, PolicyProfileRef, ProofCapsule,
        ProofCapsuleBuilder, ProofScorecard, ProofSubject, RedactionReport, ToolIdentity,
        TypedDigest,
    },
};
use uuid::Uuid;

fn sample_typed_digest() -> TypedDigest {
    TypedDigest {
        algorithm: "sha256".into(),
        value: "abc123".into(),
        public_recomputable: true,
    }
}

fn sample_private_digest() -> TypedDigest {
    TypedDigest {
        algorithm: "hmac-sha256".into(),
        value: "input-hmac".into(),
        public_recomputable: false,
    }
}

#[test]
fn proof_capsule_builder_builds_minimal_capsule() {
    let capsule =
        ProofCapsuleBuilder::new("bench_tool", sample_typed_digest(), sample_private_digest())
            .build();

    assert_eq!(capsule.version, "1");
    assert_eq!(capsule.subject.tool_name, "bench_tool");
    assert_eq!(capsule.tool.module_name, "bench_tool");
    assert_eq!(capsule.tool.entrypoint, "_start");
    assert_eq!(capsule.input.media_type, "application/json");
    assert!(!capsule.input.raw_included);
    assert_eq!(capsule.policy.mode, PolicyEnforcementMode::UnprofiledDev);
    assert!(capsule.capabilities.required.is_empty());
    assert!(capsule.capabilities.granted.is_empty());
    assert!(capsule
        .limitations
        .contains(&"runtime_attestation_only".to_owned()));
    assert!(capsule.signature.is_none());
}

fn sample_capsule() -> ProofCapsule {
    let now = Utc::now();
    ProofCapsule {
        version: "1".into(),
        capsule_id: Uuid::new_v4(),
        subject: ProofSubject {
            run_id: Uuid::new_v4(),
            tool_name: "test_tool".into(),
            started_at: now,
            finished_at: now,
            duration_ms: 42,
        },
        tool: ToolIdentity {
            module_digest: sample_typed_digest(),
            module_name: "test.wasm".into(),
            entrypoint: "_start".into(),
        },
        input: InputIdentity {
            digest: sample_private_digest(),
            media_type: "application/json".into(),
            raw_included: false,
        },
        policy: PolicyProfileRef {
            profile_digest: None,
            profile_name: None,
            mode: PolicyEnforcementMode::UnprofiledDev,
        },
        capabilities: CapabilityEvidence {
            required: vec!["read".into()],
            granted: vec!["read".into()],
            mismatch: None,
            #[cfg(feature = "aeon-memory")]
            negotiation_rounds: None,
        },
        snapshot: None,
        failure: None,
        rollback: None,
        branches: None,
        redaction: RedactionReport {
            hashed_fields: vec![],
            truncated_fields: vec![],
            removed_fields: vec![],
            hmac_fields: vec!["input.digest".into()],
        },
        limitations: default_proof_capsule_limitations(),
        #[cfg(feature = "aeon-memory")]
        memory_evidence: None,
        #[cfg(feature = "aeon-memory")]
        memory_mode: None,
        signature: None,
    }
}

fn sample_receipt() -> ExecutionReceipt {
    let now = Utc::now();
    ExecutionReceipt {
        run_id: Uuid::new_v4(),
        started_at: now,
        finished_at: now,
        tool_name: "test_tool".into(),
        entrypoint: "_start".into(),
        module_sha256: "deadbeef".into(),
        input_sha256: sample_typed_digest(),
        input_bytes_len: 16,
        required_caps: vec!["read".into()],
        granted_caps: vec!["read".into()],
        policy_mode: PolicyEnforcementMode::UnprofiledDev,
        profile: None,
        snapshot: None,
        failure: None,
        rollback: None,
        branches: None,
        #[cfg(feature = "aeon-memory")]
        aeon_agent_id: None,
        #[cfg(feature = "aeon-memory")]
        aeon_session_id: None,
        #[cfg(feature = "aeon-memory")]
        negotiation_rounds: None,
        #[cfg(feature = "aeon-memory")]
        aeon_memory_evidence_digest: None,
    }
}

#[test]
fn proof_capsule_serde_round_trip() {
    let capsule = sample_capsule();
    let json = serde_json::to_string(&capsule).unwrap();
    let back: ProofCapsule = serde_json::from_str(&json).unwrap();
    assert_eq!(capsule.version, back.version);
    assert_eq!(capsule.capsule_id, back.capsule_id);
    assert_eq!(capsule.limitations, back.limitations);
}

#[test]
fn execution_receipt_serde_round_trip() {
    let receipt = sample_receipt();
    let json = serde_json::to_string(&receipt).unwrap();
    let back: ExecutionReceipt = serde_json::from_str(&json).unwrap();
    assert_eq!(receipt.run_id, back.run_id);
    assert_eq!(receipt.tool_name, back.tool_name);
}

#[test]
fn proof_capsule_unknown_json_field_is_forward_compatible() {
    let mut value: serde_json::Value = serde_json::to_value(sample_capsule()).unwrap();
    value["unknown_future_field"] = serde_json::json!("ignored");
    let result: Result<ProofCapsule, _> = serde_json::from_value(value);
    assert!(
        result.is_ok(),
        "unknown fields must not cause deserialization failure"
    );
}

#[test]
fn proof_capsule_version_deserializes_as_one() {
    let capsule = sample_capsule();
    let json = serde_json::to_string(&capsule).unwrap();
    let back: ProofCapsule = serde_json::from_str(&json).unwrap();
    assert_eq!(back.version, "1");
}

#[test]
fn unprofiled_dev_policy_mode_serializes_to_expected_string() {
    let mode = PolicyEnforcementMode::UnprofiledDev;
    let s = serde_json::to_string(&mode).unwrap();
    assert_eq!(s, r#""UnprofiledDev""#);
}

#[test]
fn input_identity_serializes_raw_included_field() {
    let input = InputIdentity {
        digest: sample_typed_digest(),
        media_type: "application/json".into(),
        raw_included: false,
    };
    let value: serde_json::Value = serde_json::to_value(&input).unwrap();
    assert!(value.get("raw_included").is_some());
}

#[test]
fn proof_scorecard_pass_can_be_true_when_limitations_are_present() {
    let scorecard = ProofScorecard {
        capsule_id: Uuid::new_v4(),
        version: "1".into(),
        has_signature: false,
        has_failure: false,
        has_rollback: false,
        redaction_count: 0,
        limitations_count: 6,
        scorecard_pass: true,
    };
    assert!(scorecard.scorecard_pass);
    assert!(scorecard.limitations_count > 0);
}

#[test]
fn proof_capsule_limitations_serializes_as_json_array() {
    let capsule = sample_capsule();
    let value: serde_json::Value = serde_json::to_value(&capsule).unwrap();
    assert!(value["limitations"].is_array());
    assert!(!value["limitations"].is_null());
}
