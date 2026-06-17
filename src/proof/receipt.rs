use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::proof::schema::{
    BranchRaceEvidence, PolicyEnforcementMode, ProofScorecard, SnapshotEvidence, TypedDigest,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailureModeLite {
    pub category: String,
    pub requires_rollback: bool,
    pub is_deterministic: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveCapabilityProfile {
    pub manifest_name: String,
    pub source_digest: TypedDigest,
    pub source_path_redacted: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpProofReference {
    pub capsule_digest: TypedDigest,
    pub artifact_id: Option<String>,
    pub inline_summary: ProofScorecard,
}

/// Proof HMAC key source. `FromEnv` holds the env-var NAME, not the value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProofHmacKey {
    Disabled,
    FromEnv(String),
    EphemeralTestOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProofCaptureMode {
    Disabled,
    ReceiptOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionReceipt {
    pub run_id: Uuid,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub tool_name: String,
    pub entrypoint: String,
    pub module_sha256: String,
    pub input_sha256: String,
    pub input_bytes_len: usize,
    pub required_caps: Vec<String>,
    pub granted_caps: Vec<String>,
    pub policy_mode: PolicyEnforcementMode,
    /// (profile_name, toml_sha256)
    pub profile: Option<(String, String)>,
    pub snapshot: Option<SnapshotEvidence>,
    pub failure: Option<FailureModeLite>,
    /// (occurred, from_snapshot_id, reason)
    pub rollback: Option<(bool, Uuid, String)>,
    pub branches: Option<BranchRaceEvidence>,
}
