//! Integration tests for the content-addressed snapshot digest (RFC 0001 §4.1).
//!
//! Covers: pinned digest vectors, determinism (incl. proptest), distinct-content
//! distinct-digest, and corruption rejection.

use nexus::snapshot::manager::{
    ExecutionState, FileChange, FilesystemDiff, GlobalSnapshot, GlobalValue, Snapshot,
    SnapshotMetadata, TableSnapshot,
};
use nexus::snapshot::sync::{canonical_encode_snapshot_tail, digest_of, verify_snapshot_digest};
use proptest::prelude::*;

/// Vector 1 — minimal snapshot. Deterministic: id/timestamp are excluded from
/// the digest, and metadata fields are fixed.
fn vector_1() -> Snapshot {
    Snapshot::new(
        b"hello".to_vec(),
        FilesystemDiff::new(),
        ExecutionState::default(),
        SnapshotMetadata::new("op".into(), "inputhash".into()),
    )
    .unwrap()
}

/// Vector 2 — exercises globals (all value kinds touched across the suite),
/// tables, every fs_changes bucket, and preconditions.
fn vector_2() -> Snapshot {
    let mut es = ExecutionState::default();
    es.captured_globals.push(GlobalSnapshot {
        name: "g_i32".into(),
        value: GlobalValue::I32(-7),
        mutable: true,
    });
    es.captured_globals.push(GlobalSnapshot {
        name: "g_f64".into(),
        value: GlobalValue::F64(2.5),
        mutable: false,
    });
    es.captured_tables.push(TableSnapshot {
        name: "t".into(),
        size: 3,
    });

    let mut fs = FilesystemDiff::new();
    fs.created.push(FileChange {
        path: "/a.txt".into(),
        content: b"abc".to_vec(),
        old_content: None,
    });
    fs.modified.push(FileChange {
        path: "/b.txt".into(),
        content: b"new".to_vec(),
        old_content: Some(b"old".to_vec()),
    });
    fs.deleted.push("/c.txt".into());
    fs.dirs_created.push("/d".into());

    let mut md = SnapshotMetadata::new("rich-op".into(), "deadbeef".into());
    md.creation_time_us = 123_456;
    md.memory_pages = 2;
    md.preconditions = vec!["read:/a".into(), "write:/b".into()];

    Snapshot::new(b"world-memory".to_vec(), fs, es, md).unwrap()
}

// Pinned digest vectors — captured once and locked as regression guards.
// Any change here means the canonical encoding changed (must bump schema/domain).
const VECTOR_1_DIGEST: &str = "259c7eb36029008ad0cd3743be7ce14208b73e2b31a7d50533da962840b549ac";
const VECTOR_2_DIGEST: &str = "4ddd38e543576f15e7d699d4a52046bdbc9165b46df762289880bc1a8921732c";

#[test]
#[ignore = "run manually to (re)capture pinned vectors"]
fn print_vectors() {
    println!("VECTOR_1_DIGEST = {}", digest_of(&vector_1()).unwrap());
    println!("VECTOR_2_DIGEST = {}", digest_of(&vector_2()).unwrap());
}

#[test]
fn pinned_vector_1() {
    assert_eq!(digest_of(&vector_1()).unwrap().to_hex(), VECTOR_1_DIGEST);
}

#[test]
fn pinned_vector_2() {
    assert_eq!(digest_of(&vector_2()).unwrap().to_hex(), VECTOR_2_DIGEST);
}

#[test]
fn digest_is_deterministic_across_calls() {
    let s = vector_2();
    assert_eq!(digest_of(&s).unwrap(), digest_of(&s).unwrap());
    // Canonical encoding is byte-stable too.
    assert_eq!(
        canonical_encode_snapshot_tail(&s),
        canonical_encode_snapshot_tail(&s)
    );
}

#[test]
fn same_content_built_twice_same_digest() {
    // id/timestamp differ (random/now) but are excluded from the digest.
    assert_eq!(
        digest_of(&vector_2()).unwrap(),
        digest_of(&vector_2()).unwrap()
    );
    assert_ne!(
        vector_2().id,
        vector_2().id,
        "ids are random per construction"
    );
}

#[test]
fn distinct_memory_distinct_digest() {
    let a = Snapshot::new(
        b"aaaa".to_vec(),
        FilesystemDiff::new(),
        ExecutionState::default(),
        SnapshotMetadata::new("op".into(), "in".into()),
    )
    .unwrap();
    let b = Snapshot::new(
        b"bbbb".to_vec(),
        FilesystemDiff::new(),
        ExecutionState::default(),
        SnapshotMetadata::new("op".into(), "in".into()),
    )
    .unwrap();
    assert_ne!(digest_of(&a).unwrap(), digest_of(&b).unwrap());
}

#[test]
fn distinct_globals_distinct_digest() {
    let base = vector_2();
    let mut other = vector_2();
    other.execution_state.captured_globals[0].value = GlobalValue::I32(-8);
    assert_ne!(digest_of(&base).unwrap(), digest_of(&other).unwrap());
}

#[test]
fn distinct_table_size_distinct_digest() {
    let base = vector_2();
    let mut other = vector_2();
    other.execution_state.captured_tables[0].size = 4;
    assert_ne!(digest_of(&base).unwrap(), digest_of(&other).unwrap());
}

#[test]
fn distinct_fs_path_distinct_digest() {
    let base = vector_2();
    let mut other = vector_2();
    other.fs_changes.created[0].path = "/a2.txt".into();
    assert_ne!(digest_of(&base).unwrap(), digest_of(&other).unwrap());
}

#[test]
fn distinct_metadata_distinct_digest() {
    let base = vector_2();
    let mut other = vector_2();
    other.metadata.preconditions.push("exec:/x".into());
    assert_ne!(digest_of(&base).unwrap(), digest_of(&other).unwrap());
}

#[test]
fn digest_is_compression_invariant() {
    // Headline guarantee (RFC 0001 §4): the digest is over decompressed content,
    // so it must NOT change when only the compressed bytes / compressed_size
    // differ (same memory_checksum, original_size, and tail).
    let a = vector_1();
    let mut b = vector_1();
    b.compressed_size = a.compressed_size + 999; // excluded from digest
    b.memory = vec![0xff; 16]; // compressed bytes excluded from digest
    assert_eq!(a.memory_checksum, b.memory_checksum);
    assert_eq!(a.original_size, b.original_size);
    assert_eq!(digest_of(&a).unwrap(), digest_of(&b).unwrap());
}

#[test]
fn global_value_tag_is_digested() {
    // Same numeric "zero" but different GlobalValue variant => different encoding
    // (tag byte + width), so the digest must differ.
    let mk = |v: GlobalValue| {
        let mut es = ExecutionState::default();
        es.captured_globals.push(GlobalSnapshot {
            name: "g".into(),
            value: v,
            mutable: false,
        });
        Snapshot::new(
            b"m".to_vec(),
            FilesystemDiff::new(),
            es,
            SnapshotMetadata::new("op".into(), "in".into()),
        )
        .unwrap()
    };
    assert_ne!(
        digest_of(&mk(GlobalValue::I32(0))).unwrap(),
        digest_of(&mk(GlobalValue::I64(0))).unwrap()
    );
}

#[test]
fn option_tag_none_vs_empty_some_distinct() {
    // old_content: None (tag 0x00) must differ from Some(empty) (tag 0x01 + len 0).
    let mk = |old: Option<Vec<u8>>| {
        let mut fs = FilesystemDiff::new();
        fs.modified.push(FileChange {
            path: "/p".into(),
            content: b"c".to_vec(),
            old_content: old,
        });
        Snapshot::new(
            b"m".to_vec(),
            fs,
            ExecutionState::default(),
            SnapshotMetadata::new("op".into(), "in".into()),
        )
        .unwrap()
    };
    assert_ne!(
        digest_of(&mk(None)).unwrap(),
        digest_of(&mk(Some(Vec::new()))).unwrap()
    );
}

#[test]
fn verify_accepts_matching_and_rejects_tampering() {
    let s = vector_2();
    let d = digest_of(&s).unwrap();
    assert!(verify_snapshot_digest(&s, &d));

    // Tamper a digested field: verification must fail against the old digest.
    let mut tampered = vector_2();
    tampered.metadata.operation_name = "evil".into();
    assert!(!verify_snapshot_digest(&tampered, &d));
}

#[test]
fn corrupt_memory_checksum_fails_verification() {
    let s = vector_1();
    let d = digest_of(&s).unwrap();
    // Flip the checksum to a different valid 32-byte hex value.
    let mut corrupt = vector_1();
    corrupt.memory_checksum = "0".repeat(64);
    assert!(!verify_snapshot_digest(&corrupt, &d));
    // And a malformed (non-hex) checksum verifies as false, never panics.
    let mut malformed = vector_1();
    malformed.memory_checksum = "xyz".into();
    assert!(!verify_snapshot_digest(&malformed, &d));
}

proptest! {
    /// Digest is a deterministic function of content: recomputing yields the
    /// same value, and verification against the freshly computed digest holds.
    #[test]
    fn prop_digest_deterministic(
        mem in proptest::collection::vec(any::<u8>(), 0..512),
        op in ".{0,32}",
        input_hash in ".{0,32}",
        pages in any::<u32>(),
        precos in proptest::collection::vec(".{0,16}", 0..4),
    ) {
        let mut md = SnapshotMetadata::new(op, input_hash);
        md.memory_pages = pages;
        md.preconditions = precos;
        let s = Snapshot::new(mem, FilesystemDiff::new(), ExecutionState::default(), md).unwrap();

        let d1 = digest_of(&s).unwrap();
        let d2 = digest_of(&s).unwrap();
        prop_assert_eq!(d1, d2);
        prop_assert!(verify_snapshot_digest(&s, &d1));
        prop_assert_eq!(
            canonical_encode_snapshot_tail(&s),
            canonical_encode_snapshot_tail(&s)
        );
    }
}
