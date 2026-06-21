use chrono::{DateTime, Utc};
use nexus::proof::{
    canonical_bytes, capsule_digest, default_proof_capsule_limitations, digest_with_key,
    CapabilityEvidence, InputIdentity, PolicyEnforcementMode, PolicyProfileRef, ProofCapsule,
    ProofHmacKey, ProofSubject, RedactionReport, SignatureEnvelope, ToolIdentity, TypedDigest,
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

fn sample_signature() -> SignatureEnvelope {
    SignatureEnvelope {
        signer: "local-test-signer".to_owned(),
        key_id: "test-key-1".to_owned(),
        signature: "base64-signature".to_owned(),
        signed_payload_digest: typed_digest("sha256", "signed-payload", true),
    }
}

fn sample_capsule(signature: Option<SignatureEnvelope>) -> ProofCapsule {
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
            profile_digest: Some(typed_digest("sha256", "profile-digest", true)),
            profile_name: Some("strict-readonly".to_owned()),
            mode: PolicyEnforcementMode::ProfileEnforcedMcpCapabilitiesOnly,
        },
        capabilities: CapabilityEvidence {
            required: vec!["fs:read:/input/orders.csv".to_owned()],
            granted: vec!["fs:read:/input/orders.csv".to_owned()],
            mismatch: None,
            #[cfg(feature = "aeon-memory")]
            negotiation_rounds: None,
        },
        snapshot: None,
        failure: None,
        rollback: None,
        branches: None,
        redaction: RedactionReport {
            hashed_fields: vec!["input.body".to_owned()],
            truncated_fields: Vec::new(),
            removed_fields: Vec::new(),
            hmac_fields: vec!["input.digest".to_owned()],
        },
        limitations: default_proof_capsule_limitations(),
        #[cfg(feature = "aeon-memory")]
        memory_evidence: None,
        #[cfg(feature = "aeon-memory")]
        memory_mode: None,
        signature,
    }
}

#[test]
fn sha256_public_round_trip_uses_known_hex() {
    let digest = TypedDigest::sha256_public(b"abc");

    assert_eq!(digest.algorithm, "sha256");
    assert_eq!(
        digest.value,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert!(digest.public_recomputable);
}

#[test]
fn hmac_sha256_private_differs_from_public_sha256_for_same_data() {
    let private = TypedDigest::hmac_sha256_private(b"key", b"abc");
    let public = TypedDigest::sha256_public(b"abc");

    assert_eq!(private.algorithm, "hmac-sha256");
    assert!(!private.public_recomputable);
    assert_ne!(private.value, public.value);
}

#[test]
fn disabled_proof_hmac_key_redacts_digest() {
    let digest = digest_with_key(&ProofHmacKey::Disabled, b"low-entropy-secret");

    assert_eq!(digest, TypedDigest::redacted());
    assert_eq!(digest.algorithm, "none");
    assert!(!digest.public_recomputable);
}

#[test]
fn from_env_proof_hmac_key_uses_configured_secret() {
    const ENV_KEY: &str = "NEXUS_TEST_PROOF_HMAC_KEY";
    std::env::set_var(ENV_KEY, "key");

    let digest = digest_with_key(
        &ProofHmacKey::FromEnv(ENV_KEY.to_owned()),
        b"The quick brown fox jumps over the lazy dog",
    );

    assert_eq!(digest.algorithm, "hmac-sha256");
    assert_eq!(
        digest.value,
        "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
    );
    assert!(!digest.public_recomputable);
}

#[test]
fn canonical_bytes_ignore_signature_field() {
    let signed = sample_capsule(Some(sample_signature()));
    let unsigned = sample_capsule(None);

    assert_eq!(
        canonical_bytes(&signed).unwrap(),
        canonical_bytes(&unsigned).unwrap()
    );
}

#[test]
fn capsule_digest_is_publicly_recomputable_sha256() {
    let digest = capsule_digest(&sample_capsule(Some(sample_signature()))).unwrap();

    assert_eq!(digest.algorithm, "sha256");
    assert!(digest.public_recomputable);
}
