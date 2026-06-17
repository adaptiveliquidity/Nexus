//! Runtime attestation capsule.

pub mod receipt;
pub mod schema;

pub use receipt::ExecutionReceipt;
pub use schema::{
    BranchRaceEvidence, CapabilityEvidence, DigestMode, FailureEvidence, InputIdentity,
    PolicyEnforcementMode, PolicyProfileRef, ProofCapsule, ProofSubject, RedactionReport,
    RollbackEvidence, SignatureEnvelope, SnapshotEvidence, SnapshotKind, ToolIdentity, TypedDigest,
    PROOF_CAPSULE_VERSION,
};
