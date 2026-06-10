//! Differential Snapshots
//!
//! Stores only the memory pages that changed since a base (full) snapshot,
//! making snapshot cost proportional to mutations rather than total memory size.

use crate::error::{NexusError, Result};
use crate::snapshot::manager::{ExecutionState, SnapshotMetadata};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

/// Page size in bytes (4 KB, matching the WebAssembly page granularity we track).
pub const PAGE_SIZE: usize = 4096;

/// A differential snapshot storing only pages that changed since a base snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffSnapshot {
    /// Unique identifier for this diff snapshot.
    pub id: Uuid,
    /// The full (or diff) snapshot this diff is based on.
    pub base_id: Uuid,
    /// Timestamp of snapshot creation.
    pub timestamp: DateTime<Utc>,
    /// Map of page_index -> compressed page bytes (4 KB pages).
    pub dirty_pages: HashMap<u32, Vec<u8>>,
    /// Number of pages that changed.
    pub dirty_count: u32,
    /// Total memory size in bytes (for validation on apply).
    pub total_memory_size: usize,
    /// Execution state at time of diff.
    pub execution_state: ExecutionState,
    /// Metadata.
    pub metadata: SnapshotMetadata,
    /// Generation: how many diffs deep from a full snapshot (1 = first diff).
    pub generation: u32,
}

/// Compare two memory slices page-by-page and return compressed bytes for every
/// page that differs. Both slices must have the same length.
pub fn compute_dirty_pages(
    base_memory: &[u8],
    current_memory: &[u8],
) -> Result<HashMap<u32, Vec<u8>>> {
    if base_memory.len() != current_memory.len() {
        return Err(NexusError::SnapshotCreateFailed(format!(
            "memory size mismatch: base={} current={}",
            base_memory.len(),
            current_memory.len()
        )));
    }

    let total_pages = base_memory.len().div_ceil(PAGE_SIZE);
    let mut dirty = HashMap::new();

    for page_idx in 0..total_pages {
        let start = page_idx * PAGE_SIZE;
        let end = std::cmp::min(start + PAGE_SIZE, base_memory.len());

        let base_page = &base_memory[start..end];
        let current_page = &current_memory[start..end];

        if base_page != current_page {
            let mut compressed = Vec::new();
            zstd::stream::copy_encode(current_page, &mut compressed, 3).map_err(|e| {
                NexusError::SerializationError(format!("page compression failed: {e}"))
            })?;
            dirty.insert(page_idx as u32, compressed);
        }
    }

    Ok(dirty)
}

impl DiffSnapshot {
    /// Create a differential snapshot by comparing current memory against a base.
    pub fn new(
        base_id: Uuid,
        base_memory: &[u8],
        current_memory: &[u8],
        execution_state: ExecutionState,
        metadata: SnapshotMetadata,
        generation: u32,
    ) -> Result<Self> {
        let dirty_pages = compute_dirty_pages(base_memory, current_memory)?;
        let dirty_count = dirty_pages.len() as u32;

        Ok(DiffSnapshot {
            id: Uuid::new_v4(),
            base_id,
            timestamp: Utc::now(),
            dirty_pages,
            dirty_count,
            total_memory_size: current_memory.len(),
            execution_state,
            metadata,
            generation,
        })
    }

    /// Total compressed size of dirty pages (useful for statistics).
    pub fn compressed_size(&self) -> usize {
        self.dirty_pages.values().map(|v| v.len()).sum()
    }
}

/// Reconstruct full memory by overlaying a single diff's dirty pages onto base memory.
pub fn apply_diff(base_memory: &[u8], diff: &DiffSnapshot) -> Result<Vec<u8>> {
    if base_memory.len() != diff.total_memory_size {
        return Err(NexusError::RollbackFailed(format!(
            "base memory size ({}) does not match diff total_memory_size ({})",
            base_memory.len(),
            diff.total_memory_size
        )));
    }

    let mut result = base_memory.to_vec();

    for (&page_idx, compressed) in &diff.dirty_pages {
        let start = page_idx as usize * PAGE_SIZE;
        let end = std::cmp::min(start + PAGE_SIZE, result.len());

        if start >= result.len() {
            return Err(NexusError::RollbackFailed(format!(
                "dirty page index {} out of bounds for memory size {}",
                page_idx,
                result.len()
            )));
        }

        let mut decompressed = Vec::new();
        zstd::stream::copy_decode(&compressed[..], &mut decompressed).map_err(|e| {
            NexusError::SerializationError(format!("page decompression failed: {e}"))
        })?;

        let page_len = end - start;
        if decompressed.len() != page_len {
            return Err(NexusError::RollbackFailed(format!(
                "decompressed page {} has size {} but expected {}",
                page_idx,
                decompressed.len(),
                page_len
            )));
        }

        result[start..end].copy_from_slice(&decompressed);
    }

    Ok(result)
}

/// Apply a chain of diffs in order onto base memory, producing the final state.
///
/// `diffs` must be ordered from oldest to newest (i.e. the first diff's base_id
/// should correspond to the full snapshot whose memory is `base_memory`).
pub fn apply_diff_chain(base_memory: &[u8], diffs: &[&DiffSnapshot]) -> Result<Vec<u8>> {
    let mut current = base_memory.to_vec();
    for diff in diffs {
        current = apply_diff(&current, diff)?;
    }
    Ok(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_metadata() -> SnapshotMetadata {
        SnapshotMetadata::new("test".to_string(), "hash".to_string())
    }

    #[test]
    fn identical_memory_produces_empty_diff() {
        let mem = vec![42u8; PAGE_SIZE * 4];
        let dirty = compute_dirty_pages(&mem, &mem).unwrap();
        assert!(dirty.is_empty());
    }

    #[test]
    fn single_page_change_detected() {
        let base = vec![0u8; PAGE_SIZE * 4];
        let mut current = base.clone();
        current[PAGE_SIZE] = 0xFF; // modify page 1

        let dirty = compute_dirty_pages(&base, &current).unwrap();
        assert_eq!(dirty.len(), 1);
        assert!(dirty.contains_key(&1));
    }

    #[test]
    fn apply_diff_reconstructs_memory() {
        let base = vec![0u8; PAGE_SIZE * 4];
        let mut current = base.clone();
        current[0] = 1;
        current[PAGE_SIZE * 2] = 2;

        let diff = DiffSnapshot::new(
            Uuid::new_v4(),
            &base,
            &current,
            ExecutionState::default(),
            make_metadata(),
            1,
        )
        .unwrap();

        let reconstructed = apply_diff(&base, &diff).unwrap();
        assert_eq!(reconstructed, current);
    }

    #[test]
    fn chain_applies_correctly() {
        let base = vec![0u8; PAGE_SIZE * 4];

        // First mutation
        let mut mem1 = base.clone();
        mem1[0] = 1;
        let diff1 = DiffSnapshot::new(
            Uuid::new_v4(),
            &base,
            &mem1,
            ExecutionState::default(),
            make_metadata(),
            1,
        )
        .unwrap();

        // Second mutation (on top of mem1)
        let mut mem2 = mem1.clone();
        mem2[PAGE_SIZE] = 2;
        let diff2 = DiffSnapshot::new(
            diff1.id,
            &mem1,
            &mem2,
            ExecutionState::default(),
            make_metadata(),
            2,
        )
        .unwrap();

        let reconstructed = apply_diff_chain(&base, &[&diff1, &diff2]).unwrap();
        assert_eq!(reconstructed, mem2);
    }

    #[test]
    fn mismatched_sizes_error() {
        let base = vec![0u8; PAGE_SIZE];
        let current = vec![0u8; PAGE_SIZE * 2];
        assert!(compute_dirty_pages(&base, &current).is_err());
    }
}
