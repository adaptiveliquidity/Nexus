//! Execution Replay / Time-Travel Debugging
//!
//! Records lightweight, fuel-stamped checkpoints of an execution and exposes a
//! [`TraceReplay`] cursor to step forward/backward through them.
//!
//! ## Claim taxonomy (anti-overclaim)
//! - The trace/replay engine ([`ExecutionTrace`], [`TraceReplay`], [`hash_memory`])
//!   is a **benchmarked-primitive**: self-contained, deterministic, unit-tested
//!   with no wasmtime dependency.
//! - Recording integration ([`NexusHypervisor::execute_tool_traced`](crate::hypervisor::NexusHypervisor::execute_tool_traced))
//!   captures a checkpoint from the execution's already-captured state (memory
//!   hash + exported globals). It is **opt-in**.
//! - Fuel-interval checkpoint recording is provided by
//!   [`NexusHypervisor::record_trace`](crate::hypervisor::NexusHypervisor::record_trace)
//!   through bounded deterministic re-execution. A lower-cost single-pass
//!   paused recorder remains roadmap.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::snapshot::GlobalSnapshot;

/// Configuration for trace recording.
#[derive(Debug, Clone)]
pub struct TraceConfig {
    /// Fuel between bounded re-execution checkpoints.
    pub checkpoint_interval_fuel: u64,
    /// Hard cap on stored checkpoints per trace.
    pub max_checkpoints: usize,
    /// Whether to record the memory hash in each checkpoint.
    pub capture_memory: bool,
}

impl Default for TraceConfig {
    fn default() -> Self {
        TraceConfig {
            checkpoint_interval_fuel: 10_000,
            max_checkpoints: 256,
            capture_memory: true,
        }
    }
}

/// A lightweight point-in-time snapshot of an execution: fuel consumed so far,
/// a hash of linear memory, and the exported globals.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// 0-based position of this checkpoint within the trace.
    pub sequence: usize,
    /// Fuel consumed at this checkpoint.
    pub fuel_at: u64,
    /// Hex SHA-256 of linear memory (empty string when memory was not captured).
    pub memory_hash: String,
    /// Exported globals captured at this checkpoint.
    pub globals: Vec<GlobalSnapshot>,
}

/// A recorded execution: an ordered list of checkpoints for one tool run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionTrace {
    pub id: Uuid,
    pub tool_name: String,
    pub checkpoints: Vec<Checkpoint>,
}

impl ExecutionTrace {
    /// Start an empty trace for `tool_name`.
    pub fn new(tool_name: impl Into<String>) -> Self {
        ExecutionTrace {
            id: Uuid::new_v4(),
            tool_name: tool_name.into(),
            checkpoints: Vec::new(),
        }
    }

    /// Append a checkpoint, assigning its `sequence` from the current length.
    /// Returns `false` (and does not push) when `max` checkpoints is reached.
    pub fn push(
        &mut self,
        fuel_at: u64,
        memory_hash: String,
        globals: Vec<GlobalSnapshot>,
        max: usize,
    ) -> bool {
        if self.checkpoints.len() >= max {
            return false;
        }
        let sequence = self.checkpoints.len();
        self.checkpoints.push(Checkpoint {
            sequence,
            fuel_at,
            memory_hash,
            globals,
        });
        true
    }

    /// Number of checkpoints.
    pub fn len(&self) -> usize {
        self.checkpoints.len()
    }

    /// Whether the trace has no checkpoints.
    pub fn is_empty(&self) -> bool {
        self.checkpoints.is_empty()
    }

    /// Open a replay cursor over this trace.
    pub fn replay(&self) -> TraceReplay<'_> {
        TraceReplay {
            trace: self,
            cursor: 0,
        }
    }
}

/// Hex SHA-256 of `memory`. Deterministic across runs of the same bytes —
/// the property that makes hash-verified replay sound.
pub fn hash_memory(memory: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(memory);
    format!("{:x}", hasher.finalize())
}

/// A forward/backward cursor over an [`ExecutionTrace`]'s checkpoints.
pub struct TraceReplay<'a> {
    trace: &'a ExecutionTrace,
    cursor: usize,
}

impl<'a> TraceReplay<'a> {
    /// The checkpoint at the current cursor, or `None` if the trace is empty.
    pub fn current(&self) -> Option<&'a Checkpoint> {
        self.trace.checkpoints.get(self.cursor)
    }

    /// Advance one checkpoint. Returns the new current, or `None` if already at
    /// the last checkpoint (cursor unchanged).
    pub fn step_forward(&mut self) -> Option<&'a Checkpoint> {
        if self.cursor + 1 < self.trace.checkpoints.len() {
            self.cursor += 1;
            self.current()
        } else {
            None
        }
    }

    /// Go back one checkpoint. Returns the new current, or `None` if already at
    /// the first checkpoint (cursor unchanged).
    pub fn step_backward(&mut self) -> Option<&'a Checkpoint> {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.current()
        } else {
            None
        }
    }

    /// Jump to `idx`. Returns the checkpoint there, or `None` (cursor unchanged)
    /// if `idx` is out of bounds.
    pub fn goto_checkpoint(&mut self, idx: usize) -> Option<&'a Checkpoint> {
        if idx < self.trace.checkpoints.len() {
            self.cursor = idx;
            self.current()
        } else {
            None
        }
    }

    /// Fuel consumed at checkpoint `idx`.
    pub fn fuel_at(&self, idx: usize) -> Option<u64> {
        self.trace.checkpoints.get(idx).map(|c| c.fuel_at)
    }

    /// Current cursor position.
    pub fn position(&self) -> usize {
        self.cursor
    }

    /// Total checkpoints.
    pub fn len(&self) -> usize {
        self.trace.checkpoints.len()
    }

    /// Whether the underlying trace is empty.
    pub fn is_empty(&self) -> bool {
        self.trace.checkpoints.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trace_with(n: usize) -> ExecutionTrace {
        let mut t = ExecutionTrace::new("tool");
        for i in 0..n {
            t.push((i as u64) * 10_000, format!("hash{i}"), Vec::new(), 256);
        }
        t
    }

    #[test]
    fn hash_is_deterministic_and_distinct() {
        assert_eq!(hash_memory(b"abc"), hash_memory(b"abc"));
        assert_ne!(hash_memory(b"abc"), hash_memory(b"abd"));
        assert_eq!(hash_memory(b"abc").len(), 64); // hex sha256
    }

    #[test]
    fn push_assigns_sequence_and_respects_max() {
        let mut t = ExecutionTrace::new("tool");
        assert!(t.push(0, "h0".into(), Vec::new(), 2));
        assert!(t.push(1, "h1".into(), Vec::new(), 2));
        assert!(!t.push(2, "h2".into(), Vec::new(), 2)); // capped
        assert_eq!(t.len(), 2);
        assert_eq!(t.checkpoints[0].sequence, 0);
        assert_eq!(t.checkpoints[1].sequence, 1);
    }

    #[test]
    fn step_forward_walks_to_end_then_stops() {
        let t = trace_with(5);
        let mut r = t.replay();
        assert_eq!(r.current().unwrap().sequence, 0);
        for expected in 1..5 {
            assert_eq!(r.step_forward().unwrap().sequence, expected);
        }
        assert!(r.step_forward().is_none()); // at last, cannot advance
        assert_eq!(r.position(), 4);
    }

    #[test]
    fn step_backward_walks_to_start_then_stops() {
        let t = trace_with(3);
        let mut r = t.replay();
        r.goto_checkpoint(2);
        assert_eq!(r.step_backward().unwrap().sequence, 1);
        assert_eq!(r.step_backward().unwrap().sequence, 0);
        assert!(r.step_backward().is_none());
        assert_eq!(r.position(), 0);
    }

    #[test]
    fn goto_and_fuel_at() {
        let t = trace_with(5);
        let mut r = t.replay();
        assert_eq!(r.goto_checkpoint(3).unwrap().fuel_at, 30_000);
        assert_eq!(r.fuel_at(3), Some(30_000));
        assert!(r.goto_checkpoint(99).is_none()); // out of bounds, no move
        assert_eq!(r.position(), 3);
        assert_eq!(r.fuel_at(99), None);
    }

    #[test]
    fn empty_trace_has_no_checkpoints() {
        let t = ExecutionTrace::new("empty");
        let mut r = t.replay();
        assert_eq!(t.len(), 0);
        assert!(t.is_empty());
        assert!(r.current().is_none());
        assert!(r.step_forward().is_none());
        assert!(r.step_backward().is_none());
        assert!(r.goto_checkpoint(0).is_none());
    }

    #[test]
    fn replay_globals_match_recorded() {
        use crate::snapshot::{GlobalSnapshot, GlobalValue};
        let mut t = ExecutionTrace::new("tool");
        let g = vec![GlobalSnapshot {
            name: "counter".into(),
            value: GlobalValue::I32(42),
            mutable: true,
        }];
        t.push(1234, "h".into(), g.clone(), 256);
        let r = t.replay();
        assert_eq!(r.current().unwrap().globals, g);
    }
}
