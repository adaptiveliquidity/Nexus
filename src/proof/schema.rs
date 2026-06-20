use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypedDigest {
    pub algorithm: String,
    pub value: String,
    pub public_recomputable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DigestMode {
    Sha256Public,
    HmacSha256Private,
    RedactedNoDigest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SnapshotKind {
    LatestRuntime,
    EmptyBaseline,
    Diff,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProofSubject {
    pub run_id: Uuid,
    pub tool_name: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolIdentity {
    pub module_digest: TypedDigest,
    pub module_name: String,
    pub entrypoint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputIdentity {
    pub digest: TypedDigest,
    pub media_type: String,
    pub raw_included: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolicyEnforcementMode {
    UnprofiledDev,
    ProfileValidatedOnly,
    ProfileLoadedMcp,
    ProfileEnforcedMcpCapabilitiesOnly,
    /// RESERVED: never emitted in v1
    ProfileEnforcedMcpToolAndCapability,
    /// RESERVED: never emitted in v1
    ProfileEnforcedRuntime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyProfileRef {
    pub profile_digest: Option<TypedDigest>,
    pub profile_name: Option<String>,
    pub mode: PolicyEnforcementMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityEvidence {
    pub required: Vec<String>,
    pub granted: Vec<String>,
    pub mismatch: Option<Vec<String>>,
    #[cfg(feature = "aeon-memory")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub negotiation_rounds: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotEvidence {
    pub snapshot_id: Uuid,
    pub snapshot_kind: SnapshotKind,
    pub memory_digest: TypedDigest,
    pub original_size: u64,
    pub compressed_size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureEvidence {
    pub failure_category: String,
    pub requires_rollback: bool,
    pub deterministic: Option<bool>,
    pub error_summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RollbackEvidence {
    pub occurred: bool,
    pub from_snapshot_id: Option<Uuid>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchRaceEvidence {
    pub source_snapshot_id: Option<Uuid>,
    pub winner_branch_id: String,
    pub branches_tried: u32,
    pub branches_succeeded: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedactionReport {
    pub hashed_fields: Vec<String>,
    pub truncated_fields: Vec<String>,
    pub removed_fields: Vec<String>,
    pub hmac_fields: Vec<String>,
}

/// Records whether AEON-IQ memory was consulted and what the result was.
///
/// Not feature-gated so non-aeon-memory builds can still reference the type
/// (e.g. for display or configuration). Fields that carry actual evidence are
/// gated individually on the `aeon-memory` feature in `ProofCapsule`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum MemoryAttestationMode {
    /// Memory was not configured or not consulted (default).
    #[default]
    Advisory,
    /// Memory was consulted and evidence is cryptographically attested.
    Attested,
    /// Memory was consulted but the evidence could not be fully attested.
    Degraded,
    /// Memory sidecar is not configured; no HMAC key present.
    Absent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignatureEnvelope {
    pub signer: String,
    pub key_id: String,
    pub signature: String,
    pub signed_payload_digest: TypedDigest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProofCapsule {
    pub version: String,
    pub capsule_id: Uuid,
    pub subject: ProofSubject,
    pub tool: ToolIdentity,
    pub input: InputIdentity,
    pub policy: PolicyProfileRef,
    pub capabilities: CapabilityEvidence,
    pub snapshot: Option<SnapshotEvidence>,
    pub failure: Option<FailureEvidence>,
    pub rollback: Option<RollbackEvidence>,
    pub branches: Option<BranchRaceEvidence>,
    pub redaction: RedactionReport,
    pub limitations: Vec<String>,
    #[cfg(feature = "aeon-memory")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_evidence: Option<aeon_nexus_bridge::MemoryEvidenceRef>,
    #[cfg(feature = "aeon-memory")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mode: Option<MemoryAttestationMode>,
    pub signature: Option<SignatureEnvelope>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProofScorecard {
    pub capsule_id: Uuid,
    pub version: String,
    pub has_signature: bool,
    pub has_failure: bool,
    pub has_rollback: bool,
    pub redaction_count: usize,
    pub limitations_count: usize,
    pub scorecard_pass: bool,
}
