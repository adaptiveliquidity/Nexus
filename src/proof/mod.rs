//! Runtime attestation capsule.

pub mod canonical;
pub mod digest;
pub mod receipt;
pub mod redaction;
pub mod schema;
pub mod scorecard;
pub mod signing;

pub use canonical::{canonical_bytes, capsule_digest};
pub use digest::digest_with_key;
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
pub use signing::{sign_capsule, verify_capsule, ProofSigningConfig};
