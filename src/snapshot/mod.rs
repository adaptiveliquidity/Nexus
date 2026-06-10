//! Snapshot Module
//!
//! Provides snapshot creation, compression, and rollback capabilities.

pub mod compression;
pub mod manager;

pub use compression::{
    compress, compress_snapshot_memory, decompress, decompress_snapshot_memory, CompressedData,
    CompressionAlgo, CompressionConfig,
};
pub use manager::{
    ExecutionState, FileChange, FilePath, FilesystemDiff, RevertOperation, RollbackResult,
    Snapshot, SnapshotManager, SnapshotMetadata, SnapshotRingBuffer, SnapshotStats,
};
