//! Differential Snapshot Integration Tests
//!
//! End-to-end tests for the differential snapshot system, exercising
//! `DiffSnapshot`, `compute_dirty_pages`, `apply_diff`, `apply_diff_chain`,
//! and the `SnapshotManager` diff helpers.

use nexus::{
    apply_diff, apply_diff_chain, compute_dirty_pages, DiffSnapshot, DiffSnapshotResult,
    ExecutionState, FilesystemDiff, SnapshotManager, SnapshotMetadata, PAGE_SIZE,
};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_metadata(name: &str) -> SnapshotMetadata {
    SnapshotMetadata::new(name.to_string(), "test-hash".to_string())
}

fn make_state() -> ExecutionState {
    ExecutionState::default()
}

// ---------------------------------------------------------------------------
// 1. diff_captures_only_changed_pages
// ---------------------------------------------------------------------------

/// Modify exactly 2 pages of a 256-page memory and verify only those 2 pages
/// appear in `dirty_pages`.
#[test]
fn diff_captures_only_changed_pages() {
    let total_pages = 256;
    let mem_size = total_pages * PAGE_SIZE;
    let base = vec![0u8; mem_size];
    let mut current = base.clone();

    // Touch page 10 and page 200
    current[10 * PAGE_SIZE] = 0xAB;
    current[200 * PAGE_SIZE + 100] = 0xCD;

    let dirty = compute_dirty_pages(&base, &current).unwrap();

    assert_eq!(dirty.len(), 2, "expected exactly 2 dirty pages");
    assert!(dirty.contains_key(&10), "page 10 should be dirty");
    assert!(dirty.contains_key(&200), "page 200 should be dirty");
}

// ---------------------------------------------------------------------------
// 2. apply_diff_reconstructs_exact_memory
// ---------------------------------------------------------------------------

/// Create a base snapshot, modify several bytes, produce a diff, then apply it
/// and verify byte-exact equality with the modified memory.
#[test]
fn apply_diff_reconstructs_exact_memory() {
    let mem_size = 64 * PAGE_SIZE;
    let base = vec![0xAAu8; mem_size];
    let mut current = base.clone();

    // Scatter mutations across different pages
    current[0] = 0x00;
    current[PAGE_SIZE * 5 + 17] = 0xFF;
    current[PAGE_SIZE * 63 + PAGE_SIZE - 1] = 0x42;

    let diff = DiffSnapshot::new(
        Uuid::new_v4(),
        &base,
        &current,
        make_state(),
        make_metadata("reconstruct"),
        1,
    )
    .unwrap();

    let reconstructed = apply_diff(&base, &diff).unwrap();
    assert_eq!(
        reconstructed, current,
        "reconstructed memory must match current exactly"
    );
}

// ---------------------------------------------------------------------------
// 3. diff_chain_reconstructs_correctly
// ---------------------------------------------------------------------------

/// base -> diff1 -> diff2 -> reconstruct, verify all modifications are present.
#[test]
fn diff_chain_reconstructs_correctly() {
    let mem_size = 32 * PAGE_SIZE;
    let base = vec![0u8; mem_size];

    // First mutation: write page 0
    let mut mem1 = base.clone();
    mem1[0] = 0x11;
    mem1[1] = 0x22;

    let base_id = Uuid::new_v4();
    let diff1 = DiffSnapshot::new(
        base_id,
        &base,
        &mem1,
        make_state(),
        make_metadata("diff1"),
        1,
    )
    .unwrap();

    // Second mutation (on top of mem1): write page 5
    let mut mem2 = mem1.clone();
    mem2[5 * PAGE_SIZE] = 0x33;
    mem2[5 * PAGE_SIZE + 1] = 0x44;

    let diff2 = DiffSnapshot::new(
        diff1.id,
        &mem1,
        &mem2,
        make_state(),
        make_metadata("diff2"),
        2,
    )
    .unwrap();

    let reconstructed = apply_diff_chain(&base, &[&diff1, &diff2]).unwrap();

    assert_eq!(reconstructed[0], 0x11);
    assert_eq!(reconstructed[1], 0x22);
    assert_eq!(reconstructed[5 * PAGE_SIZE], 0x33);
    assert_eq!(reconstructed[5 * PAGE_SIZE + 1], 0x44);
    assert_eq!(reconstructed, mem2);
}

// ---------------------------------------------------------------------------
// 4. empty_diff_for_identical_memory
// ---------------------------------------------------------------------------

/// When memory hasn't changed at all, `dirty_pages` must be empty.
#[test]
fn empty_diff_for_identical_memory() {
    let mem = vec![0xFFu8; 16 * PAGE_SIZE];

    let diff = DiffSnapshot::new(
        Uuid::new_v4(),
        &mem,
        &mem,
        make_state(),
        make_metadata("no-change"),
        1,
    )
    .unwrap();

    assert!(
        diff.dirty_pages.is_empty(),
        "identical memory should produce zero dirty pages"
    );
    assert_eq!(diff.dirty_count, 0);
}

// ---------------------------------------------------------------------------
// 5. auto_promote_to_full_after_max_depth
// ---------------------------------------------------------------------------

/// Create `max_diff_depth + 1` diffs; the last one should be auto-promoted
/// to a full snapshot.
#[test]
fn auto_promote_to_full_after_max_depth() {
    let max_depth: u32 = 3; // small for fast testing
    let manager = SnapshotManager::with_max_diff_depth(16, max_depth);

    let mem_size = 4 * PAGE_SIZE;
    let base_memory = vec![0u8; mem_size];

    // Create a full base snapshot
    let base_snap = manager
        .create_snapshot(
            base_memory.clone(),
            FilesystemDiff::new(),
            make_state(),
            make_metadata("base"),
        )
        .unwrap();

    let mut prev_id = base_snap.id;
    let mut current_mem = base_memory.clone();

    // Create exactly max_depth diffs (generations 1..=max_depth)
    for i in 0..max_depth {
        current_mem[(i as usize) * PAGE_SIZE] = (i + 1) as u8;

        let result = manager
            .create_diff_snapshot(
                current_mem.clone(),
                &prev_id,
                make_state(),
                make_metadata(&format!("diff-{}", i)),
            )
            .unwrap();

        match &result {
            DiffSnapshotResult::Diff(d) => {
                prev_id = d.id;
            }
            DiffSnapshotResult::Promoted(_) => {
                panic!(
                    "should NOT promote at generation {} (max_depth={})",
                    i + 1,
                    max_depth
                );
            }
        }
    }

    // The next diff (generation max_depth + 1) should trigger promotion
    current_mem[max_depth as usize * PAGE_SIZE] = 0xFF;
    let result = manager
        .create_diff_snapshot(
            current_mem.clone(),
            &prev_id,
            make_state(),
            make_metadata("should-promote"),
        )
        .unwrap();

    assert!(
        matches!(result, DiffSnapshotResult::Promoted(_)),
        "exceeding max_diff_depth should auto-promote to a full snapshot"
    );
}

// ---------------------------------------------------------------------------
// 6. diff_snapshot_size_is_proportional_to_changes
// ---------------------------------------------------------------------------

/// With 32 MB of memory and only a 4 KB change, the diff's compressed size
/// should be orders of magnitude smaller than a full snapshot.
#[test]
fn diff_snapshot_size_is_proportional_to_changes() {
    let mem_size = 32 * 1024 * 1024; // 32 MB
    let base = vec![0u8; mem_size];
    let mut current = base.clone();

    // Touch exactly one page
    current[PAGE_SIZE * 100] = 0xDE;
    current[PAGE_SIZE * 100 + 1] = 0xAD;

    let diff = DiffSnapshot::new(
        Uuid::new_v4(),
        &base,
        &current,
        make_state(),
        make_metadata("proportional"),
        1,
    )
    .unwrap();

    assert_eq!(diff.dirty_count, 1, "only one page should be dirty");

    let diff_compressed = diff.compressed_size();

    // A full zstd-compressed 32 MB of zeros is small, but the diff should
    // still be far smaller since it only stores one 4 KB page.
    // The diff should be well under 1 KB (zstd on a mostly-zero 4 KB page).
    assert!(
        diff_compressed < 4096,
        "compressed diff ({} bytes) should be less than one raw page",
        diff_compressed
    );

    // Also verify it's dramatically smaller than the raw memory
    assert!(
        diff_compressed < mem_size / 1000,
        "compressed diff should be >1000x smaller than raw memory"
    );
}

// ---------------------------------------------------------------------------
// 7. rollback_to_diff_returns_correct_state
// ---------------------------------------------------------------------------

/// End-to-end test through `SnapshotManager`: create a full snapshot, create
/// a diff, then roll back to the diff and verify the reconstructed memory.
#[test]
fn rollback_to_diff_returns_correct_state() {
    let manager = SnapshotManager::new(16);
    let mem_size = 8 * PAGE_SIZE;

    let base_memory = vec![0u8; mem_size];

    // Create a full base snapshot
    let base_snap = manager
        .create_snapshot(
            base_memory.clone(),
            FilesystemDiff::new(),
            make_state(),
            make_metadata("base"),
        )
        .unwrap();

    // Mutate memory
    let mut modified = base_memory.clone();
    modified[0] = 0x42;
    modified[PAGE_SIZE * 3] = 0x99;
    modified[PAGE_SIZE * 7 + 10] = 0xEE;

    // Create a diff snapshot
    let diff_result = manager
        .create_diff_snapshot(
            modified.clone(),
            &base_snap.id,
            make_state(),
            make_metadata("diff-1"),
        )
        .unwrap();

    let diff_id = match &diff_result {
        DiffSnapshotResult::Diff(d) => d.id,
        DiffSnapshotResult::Promoted(_) => panic!("unexpected promotion"),
    };

    // Rollback to the diff
    let rollback = manager.rollback_to_diff(&diff_id).unwrap();

    assert_eq!(rollback.snapshot_id, diff_id);
    assert_eq!(
        rollback.memory, modified,
        "rollback memory must match the modified state"
    );
}
