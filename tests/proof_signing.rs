use chrono::{DateTime, Utc};
use ed25519_dalek::{SigningKey, VerifyingKey};
use nexus::proof::{
    sign_capsule, verify_capsule, CapabilityEvidence, InputIdentity, PolicyEnforcementMode,
    PolicyProfileRef, ProofCapsule, ProofScorecard, ProofSubject, RedactionReport, ToolIdentity,
    TypedDigest,
};
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
