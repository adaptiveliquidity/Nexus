//! WASM Memory State Capture and Restore
//!
//! Captures actual WASM linear memory for true state snapshots.

use serde::{Deserialize, Serialize};

/// Captured WASM memory state (serializable for snapshots)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasmMemoryState {
    /// Memory pages (each page is 64KB)
    pub pages: Vec<Vec<u8>>,
    /// Current page count
    pub page_count: u32,
    /// Memory size in bytes
    pub size_bytes: usize,
}

impl WasmMemoryState {
    /// Create empty memory state
    pub fn empty() -> Self {
        WasmMemoryState {
            pages: Vec::new(),
            page_count: 0,
            size_bytes: 0,
        }
    }

    /// Create from raw memory bytes
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let page_count = bytes.len().div_ceil(65536) as u32;
        let size_bytes = bytes.len();

        // Split into pages
        let pages: Vec<Vec<u8>> = bytes.chunks(65536).map(|chunk| chunk.to_vec()).collect();

        WasmMemoryState {
            pages,
            page_count,
            size_bytes,
        }
    }

    /// Get total memory size
    pub fn total_size(&self) -> usize {
        self.pages.iter().map(|p| p.len()).sum()
    }

    /// Get raw bytes
    pub fn as_bytes(&self) -> Vec<u8> {
        let mut result = Vec::with_capacity(self.size_bytes);
        for page in &self.pages {
            result.extend_from_slice(page);
        }
        result
    }

    /// Get compression ratio
    pub fn compression_info(&self) -> (usize, usize) {
        (self.size_bytes, self.total_size())
    }
}

/// Snapshot of WASM execution state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasmExecutionSnapshot {
    pub memory: WasmMemoryState,
    pub captured_globals: Vec<crate::snapshot::GlobalSnapshot>,
    pub captured_tables: Vec<crate::snapshot::TableSnapshot>,
}

impl WasmExecutionSnapshot {
    pub fn new(memory: WasmMemoryState) -> Self {
        WasmExecutionSnapshot {
            memory,
            captured_globals: Vec::new(),
            captured_tables: Vec::new(),
        }
    }

    pub fn empty() -> Self {
        WasmExecutionSnapshot::new(WasmMemoryState::empty())
    }

    pub fn size_bytes(&self) -> usize {
        self.memory.total_size() + 16
    }
}

/// Memory operation statistics
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryStats {
    pub captures: u64,
    pub restorations: u64,
    pub total_bytes_captured: u64,
    pub total_bytes_restored: u64,
}

impl MemoryStats {
    /// Create new stats
    pub fn new() -> Self {
        MemoryStats::default()
    }

    /// Record a capture operation
    pub fn record_capture(&mut self, bytes: usize) {
        self.captures += 1;
        self.total_bytes_captured += bytes as u64;
    }

    /// Record a restore operation
    pub fn record_restore(&mut self, bytes: usize) {
        self.restorations += 1;
        self.total_bytes_restored += bytes as u64;
    }

    /// Get capture rate (bytes per capture)
    pub fn avg_capture_size(&self) -> f64 {
        if self.captures == 0 {
            0.0
        } else {
            self.total_bytes_captured as f64 / self.captures as f64
        }
    }
}
