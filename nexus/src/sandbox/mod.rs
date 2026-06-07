//! Sandbox Module
//! 
//! WASM micro-sandbox with fuel metering for safe AI agent code execution.

pub mod wasm_runtime;
pub mod fuel_meter;
pub mod wasm_memory;

pub use wasm_runtime::{SandboxConfig, ExecutionResult, WasmSandbox};
pub use fuel_meter::{FuelMeter, FuelStats, presets};
pub use wasm_memory::{WasmMemoryState, WasmExecutionSnapshot, MemoryStats};