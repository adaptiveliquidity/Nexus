//! Snapshot Manager with Ring Buffer
//!
//! Provides microsecond snapshots and instant rollback for WASM state.

use crate::error::{NexusError, Result};
use crate::snapshot::differential::DiffSnapshot;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::sync::RwLock;
use std::time::Instant;
use uuid::Uuid;

/// Represents a complete state snapshot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    /// Unique snapshot identifier
    pub id: Uuid,
    /// Timestamp of snapshot creation
    pub timestamp: DateTime<Utc>,
    /// Compressed memory pages (WASM linear memory)
    pub memory: Vec<u8>,
    /// Memory checksum for integrity verification
    pub memory_checksum: String,
    /// Filesystem changes (overlay changeset)
    pub fs_changes: FilesystemDiff,
    /// Execution state (stack, registers, etc.)
    pub execution_state: ExecutionState,
    /// Snapshot metadata
    pub metadata: SnapshotMetadata,
    /// Original memory size (before compression)
    pub original_size: usize,
    /// Compressed size
    pub compressed_size: usize,
}

impl Snapshot {
    /// Create a new snapshot
    pub fn new(
        memory: Vec<u8>,
        fs_changes: FilesystemDiff,
        execution_state: ExecutionState,
        metadata: SnapshotMetadata,
    ) -> Result<Self> {
        let original_size = memory.len();

        let mut compressed = Vec::new();
        let compression_level = 3;
        zstd::stream::copy_encode(&memory[..], &mut compressed, compression_level)
            .map_err(|e| NexusError::SerializationError(format!("compression failed: {e}")))?;

        let compressed_size = compressed.len();

        let mut hasher = Sha256::new();
        hasher.update(&memory);
        let memory_checksum = format!("{:x}", hasher.finalize());

        Ok(Snapshot {
            id: Uuid::new_v4(),
            timestamp: Utc::now(),
            memory: compressed,
            memory_checksum,
            fs_changes,
            execution_state,
            metadata,
            original_size,
            compressed_size,
        })
    }

    /// Verify memory integrity
    pub fn verify(&self, memory: &[u8]) -> bool {
        let mut hasher = Sha256::new();
        hasher.update(memory);
        let checksum = format!("{:x}", hasher.finalize());
        checksum == self.memory_checksum
    }

    /// Decompress memory back to original
    pub fn decompress_memory(&self) -> Result<Vec<u8>> {
        let mut decompressed = Vec::new();
        zstd::stream::copy_decode(&self.memory[..], &mut decompressed)
            .map_err(|e| NexusError::SerializationError(format!("Failed to decompress: {}", e)))?;
        Ok(decompressed)
    }

    /// Get compression ratio
    pub fn compression_ratio(&self) -> f64 {
        if self.original_size == 0 {
            return 1.0;
        }
        self.compressed_size as f64 / self.original_size as f64
    }
}

/// Filesystem changes since last snapshot
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FilesystemDiff {
    /// Files created
    pub created: Vec<FileChange>,
    /// Files modified
    pub modified: Vec<FileChange>,
    /// Files deleted
    pub deleted: Vec<FilePath>,
    /// Directories created
    pub dirs_created: Vec<FilePath>,
    /// Directories deleted
    pub dirs_deleted: Vec<FilePath>,
}

impl FilesystemDiff {
    pub fn new() -> Self {
        FilesystemDiff::default()
    }

    /// Record a file creation
    pub fn record_create(&mut self, path: FilePath, content: Vec<u8>) {
        self.created.push(FileChange {
            path,
            content,
            old_content: None,
        });
    }

    /// Record a file modification
    pub fn record_modify(&mut self, path: FilePath, new_content: Vec<u8>, old_content: Vec<u8>) {
        self.modified.push(FileChange {
            path,
            content: new_content,
            old_content: Some(old_content),
        });
    }

    /// Record a file deletion
    pub fn record_delete(&mut self, path: FilePath) {
        self.deleted.push(path);
    }

    /// Revert all changes
    pub fn revert(&self) -> Vec<RevertOperation> {
        let mut ops = Vec::new();

        // Undo deletes (restore files)
        for path in &self.deleted {
            ops.push(RevertOperation::Restore(path.clone()));
        }

        // Undo modifies (restore original content)
        for change in &self.modified {
            if let Some(old) = &change.old_content {
                ops.push(RevertOperation::Overwrite(change.path.clone(), old.clone()));
            }
        }

        // Undo creates (delete files)
        for change in &self.created {
            ops.push(RevertOperation::Delete(change.path.clone()));
        }

        ops
    }

    /// Apply changes (forward)
    pub fn apply(&self) -> Vec<RevertOperation> {
        let mut ops = Vec::new();

        // Create directories first
        for path in &self.dirs_created {
            ops.push(RevertOperation::CreateDir(path.clone()));
        }

        // Create files
        for change in &self.created {
            ops.push(RevertOperation::Create(
                change.path.clone(),
                change.content.clone(),
            ));
        }

        // Modify files
        for change in &self.modified {
            ops.push(RevertOperation::Overwrite(
                change.path.clone(),
                change.content.clone(),
            ));
        }

        // Delete files
        for path in &self.deleted {
            ops.push(RevertOperation::Delete(path.clone()));
        }

        ops
    }
}

/// A file path
pub type FilePath = String;

/// A file change record
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChange {
    pub path: FilePath,
    pub content: Vec<u8>,
    pub old_content: Option<Vec<u8>>,
}

/// A revert operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RevertOperation {
    Create(FilePath, Vec<u8>),
    CreateDir(FilePath),
    Overwrite(FilePath, Vec<u8>),
    Delete(FilePath),
    Restore(FilePath),
}

/// Execution state for WASM
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecutionState {
    /// Captured exported globals (name, value, mutability).
    /// Empty for modules with no exported globals.
    pub captured_globals: Vec<GlobalSnapshot>,
    /// Captured exported tables (name, size, element type info).
    /// Empty for modules with no exported tables.
    pub captured_tables: Vec<TableSnapshot>,
}

/// A snapshot of a single WASM exported global.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GlobalSnapshot {
    pub name: String,
    pub value: GlobalValue,
    pub mutable: bool,
}

/// Typed global value matching the WASM value types.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum GlobalValue {
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
}

/// A snapshot of a single WASM exported table.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TableSnapshot {
    pub name: String,
    pub size: u32,
}

/// Snapshot metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotMetadata {
    /// Tool or operation name
    pub operation_name: String,
    /// Input hash for reproducibility
    pub input_hash: String,
    /// Snapshot creation time in microseconds
    pub creation_time_us: u64,
    /// Number of pages in memory
    pub memory_pages: u32,
    /// Preconditions (required capabilities)
    pub preconditions: Vec<String>,
}

impl SnapshotMetadata {
    pub fn new(operation_name: String, input_hash: String) -> Self {
        SnapshotMetadata {
            operation_name,
            input_hash,
            creation_time_us: 0, // Set during creation
            memory_pages: 0,
            preconditions: Vec::new(),
        }
    }
}

/// Ring buffer for snapshot management
pub struct SnapshotRingBuffer {
    /// Maximum number of snapshots to keep
    capacity: usize,
    /// The actual snapshots
    snapshots: VecDeque<Snapshot>,
    /// Index mapping for quick lookup
    index: std::collections::HashMap<Uuid, usize>,
}

impl SnapshotRingBuffer {
    /// Create a new ring buffer
    pub fn new(capacity: usize) -> Self {
        SnapshotRingBuffer {
            capacity,
            snapshots: VecDeque::with_capacity(capacity),
            index: std::collections::HashMap::new(),
        }
    }

    /// Add a snapshot (evicts oldest if full)
    pub fn push(&mut self, snapshot: Snapshot) {
        // Evict oldest if at capacity
        if self.snapshots.len() >= self.capacity {
            if let Some(old) = self.snapshots.pop_front() {
                self.index.remove(&old.id);
            }
            // Rebuild all positions: pop_front shifts the VecDeque so every
            // surviving entry's position decreases by 1.
            self.index.clear();
            for (i, s) in self.snapshots.iter().enumerate() {
                self.index.insert(s.id, i);
            }
        }

        let id = snapshot.id;
        let pos = self.snapshots.len();
        self.snapshots.push_back(snapshot);
        self.index.insert(id, pos);
    }

    /// Get a snapshot by ID
    pub fn get(&self, id: &Uuid) -> Option<&Snapshot> {
        self.index.get(id).and_then(|&i| self.snapshots.get(i))
    }

    /// Get the most recent snapshot
    pub fn latest(&self) -> Option<&Snapshot> {
        self.snapshots.back()
    }

    /// Get all snapshots
    pub fn all(&self) -> Vec<&Snapshot> {
        self.snapshots.iter().collect()
    }

    /// Clear all snapshots
    pub fn clear(&mut self) {
        self.snapshots.clear();
        self.index.clear();
    }

    /// Get number of snapshots
    pub fn len(&self) -> usize {
        self.snapshots.len()
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.snapshots.is_empty()
    }
}

/// Default maximum diff chain depth before auto-promoting to a full snapshot.
const DEFAULT_MAX_DIFF_DEPTH: u32 = 8;

/// Snapshot manager with compression and persistence
pub struct SnapshotManager {
    /// Ring buffer for in-memory snapshots
    buffer: RwLock<SnapshotRingBuffer>,
    /// Buffer for differential snapshots
    diff_buffer: RwLock<VecDeque<DiffSnapshot>>,
    /// Maximum diff chain depth before auto-promotion to a full snapshot
    max_diff_depth: u32,
    /// Enable persistent storage
    persist_enabled: bool,
    /// Persistence directory
    persist_dir: Option<std::path::PathBuf>,
    /// Statistics
    stats: RwLock<SnapshotStats>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SnapshotStats {
    pub total_snapshots: u64,
    pub total_rollbacks: u64,
    pub total_memory_saved_mb: f64,
    pub avg_compression_ratio: f64,
    pub last_snapshot_time_us: u64,
}

/// The result of `create_diff_snapshot`: either a diff or a full snapshot
/// (when auto-promotion kicks in because the chain depth exceeded
/// `max_diff_depth`).
#[derive(Debug, Clone)]
pub enum DiffSnapshotResult {
    /// A normal differential snapshot was created.
    Diff(DiffSnapshot),
    /// The diff chain was too deep; a full snapshot was created instead.
    Promoted(Snapshot),
}

impl SnapshotManager {
    /// Create a new snapshot manager
    pub fn new(capacity: usize) -> Self {
        SnapshotManager {
            buffer: RwLock::new(SnapshotRingBuffer::new(capacity)),
            diff_buffer: RwLock::new(VecDeque::new()),
            max_diff_depth: DEFAULT_MAX_DIFF_DEPTH,
            persist_enabled: false,
            persist_dir: None,
            stats: RwLock::new(SnapshotStats::default()),
        }
    }

    /// Create with persistence enabled
    pub fn with_persistence(capacity: usize, persist_dir: std::path::PathBuf) -> Self {
        // Ensure directory exists
        std::fs::create_dir_all(&persist_dir).ok();

        SnapshotManager {
            buffer: RwLock::new(SnapshotRingBuffer::new(capacity)),
            diff_buffer: RwLock::new(VecDeque::new()),
            max_diff_depth: DEFAULT_MAX_DIFF_DEPTH,
            persist_enabled: true,
            persist_dir: Some(persist_dir),
            stats: RwLock::new(SnapshotStats::default()),
        }
    }

    /// Create with a custom max diff depth.
    pub fn with_max_diff_depth(capacity: usize, max_diff_depth: u32) -> Self {
        SnapshotManager {
            buffer: RwLock::new(SnapshotRingBuffer::new(capacity)),
            diff_buffer: RwLock::new(VecDeque::new()),
            max_diff_depth,
            persist_enabled: false,
            persist_dir: None,
            stats: RwLock::new(SnapshotStats::default()),
        }
    }

    /// Create a new snapshot
    pub fn create_snapshot(
        &self,
        memory: Vec<u8>,
        fs_changes: FilesystemDiff,
        execution_state: ExecutionState,
        metadata: SnapshotMetadata,
    ) -> Result<Snapshot> {
        let start = Instant::now();

        let snapshot = Snapshot::new(memory, fs_changes, execution_state, metadata)?;

        // Update stats
        {
            let mut stats = self.stats.write().unwrap();
            stats.total_snapshots += 1;
            stats.total_memory_saved_mb += snapshot
                .original_size
                .saturating_sub(snapshot.compressed_size)
                as f64
                / 1_048_576.0;
            stats.avg_compression_ratio = (stats.avg_compression_ratio
                * (stats.total_snapshots - 1) as f64
                + snapshot.compression_ratio())
                / stats.total_snapshots as f64;
            stats.last_snapshot_time_us = start.elapsed().as_micros() as u64;
        }

        // Add to buffer
        self.buffer.write().unwrap().push(snapshot.clone());

        // Optionally persist
        if self.persist_enabled {
            self.persist_snapshot(&snapshot)?;
        }

        Ok(snapshot)
    }

    /// Rollback to a specific snapshot
    pub fn rollback_to(&self, snapshot_id: &Uuid) -> Result<RollbackResult> {
        let buffer = self.buffer.read().unwrap();

        let snapshot = buffer.get(snapshot_id).ok_or_else(|| {
            NexusError::RollbackFailed(format!("Snapshot {} not found", snapshot_id))
        })?;

        // Decompress memory
        let memory = snapshot.decompress_memory()?;
        let execution_state = snapshot.execution_state.clone();

        // Get filesystem revert operations
        let fs_ops = snapshot.fs_changes.revert();

        // Update stats
        drop(buffer);
        {
            let mut stats = self.stats.write().unwrap();
            stats.total_rollbacks += 1;
        }

        Ok(RollbackResult {
            snapshot_id: *snapshot_id,
            memory,
            execution_state,
            fs_operations: fs_ops,
            timestamp: Utc::now(),
        })
    }

    /// Get the latest snapshot
    pub fn latest(&self) -> Option<Snapshot> {
        self.buffer.read().unwrap().latest().cloned()
    }

    /// Get statistics
    pub fn stats(&self) -> SnapshotStats {
        self.stats.read().unwrap().clone()
    }

    /// Number of differential snapshots currently retained in memory.
    pub fn diff_snapshot_count(&self) -> usize {
        self.diff_buffer.read().unwrap().len()
    }

    /// Create a differential snapshot against a base full snapshot.
    ///
    /// If the resulting diff's generation would exceed `max_diff_depth`, the
    /// method auto-promotes by creating a full snapshot instead and returns
    /// `None` for the diff (the full snapshot is returned via the `Ok` variant
    /// of the inner enum).
    pub fn create_diff_snapshot(
        &self,
        current_memory: Vec<u8>,
        base_id: &Uuid,
        execution_state: ExecutionState,
        metadata: SnapshotMetadata,
    ) -> Result<DiffSnapshotResult> {
        // Figure out the generation: look at diff_buffer for a diff with this
        // base_id to determine current chain depth.
        let generation = {
            let diff_buf = self.diff_buffer.read().unwrap();
            // If the base_id refers to a diff, its generation + 1; otherwise 1.
            diff_buf
                .iter()
                .find(|d| d.id == *base_id)
                .map(|d| d.generation + 1)
                .unwrap_or(1)
        };

        // Auto-promote: if the generation would exceed max_diff_depth,
        // create a full snapshot instead.
        if generation > self.max_diff_depth {
            let full = self.create_snapshot(
                current_memory,
                FilesystemDiff::new(),
                execution_state,
                metadata,
            )?;
            return Ok(DiffSnapshotResult::Promoted(full));
        }

        // Decompress the base memory. The base might be a full snapshot or
        // we might need to reconstruct through a diff chain.
        let base_memory = self.reconstruct_memory_for(base_id)?;

        let diff = DiffSnapshot::new(
            *base_id,
            &base_memory,
            &current_memory,
            execution_state,
            metadata,
            generation,
        )?;

        self.diff_buffer.write().unwrap().push_back(diff.clone());

        Ok(DiffSnapshotResult::Diff(diff))
    }

    /// Rollback to a differential snapshot by ID.
    ///
    /// Walks the chain back to the base full snapshot, decompresses it,
    /// then applies diffs in order to reconstruct the target state.
    pub fn rollback_to_diff(&self, diff_id: &Uuid) -> Result<RollbackResult> {
        // Collect the chain of diffs from diff_id back to the base full snapshot.
        let diff_buf = self.diff_buffer.read().unwrap();

        let target_diff = diff_buf
            .iter()
            .find(|d| d.id == *diff_id)
            .ok_or_else(|| {
                NexusError::RollbackFailed(format!("Diff snapshot {} not found", diff_id))
            })?
            .clone();

        // Build chain from target back to the full snapshot.
        let mut chain: Vec<DiffSnapshot> = vec![target_diff.clone()];
        let mut current_base = target_diff.base_id;

        loop {
            // Check if current_base is a full snapshot
            let buffer = self.buffer.read().unwrap();
            if buffer.get(&current_base).is_some() {
                break; // Found the full snapshot base
            }
            drop(buffer);

            // Otherwise it must be another diff
            let parent = diff_buf
                .iter()
                .find(|d| d.id == current_base)
                .ok_or_else(|| {
                    NexusError::RollbackFailed(format!(
                        "Diff chain broken: cannot find snapshot {}",
                        current_base
                    ))
                })?
                .clone();

            current_base = parent.base_id;
            chain.push(parent);
        }

        // `chain` is in reverse order (target first, oldest last). Reverse it.
        chain.reverse();

        // Decompress base full snapshot memory
        let base_memory = {
            let buffer = self.buffer.read().unwrap();
            let base_snap = buffer.get(&current_base).ok_or_else(|| {
                NexusError::RollbackFailed(format!("Base snapshot {} not found", current_base))
            })?;
            base_snap.decompress_memory()?
        };

        drop(diff_buf);

        // Apply the chain
        let chain_refs: Vec<&DiffSnapshot> = chain.iter().collect();
        let memory = crate::snapshot::differential::apply_diff_chain(&base_memory, &chain_refs)?;

        let execution_state = target_diff.execution_state.clone();

        // Update stats
        {
            let mut stats = self.stats.write().unwrap();
            stats.total_rollbacks += 1;
        }

        Ok(RollbackResult {
            snapshot_id: *diff_id,
            memory,
            execution_state,
            fs_operations: Vec::new(),
            timestamp: Utc::now(),
        })
    }

    /// Get the current max diff depth.
    pub fn max_diff_depth(&self) -> u32 {
        self.max_diff_depth
    }

    /// Reconstruct the full memory for a given snapshot id (full or diff).
    fn reconstruct_memory_for(&self, id: &Uuid) -> Result<Vec<u8>> {
        // Check full snapshots first
        {
            let buffer = self.buffer.read().unwrap();
            if let Some(snap) = buffer.get(id) {
                return snap.decompress_memory();
            }
        }

        // Must be a diff — walk the chain
        let diff_buf = self.diff_buffer.read().unwrap();
        let target = diff_buf
            .iter()
            .find(|d| d.id == *id)
            .ok_or_else(|| {
                NexusError::RollbackFailed(format!("Snapshot {} not found in any buffer", id))
            })?
            .clone();

        let mut chain = vec![target.clone()];
        let mut current_base = target.base_id;

        loop {
            let buffer = self.buffer.read().unwrap();
            if buffer.get(&current_base).is_some() {
                break;
            }
            drop(buffer);

            let parent = diff_buf
                .iter()
                .find(|d| d.id == current_base)
                .ok_or_else(|| {
                    NexusError::RollbackFailed(format!("Diff chain broken at {}", current_base))
                })?
                .clone();
            current_base = parent.base_id;
            chain.push(parent);
        }

        chain.reverse();
        drop(diff_buf);

        let base_memory = {
            let buffer = self.buffer.read().unwrap();
            buffer
                .get(&current_base)
                .ok_or_else(|| {
                    NexusError::RollbackFailed(format!(
                        "base snapshot {current_base} was evicted before diff reconstruction completed"
                    ))
                })?
                .decompress_memory()?
        };

        let chain_refs: Vec<&DiffSnapshot> = chain.iter().collect();
        crate::snapshot::differential::apply_diff_chain(&base_memory, &chain_refs)
    }

    /// Persist snapshot to disk
    fn persist_snapshot(&self, snapshot: &Snapshot) -> Result<()> {
        let dir = self
            .persist_dir
            .as_ref()
            .ok_or_else(|| NexusError::ConfigError("No persistence directory set".to_string()))?;

        let path = dir.join(format!("{}.snap", snapshot.id));

        let bytes = bincode::serialize(snapshot)
            .map_err(|e| NexusError::SerializationError(format!("Failed to serialize: {}", e)))?;

        std::fs::write(&path, &bytes)
            .map_err(|e| NexusError::FilesystemError(format!("Failed to write: {}", e)))?;

        Ok(())
    }

    /// Load snapshot from disk
    pub fn load_snapshot(&self, snapshot_id: &Uuid) -> Result<Option<Snapshot>> {
        let dir = self
            .persist_dir
            .as_ref()
            .ok_or_else(|| NexusError::ConfigError("No persistence directory set".to_string()))?;

        let path = dir.join(format!("{}.snap", snapshot_id));

        if !path.exists() {
            return Ok(None);
        }

        let bytes = std::fs::read(&path)
            .map_err(|e| NexusError::FilesystemError(format!("Failed to read: {}", e)))?;

        // Guard against crafted oversized files that would exhaust memory
        // before bincode's recursive deserializer can produce an error.
        const MAX_SNAPSHOT_BYTES: usize = 256 * 1024 * 1024; // 256 MiB
        if bytes.len() > MAX_SNAPSHOT_BYTES {
            return Err(NexusError::SerializationError(format!(
                "snapshot file too large ({} bytes; max {MAX_SNAPSHOT_BYTES})",
                bytes.len()
            )));
        }

        let snapshot: Snapshot = bincode::deserialize(&bytes)
            .map_err(|e| NexusError::SerializationError(format!("Failed to deserialize: {}", e)))?;

        Ok(Some(snapshot))
    }
}

/// Result of a rollback operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollbackResult {
    pub snapshot_id: Uuid,
    pub memory: Vec<u8>,
    pub execution_state: ExecutionState,
    pub fs_operations: Vec<RevertOperation>,
    pub timestamp: DateTime<Utc>,
}

/// Write `bytes` into a live wasmtime `Memory`, growing it if necessary.
/// This is the mechanism that makes rollback actually restore state: the
/// caller decompresses a snapshot's memory via `RollbackResult.memory` and
/// passes it here to overwrite the instance's linear memory.
pub fn restore_memory<T>(
    memory: &wasmtime::Memory,
    store: &mut wasmtime::Store<T>,
    bytes: &[u8],
) -> Result<()> {
    let current_size = memory.data_size(&*store);
    if bytes.len() > current_size {
        let pages_needed = (bytes.len() - current_size).div_ceil(65536);
        memory
            .grow(&mut *store, pages_needed as u64)
            .map_err(|e| NexusError::RollbackFailed(format!("memory grow failed: {e}")))?;
    }
    let dest = memory.data_mut(&mut *store);
    dest[..bytes.len()].copy_from_slice(bytes);
    Ok(())
}

/// Write captured global values back into a live wasmtime instance.
/// Only restores mutable globals; immutable globals cannot change
/// and are skipped. Unknown or type-mismatched globals are silently
/// skipped (the module may have been recompiled with different exports).
pub fn restore_globals<T>(
    instance: &wasmtime::Instance,
    store: &mut wasmtime::Store<T>,
    globals: &[GlobalSnapshot],
) -> Result<()> {
    for snap in globals {
        if !snap.mutable {
            continue;
        }
        if let Some(global) = instance.get_global(&mut *store, &snap.name) {
            let val = match &snap.value {
                GlobalValue::I32(v) => wasmtime::Val::I32(*v),
                GlobalValue::I64(v) => wasmtime::Val::I64(*v),
                GlobalValue::F32(v) => wasmtime::Val::F32(v.to_bits()),
                GlobalValue::F64(v) => wasmtime::Val::F64(v.to_bits()),
            };
            global.set(&mut *store, val).map_err(|e| {
                NexusError::RollbackFailed(format!(
                    "failed to restore global '{}': {e}",
                    snap.name
                ))
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_snapshot_compression() {
        let data = vec![0u8; 10000]; // 10KB of zeros
        let diff = FilesystemDiff::new();
        let state = ExecutionState::default();
        let metadata = SnapshotMetadata::new("test".to_string(), "hash".to_string());

        let snapshot = Snapshot::new(data.clone(), diff, state, metadata).unwrap();

        // Should be highly compressed
        assert!(snapshot.compression_ratio() < 0.1);

        // Should decompress correctly
        let decompressed = snapshot.decompress_memory().unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn test_ring_buffer() {
        let mut buffer = SnapshotRingBuffer::new(3);

        // Add 3 snapshots
        for i in 0..3 {
            let diff = FilesystemDiff::new();
            let state = ExecutionState::default();
            let metadata = SnapshotMetadata::new(format!("snap{}", i), "hash".to_string());
            let snap = Snapshot::new(vec![i as u8], diff, state, metadata).unwrap();
            buffer.push(snap);
        }

        assert_eq!(buffer.len(), 3);

        // Add 4th - should evict first
        let diff = FilesystemDiff::new();
        let state = ExecutionState::default();
        let metadata = SnapshotMetadata::new("snap3".to_string(), "hash".to_string());
        let snap = Snapshot::new(vec![3u8], diff, state, metadata).unwrap();
        buffer.push(snap);

        assert_eq!(buffer.len(), 3);
    }

    #[test]
    fn test_filesystem_diff_revert() {
        let mut diff = FilesystemDiff::new();
        diff.record_create("/tmp/test.txt".to_string(), b"hello".to_vec());
        diff.record_delete("/tmp/old.txt".to_string());

        let ops = diff.revert();
        assert_eq!(ops.len(), 2);
    }
}
