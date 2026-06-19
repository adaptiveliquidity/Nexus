use chrono::{DateTime, Utc};
use nexus::proof::{
    redaction::{RedactionField, RedactionPolicy},
    CapabilityEvidence, InputIdentity, PolicyEnforcementMode, PolicyProfileRef, ProofCapsule,
    ProofHmacKey, ProofSubject, RedactionReport, ToolIdentity, TypedDigest,
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

fn sample_capsule(redaction: RedactionReport) -> ProofCapsule {
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
            required: vec!["fs:read:sandbox-input".to_owned()],
            granted: vec!["fs:read:sandbox-input".to_owned()],
            mismatch: None,
        },
        snapshot: None,
        failure: None,
        rollback: None,
        branches: None,
        redaction,
        limitations: vec!["runtime attestation only".to_owned()],
        signature: None,
    }
}

#[test]
fn redact_env_value_never_leaks_input_value() {
    let policy = RedactionPolicy::new(ProofHmacKey::Disabled);
    let secret_value = "sensitive-env-value-123";

    let (redacted, field) = policy.redact_env_value(secret_value);

    assert_eq!(redacted, "[ENV_REDACTED]");
    assert_eq!(field, RedactionField::Removed);
    assert!(!redacted.contains(secret_value));
}

#[test]
fn redact_error_truncates_to_at_most_256_chars() {
    let policy = RedactionPolicy::new(ProofHmacKey::Disabled);
    let err = "x".repeat(500);

    let (redacted, field) = policy.redact_error(&err);

    assert_eq!(field, RedactionField::Truncated);
    assert!(redacted.chars().count() <= 256);
    assert_eq!(redacted.chars().count(), 256);
}

#[test]
fn redact_token_does_not_leak_prefix() {
    let policy = RedactionPolicy::new(ProofHmacKey::Disabled);

    let (redacted, field) = policy.redact_token("abcdefghijklmno");

    // Token must be fully opaque — no leading characters exposed.
    assert_eq!(redacted, "[TOKEN_REDACTED]");
    assert_eq!(field, RedactionField::HmacOrPlaceholder);
    assert!(!redacted.contains("abcdefgh"), "token prefix must not appear in redacted output");
}

#[test]
fn redact_path_with_disabled_key_uses_placeholder_not_sha256() {
    let policy = RedactionPolicy::new(ProofHmacKey::Disabled);

    let (redacted, field) = policy.redact_path("/sensitive/path.txt");

    assert_eq!(redacted, "[PATH_REDACTED]");
    assert_eq!(field, RedactionField::HmacOrPlaceholder);
    assert_ne!(
        redacted,
        "8ff7f9cb5303c0a99ad9085c192ebba97f9bbc6eaf66912d3c553307ec2d93ea"
    );
}

#[test]
fn proof_capsule_json_excludes_forbidden_redaction_fields() {
    let policy = RedactionPolicy::new(ProofHmacKey::Disabled);
    let redaction = policy.build_report(vec![
        ("env.provider_key".to_owned(), RedactionField::Removed),
        ("auth.session".to_owned(), RedactionField::Truncated),
        (
            "fs.input_path".to_owned(),
            RedactionField::HmacOrPlaceholder,
        ),
    ]);
    let capsule = sample_capsule(redaction);

    let json = serde_json::to_string(&capsule).unwrap();

    for forbidden in [
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "NEXUS_AGENTD_AUTH_TOKEN",
        "raw_token",
        "private_key",
        "/home/",
        "C:\\Users\\",
        "BEGIN PRIVATE KEY",
        "preview_base64",
    ] {
        assert!(
            !json.contains(forbidden),
            "capsule JSON leaked forbidden field/value: {forbidden}"
        );
    }
}

#[test]
fn build_report_sorts_fields_by_redaction_type() {
    let policy = RedactionPolicy::new(ProofHmacKey::Disabled);

    let report = policy.build_report(vec![
        (
            "failure.error_summary".to_owned(),
            RedactionField::Truncated,
        ),
        ("env.secret".to_owned(), RedactionField::Removed),
        ("input.path".to_owned(), RedactionField::HmacOrPlaceholder),
    ]);

    assert_eq!(report.hashed_fields, Vec::<String>::new());
    assert_eq!(
        report.truncated_fields,
        vec!["failure.error_summary".to_owned()]
    );
    assert_eq!(report.removed_fields, vec!["env.secret".to_owned()]);
    assert_eq!(report.hmac_fields, vec!["input.path".to_owned()]);
}
