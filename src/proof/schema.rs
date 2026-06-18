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
