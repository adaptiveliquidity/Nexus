//! Integration tests for the snapshot-sync protocol + in-memory transport
//! (RFC 0001, Phase 2). Pure in-process state machine — no networking.

use nexus::snapshot::manager::{ExecutionState, FilesystemDiff, Snapshot, SnapshotMetadata};
use nexus::snapshot::sync::{
    digest_of, replicate, InMemoryTransport, NackReason, SyncMessage, SyncNode,
};

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
fn full_replication_a_to_b() {
    let mut a = SyncNode::new();
    let mut b = SyncNode::new();
    let d1 = a.try_insert(snap(b"one", "op1")).unwrap();
    let d2 = a.try_insert(snap(b"two", "op2")).unwrap();

    let (mut ta, mut tb) = InMemoryTransport::pair();
    replicate(&mut a, &mut ta, &mut b, &mut tb, 64).unwrap();

    assert!(b.has(&d1));
    assert!(b.has(&d2));
    assert_eq!(b.len(), 2);
    // Replicated snapshots verify under their digests on the receiver.
    assert_eq!(digest_of(b.get(&d1).unwrap()).unwrap(), d1);
}

#[test]
fn replicate_is_noop_when_already_in_sync() {
    let mut a = SyncNode::new();
    let mut b = SyncNode::new();
    let d = snap(b"same", "op");
    let dig = a.try_insert(d.clone()).unwrap();
    b.try_insert(d).unwrap();

    let (mut ta, mut tb) = InMemoryTransport::pair();
    replicate(&mut a, &mut ta, &mut b, &mut tb, 64).unwrap();

    assert_eq!(b.len(), 1, "no duplicate insertion when already in sync");
    assert!(b.has(&dig));
}

#[test]
fn replicate_reaches_quiescence() {
    // A generous bound must complete without erroring.
    let mut a = SyncNode::new();
    let mut b = SyncNode::new();
    a.try_insert(snap(b"x", "op")).unwrap();
    let (mut ta, mut tb) = InMemoryTransport::pair();
    assert!(replicate(&mut a, &mut ta, &mut b, &mut tb, 64).is_ok());
}

#[test]
fn replicate_stops_at_max_steps() {
    // One snapshot needs multiple pump rounds; a too-small bound must error
    // rather than hang or silently under-replicate.
    let mut a = SyncNode::new();
    let mut b = SyncNode::new();
    a.try_insert(snap(b"x", "op")).unwrap();
    let (mut ta, mut tb) = InMemoryTransport::pair();
    let res = replicate(&mut a, &mut ta, &mut b, &mut tb, 1);
    assert!(res.is_err(), "max_steps=1 is insufficient and must error");
}

#[test]
fn duplicate_transfer_is_idempotent() {
    let mut b = SyncNode::new();
    let s = snap(b"dup", "op");
    let d = digest_of(&s).unwrap();

    let out1 = b.handle(SyncMessage::Snapshot {
        digest: d,
        snapshot: Box::new(s.clone()),
    });
    let out2 = b.handle(SyncMessage::Snapshot {
        digest: d,
        snapshot: Box::new(s),
    });

    assert!(matches!(out1.as_slice(), [SyncMessage::Ack { .. }]));
    assert!(matches!(out2.as_slice(), [SyncMessage::Ack { .. }]));
    assert_eq!(b.len(), 1, "second transfer must not double-insert");
}

#[test]
fn tampered_transfer_is_rejected() {
    let mut b = SyncNode::new();
    let real = snap(b"real", "op");
    let wrong_digest = digest_of(&snap(b"other", "op")).unwrap();

    let out = b.handle(SyncMessage::Snapshot {
        digest: wrong_digest,
        snapshot: Box::new(real),
    });
    match out.as_slice() {
        [SyncMessage::Nack { reason, .. }] => assert_eq!(*reason, NackReason::DigestMismatch),
        other => panic!("expected Nack(DigestMismatch), got {other:?}"),
    }
    assert!(b.is_empty(), "tampered snapshot must not be stored");
}

#[test]
fn want_unknown_digest_yields_gone() {
    let mut a = SyncNode::new();
    let unknown = digest_of(&snap(b"nope", "op")).unwrap();
    let out = a.handle(SyncMessage::Want {
        digests: vec![unknown],
    });
    match out.as_slice() {
        [SyncMessage::Nack { reason, .. }] => assert_eq!(*reason, NackReason::Gone),
        other => panic!("expected Nack(Gone), got {other:?}"),
    }
}
