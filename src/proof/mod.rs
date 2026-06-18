//! Runtime attestation capsule.

pub mod canonical;
pub mod digest;
pub mod redaction;
pub mod receipt;
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
