//! Snapshot-sync protocol state machine (RFC 0001, Phase 2).
//!
//! A pure, in-process state machine for negotiating and copying verified
//! snapshots between two nodes. There is **no networking here** — messages are
//! plain values produced/consumed by [`SyncNode::handle`]. Real transport
//! (daemon framing, HMAC/anti-replay, gRPC) is deferred to later RFC-0001
//! phases; see [`super::transport`] for the test-only wire abstraction.
//!
//! Protocol sketch:
//! ```text
//! A: Advertise { digests I have }
//! B: Want { digests I lack }
//! A: Snapshot { digest, payload }
//! B: verify digest -> Ack { digest }   (or Nack on mismatch)
//! ```
//! A duplicate but valid snapshot is a successful idempotent no-op (`Ack`),
//! never a `Nack`.

use std::collections::HashMap;

use crate::error::Result;
use crate::snapshot::manager::Snapshot;
use crate::snapshot::sync::digest::{digest_of, verify_snapshot_digest, SnapshotDigest};

/// Why a peer rejected a message. Only genuine failures — a duplicate valid
/// snapshot is an `Ack`, not a `Nack`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NackReason {
    /// The payload's recomputed digest did not match the advertised digest.
    DigestMismatch,
    /// The requested digest is not held by this node.
    Gone,
}

/// A protocol message exchanged between two sync nodes.
#[derive(Debug, Clone)]
pub enum SyncMessage {
    /// "Here are the snapshot digests I have."
    Advertise { digests: Vec<SnapshotDigest> },
    /// "Send me these snapshots."
    Want { digests: Vec<SnapshotDigest> },
    /// "Here is a snapshot payload, addressed by digest."
    Snapshot {
        digest: SnapshotDigest,
        snapshot: Box<Snapshot>,
    },
    /// "Accepted (or already had it)."
    Ack { digest: SnapshotDigest },
    /// "Rejected."
    Nack {
        digest: SnapshotDigest,
        reason: NackReason,
    },
}

/// A node holding a content-addressed store of snapshots, keyed by the digest
/// the node itself computes (never a caller-supplied one).
#[derive(Default)]
pub struct SyncNode {
    store: HashMap<SnapshotDigest, Snapshot>,
}

impl SyncNode {
    pub fn new() -> Self {
        Self::default()
    }

    /// Compute the digest of `snapshot` and store it under that digest.
    /// Returns the computed digest. Errors only if the snapshot's checksum is
    /// malformed (so the digest cannot be computed).
    pub fn try_insert(&mut self, snapshot: Snapshot) -> Result<SnapshotDigest> {
        let d = digest_of(&snapshot)?;
        self.store.insert(d, snapshot);
        Ok(d)
    }

    pub fn has(&self, digest: &SnapshotDigest) -> bool {
        self.store.contains_key(digest)
    }

    pub fn get(&self, digest: &SnapshotDigest) -> Option<&Snapshot> {
        self.store.get(digest)
    }

    pub fn len(&self) -> usize {
        self.store.len()
    }

    pub fn is_empty(&self) -> bool {
        self.store.is_empty()
    }

    /// All digests this node currently holds.
    pub fn digests(&self) -> Vec<SnapshotDigest> {
        self.store.keys().copied().collect()
    }

    /// Build an `Advertise` message for everything this node holds.
    pub fn advertise(&self) -> SyncMessage {
        SyncMessage::Advertise {
            digests: self.digests(),
        }
    }

    /// Process one incoming message and return zero or more responses.
    /// Pure state transition — the only side effect is updating the local store.
    pub fn handle(&mut self, msg: SyncMessage) -> Vec<SyncMessage> {
        match msg {
            SyncMessage::Advertise { digests } => {
                let want: Vec<SnapshotDigest> =
                    digests.into_iter().filter(|d| !self.has(d)).collect();
                if want.is_empty() {
                    Vec::new()
                } else {
                    vec![SyncMessage::Want { digests: want }]
                }
            }
            SyncMessage::Want { digests } => digests
                .into_iter()
                .map(|d| match self.store.get(&d) {
                    Some(s) => SyncMessage::Snapshot {
                        digest: d,
                        snapshot: Box::new(s.clone()),
                    },
                    None => SyncMessage::Nack {
                        digest: d,
                        reason: NackReason::Gone,
                    },
                })
                .collect(),
            SyncMessage::Snapshot { digest, snapshot } => {
                // Verify the payload against the advertised digest before trust.
                if !verify_snapshot_digest(&snapshot, &digest) {
                    return vec![SyncMessage::Nack {
                        digest,
                        reason: NackReason::DigestMismatch,
                    }];
                }
                // Duplicate valid snapshot: idempotent no-op, still Ack.
                if self.has(&digest) {
                    return vec![SyncMessage::Ack { digest }];
                }
                match self.try_insert(*snapshot) {
                    Ok(d) => vec![SyncMessage::Ack { digest: d }],
                    // try_insert only fails on a malformed checksum, which
                    // verify_snapshot_digest already rejected — treat defensively.
                    Err(_) => vec![SyncMessage::Nack {
                        digest,
                        reason: NackReason::DigestMismatch,
                    }],
                }
            }
            // Terminal acknowledgements produce no further messages.
            SyncMessage::Ack { .. } | SyncMessage::Nack { .. } => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::manager::{ExecutionState, FilesystemDiff, SnapshotMetadata};

    fn snap(mem: &[u8], op: &str) -> Snapshot {
        Snapshot::new(
            mem.to_vec(),
            FilesystemDiff::new(),
            ExecutionState::default(),
            SnapshotMetadata::new(op.into(), "in".into()),
        )
        .unwrap()
    }

    #[test]
    fn advertise_missing_digest_emits_want() {
        let mut b = SyncNode::new();
        let s = snap(b"x", "op");
        let d = digest_of(&s).unwrap();
        let out = b.handle(SyncMessage::Advertise { digests: vec![d] });
        match out.as_slice() {
            [SyncMessage::Want { digests }] => assert_eq!(digests, &vec![d]),
            other => panic!("expected Want, got {other:?}"),
        }
    }

    #[test]
    fn advertise_known_digest_emits_nothing() {
        let mut a = SyncNode::new();
        let d = a.try_insert(snap(b"x", "op")).unwrap();
        let out = a.handle(SyncMessage::Advertise { digests: vec![d] });
        assert!(out.is_empty());
    }

    #[test]
    fn want_unknown_digest_emits_nack_gone() {
        let mut a = SyncNode::new();
        let d = digest_of(&snap(b"missing", "op")).unwrap();
        let out = a.handle(SyncMessage::Want { digests: vec![d] });
        match out.as_slice() {
            [SyncMessage::Nack { reason, .. }] => assert_eq!(*reason, NackReason::Gone),
            other => panic!("expected Nack(Gone), got {other:?}"),
        }
    }

    #[test]
    fn snapshot_valid_new_inserts_and_acks() {
        let mut b = SyncNode::new();
        let s = snap(b"x", "op");
        let d = digest_of(&s).unwrap();
        let out = b.handle(SyncMessage::Snapshot {
            digest: d,
            snapshot: Box::new(s),
        });
        assert!(matches!(out.as_slice(), [SyncMessage::Ack { .. }]));
        assert!(b.has(&d));
        assert_eq!(b.len(), 1);
    }

    #[test]
    fn snapshot_valid_duplicate_acks_no_double_insert() {
        let mut b = SyncNode::new();
        let s = snap(b"x", "op");
        let d = b.try_insert(s.clone()).unwrap();
        let out = b.handle(SyncMessage::Snapshot {
            digest: d,
            snapshot: Box::new(s),
        });
        assert!(matches!(out.as_slice(), [SyncMessage::Ack { .. }]));
        assert_eq!(b.len(), 1, "duplicate must not double-insert");
    }

    #[test]
    fn snapshot_tampered_digest_rejected() {
        let mut b = SyncNode::new();
        let s = snap(b"x", "op");
        let wrong = digest_of(&snap(b"y", "op")).unwrap();
        let out = b.handle(SyncMessage::Snapshot {
            digest: wrong,
            snapshot: Box::new(s),
        });
        match out.as_slice() {
            [SyncMessage::Nack { reason, .. }] => assert_eq!(*reason, NackReason::DigestMismatch),
            other => panic!("expected Nack(DigestMismatch), got {other:?}"),
        }
        assert!(b.is_empty(), "tampered snapshot must not be stored");
    }
}
