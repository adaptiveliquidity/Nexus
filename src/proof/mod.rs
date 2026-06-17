//! Runtime attestation capsule.

pub mod receipt;
pub mod schema;

pub use receipt::{
    ActiveCapabilityProfile, ExecutionReceipt, FailureModeLite, McpProofReference,
    ProofCaptureMode, ProofHmacKey,
};
pub use schema::{
    BranchRaceEvidence, CapabilityEvidence, DigestMode, FailureEvidence, InputIdentity,
    PolicyEnforcementMode, PolicyProfileRef, ProofCapsule, ProofScorecard, ProofSubject,
    RedactionReport, RollbackEvidence, SignatureEnvelope, SnapshotEvidence, SnapshotKind,
    ToolIdentity, TypedDigest,
};
