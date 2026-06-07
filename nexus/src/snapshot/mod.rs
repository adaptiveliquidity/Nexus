//! Snapshot Module
//! 
//! Provides snapshot creation, compression, and rollback capabilities.

pub mod manager;
pub mod compression;

pub use manager::{
    Snapshot, SnapshotManager, SnapshotRingBuffer, SnapshotStats,
    FilesystemDiff, FileChange, FilePath, ExecutionState, SnapshotMetadata,
    RevertOperation, RollbackResult,
};
pub use compression::{
    CompressionAlgo, CompressionConfig, CompressedData,
    compress, decompress, compress_snapshot_memory, decompress_snapshot_memory,
};