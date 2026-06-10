//! Snapshot Manager with Ring Buffer
//!
//! Provides microsecond snapshots and instant rollback for WASM state.

use crate::error::{NexusError, Result};
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
    ) -> Self {
        let original_size = memory.len();

        // Compress memory
        let mut compressed = Vec::new();
        let compression_level = 3; // Balance speed and size
        zstd::stream::copy_encode(&memory[..], &mut compressed, compression_level)
            .expect("compression should not fail");

        let compressed_size = compressed.len();

        // Calculate checksum
        let mut hasher = Sha256::new();
        hasher.update(&memory);
        let memory_checksum = format!("{:x}", hasher.finalize());

        Snapshot {
            id: Uuid::new_v4(),
            timestamp: Utc::now(),
            memory: compressed,
            memory_checksum,
            fs_changes,
            execution_state,
            metadata,
            original_size,
            compressed_size,
        }
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
    /// Program counter
    pub pc: u64,
    /// Stack pointer
    pub sp: u64,
    /// Current stack depth
    pub stack_depth: u32,
    /// Call stack (function addresses)
    pub call_stack: Vec<u64>,
    /// Local variables
    pub locals: Vec<i32>,
    /// Global values
    pub globals: Vec<i64>,
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
        }

        let id = snapshot.id;
        self.snapshots.push_back(snapshot);
        self.index.insert(id, self.snapshots.len() - 1);
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

/// Snapshot manager with compression and persistence
pub struct SnapshotManager {
    /// Ring buffer for in-memory snapshots
    buffer: RwLock<SnapshotRingBuffer>,
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

impl SnapshotManager {
    /// Create a new snapshot manager
    pub fn new(capacity: usize) -> Self {
        SnapshotManager {
            buffer: RwLock::new(SnapshotRingBuffer::new(capacity)),
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
            persist_enabled: true,
            persist_dir: Some(persist_dir),
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

        let snapshot = Snapshot::new(memory, fs_changes, execution_state, metadata);

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_snapshot_compression() {
        let data = vec![0u8; 10000]; // 10KB of zeros
        let diff = FilesystemDiff::new();
        let state = ExecutionState::default();
        let metadata = SnapshotMetadata::new("test".to_string(), "hash".to_string());

        let snapshot = Snapshot::new(data.clone(), diff, state, metadata);

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
            let snap = Snapshot::new(vec![i as u8], diff, state, metadata);
            buffer.push(snap);
        }

        assert_eq!(buffer.len(), 3);

        // Add 4th - should evict first
        let diff = FilesystemDiff::new();
        let state = ExecutionState::default();
        let metadata = SnapshotMetadata::new("snap3".to_string(), "hash".to_string());
        let snap = Snapshot::new(vec![3u8], diff, state, metadata);
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
