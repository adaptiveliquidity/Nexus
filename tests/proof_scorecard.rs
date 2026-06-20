use chrono::{DateTime, Utc};
use nexus::proof::{
    CapabilityEvidence, InputIdentity, PolicyEnforcementMode, PolicyProfileRef, ProofCapsule,
    ProofScorecard, ProofSubject, RedactionReport, RollbackEvidence, ToolIdentity, TypedDigest,
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
            digest: typed_digest("hmac-sha256", "input-hmac", false),
            media_type: "application/json".to_owned(),
            raw_included: false,
        },
        policy: PolicyProfileRef {
            profile_digest: None,
            profile_name: Some("strict-readonly".to_owned()),
            mode: PolicyEnforcementMode::ProfileLoadedMcp,
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
            hashed_fields: Vec::new(),
            truncated_fields: Vec::new(),
            removed_fields: Vec::new(),
            hmac_fields: Vec::new(),
        },
        limitations: Vec::new(),
        #[cfg(feature = "aeon-memory")]
        memory_evidence: None,
        #[cfg(feature = "aeon-memory")]
        memory_mode: None,
        signature: None,
    }
}

#[test]
fn scorecard_passes_when_limitations_are_present() {
    let mut capsule = sample_capsule();
    capsule.limitations = vec!["runtime attestation only".to_owned()];

    let scorecard = ProofScorecard::from_capsule(&capsule);

    assert!(scorecard.scorecard_pass);
    assert_eq!(scorecard.limitations_count, 1);
}

#[test]
fn scorecard_reports_rollback_when_rollback_occurred() {
    let mut capsule = sample_capsule();
    capsule.rollback = Some(RollbackEvidence {
        occurred: true,
        from_snapshot_id: Some(uuid("22222222-2222-4222-8222-222222222222")),
        reason: Some("restore latest runtime state".to_owned()),
    });

    let scorecard = ProofScorecard::from_capsule(&capsule);

    assert!(scorecard.has_rollback);
}

#[test]
fn scorecard_redaction_count_sums_all_redaction_lists() {
    let mut capsule = sample_capsule();
    capsule.redaction = RedactionReport {
        hashed_fields: vec!["input.body".to_owned(), "tool.env".to_owned()],
        hmac_fields: vec!["input.digest".to_owned()],
        truncated_fields: vec!["failure.error_summary".to_owned()],
        removed_fields: vec!["env.SECRET_TOKEN".to_owned(), "env.API_KEY".to_owned()],
    };

    let scorecard = ProofScorecard::from_capsule(&capsule);

    assert_eq!(scorecard.redaction_count, 6);
}
