use crate::proof::schema::{ProofCapsule, ProofScorecard};

impl ProofScorecard {
    pub fn from_capsule(c: &ProofCapsule) -> Self {
        let limitations_count = c.limitations.len();
        let has_signature = c.signature.is_some();
        Self {
            capsule_id: c.capsule_id,
            version: c.version.clone(),
            has_signature,
            has_failure: c.failure.is_some(),
            has_rollback: c.rollback.as_ref().map(|r| r.occurred).unwrap_or(false),
            redaction_count: c.redaction.hashed_fields.len()
                + c.redaction.hmac_fields.len()
                + c.redaction.truncated_fields.len()
                + c.redaction.removed_fields.len(),
            limitations_count,
            scorecard_pass: has_signature,
        }
    }
}
