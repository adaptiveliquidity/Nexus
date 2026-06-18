use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::proof::ProofCapsule;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProofScorecard {
    pub capsule_id: Uuid,
    pub version: u32,
    pub has_signature: bool,
    pub has_failure: bool,
    pub has_rollback: bool,
    pub redaction_count: usize,
    pub limitations_count: usize,
    pub scorecard_pass: bool,
}

impl ProofScorecard {
    pub fn from_capsule(c: &ProofCapsule) -> Self {
        let limitations_count = c.limitations.len();

        Self {
            capsule_id: c.capsule_id,
            version: c.version,
            has_signature: c.signature.is_some(),
            has_failure: c.failure.is_some(),
            has_rollback: c.rollback.as_ref().map(|r| r.occurred).unwrap_or(false),
            redaction_count: c.redaction.hashed_fields.len()
                + c.redaction.hmac_fields.len()
                + c.redaction.truncated_fields.len()
                + c.redaction.removed_fields.len(),
            limitations_count,
            scorecard_pass: limitations_count > 0,
        }
    }
}
