//! Runtime attestation capsule.

pub mod canonical;
pub mod digest;
pub mod redaction;
pub mod receipt {
    include!("receipt.rs");

    /// Controls how low-entropy sensitive values are digested. See RFC 0005 §6.
    #[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    pub enum ProofHmacKey {
        Disabled,
        FromEnv(String),
        EphemeralTestOnly,
    }
}
pub mod schema;
pub mod scorecard;

pub use canonical::{canonical_bytes, capsule_digest};
pub use digest::digest_with_key;
pub use receipt::{ExecutionReceipt, ProofHmacKey};
pub use schema::{
    BranchRaceEvidence, CapabilityEvidence, DigestMode, FailureEvidence, InputIdentity,
    PolicyEnforcementMode, PolicyProfileRef, ProofCapsule, ProofSubject, RedactionReport,
    RollbackEvidence, SignatureEnvelope, SnapshotEvidence, SnapshotKind, ToolIdentity, TypedDigest,
    PROOF_CAPSULE_VERSION,
};
pub use scorecard::ProofScorecard;
