use chrono::Utc;
use nexus::proof::default_proof_capsule_limitations;
#[cfg(feature = "aeon-memory")]
use nexus::proof::schema::MemoryAttestationMode;
use nexus::proof::schema::{
    CapabilityEvidence, InputIdentity, PolicyEnforcementMode, PolicyProfileRef, ProofCapsule,
    ProofSubject, RedactionReport, ToolIdentity, TypedDigest,
};
#[cfg(feature = "aeon-memory")]
use nexus::{AeonConfig, HypervisorConfig, NexusHypervisor, ToolDefinition};
use uuid::Uuid;

fn typed_digest() -> TypedDigest {
    TypedDigest {
        algorithm: "sha256".to_string(),
        value: "abc123".to_string(),
        public_recomputable: true,
    }
}

fn private_input_digest() -> TypedDigest {
    TypedDigest {
        algorithm: "hmac-sha256".to_string(),
        value: "input-hmac".to_string(),
        public_recomputable: false,
    }
}

fn sample_capsule_without_memory() -> ProofCapsule {
    let now = Utc::now();

    ProofCapsule {
        version: "1".to_string(),
        capsule_id: Uuid::new_v4(),
        subject: ProofSubject {
            run_id: Uuid::new_v4(),
            tool_name: "conformance_tool".to_string(),
            started_at: now,
            finished_at: now,
            duration_ms: 1,
        },
        tool: ToolIdentity {
            module_digest: typed_digest(),
            module_name: "conformance.wasm".to_string(),
            entrypoint: "_start".to_string(),
        },
        input: InputIdentity {
            digest: private_input_digest(),
            media_type: "application/json".to_string(),
            raw_included: false,
        },
        policy: PolicyProfileRef {
            profile_digest: None,
            profile_name: None,
            mode: PolicyEnforcementMode::UnprofiledDev,
        },
        capabilities: CapabilityEvidence {
            required: Vec::new(),
            granted: Vec::new(),
            mismatch: None,
            #[cfg(feature = "aeon-memory")]
            negotiation_rounds: None,
        },
        snapshot: None,
        failure: None,
        rollback: None,
        branches: None,
        redaction: RedactionReport {
            hashed_fields: Vec::new(),
            truncated_fields: Vec::new(),
            removed_fields: Vec::new(),
            hmac_fields: vec!["input.digest".to_owned()],
        },
        limitations: default_proof_capsule_limitations(),
        #[cfg(feature = "aeon-memory")]
        memory_evidence: None,
        #[cfg(feature = "aeon-memory")]
        memory_mode: None,
        signature: None,
    }
}

#[cfg(feature = "aeon-memory")]
fn trivial_wasm() -> Vec<u8> {
    wat::parse_str(r#"(module (memory (export "memory") 1) (func (export "_start")))"#).unwrap()
}

#[cfg(feature = "aeon-memory")]
fn trap_wasm() -> Vec<u8> {
    wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (func (export "_start")
                i32.const 1
                i32.const 0
                i32.div_s
                drop))"#,
    )
    .unwrap()
}

#[cfg(feature = "aeon-memory")]
fn unreachable_aeon_config() -> AeonConfig {
    AeonConfig {
        enabled: true,
        base_url: "http://127.0.0.1:1".to_string(),
        agent_id: "agent-1".to_string(),
        session_id: Some("session-1".to_string()),
        timeout_ms: 1,
        management_key: Some(format!("test-management-key-{}", Uuid::new_v4())),
        hmac_key: None,
    }
}

#[cfg(feature = "aeon-memory")]
#[test]
fn feature_capsule_without_memory_omits_aeon_keys() {
    let capsule = sample_capsule_without_memory();
    assert!(capsule.memory_evidence.is_none());
    assert!(capsule.memory_mode.is_none());
    assert!(capsule.capabilities.negotiation_rounds.is_none());

    let value = serde_json::to_value(capsule).unwrap();
    assert!(value.get("memory_evidence").is_none());
    assert!(value.get("memory_mode").is_none());
    assert!(value["capabilities"].get("negotiation_rounds").is_none());
}

#[cfg(not(feature = "aeon-memory"))]
#[test]
fn default_build_capsule_json_has_no_aeon_keys() {
    // This is the default-off shape gate: when aeon-memory is not compiled,
    // proof JSON must not expose AEON-IQ memory or negotiation fields at all.
    let value = serde_json::to_value(sample_capsule_without_memory()).unwrap();
    assert!(value.get("memory_evidence").is_none());
    assert!(value.get("memory_mode").is_none());
    assert!(value["capabilities"].get("negotiation_rounds").is_none());
}

#[cfg(feature = "aeon-memory")]
#[tokio::test]
async fn aeon_outage_is_fail_open_for_proof_execution() {
    let config = HypervisorConfig {
        aeon_config: Some(unreachable_aeon_config()),
        ..HypervisorConfig::default()
    };
    let hypervisor = NexusHypervisor::new(config).unwrap();
    let tool = ToolDefinition::new("aeon_outage_trap".to_string(), trap_wasm())
        .with_aeon_context(Some("agent-1".to_string()), Some("session-1".to_string()));

    let (output, capsule) = hypervisor
        .execute_tool_proof(tool, serde_json::json!({}))
        .await
        .expect("AEON-IQ outage must not block proof execution");

    assert!(!output.success);
    assert_ne!(capsule.capsule_id, Uuid::nil());
    assert!(matches!(
        capsule.memory_mode,
        Some(MemoryAttestationMode::Advisory) | Some(MemoryAttestationMode::Absent)
    ));
}

#[cfg(feature = "aeon-memory")]
#[tokio::test]
async fn honest_proof_modes_cover_absent_and_advisory() {
    // Mapping locked by Phase 9:
    // - Absent: no HMAC key exists, so no memory evidence can be bound.
    // - Advisory: AEON correlation is present, but no attested evidence ref is
    //   embedded in this capsule.
    // - Degraded: evidence construction or verification fails.
    // - Attested: an HMAC-bound MemoryEvidenceRef is embedded.
    let config = unreachable_aeon_config();
    let (evidence, mode) =
        nexus::aeon::build_memory_evidence_ref(&config, &[], Some("session-1".to_string()));
    assert!(evidence.is_none());
    assert_eq!(mode, MemoryAttestationMode::Absent);

    let hypervisor = NexusHypervisor::new(HypervisorConfig::default()).unwrap();
    let tool = ToolDefinition::new("aeon_advisory_noop".to_string(), trivial_wasm())
        .with_aeon_context(Some("agent-1".to_string()), Some("session-1".to_string()));

    let (_output, capsule) = hypervisor
        .execute_tool_proof(tool, serde_json::json!({}))
        .await
        .unwrap();

    assert!(capsule.memory_evidence.is_none());
    assert_eq!(capsule.memory_mode, Some(MemoryAttestationMode::Advisory));
}
