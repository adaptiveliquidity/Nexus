use aeon_nexus_bridge::{
    canonical_sha256_digest, content_digest, hmac_agent_id, memory_evidence_digest,
    verify_agent_id_hmac, AgentSessionMapping, MemoryEvidence, MemoryEvidenceHit, MemoryScore,
    HMAC_SHA256_ALGORITHM, SHA256_ALGORITHM,
};
use serde_json::json;

fn sample_evidence() -> MemoryEvidence {
    let agent_handle = hmac_agent_id(b"operator-key", "aeon-agent-1");
    MemoryEvidence::new(
        agent_handle,
        Some("session-7".to_owned()),
        vec![
            MemoryEvidenceHit::new("memory-a", "alpha content", Some(0.91)).unwrap(),
            MemoryEvidenceHit::new("memory-b", "beta content", Some(0.73)).unwrap(),
        ],
    )
}

#[test]
fn digest_is_deterministic_for_reordered_fields_and_maps() {
    let evidence = sample_evidence();
    let typed_digest = memory_evidence_digest(&evidence).unwrap();

    let value_a = json!({
        "version": MemoryEvidence::VERSION,
        "agent_handle": hmac_agent_id(b"operator-key", "aeon-agent-1"),
        "session_id": "session-7",
        "injected_hits": [
            {
                "memory_id": "memory-a",
                "score": MemoryScore::new(0.91).unwrap(),
                "content_digest": content_digest("alpha content"),
            },
            {
                "memory_id": "memory-b",
                "score": MemoryScore::new(0.73).unwrap(),
                "content_digest": content_digest("beta content"),
            }
        ],
        "metadata": {
            "plane": "aeon-iq",
            "source": "recall"
        }
    });
    let value_b = json!({
        "metadata": {
            "source": "recall",
            "plane": "aeon-iq"
        },
        "injected_hits": [
            {
                "content_digest": content_digest("alpha content"),
                "score": MemoryScore::new(0.91).unwrap(),
                "memory_id": "memory-a",
            },
            {
                "content_digest": content_digest("beta content"),
                "score": MemoryScore::new(0.73).unwrap(),
                "memory_id": "memory-b",
            }
        ],
        "session_id": "session-7",
        "agent_handle": hmac_agent_id(b"operator-key", "aeon-agent-1"),
        "version": MemoryEvidence::VERSION
    });

    assert_eq!(
        canonical_sha256_digest(&value_a).unwrap(),
        canonical_sha256_digest(&value_b).unwrap()
    );
    assert_eq!(typed_digest, canonical_sha256_digest(&evidence).unwrap());
}

#[test]
fn digest_changes_when_memory_id_or_score_changes() {
    let evidence = sample_evidence();
    let original = memory_evidence_digest(&evidence).unwrap();

    let mut changed_id = evidence.clone();
    changed_id.injected_hits[0].memory_id = "memory-z".to_owned();
    assert_ne!(original, memory_evidence_digest(&changed_id).unwrap());

    let mut changed_score = evidence;
    changed_score.injected_hits[0].score = Some(MemoryScore::new(0.92).unwrap());
    assert_ne!(original, memory_evidence_digest(&changed_score).unwrap());
}

#[test]
fn hmac_agent_handle_is_stable_and_verifiable() {
    let handle = hmac_agent_id(b"key", "The quick brown fox jumps over the lazy dog");

    assert_eq!(handle.algorithm, HMAC_SHA256_ALGORITHM);
    assert!(!handle.public_recomputable);
    assert_eq!(
        handle.value,
        "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
    );
    assert!(verify_agent_id_hmac(
        b"key",
        "The quick brown fox jumps over the lazy dog",
        &handle
    ));
    assert!(!verify_agent_id_hmac(
        b"different-key",
        "The quick brown fox jumps over the lazy dog",
        &handle
    ));
    assert!(!verify_agent_id_hmac(b"key", "different-agent", &handle));
}

#[test]
fn memory_evidence_round_trips_through_serde() {
    let evidence = sample_evidence();
    let json = serde_json::to_string(&evidence).unwrap();
    let decoded: MemoryEvidence = serde_json::from_str(&json).unwrap();

    assert_eq!(decoded, evidence);
}

#[test]
fn memory_evidence_ref_contains_capsule_ready_digest() {
    let evidence = sample_evidence();
    let evidence_ref = evidence.to_ref().unwrap();

    assert_eq!(
        evidence_ref.digest,
        memory_evidence_digest(&evidence).unwrap()
    );
    assert_eq!(evidence_ref.digest.algorithm, SHA256_ALGORITHM);
    assert_eq!(evidence_ref.agent_handle, evidence.agent_handle);
    assert_eq!(evidence_ref.session_id.as_deref(), Some("session-7"));
    assert_eq!(evidence_ref.injected_count, 2);
}

#[test]
fn agent_session_mapping_hmacs_the_aeon_agent_id() {
    let mapping =
        AgentSessionMapping::new("nexus-worker", Some("session-7".to_owned()), "aeon-agent-1");

    assert_eq!(mapping.nexus_agent_id(), "nexus-worker");
    assert_eq!(mapping.session_id(), Some("session-7"));
    assert_eq!(mapping.aeon_agent_id(), "aeon-agent-1");
    assert_eq!(
        mapping.agent_handle(b"operator-key"),
        hmac_agent_id(b"operator-key", "aeon-agent-1")
    );
}
