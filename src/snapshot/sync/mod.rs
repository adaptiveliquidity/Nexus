//! Snapshot synchronization (RFC 0001).
//!
//! Phase 1 only: content-addressed snapshot digests. Transport, lineage heads,
//! anti-replay, and restore authorization are deferred to later RFC-0001 PRs and
//! are intentionally absent here.

pub mod digest;
pub mod protocol;
pub mod transport;

pub use digest::{
    canonical_encode_snapshot_tail, digest_of, verify_snapshot_digest, SnapshotDigest,
    DIGEST_DOMAIN, SNAPSHOT_DIGEST_SCHEMA_VERSION,
};
pub use protocol::{NackReason, SyncMessage, SyncNode};
pub use transport::{replicate, InMemoryTransport, SyncTransport};
