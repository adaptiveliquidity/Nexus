use chrono::{DateTime, Utc};
use nexus::proof::{
    BranchRaceEvidence, CapabilityEvidence, DigestMode, ExecutionReceipt, FailureEvidence,
    InputIdentity, PolicyEnforcementMode, PolicyProfileRef, ProofCapsule, ProofSubject,
    RedactionReport, RollbackEvidence, SignatureEnvelope, SnapshotEvidence, SnapshotKind,
    ToolIdentity, TypedDigest, PROOF_CAPSULE_VERSION,
};
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

fn sample_snapshot() -> SnapshotEvidence {
    SnapshotEvidence {
        snapshot_id: uuid("11111111-1111-4111-8111-111111111111"),
        snapshot_kind: SnapshotKind::LatestRuntime,
        memory_digest: typed_digest("sha256", "memory-digest", true),
        original_size: 8192,
        compressed_size: 2048,
    }
}

fn sample_failure() -> FailureEvidence {
    FailureEvidence {
        failure_category: "trap".to_owned(),
        requires_rollback: true,
        deterministic: Some(false),
        error_summary: "guest trapped after capability denial".to_owned(),
    }
}

fn sample_rollback() -> RollbackEvidence {
    RollbackEvidence {
        occurred: true,
        from_snapshot_id: uuid("22222222-2222-4222-8222-222222222222"),
        reason: "restore latest runtime state".to_owned(),
    }
}

fn sample_branches() -> BranchRaceEvidence {
    BranchRaceEvidence {
        source_snapshot_id: Some(uuid("33333333-3333-4333-8333-333333333333")),
        winner_branch_id: uuid("44444444-4444-4444-8444-444444444444"),
        branches_tried: 3,
        branches_succeeded: 1,
    }
}

fn sample_capsule() -> ProofCapsule {
    ProofCapsule {
        version: PROOF_CAPSULE_VERSION,
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
            digest: typed_digest("hmac-sha256", "input-hmac", false),
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
            mismatch: Some(vec!["net:deny".to_owned()]),
        },
        snapshot: Some(sample_snapshot()),
        failure: Some(sample_failure()),
        rollback: Some(sample_rollback()),
        branches: Some(sample_branches()),
        redaction: RedactionReport {
            hashed_fields: vec!["input.body".to_owned()],
            truncated_fields: vec!["failure.error_summary".to_owned()],
            removed_fields: vec!["env.SECRET_TOKEN".to_owned()],
            hmac_fields: vec!["input.digest".to_owned()],
        },
        limitations: vec![
            "runtime attestation only".to_owned(),
            "does not prove correct execution".to_owned(),
        ],
        signature: Some(SignatureEnvelope {
            signer: "local-test-signer".to_owned(),
            key_id: "test-key-1".to_owned(),
            signature: "base64-signature".to_owned(),
            signed_payload_digest: typed_digest("sha256", "signed-payload", true),
        }),
    }
}

fn sample_receipt() -> ExecutionReceipt {
    ExecutionReceipt {
        run_id: uuid("77777777-7777-4777-8777-777777777777"),
        started_at: timestamp("2026-06-17T12:00:00Z"),
        finished_at: timestamp("2026-06-17T12:00:01Z"),
        tool_name: "csv_reporter".to_owned(),
        entrypoint: "_start".to_owned(),
        module_sha256: "module-sha256".to_owned(),
        input_sha256: "input-sha256".to_owned(),
        input_bytes_len: 4096,
        required_caps: vec!["fs:read:/input/orders.csv".to_owned()],
        granted_caps: vec!["fs:read:/input/orders.csv".to_owned()],
        policy_mode: PolicyEnforcementMode::ProfileLoadedMcp,
        profile: Some(("strict-readonly".to_owned(), "profile-sha256".to_owned())),
        snapshot: Some(sample_snapshot()),
        failure: Some(sample_failure()),
        rollback: Some(sample_rollback()),
        branches: Some(sample_branches()),
    }
}

#[test]
fn proof_capsule_serde_round_trip() {
    let capsule = sample_capsule();
    let json = serde_json::to_string(&capsule).unwrap();
    let back: ProofCapsule = serde_json::from_str(&json).unwrap();

    assert_eq!(back, capsule);
}

#[test]
fn proof_execution_receipt_serde_round_trip() {
    let receipt = sample_receipt();
    let json = serde_json::to_string(&receipt).unwrap();
    let back: ExecutionReceipt = serde_json::from_str(&json).unwrap();

    assert_eq!(back, receipt);
}

#[test]
fn proof_capsule_unknown_json_field_is_forward_compatible() {
    let mut value = serde_json::to_value(sample_capsule()).unwrap();
    value["unknown_future_field"] = serde_json::json!("ignored");

    let back: ProofCapsule = serde_json::from_value(value).unwrap();

    assert_eq!(back.version, PROOF_CAPSULE_VERSION);
}

#[test]
fn proof_capsule_version_serializes_as_one() {
    let value = serde_json::to_value(sample_capsule()).unwrap();

    assert_eq!(PROOF_CAPSULE_VERSION, 1);
    assert_eq!(value["version"], serde_json::json!(1));
}

#[test]
fn proof_digest_mode_is_part_of_the_schema_contract() {
    assert_eq!(
        serde_json::to_string(&DigestMode::RedactedNoDigest).unwrap(),
        r#""RedactedNoDigest""#
    );
}
