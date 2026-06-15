//! Diagnostic-only WASM call-stack capture.
//!
//! RFC 0002 Option A deliberately treats these frames as telemetry metadata.
//! Wasmtime 45 exposes frame identities for diagnostics, but not operand-stack
//! values, locals, or any public primitive to serialize and restore execution.

use serde::{Deserialize, Serialize};
use wasmtime::WasmBacktrace;

/// Where a diagnostic call stack was captured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaptureSite {
    /// Captured when a WASM call trapped or otherwise failed.
    Trap,
    /// Captured at an explicit diagnostic checkpoint.
    Checkpoint,
}

/// Owned, serializable WASM call-stack metadata.
///
/// This is not restorable execution state and must not be included in snapshot
/// bytes, snapshot digests, or memory checksums.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapturedCallStack {
    pub frames: Vec<StackFrame>,
    pub captured_at: CaptureSite,
}

/// One WASM frame as exposed by wasmtime 45 backtrace metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackFrame {
    pub module_name: Option<String>,
    pub func_index: u32,
    pub func_name: Option<String>,
    pub module_offset: Option<u32>,
    pub func_offset: Option<u32>,
}

impl CapturedCallStack {
    /// Convert wasmtime's store-tied diagnostic backtrace into owned metadata.
    pub fn from_wasm_backtrace(backtrace: &WasmBacktrace, captured_at: CaptureSite) -> Self {
        let frames = backtrace
            .frames()
            .iter()
            .map(|frame| StackFrame {
                module_name: frame.module().name().map(str::to_string),
                func_index: frame.func_index(),
                func_name: frame.func_name().map(str::to_string),
                module_offset: frame.module_offset().and_then(to_u32),
                func_offset: frame.func_offset().and_then(to_u32),
            })
            .collect();

        CapturedCallStack {
            frames,
            captured_at,
        }
    }

    /// Top frames for compact diagnostic prompts.
    pub fn top_frames(&self, limit: usize) -> impl Iterator<Item = &StackFrame> {
        self.frames.iter().take(limit)
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }
}

fn to_u32(value: usize) -> Option<u32> {
    u32::try_from(value).ok()
}
