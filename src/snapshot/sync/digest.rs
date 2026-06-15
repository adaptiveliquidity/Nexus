//! Content-addressed snapshot digest (RFC 0001, Phase 1).
//!
//! Defines a deterministic, byte-reproducible digest over a [`Snapshot`]'s
//! content. The encoding is hand-rolled and length-prefixed (no CBOR/bincode) so
//! determinism is fully under our control and pinnable with test vectors.
//!
//! The digest binds the SHA-256 of the *decompressed* memory (reused from
//! `Snapshot.memory_checksum`, not re-hashed here) plus a canonical encoding of
//! the structured tail (execution state, filesystem diff, metadata). It
//! deliberately **excludes** `compressed_size` and the compressed bytes, so
//! content identity is invariant to the compression level. `Snapshot.id` (a
//! random UUID) and `Snapshot.timestamp` are also excluded — see RFC 0001 §4.2.
//!
//! This module implements only Phase 1 (digest + canonical encoding). Transport,
//! lineage heads, anti-replay, and restore authorization are out of scope.

use sha2::{Digest, Sha256};

use crate::error::{NexusError, Result};
use crate::snapshot::manager::{
    ExecutionState, FileChange, FilesystemDiff, GlobalSnapshot, GlobalValue, Snapshot,
    SnapshotMetadata, TableSnapshot,
};

/// Domain separator for the snapshot digest. 24 raw ASCII bytes, hashed with no
/// length prefix. Bump the version suffix together with
/// [`SNAPSHOT_DIGEST_SCHEMA_VERSION`] on any breaking encoding change.
pub const DIGEST_DOMAIN: &[u8] = b"NEXUS-SNAPSHOT-DIGEST-v1";

/// Schema version bound into the digest preimage. Any change to the canonical
/// encoding is a breaking digest change and MUST bump this.
pub const SNAPSHOT_DIGEST_SCHEMA_VERSION: u32 = 1;

/// A 32-byte content-addressed snapshot digest (SHA-256 of the canonical
/// preimage). The wire protocol keys snapshots by this value, not by UUID.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SnapshotDigest([u8; 32]);

impl SnapshotDigest {
    /// Wrap raw digest bytes.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        SnapshotDigest(bytes)
    }

    /// The raw 32 digest bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Lowercase hex encoding (64 chars).
    pub fn to_hex(&self) -> String {
        to_hex(&self.0)
    }

    /// Parse a 64-char lowercase/uppercase hex string into a digest.
    pub fn from_hex(s: &str) -> Result<Self> {
        let bytes = from_hex(s)?;
        let arr: [u8; 32] = bytes.try_into().map_err(|_| {
            NexusError::SerializationError("snapshot digest must be 32 bytes (64 hex chars)".into())
        })?;
        Ok(SnapshotDigest(arr))
    }
}

impl std::fmt::Debug for SnapshotDigest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SnapshotDigest({})", self.to_hex())
    }
}

impl std::fmt::Display for SnapshotDigest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// Compute the content digest of a snapshot.
///
/// Returns an error if `memory_checksum` is not a valid 32-byte hex string.
pub fn digest_of(snapshot: &Snapshot) -> Result<SnapshotDigest> {
    let raw_memory_hash = from_hex(&snapshot.memory_checksum)?;
    if raw_memory_hash.len() != 32 {
        return Err(NexusError::SerializationError(format!(
            "memory_checksum must decode to 32 bytes, got {}",
            raw_memory_hash.len()
        )));
    }

    let mut preimage = Vec::new();
    preimage.extend_from_slice(DIGEST_DOMAIN);
    put_u32(&mut preimage, SNAPSHOT_DIGEST_SCHEMA_VERSION);
    put_len_prefixed(&mut preimage, &raw_memory_hash);
    put_u64(&mut preimage, snapshot.original_size as u64);
    preimage.extend_from_slice(&canonical_encode_snapshot_tail(snapshot));

    let mut hasher = Sha256::new();
    hasher.update(&preimage);
    Ok(SnapshotDigest(hasher.finalize().into()))
}

/// Verify a snapshot against an expected digest. Constant-time comparison.
/// Returns false if the snapshot's checksum is malformed.
pub fn verify_snapshot_digest(snapshot: &Snapshot, expected: &SnapshotDigest) -> bool {
    match digest_of(snapshot) {
        Ok(actual) => ct_eq(actual.as_bytes(), expected.as_bytes()),
        Err(_) => false,
    }
}

/// Canonical encoding of the structured "tail" — execution state, filesystem
/// diff, and metadata — each length-prefixed. This is the non-memory portion of
/// the digest preimage; see RFC 0001 §4.1 for the normative byte layout.
pub fn canonical_encode_snapshot_tail(snapshot: &Snapshot) -> Vec<u8> {
    let mut es = Vec::new();
    encode_execution_state(&mut es, &snapshot.execution_state);
    let mut fs = Vec::new();
    encode_filesystem_diff(&mut fs, &snapshot.fs_changes);
    let mut md = Vec::new();
    encode_metadata(&mut md, &snapshot.metadata);

    let mut out = Vec::new();
    put_len_prefixed(&mut out, &es);
    put_len_prefixed(&mut out, &fs);
    put_len_prefixed(&mut out, &md);
    out
}

// ---- composite encoders -------------------------------------------------

fn encode_execution_state(out: &mut Vec<u8>, es: &ExecutionState) {
    put_u32(out, es.captured_globals.len() as u32);
    for g in &es.captured_globals {
        encode_global(out, g);
    }
    put_u32(out, es.captured_tables.len() as u32);
    for t in &es.captured_tables {
        encode_table(out, t);
    }
}

fn encode_global(out: &mut Vec<u8>, g: &GlobalSnapshot) {
    put_str(out, &g.name);
    encode_global_value(out, &g.value);
    out.push(g.mutable as u8);
}

// Floats are encoded by their raw IEEE-754 bit pattern (`to_le_bytes`), per
// RFC 0001 §4.1. This is the correct deterministic choice but means two values
// that are both NaN with different bit patterns (payload/sign) produce different
// digests — float `==` semantics do not apply to digest identity.
fn encode_global_value(out: &mut Vec<u8>, v: &GlobalValue) {
    match v {
        GlobalValue::I32(x) => {
            out.push(0x00);
            out.extend_from_slice(&x.to_le_bytes());
        }
        GlobalValue::I64(x) => {
            out.push(0x01);
            out.extend_from_slice(&x.to_le_bytes());
        }
        GlobalValue::F32(x) => {
            out.push(0x02);
            out.extend_from_slice(&x.to_le_bytes());
        }
        GlobalValue::F64(x) => {
            out.push(0x03);
            out.extend_from_slice(&x.to_le_bytes());
        }
    }
}

fn encode_table(out: &mut Vec<u8>, t: &TableSnapshot) {
    put_str(out, &t.name);
    put_u32(out, t.size);
}

fn encode_filesystem_diff(out: &mut Vec<u8>, fs: &FilesystemDiff) {
    put_u32(out, fs.created.len() as u32);
    for fc in &fs.created {
        encode_file_change(out, fc);
    }
    put_u32(out, fs.modified.len() as u32);
    for fc in &fs.modified {
        encode_file_change(out, fc);
    }
    put_u32(out, fs.deleted.len() as u32);
    for p in &fs.deleted {
        put_str(out, p);
    }
    put_u32(out, fs.dirs_created.len() as u32);
    for p in &fs.dirs_created {
        put_str(out, p);
    }
    put_u32(out, fs.dirs_deleted.len() as u32);
    for p in &fs.dirs_deleted {
        put_str(out, p);
    }
}

fn encode_file_change(out: &mut Vec<u8>, fc: &FileChange) {
    put_str(out, &fc.path);
    put_bytes(out, &fc.content);
    match &fc.old_content {
        None => out.push(0x00),
        Some(b) => {
            out.push(0x01);
            put_bytes(out, b);
        }
    }
}

fn encode_metadata(out: &mut Vec<u8>, m: &SnapshotMetadata) {
    put_str(out, &m.operation_name);
    put_str(out, &m.input_hash);
    put_u64(out, m.creation_time_us);
    put_u32(out, m.memory_pages);
    put_u32(out, m.preconditions.len() as u32);
    for p in &m.preconditions {
        put_str(out, p);
    }
}

// ---- primitive encoders -------------------------------------------------

fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

// Length prefixes are u32 per RFC 0001 §4.1. Snapshot structured fields
// (paths, names, preconditions, per-file content in a diff) are realistically
// well under 4 GiB; a single field at or beyond u32::MAX would wrap the prefix
// and break injectivity, so we treat that as a programming error.
fn put_len_prefixed(out: &mut Vec<u8>, b: &[u8]) {
    debug_assert!(
        b.len() <= u32::MAX as usize,
        "canonical-encoding field exceeds u32 length prefix"
    );
    put_u32(out, b.len() as u32);
    out.extend_from_slice(b);
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    put_len_prefixed(out, s.as_bytes());
}

fn put_bytes(out: &mut Vec<u8>, b: &[u8]) {
    put_len_prefixed(out, b);
}

// ---- helpers ------------------------------------------------------------

/// Constant-time equality over 32-byte arrays.
fn ct_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff = 0u8;
    for i in 0..32 {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

fn from_hex(s: &str) -> Result<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return Err(NexusError::SerializationError(
            "hex string must have even length".into(),
        ));
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    for chunk in bytes.chunks(2) {
        let hi = hex_val(chunk[0])?;
        let lo = hex_val(chunk[1])?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn hex_val(c: u8) -> Result<u8> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(NexusError::SerializationError(format!(
            "invalid hex character: {:?}",
            c as char
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_tail_matches_hand_derived_bytes() {
        // Independently constructed from RFC 0001 §4.1 (does not call the
        // production composite encoders), so it validates the encoder spec.
        let snap = Snapshot::new(
            Vec::new(),
            FilesystemDiff::new(),
            ExecutionState::default(),
            SnapshotMetadata::new(String::new(), String::new()),
        )
        .unwrap();

        let mut expected = Vec::new();
        // execution_state: u32(0) globals + u32(0) tables = 8 bytes, len-prefixed
        expected.extend_from_slice(&8u32.to_le_bytes());
        expected.extend_from_slice(&[0u8; 8]);
        // fs_changes: 5 empty vecs = 20 bytes, len-prefixed
        expected.extend_from_slice(&20u32.to_le_bytes());
        expected.extend_from_slice(&[0u8; 20]);
        // metadata: str("")+str("")+u64(0)+u32(0)+vec(0) = 4+4+8+4+4 = 24 bytes
        expected.extend_from_slice(&24u32.to_le_bytes());
        expected.extend_from_slice(&[0u8; 24]);

        assert_eq!(canonical_encode_snapshot_tail(&snap), expected);
        assert_eq!(canonical_encode_snapshot_tail(&snap).len(), 64);
    }

    #[test]
    fn hex_roundtrip() {
        let d = SnapshotDigest::from_bytes([0xab; 32]);
        assert_eq!(d.to_hex().len(), 64);
        assert_eq!(SnapshotDigest::from_hex(&d.to_hex()).unwrap(), d);
    }

    #[test]
    fn malformed_checksum_is_error() {
        let mut snap = Snapshot::new(
            b"x".to_vec(),
            FilesystemDiff::new(),
            ExecutionState::default(),
            SnapshotMetadata::new("op".into(), "in".into()),
        )
        .unwrap();
        snap.memory_checksum = "not-hex".into();
        assert!(digest_of(&snap).is_err());
    }
}
