//! Snapshot synchronization (RFC 0001).
//!
//! Content-addressed snapshot digesting, transport-agnostic copy protocol, and
//! lineage-head bookkeeping from RFC 0001.

pub mod digest;
pub mod framed;
pub mod lineage;
pub mod protocol;
pub mod transport;

pub use digest::{
    canonical_encode_snapshot_tail, digest_of, verify_snapshot_digest, SnapshotDigest,
    DIGEST_DOMAIN, SNAPSHOT_DIGEST_SCHEMA_VERSION,
};
pub use framed::{replicate_framed, FramedSyncTransport, SyncAuthConfig};
pub use lineage::{
    AgentId, HlcTimestamp, LineageFork, LineageHead, LineageStore, LineageUpdate, NodeId,
};
pub use protocol::{NackReason, SyncMessage, SyncNode};
pub use transport::{replicate, InMemoryTransport, SyncTransport};
