use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::proof::schema::{
    BranchRaceEvidence, FailureEvidence, PolicyEnforcementMode, RollbackEvidence, SnapshotEvidence,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    pub profile: Option<(String, String)>,
    pub snapshot: Option<SnapshotEvidence>,
    pub failure: Option<FailureEvidence>,
    pub rollback: Option<RollbackEvidence>,
    pub branches: Option<BranchRaceEvidence>,
}
