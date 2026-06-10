//! Snapshot Module
//!
//! Provides snapshot creation, compression, and rollback capabilities.

pub mod compression;
pub mod differential;
pub mod manager;

pub use compression::{
    compress, compress_snapshot_memory, decompress, decompress_snapshot_memory, CompressedData,
    CompressionAlgo, CompressionConfig,
};
pub use differential::{
    apply_diff, apply_diff_chain, compute_dirty_pages, DiffSnapshot, PAGE_SIZE,
};
pub use manager::{
    restore_globals, restore_memory, DiffSnapshotResult, ExecutionState, FileChange, FilePath,
    FilesystemDiff, GlobalSnapshot, GlobalValue, RevertOperation, RollbackResult, Snapshot,
    SnapshotManager, SnapshotMetadata, SnapshotRingBuffer, SnapshotStats, TableSnapshot,
};
